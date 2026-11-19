// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that declares an `AWS::MediaConvert::Queue`, an
//! `AWS::MediaConvert::JobTemplate` or an `AWS::MediaConvert::Preset` really creates it in
//! AWS Elemental MediaConvert, and the handle the stack publishes addresses the resource that
//! service holds.
//!
//! The rule under test: **a resource of a type this emulator provisions is created in the
//! service that OWNS it, through that service's own public API and with every declared
//! property applied — so the identifier a stack publishes for it resolves in that service's
//! own reads right after the stack completes, a changed declaration is applied to the same
//! resource, deleting the stack releases it there, and a declaration the owning service
//! REFUSES fails the resource instead of being reported as created.** For these three types
//! the identifier is the NAME: "Ref returns the name" for each, which is what `GetQueue`,
//! `GetJobTemplate` and `GetPreset` address one by, and `Fn::GetAtt Arn` is the ARN that
//! service reports for it. Two crates have to agree for that to hold (CloudFormation makes the
//! calls, MediaConvert serves them), so the suite lives here: a crate-local test can only ever
//! see one side of it.
//!
//! The rows differ along what the rule generalises over: whether the template NAMES each
//! resource or leaves the name to CloudFormation, whether it is written in JSON or YAML, which
//! optional properties it declares (description, status, category, priority, hop destinations,
//! acceleration settings, tags), whether the job template names its queue by `Ref` or by
//! `Fn::GetAtt … Arn`, whether a number is written as JSON's own or as the text a `String`
//! parameter resolves to, and the Region the stack is deployed in.
//!
//! No expected value is read out of the answer being checked: each is either what the row's own
//! template declares, or what the OWNING service reports for the resource.
//!
//! The awsQuery form encoder and the XML element readers are this test crate's own
//! ([`crate::query_wire`]), not another copy.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::query_wire::{elements, form_encode, opt_element, outputs_of};

/// How long one `DescribeStacks` poll waits before the next, and how many are made.
///
/// A stack's own status transitions are owed on its clock, so the suite waits for the STATUS
/// rather than for a span, with a deadline generous enough for a loaded host.
const POLL_INTERVAL: Duration = Duration::from_millis(50);
/// See [`POLL_INTERVAL`].
const POLLS: usize = 600;

/// The account id every ARN this emulator mints carries, which is the account whose
/// MediaConvert resources these stacks create.
const ACCOUNT: &str = "000000000000";

/// The message AWS words when a reserved queue is submitted without the reservation plan
/// settings such a queue requires, which `AWS::MediaConvert::Queue` publishes no property for
/// at all — so every `PricingPlan: RESERVED` declaration reaches `CreateQueue` without one.
const RESERVED_WITHOUT_A_PLAN: &str =
    "You must provide reservation plan settings when creating a reserved queue.";

/// The emulator with the two services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them. Request authentication is off, as in every other suite
/// here; the in-process callback CloudFormation makes signs its request either way (the
/// credential scope is what routes it).
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_mediaconvert_mediaconvert::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service` in `region`, which is
/// what routes the request and what each handler reads its Region from. The signature is never
/// checked: this router runs with request authentication off.
fn authorization(service: &str, region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{region}/{service}/aws4_request, \
         SignedHeaders=host, Signature=000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one CloudFormation operation over its awsQuery wire, signed for `region`.
async fn cloudformation(
    router: &MultiServiceRouter,
    region: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cloudformation.{region}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation", region))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = String::from_utf8_lossy(response.body()).to_string();
    (status, payload)
}

/// Call one AWS Elemental MediaConvert operation over the `restJson1` wire its model declares,
/// on the URI that operation's own `@http` binding states, signed for `region`.
async fn mediaconvert(
    router: &MultiServiceRouter,
    region: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let payload = match &body {
        Some(document) => serde_json::to_vec(document).expect("the request body serializes"),
        None => Vec::new(),
    };
    let request = http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", format!("mediaconvert.{region}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("mediaconvert", region))
        .body(Bytes::from(payload))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let answered: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, answered)
}

/// Every entry of one of MediaConvert's paginated listings, read through every page its
/// `nextToken` offers — which is how a client reconciling its estate reads one.
async fn listed(
    router: &MultiServiceRouter,
    region: &str,
    collection: &str,
    member: &str,
    case: &str,
) -> Vec<Value> {
    let mut entries = Vec::new();
    let mut token: Option<String> = None;
    // Each page carries at least one entry, so the number of pages is bounded by the number of
    // resources any row of this suite creates; the bound is generous.
    for _ in 0..64 {
        let path = match &token {
            Some(token) => format!("/2017-08-29/{collection}?nextToken={token}"),
            None => format!("/2017-08-29/{collection}"),
        };
        let (status, answered) = mediaconvert(router, region, "GET", &path, None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from the {collection} listing, got {status} with \
             body {answered}"
        );
        entries.extend(
            answered
                .get(member)
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
        );
        match answered.get("nextToken").and_then(Value::as_str) {
            Some(next) if !next.is_empty() => token = Some(next.to_string()),
            _ => return entries,
        }
    }
    panic!("case {case:?}: the {collection} listing never ran out of pages");
}

/// Wait for `stack` to report `wanted`, looking again until it does or the deadline passes. A
/// stack that reaches another TERMINAL status fails the suite at once, quoting the answer: the
/// reason a resource failed is in it.
async fn wait_for_stack_status(
    router: &MultiServiceRouter,
    region: &str,
    stack: &str,
    wanted: &str,
) -> String {
    let mut body = String::new();
    for _ in 0..POLLS {
        let (status, reported) =
            cloudformation(router, region, "DescribeStacks", &[("StackName", stack)]).await;
        assert_eq!(
            status, 200,
            "DescribeStacks {stack:?}: expected HTTP 200, got {status} with body {reported}"
        );
        body = reported;
        let reported_status = opt_element(&body, "StackStatus").unwrap_or_default();
        if reported_status == wanted {
            return body;
        }
        assert!(
            !reported_status.ends_with("_FAILED") && !reported_status.starts_with("ROLLBACK"),
            "stack {stack:?}: expected {wanted:?}, got {reported_status:?} with answer {body}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!("stack {stack:?} never reached {wanted:?}; last answer was {body}");
}

/// Wait for `stack` to reach any TERMINAL status, which is what a deployment that fails ends in.
async fn wait_for_terminal_status(
    router: &MultiServiceRouter,
    region: &str,
    stack: &str,
) -> (String, String) {
    let mut body = String::new();
    for _ in 0..POLLS {
        let (status, reported) =
            cloudformation(router, region, "DescribeStacks", &[("StackName", stack)]).await;
        assert_eq!(
            status, 200,
            "DescribeStacks {stack:?}: expected HTTP 200, got {status} with body {reported}"
        );
        body = reported;
        let reported_status = opt_element(&body, "StackStatus").unwrap_or_default();
        if !reported_status.ends_with("_IN_PROGRESS") {
            return (reported_status, body);
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!("stack {stack:?} never reached a terminal status; last answer was {body}");
}

/// One output of a `DescribeStacks` answer, or a panic naming the case and what was there.
fn output(outputs: &BTreeMap<String, String>, key: &str, case: &str) -> String {
    outputs
        .get(key)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected an output {key:?}, got {outputs:?}"))
}

/// The ARN AWS Elemental MediaConvert reports for one of its resources: that service's own
/// `arnNamespace` with the PLURAL resource-type segment its answers carry
/// (`arn:aws:mediaconvert:<region>:<account>:queues/<name>`, as `Queue$Arn`,
/// `JobTemplate$Arn` and `Preset$Arn` report it).
fn mediaconvert_arn(region: &str, segment: &str, name: &str) -> String {
    format!("arn:aws:mediaconvert:{region}:{ACCOUNT}:{segment}/{name}")
}

/// The tags MediaConvert holds on one resource, read through its own `ListTagsForResource`.
async fn tags_on(
    router: &MultiServiceRouter,
    region: &str,
    resource_arn: &str,
    case: &str,
) -> BTreeMap<String, String> {
    let (status, listed) = mediaconvert(
        router,
        region,
        "GET",
        &format!("/2017-08-29/tags/{}", encoded_segment(resource_arn)),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from ListTagsForResource on {resource_arn}, got \
         {status} with body {listed}"
    );
    listed
        .pointer("/resourceTags/tags")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|(key, value)| Some((key.clone(), value.as_str()?.to_string())))
        .collect()
}

/// One value as a single URI path segment: everything outside RFC 3986's unreserved set
/// escaped, which is what an AWS SDK sends for a non-greedy `@httpLabel` — the form the ARN a
/// tag read addresses, and a name a read addresses, have to travel in.
///
/// That escaping is this test crate's one encoder ([`crate::query_wire::form_encode`]), which
/// leaves exactly the unreserved set literal: a suite that writes its own drifts from the
/// others.
fn encoded_segment(value: &str) -> String {
    form_encode(value)
}

/// One row of the table: a template, and what the rule says the three resources it declares
/// must then be in MediaConvert.
struct Row {
    case: &'static str,
    /// The Region the row deploys in, which is the Region the resources are created in.
    region: &'static str,
    /// The stack name the row deploys under, whose NAME is the first segment of any name
    /// CloudFormation generates for a resource the template does not name.
    stack: &'static str,
    /// The template body, in the language the row writes it in.
    template: &'static str,
    /// The queue name the template declares, or `None` for a template naming none — whose
    /// queue carries the name CloudFormation generated.
    declared_queue: Option<&'static str>,
    /// The job template's declared name, under the same rule.
    declared_job_template: Option<&'static str>,
    /// The preset's declared name, under the same rule.
    declared_preset: Option<&'static str>,
    /// The members `GetQueue` must report, as `(the member path, the JSON value)`.
    expected_queue: &'static [(&'static str, &'static str)],
    /// The members `GetJobTemplate` must report, under the same convention.
    expected_job_template: &'static [(&'static str, &'static str)],
    /// The members `GetPreset` must report, under the same convention.
    expected_preset: &'static [(&'static str, &'static str)],
    /// The tags the queue's own declaration asks for.
    expected_tags: &'static [(&'static str, &'static str)],
}

const ROWS: &[Row] = &[
    Row {
        case: "named resources, a paused queue, a job template naming its queue by GetAtt Arn, \
               in JSON",
        region: "us-west-2",
        stack: "mediaconvert-json-stack",
        template: r#"{
          "AWSTemplateFormatVersion": "2010-09-09",
          "Resources": {
            "Queue": {
              "Type": "AWS::MediaConvert::Queue",
              "Properties": {
                "Name": "orders-queue",
                "Description": "the orders transcoding queue",
                "PricingPlan": "ON_DEMAND",
                "Status": "PAUSED",
                "Tags": {"Owner": "video", "Tier": "prod"}
              }
            },
            "Template": {
              "Type": "AWS::MediaConvert::JobTemplate",
              "Properties": {
                "Name": "orders-template",
                "Description": "the orders job template",
                "Category": "orders",
                "Priority": 10,
                "StatusUpdateInterval": "SECONDS_60",
                "Queue": {"Fn::GetAtt": ["Queue", "Arn"]},
                "AccelerationSettings": {"Mode": "DISABLED"},
                "SettingsJson": {
                  "inputs": [{"timecodeSource": "ZEROBASED"}],
                  "outputGroups": [{
                    "name": "File Group",
                    "outputs": [{
                      "nameModifier": "_1",
                      "containerSettings": {"container": "MP4"}
                    }],
                    "outputGroupSettings": {
                      "type": "FILE_GROUP_SETTINGS",
                      "fileGroupSettings": {"destination": "s3://orders-out/"}
                    }
                  }]
                }
              }
            },
            "Preset": {
              "Type": "AWS::MediaConvert::Preset",
              "Properties": {
                "Name": "orders-preset",
                "Description": "the orders output preset",
                "Category": "orders",
                "SettingsJson": {
                  "containerSettings": {"container": "MP4"},
                  "videoDescription": {"width": 1280, "height": 720}
                }
              }
            }
          },
          "Outputs": {
            "QueueRef": {"Value": {"Ref": "Queue"}},
            "QueueArn": {"Value": {"Fn::GetAtt": ["Queue", "Arn"]}},
            "QueueName": {"Value": {"Fn::GetAtt": ["Queue", "Name"]}},
            "TemplateRef": {"Value": {"Ref": "Template"}},
            "TemplateArn": {"Value": {"Fn::GetAtt": ["Template", "Arn"]}},
            "PresetRef": {"Value": {"Ref": "Preset"}},
            "PresetArn": {"Value": {"Fn::GetAtt": ["Preset", "Arn"]}}
          }
        }"#,
        declared_queue: Some("orders-queue"),
        declared_job_template: Some("orders-template"),
        declared_preset: Some("orders-preset"),
        expected_queue: &[
            ("/description", "\"the orders transcoding queue\""),
            ("/pricingPlan", "\"ON_DEMAND\""),
            ("/status", "\"PAUSED\""),
            ("/type", "\"CUSTOM\""),
        ],
        expected_job_template: &[
            ("/description", "\"the orders job template\""),
            ("/category", "\"orders\""),
            ("/priority", "10"),
            ("/statusUpdateInterval", "\"SECONDS_60\""),
            ("/accelerationSettings/mode", "\"DISABLED\""),
            (
                "/settings/outputGroups/0/outputs/0/containerSettings/container",
                "\"MP4\"",
            ),
            (
                "/settings/outputGroups/0/outputGroupSettings/fileGroupSettings/destination",
                "\"s3://orders-out/\"",
            ),
        ],
        expected_preset: &[
            ("/description", "\"the orders output preset\""),
            ("/category", "\"orders\""),
            ("/settings/containerSettings/container", "\"MP4\""),
            ("/settings/videoDescription/width", "1280"),
        ],
        expected_tags: &[("Owner", "video"), ("Tier", "prod")],
    },
    Row {
        case: "resources CloudFormation names, a job template naming its queue by Ref, carrying \
               a hop destination and a settings document in CloudFormation's own spelling, in \
               YAML",
        region: "eu-west-1",
        stack: "mediaconvert-yaml-stack",
        template: concat!(
            "Resources:\n",
            "  Queue:\n",
            "    Type: AWS::MediaConvert::Queue\n",
            "    Properties:\n",
            "      Tags:\n",
            "        Owner: reporting\n",
            "  Template:\n",
            "    Type: AWS::MediaConvert::JobTemplate\n",
            "    Properties:\n",
            "      Queue: !Ref Queue\n",
            "      HopDestinations:\n",
            "        - Priority: 5\n",
            "          WaitMinutes: 10\n",
            "      SettingsJson:\n",
            "        OutputGroups:\n",
            "          - Name: File Group\n",
            "            Outputs:\n",
            "              - NameModifier: _1\n",
            "                ContainerSettings:\n",
            "                  Container: M3U8\n",
            "            OutputGroupSettings:\n",
            "              Type: FILE_GROUP_SETTINGS\n",
            "              FileGroupSettings:\n",
            "                Destination: s3://reporting-out/\n",
            "  Preset:\n",
            "    Type: AWS::MediaConvert::Preset\n",
            "    Properties:\n",
            "      SettingsJson:\n",
            "        ContainerSettings:\n",
            "          Container: M3U8\n",
            "Outputs:\n",
            "  QueueRef:\n",
            "    Value: !Ref Queue\n",
            "  QueueArn:\n",
            "    Value: !GetAtt Queue.Arn\n",
            "  QueueName:\n",
            "    Value: !GetAtt Queue.Name\n",
            "  TemplateRef:\n",
            "    Value: !Ref Template\n",
            "  TemplateArn:\n",
            "    Value: !GetAtt Template.Arn\n",
            "  PresetRef:\n",
            "    Value: !Ref Preset\n",
            "  PresetArn:\n",
            "    Value: !GetAtt Preset.Arn\n",
        ),
        declared_queue: None,
        declared_job_template: None,
        declared_preset: None,
        // A queue the declaration states no pricing plan for is on-demand ("when you use the
        // API to create a queue, the default is on-demand") and one it does not pause is
        // ACTIVE.
        expected_queue: &[("/pricingPlan", "\"ON_DEMAND\""), ("/status", "\"ACTIVE\"")],
        expected_job_template: &[
            ("/hopDestinations/0/priority", "5"),
            ("/hopDestinations/0/waitMinutes", "10"),
            (
                "/settings/outputGroups/0/outputs/0/containerSettings/container",
                "\"M3U8\"",
            ),
            (
                "/settings/outputGroups/0/outputGroupSettings/fileGroupSettings/destination",
                "\"s3://reporting-out/\"",
            ),
        ],
        expected_preset: &[("/settings/containerSettings/container", "\"M3U8\"")],
        expected_tags: &[("Owner", "reporting")],
    },
    Row {
        case: "a priority and a hop destination a String parameter feeds as quoted text",
        region: "ap-southeast-2",
        stack: "mediaconvert-parameterised-stack",
        template: r#"{
          "Parameters": {
            "Priority": {"Type": "String", "Default": "-50"},
            "WaitMinutes": {"Type": "String", "Default": "4320"}
          },
          "Resources": {
            "Queue": {
              "Type": "AWS::MediaConvert::Queue",
              "Properties": {"Name": "lake.queue_1"}
            },
            "Template": {
              "Type": "AWS::MediaConvert::JobTemplate",
              "Properties": {
                "Name": "lake-template",
                "Priority": {"Ref": "Priority"},
                "Queue": {"Fn::GetAtt": ["Queue", "Arn"]},
                "HopDestinations": [{"WaitMinutes": {"Ref": "WaitMinutes"}}],
                "SettingsJson": {
                  "outputGroups": [{
                    "name": "File Group",
                    "outputs": [{"nameModifier": "_1", "containerSettings": {"container": "MXF"}}],
                    "outputGroupSettings": {
                      "type": "FILE_GROUP_SETTINGS",
                      "fileGroupSettings": {"destination": "s3://lake-out/"}
                    }
                  }]
                }
              }
            },
            "Preset": {
              "Type": "AWS::MediaConvert::Preset",
              "Properties": {
                "Name": "lake-preset",
                "SettingsJson": {"containerSettings": {"container": "MXF"}}
              }
            }
          },
          "Outputs": {
            "QueueRef": {"Value": {"Ref": "Queue"}},
            "QueueArn": {"Value": {"Fn::GetAtt": ["Queue", "Arn"]}},
            "QueueName": {"Value": {"Fn::GetAtt": ["Queue", "Name"]}},
            "TemplateRef": {"Value": {"Ref": "Template"}},
            "TemplateArn": {"Value": {"Fn::GetAtt": ["Template", "Arn"]}},
            "PresetRef": {"Value": {"Ref": "Preset"}},
            "PresetArn": {"Value": {"Fn::GetAtt": ["Preset", "Arn"]}}
          }
        }"#,
        declared_queue: Some("lake.queue_1"),
        declared_job_template: Some("lake-template"),
        declared_preset: Some("lake-preset"),
        expected_queue: &[("/pricingPlan", "\"ON_DEMAND\""), ("/status", "\"ACTIVE\"")],
        // The published bounds of the two numeric members, read as numbers out of the text the
        // parameters resolved to (`__integerMinNegative50Max50` for a priority, and the
        // documented 1-to-4320-minute range for a hop's wait).
        expected_job_template: &[
            ("/priority", "-50"),
            ("/hopDestinations/0/waitMinutes", "4320"),
            (
                "/settings/outputGroups/0/outputs/0/containerSettings/container",
                "\"MXF\"",
            ),
        ],
        expected_preset: &[("/settings/containerSettings/container", "\"MXF\"")],
        expected_tags: &[],
    },
];

/// Every row: the names the stack publishes for its MediaConvert resources address a queue, a
/// job template and a preset MediaConvert really holds, configured as the template declared
/// them, and deleting the stack releases all three.
#[tokio::test]
async fn a_stack_provisions_the_mediaconvert_resources_its_template_declares() {
    let router = build_router();

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let region = row.region;
        let stack = format!("{}-{index}", row.stack);

        let (status, body) = cloudformation(
            &router,
            region,
            "CreateStack",
            &[
                ("StackName", &stack),
                ("TemplateBody", row.template),
                ("Tags.member.1.Key", "Stack"),
                ("Tags.member.1.Value", "mediaconvert-suite"),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
        );

        let described = wait_for_stack_status(&router, region, &stack, "CREATE_COMPLETE").await;
        let outputs = outputs_of(&described);
        let queue = output(&outputs, "QueueRef", case);
        let job_template = output(&outputs, "TemplateRef", case);
        let preset = output(&outputs, "PresetRef", case);
        for (label, published, declared) in [
            ("the queue", &queue, row.declared_queue),
            ("the job template", &job_template, row.declared_job_template),
            ("the preset", &preset, row.declared_preset),
        ] {
            if let Some(declared) = declared {
                assert_eq!(
                    published, declared,
                    "case {case:?}: expected {label} to carry the declared name {declared:?}, \
                     got {published:?}"
                );
            }
            assert!(
                !published.is_empty(),
                "case {case:?}: expected {label} to publish a name, got {published:?}"
            );
        }
        // `Fn::GetAtt Name` is the name the resource was created under, the same value `Ref`
        // returns for these types.
        assert_eq!(
            output(&outputs, "QueueName", case),
            queue,
            "case {case:?}: expected the queue's Name attribute to be the name Ref returns"
        );

        // Each listing of the owning service reports exactly one entry under the declared
        // name, its own ARN is the one the stack published, and the read of that name resolves
        // to the resource the template declared with the properties it declared applied.
        /// One of the three resources a row declares, as this loop reads it back.
        struct Provisioned<'r> {
            /// What the resource is called in an assertion message.
            label: &'r str,
            /// The URI segment its collection lives under, which is also the member its
            /// listing reports under.
            collection: &'r str,
            /// The name the stack published for it.
            name: &'r str,
            /// The member its own read answers it under.
            member: &'r str,
            /// The stack output carrying its ARN.
            arn_output: &'r str,
            /// The members its read must report.
            expected: &'r [(&'r str, &'r str)],
        }
        let provisioned: &[Provisioned<'_>] = &[
            Provisioned {
                label: "the queue",
                collection: "queues",
                name: &queue,
                member: "queue",
                arn_output: "QueueArn",
                expected: row.expected_queue,
            },
            Provisioned {
                label: "the job template",
                collection: "jobTemplates",
                name: &job_template,
                member: "jobTemplate",
                arn_output: "TemplateArn",
                expected: row.expected_job_template,
            },
            Provisioned {
                label: "the preset",
                collection: "presets",
                name: &preset,
                member: "preset",
                arn_output: "PresetArn",
                expected: row.expected_preset,
            },
        ];
        for subject in provisioned {
            let label = subject.label;
            let name = subject.name;
            let entries = listed(&router, region, subject.collection, subject.collection, case)
                .await;
            let matching: Vec<&Value> = entries
                .iter()
                .filter(|entry| entry.get("name").and_then(Value::as_str) == Some(name))
                .collect();
            assert_eq!(
                matching.len(),
                1,
                "case {case:?}: expected the {label} listing to carry exactly one entry named \
                 {name:?}, got {entries:?}"
            );
            let published_arn = output(&outputs, subject.arn_output, case);
            assert_eq!(
                matching[0].get("arn").and_then(Value::as_str),
                Some(published_arn.as_str()),
                "case {case:?}: expected the {label} entry's own ARN to be the one the stack \
                 published ({published_arn:?}), got {:?}",
                matching[0]
            );
            assert_eq!(
                published_arn,
                mediaconvert_arn(region, subject.collection, name),
                "case {case:?}: expected the {label} ARN in that service's own form"
            );

            let (status, answered) = mediaconvert(
                &router,
                region,
                "GET",
                &format!(
                    "/2017-08-29/{}/{}",
                    subject.collection,
                    encoded_segment(name)
                ),
                None,
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200 reading {label} back under the name the stack \
                 published, got {status} with body {answered}"
            );
            let reported = answered.get(subject.member).cloned().unwrap_or(Value::Null);
            assert_eq!(
                reported.get("name").and_then(Value::as_str),
                Some(name),
                "case {case:?}: expected {label} to report the name {name:?}, got {reported}"
            );
            for (path, wanted) in subject.expected {
                let wanted: Value = serde_json::from_str(wanted).expect("the expectation is JSON");
                assert_eq!(
                    reported.pointer(path),
                    Some(&wanted),
                    "case {case:?}: expected {label} to report {wanted} at {path:?}, got \
                     {reported}"
                );
            }
        }

        // The job template's queue is the queue the stack created, reported as that queue's own
        // ARN whichever form the declaration named it in.
        let (status, answered) = mediaconvert(
            &router,
            region,
            "GET",
            &format!("/2017-08-29/jobTemplates/{}", encoded_segment(&job_template)),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from GetJobTemplate, got {status} with body \
             {answered}"
        );
        assert_eq!(
            answered.pointer("/jobTemplate/queue").and_then(Value::as_str),
            Some(output(&outputs, "QueueArn", case).as_str()),
            "case {case:?}: expected the job template's queue to be the stack's own queue, got \
             {answered}"
        );

        // The tags the declaration asked for are on the queue, beside the stack's own.
        // CloudFormation's three `aws:cloudformation:*` tags are not sent: that service
        // publishes no call to write a key in AWS's reserved namespace from outside AWS.
        let tags = tags_on(
            &router,
            region,
            &mediaconvert_arn(region, "queues", &queue),
            case,
        )
        .await;
        for (key, value) in row.expected_tags {
            assert_eq!(
                tags.get(*key).map(String::as_str),
                Some(*value),
                "case {case:?}: expected tag {key:?} on the queue to be {value:?}, got {tags:?}"
            );
        }
        assert_eq!(
            tags.get("Stack").map(String::as_str),
            Some("mediaconvert-suite"),
            "case {case:?}: expected the stack's own propagated tag on the queue, got {tags:?}"
        );
        assert!(
            tags.keys().all(|key| !key.starts_with("aws:")),
            "case {case:?}: expected no reserved tag key on the queue, got {tags:?}"
        );

        // Deleting the stack releases what MediaConvert holds for it: the next read of any of
        // the three names is the modelled 404 of the service that no longer holds the resource.
        let (status, body) =
            cloudformation(&router, region, "DeleteStack", &[("StackName", &stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with body {body}"
        );
        for (label, collection, name) in [
            ("the queue", "queues", &queue),
            ("the job template", "jobTemplates", &job_template),
            ("the preset", "presets", &preset),
        ] {
            let (status, gone) = mediaconvert(
                &router,
                region,
                "GET",
                &format!("/2017-08-29/{collection}/{}", encoded_segment(name)),
                None,
            )
            .await;
            assert_eq!(
                status, 404,
                "case {case:?}: expected HTTP 404 reading {label} once the stack deleted it, got \
                 {status} with body {gone}"
            );
            assert_eq!(
                gone.get("errorType").and_then(Value::as_str),
                Some("NotFoundException"),
                "case {case:?}: expected {label} to answer NotFoundException once it is gone, \
                 got {gone}"
            );
            // The whole modelled error, message included: that sentence names the member the
            // read addressed the resource by, which for each of these reads is `name`.
            assert_eq!(
                gone.get("message").and_then(Value::as_str),
                Some("The resource you specified in name was not found."),
                "case {case:?}: expected {label}'s refusal to state the service's own sentence, \
                 got {gone}"
            );
        }
    }
}

/// A declaration MediaConvert REFUSES does not report a resource nothing created: the stack
/// rolls back carrying that service's own refusal on the resource's `CREATE_FAILED` event, and
/// the name it declared resolves to nothing afterwards.
///
/// `AWS::MediaConvert::Queue` publishes no reservation-plan property at all, so every
/// `PricingPlan: RESERVED` declaration reaches `CreateQueue` without the settings a reserved
/// queue requires — which is what the rows here drive, in both template languages and with and
/// without the other optional properties beside it.
#[tokio::test]
async fn a_reserved_queue_declaration_mediaconvert_refuses_rolls_the_stack_back() {
    let router = build_router();
    let region = "us-east-1";

    // (case, the queue name the row declares, the template)
    let rows: &[(&str, &str, &str)] = &[
        (
            "a reserved queue declared in JSON",
            "reserved-json",
            r#"{"Resources": {"Queue": {"Type": "AWS::MediaConvert::Queue",
               "Properties": {"Name": "reserved-json", "PricingPlan": "RESERVED",
               "Status": "ACTIVE"}}}}"#,
        ),
        (
            "a reserved queue carrying a description and tags",
            "reserved-tagged",
            r#"{"Resources": {"Queue": {"Type": "AWS::MediaConvert::Queue",
               "Properties": {"Name": "reserved-tagged", "PricingPlan": "RESERVED",
               "Description": "a plan nobody supplied", "Tags": {"Owner": "video"}}}}}"#,
        ),
        (
            "a reserved queue declared in YAML",
            "reserved-yaml",
            concat!(
                "Resources:\n",
                "  Queue:\n",
                "    Type: AWS::MediaConvert::Queue\n",
                "    Properties:\n",
                "      Name: reserved-yaml\n",
                "      PricingPlan: RESERVED\n",
            ),
        ),
    ];

    for (index, (case, name, template)) in rows.iter().enumerate() {
        let stack = format!("mediaconvert-reserved-{index}");
        let (status, body) = cloudformation(
            &router,
            region,
            "CreateStack",
            &[("StackName", &stack), ("TemplateBody", template)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
        );
        let (reported, described) = wait_for_terminal_status(&router, region, &stack).await;
        assert_eq!(
            reported, "ROLLBACK_COMPLETE",
            "case {case:?}: expected the stack to roll back on a declaration MediaConvert \
             refuses, got {reported:?} with answer {described}"
        );

        // The resource records the refusal, quoted in the envelope CloudFormation reports a
        // resource provider's answer in.
        let (status, events) = cloudformation(
            &router,
            region,
            "DescribeStackEvents",
            &[("StackName", &stack)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStackEvents, got {status} with body \
             {events}"
        );
        let reasons = elements(&events, "ResourceStatusReason");
        let quoted = reasons
            .iter()
            .find(|reason| reason.contains(RESERVED_WITHOUT_A_PLAN))
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected an event reason carrying the service's own refusal, \
                     got {reasons:?}"
                )
            });
        for fragment in [
            "Resource handler returned message: \"",
            "(Service: MediaConvert, Status Code: 400, Request ID: ",
            "(SDK Attempt Count: ",
            "HandlerErrorCode: InvalidRequest",
        ] {
            assert!(
                quoted.contains(fragment),
                "case {case:?}: expected the reason to carry {fragment:?}, got {quoted:?}"
            );
        }

        // Nothing was created under the name the refused declaration asked for.
        let (status, answered) = mediaconvert(
            &router,
            region,
            "GET",
            &format!("/2017-08-29/queues/{}", encoded_segment(name)),
            None,
        )
        .await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected HTTP 404 from GetQueue on the name the failed stack \
             declared, got {status} with body {answered}"
        );
    }
}

/// A stack that CHANGES its queue's declaration applies the change through MediaConvert's own
/// update, so the queue keeps its name and its creation instant and a later read reports the
/// declared value — including a property the new declaration DROPPED, which goes back to the
/// value a queue that states none carries.
#[tokio::test]
async fn a_changed_declaration_is_applied_to_the_queue_mediaconvert_holds() {
    let router = build_router();
    let region = "ca-central-1";
    let stack = "mediaconvert-update-stack";

    let before = r#"{
      "Resources": {
        "Queue": {
          "Type": "AWS::MediaConvert::Queue",
          "Properties": {
            "Name": "updated-queue",
            "Description": "before the update",
            "Status": "ACTIVE"
          }
        }
      }
    }"#;
    let after = r#"{
      "Resources": {
        "Queue": {
          "Type": "AWS::MediaConvert::Queue",
          "Properties": {
            "Name": "updated-queue",
            "Status": "PAUSED"
          }
        }
      }
    }"#;

    let (status, body) = cloudformation(
        &router,
        region,
        "CreateStack",
        &[("StackName", stack), ("TemplateBody", before)],
    )
    .await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from CreateStack, got {status} with body {body}"
    );
    wait_for_stack_status(&router, region, stack, "CREATE_COMPLETE").await;

    let (status, created) = mediaconvert(
        &router,
        region,
        "GET",
        "/2017-08-29/queues/updated-queue",
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from GetQueue after the create, got {status} with body {created}"
    );
    let created_at = created.pointer("/queue/createdAt").cloned();
    assert!(
        created_at.is_some(),
        "expected the created queue to report a createdAt, got {created}"
    );

    let (status, body) = cloudformation(
        &router,
        region,
        "UpdateStack",
        &[("StackName", stack), ("TemplateBody", after)],
    )
    .await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from UpdateStack, got {status} with body {body}"
    );
    wait_for_stack_status(&router, region, stack, "UPDATE_COMPLETE").await;

    let (status, answered) = mediaconvert(
        &router,
        region,
        "GET",
        "/2017-08-29/queues/updated-queue",
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from GetQueue after the update, got {status} with body {answered}"
    );
    let reported = answered.get("queue").cloned().unwrap_or(Value::Null);
    let cases: &[(&str, &str, Option<Value>)] = &[
        (
            "the name the queue keeps",
            "/name",
            Some(json!("updated-queue")),
        ),
        ("the status the update declared", "/status", Some(json!("PAUSED"))),
        ("the description the update dropped", "/description", None),
    ];
    for (case, path, expected) in cases {
        let actual = reported.pointer(path).cloned();
        assert_eq!(
            actual.as_ref(),
            expected.as_ref(),
            "case {case:?}: expected {expected:?} at {path:?}, got {actual:?} in {reported}"
        );
    }
    // The queue was updated IN PLACE rather than replaced, which its creation instant is what
    // says: a replaced queue would be one MediaConvert created again.
    assert_eq!(
        reported.get("createdAt").cloned(),
        created_at.clone(),
        "expected the queue's createdAt to be unchanged by the update, got {reported}"
    );

    // And the listing still carries exactly one queue under that name.
    let entries = listed(&router, region, "queues", "queues", "the update").await;
    let matching = entries
        .iter()
        .filter(|entry| entry.get("name").and_then(Value::as_str) == Some("updated-queue"))
        .count();
    assert_eq!(
        matching, 1,
        "expected exactly one queue named \"updated-queue\" after the update, got {entries:?}"
    );
}

/// A declaration that DROPS a job template's property does not leave the template carrying the
/// old value while the stack reports success: every member the owning API publishes a way to
/// clear goes back to the value a template created without it holds, and the one member no call
/// clears — the queue the template's jobs are assigned to — reports the change as not applied
/// and rolls the update back instead.
#[tokio::test]
async fn a_dropped_job_template_property_returns_to_the_value_mediaconvert_applies() {
    let router = build_router();
    let region = "us-east-2";
    let stack = "mediaconvert-dropped-stack";

    /// The settings graph every revision of the template below carries, written in the API's
    /// own spelling, with one input naming an audio selector by a name the DECLARATION chose —
    /// the name the output's `audioSourceName` names back, which is why such a key is not
    /// re-cased.
    const SETTINGS: &str = r#"{
      "inputs": [{
        "timecodeSource": "ZEROBASED",
        "audioSelectors": { "Audio Selector 1": { "defaultSelection": "DEFAULT" } }
      }],
      "outputGroups": [{
        "name": "File Group",
        "outputs": [{
          "nameModifier": "_1",
          "containerSettings": { "container": "MP4" },
          "audioDescriptions": [{ "audioSourceName": "Audio Selector 1" }]
        }],
        "outputGroupSettings": {
          "type": "FILE_GROUP_SETTINGS",
          "fileGroupSettings": { "destination": "s3://dropped-out/" }
        }
      }]
    }"#;

    /// The template the row deploys, with `extra` the properties this revision states beside
    /// the name, the queue and the settings.
    fn template(extra: &str) -> String {
        format!(
            r#"{{
              "Resources": {{
                "Queue": {{
                  "Type": "AWS::MediaConvert::Queue",
                  "Properties": {{ "Name": "dropped-queue" }}
                }},
                "Template": {{
                  "Type": "AWS::MediaConvert::JobTemplate",
                  "Properties": {{
                    "Name": "dropped-template",
                    "Queue": {{ "Fn::GetAtt": ["Queue", "Arn"] }},
                    {extra}
                    "SettingsJson": {SETTINGS}
                  }}
                }}
              }}
            }}"#
        )
    }

    let declared = template(
        r#""Category": "before",
           "Description": "before the update",
           "Priority": 10,
           "StatusUpdateInterval": "SECONDS_120",
           "AccelerationSettings": { "Mode": "PREFERRED" },
           "HopDestinations": [{ "WaitMinutes": 20 }],"#,
    );
    let (status, body) = cloudformation(
        &router,
        region,
        "CreateStack",
        &[("StackName", stack), ("TemplateBody", &declared)],
    )
    .await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from CreateStack, got {status} with body {body}"
    );
    wait_for_stack_status(&router, region, stack, "CREATE_COMPLETE").await;

    // The caller's own audio-selector name survives the deployment: the key is the declaration's
    // and the `audioSourceName` that names it back still resolves to it.
    let (status, created) = mediaconvert(
        &router,
        region,
        "GET",
        "/2017-08-29/jobTemplates/dropped-template",
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from GetJobTemplate after the create, got {status} with body {created}"
    );
    assert_eq!(
        created.pointer("/jobTemplate/settings/inputs/0/audioSelectors/Audio Selector 1/defaultSelection"),
        Some(&json!("DEFAULT")),
        "expected the declared audio selector under the name the declaration chose, got {created}"
    );

    // Every property the second revision DROPS goes back to the value MediaConvert applies to a
    // template created without it.
    let dropped = template("");
    let (status, body) = cloudformation(
        &router,
        region,
        "UpdateStack",
        &[("StackName", stack), ("TemplateBody", &dropped)],
    )
    .await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from UpdateStack, got {status} with body {body}"
    );
    wait_for_stack_status(&router, region, stack, "UPDATE_COMPLETE").await;

    let (status, answered) = mediaconvert(
        &router,
        region,
        "GET",
        "/2017-08-29/jobTemplates/dropped-template",
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from GetJobTemplate after the update, got {status} with body \
         {answered}"
    );
    let reported = answered.get("jobTemplate").cloned().unwrap_or(Value::Null);
    // (case, the member, the value a template created without the property holds)
    let cases: &[(&str, &str, Option<Value>)] = &[
        ("the category the update dropped", "/category", None),
        ("the description the update dropped", "/description", None),
        ("the priority the update dropped", "/priority", Some(json!(0))),
        (
            "the status-update cadence the update dropped",
            "/statusUpdateInterval",
            Some(json!("SECONDS_60")),
        ),
        (
            "the acceleration the update dropped",
            "/accelerationSettings/mode",
            Some(json!("DISABLED")),
        ),
        (
            "the hop destinations the update dropped",
            "/hopDestinations",
            Some(json!([])),
        ),
    ];
    for (case, path, expected) in cases {
        let actual = reported.pointer(path).cloned();
        assert_eq!(
            actual.as_ref(),
            expected.as_ref(),
            "case {case:?}: expected {expected:?} at {path:?}, got {actual:?} in {reported}"
        );
    }

    // And the one member no call of that service clears: the change is reported as NOT applied,
    // so the stack rolls the update back and the template still names the queue it was
    // deployed with.
    let without_a_queue = r#"{
      "Resources": {
        "Queue": {
          "Type": "AWS::MediaConvert::Queue",
          "Properties": { "Name": "dropped-queue" }
        },
        "Template": {
          "Type": "AWS::MediaConvert::JobTemplate",
          "Properties": {
            "Name": "dropped-template",
            "SettingsJson": { "outputGroups": [{
              "name": "File Group",
              "outputs": [{ "nameModifier": "_1", "containerSettings": { "container": "MP4" } }],
              "outputGroupSettings": {
                "type": "FILE_GROUP_SETTINGS",
                "fileGroupSettings": { "destination": "s3://dropped-out/" }
              }
            }] }
          }
        }
      }
    }"#;
    let (status, body) = cloudformation(
        &router,
        region,
        "UpdateStack",
        &[("StackName", stack), ("TemplateBody", without_a_queue)],
    )
    .await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from UpdateStack, got {status} with body {body}"
    );
    let (reported_status, described) = wait_for_terminal_status(&router, region, stack).await;
    assert!(
        reported_status.starts_with("UPDATE_ROLLBACK"),
        "expected the stack to roll the update back when the change cannot be applied, got \
         {reported_status:?} with answer {described}"
    );
    let (status, answered) = mediaconvert(
        &router,
        region,
        "GET",
        "/2017-08-29/jobTemplates/dropped-template",
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from GetJobTemplate after the rollback, got {status} with body \
         {answered}"
    );
    assert_eq!(
        answered.pointer("/jobTemplate/queue").and_then(Value::as_str),
        Some(mediaconvert_arn(region, "queues", "dropped-queue").as_str()),
        "expected the template to still name the queue it was deployed with, got {answered}"
    );
}
