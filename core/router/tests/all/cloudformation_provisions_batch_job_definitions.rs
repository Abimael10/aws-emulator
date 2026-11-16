// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that declares an `AWS::Batch::JobDefinition` really REGISTERS that
//! job definition in AWS Batch, and the handle the stack hands back is the revision-qualified
//! ARN that service answered — not the definition's bare name.
//!
//! The rule under test: **the physical resource id a stack publishes for a resource is the
//! identifier that type's published return values document, read off the owning service's own
//! answer where that service assigns it — so `Ref` of an `AWS::Batch::JobDefinition` is the
//! job definition ARN `RegisterJobDefinition` answered
//! (`arn:<partition>:batch:<region>:<account>:job-definition/<name>:<revision>`), whose
//! revision AWS Batch allocates, and every place a template feeds that `Ref` — an output, an
//! IAM policy's `Resource`, another resource's property — therefore carries an ARN.** Two
//! crates have to agree for that to hold (CloudFormation makes the call, AWS Batch serves it
//! and allocates the revision), so the suite lives here rather than in either crate.
//!
//! The rows differ along what the rule generalises over: whether the template NAMES the job
//! definition or leaves CloudFormation to generate its name, which `JobDefinitionType` it is
//! (`container` with `ContainerProperties`, `multinode` with `NodeProperties`), whether it
//! runs on Fargate or on EC2, whether it declares `Parameters`, a `RetryStrategy`, a
//! `Timeout` and `Tags`, whether the template is written in JSON or YAML, which intrinsic the
//! outputs read it with (`Ref`, `Fn::GetAtt`, `Fn::Sub`), and — the part no template decides —
//! which REVISION the registration is given, which is the next one for the name.
//!
//! Every expected value is computed here from what the row declares, never read back out of
//! the answer being checked — except the GENERATED name of the row that declares none, which
//! is CloudFormation's to compute: what is checked about that one is that the ARN the stack
//! publishes is that name's job-definition ARN at the expected revision and that AWS Batch
//! resolves it.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery form encoder and the XML element readers this crate writes ONCE
// (`tests/all/query_wire.rs`), and the stack-output reader beside them.
use super::cloudformation_provisions_stack_resources::{outputs_of, xml_text};
use super::query_wire::form_encode;

/// How long a settled stack is waited for: a generous deadline the poll below gives up at,
/// not a sleep. What a case waits for is the CONDITION — a stack status that is no longer in
/// progress — so a loaded host only makes the wait longer and never changes the answer.
const SETTLED_DEADLINE: Duration = Duration::from_secs(60);

/// How often that poll looks again.
const SETTLED_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The account id every ARN this emulator mints carries.
const ACCOUNT: &str = "000000000000";

/// The `JobDefinition$status` AWS Batch reports for a revision that can be submitted, and the
/// one it reports for a revision that has been deregistered.
const ACTIVE: &str = "ACTIVE";
const INACTIVE: &str = "INACTIVE";

/// The emulator with the services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them. Request authentication is off, as in every other
/// suite here; the in-process callback CloudFormation makes signs its request either way (the
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
        service_com_amazonaws_batch_awsbatchv20160810::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service` in `region`, which is
/// what routes the request and what each handler reads its Region from. The signature is never
/// checked: this router runs with request authentication off.
fn authorization(service: &str, region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{region}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
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
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// Call one IAM operation over its awsQuery wire. IAM is global, so the request is signed for
/// a Region only because that is what routes it.
async fn iam(
    router: &MultiServiceRouter,
    region: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-08");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", "iam.amazonaws.com")
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("iam", region))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// Call one AWS Batch operation over its restJson1 wire, signed for `region`.
async fn batch(router: &MultiServiceRouter, region: &str, uri: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri(uri)
        .header("host", format!("batch.{region}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("batch", region))
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, payload)
}

/// `ListTagsForResource` for `arn`, which AWS Batch binds to `GET /v1/tags/{resourceArn}` —
/// the ARN travels in the URI rather than in a body.
async fn batch_tags(router: &MultiServiceRouter, region: &str, arn: &str) -> (u16, Value) {
    let request = http::Request::builder()
        .method("GET")
        .uri(format!("/v1/tags/{}", form_encode(arn)))
        .header("host", format!("batch.{region}.amazonaws.com"))
        .header("authorization", authorization("batch", region))
        .body(Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, payload)
}

/// The ONE job-definition record `DescribeJobDefinitions` answers for `reference`, which that
/// reader takes as "the name, name:revision or full Amazon Resource Name (ARN)".
async fn described_definition(
    router: &MultiServiceRouter,
    region: &str,
    case: &str,
    reference: &str,
) -> Value {
    let (status, answered) = batch(
        router,
        region,
        "/v1/describejobdefinitions",
        &json!({ "jobDefinitions": [reference] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeJobDefinitions on {reference}, got \
         {status} with body {answered}"
    );
    let listed = answered["jobDefinitions"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        listed.len(),
        1,
        "case {case:?}: expected exactly one jobDefinition for {reference}, got {answered}"
    );
    listed[0].clone()
}

/// Create a stack and poll until it reports a status that is no longer in progress, then
/// answer its `DescribeStacks` body.
async fn create_and_settle(
    router: &MultiServiceRouter,
    region: &str,
    case: &str,
    stack_name: &str,
    template: &str,
    extra: &[(&str, &str)],
) -> String {
    let mut members: Vec<(&str, &str)> =
        vec![("StackName", stack_name), ("TemplateBody", template)];
    members.extend_from_slice(extra);
    let (status, body) = cloudformation(router, region, "CreateStack", &members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
    );
    settled(router, region, case, stack_name).await
}

/// The stack's `DescribeStacks` body, once its status no longer ends `_IN_PROGRESS`.
async fn settled(
    router: &MultiServiceRouter,
    region: &str,
    case: &str,
    stack_name: &str,
) -> String {
    let deadline = ::std::time::Instant::now() + SETTLED_DEADLINE;
    loop {
        let (status, body) =
            cloudformation(router, region, "DescribeStacks", &[("StackName", stack_name)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStacks, got {status} with body {body}"
        );
        let reported = xml_text(&body, "StackStatus").unwrap_or_default();
        if !reported.ends_with("_IN_PROGRESS") {
            return body;
        }
        assert!(
            ::std::time::Instant::now() < deadline,
            "case {case:?}: the stack never settled, last status {reported:?}"
        );
        tokio::time::sleep(SETTLED_POLL_INTERVAL).await;
    }
}

/// The ARN AWS Batch mints for revision `revision` of the job definition `name` in `region`:
/// `arn:aws:batch:<region>:<account>:job-definition/<name>:<revision>`.
fn job_definition_arn(region: &str, name: &str, revision: i64) -> String {
    format!("arn:aws:batch:{region}:{ACCOUNT}:job-definition/{name}:{revision}")
}

/// One row of the table: a template, and what the rule says the job definition it declares
/// must then be.
struct Row {
    case: &'static str,
    /// The Region the row deploys in, which is the Region the revision is registered in and
    /// the one its ARN carries.
    region: &'static str,
    /// The template body, in the form the row writes it (JSON or YAML).
    template: fn() -> String,
    /// The job definition name the template declares, or `None` for a template that names
    /// none — whose revision carries the name CloudFormation generated.
    definition_name: Option<&'static str>,
    /// The revision the registration must be given: the next one for the name, which is 1 for
    /// a name no earlier row registered.
    revision: i64,
    /// The `JobDefinitionType` the revision must report.
    definition_type: &'static str,
    /// The output keys whose value must be the revision's ARN.
    outputs: &'static [&'static str],
    /// The `image` the registered revision must carry, at the path its properties object puts
    /// it (a container definition's own, or the one its single node range runs).
    image: &'static str,
    /// Whether that image sits under `containerProperties` (a `container` definition) or under
    /// the first `nodeProperties.nodeRangeProperties` entry's `container` (a `multinode` one).
    multinode: bool,
    /// The `Parameters` the declaration asks for, whose keys are the template's own and travel
    /// unchanged.
    parameters: &'static [(&'static str, &'static str)],
    /// The tags the declaration asks for, beside the three CloudFormation records on every
    /// resource it provisions.
    tags: &'static [(&'static str, &'static str)],
}

const ROWS: &[Row] = &[
    Row {
        case: "a named Fargate container definition in JSON, read with Ref and the ARN \
               attribute",
        region: "us-east-1",
        template: || {
            json!({
                "AWSTemplateFormatVersion": "2010-09-09",
                "Description": "an AWS Batch job definition",
                "Resources": {
                    "JobDefinition": {
                        "Type": "AWS::Batch::JobDefinition",
                        "Properties": {
                            "JobDefinitionName": "orders-jd",
                            "Type": "container",
                            "PlatformCapabilities": ["FARGATE"],
                            "Parameters": { "codec": "mp4", "BitRate": "128" },
                            "Timeout": { "AttemptDurationSeconds": 120 },
                            "RetryStrategy": {
                                "Attempts": 2,
                                "EvaluateOnExit": [
                                    { "Action": "RETRY", "OnStatusReason": "Host EC2*" }
                                ]
                            },
                            "ContainerProperties": {
                                "Image": "public.ecr.aws/example/orders:1",
                                "ExecutionRoleArn":
                                    "arn:aws:iam::000000000000:role/orders-execution",
                                "ResourceRequirements": [
                                    { "Type": "VCPU", "Value": "0.25" },
                                    { "Type": "MEMORY", "Value": "512" }
                                ],
                                "NetworkConfiguration": { "AssignPublicIp": "ENABLED" },
                                "LogConfiguration": {
                                    "LogDriver": "awslogs",
                                    "Options": {
                                        "awslogs-group": "/aws/batch/orders",
                                        "awslogs-stream-prefix": "orders"
                                    }
                                }
                            },
                            "Tags": { "Owner": "orders", "Tier": "prod" }
                        }
                    }
                },
                "Outputs": {
                    "DefinitionRef": { "Value": { "Ref": "JobDefinition" } },
                    "DefinitionArn": {
                        "Value": { "Fn::GetAtt": ["JobDefinition", "JobDefinitionArn"] }
                    }
                }
            })
            .to_string()
        },
        definition_name: Some("orders-jd"),
        revision: 1,
        definition_type: "container",
        outputs: &["DefinitionRef", "DefinitionArn"],
        image: "public.ecr.aws/example/orders:1",
        multinode: false,
        parameters: &[("codec", "mp4"), ("BitRate", "128")],
        tags: &[("Owner", "orders"), ("Tier", "prod")],
    },
    Row {
        case: "an unnamed EC2 container definition in YAML, read with Fn::Sub",
        region: "eu-west-1",
        template: || {
            [
                "Resources:",
                "  JobDefinition:",
                "    Type: AWS::Batch::JobDefinition",
                "    Properties:",
                "      Type: container",
                "      ContainerProperties:",
                "        Image: public.ecr.aws/example/billing:2",
                "        Command:",
                "          - /bin/sh",
                "          - -c",
                "          - run",
                "        ResourceRequirements:",
                "          - Type: VCPU",
                "            Value: '1'",
                "          - Type: MEMORY",
                "            Value: '2048'",
                "Outputs:",
                "  DefinitionSub:",
                "    Value: !Sub '${JobDefinition}'",
                "",
            ]
            .join("\n")
        },
        definition_name: None,
        revision: 1,
        definition_type: "container",
        outputs: &["DefinitionSub"],
        image: "public.ecr.aws/example/billing:2",
        multinode: false,
        parameters: &[],
        tags: &[],
    },
    Row {
        case: "a multinode definition whose node range runs the container",
        region: "ap-northeast-1",
        template: || {
            json!({
                "Resources": {
                    "JobDefinition": {
                        "Type": "AWS::Batch::JobDefinition",
                        "Properties": {
                            "JobDefinitionName": "render-jd",
                            "Type": "multinode",
                            "NodeProperties": {
                                "NumNodes": 2,
                                "MainNode": 0,
                                "NodeRangeProperties": [{
                                    "TargetNodes": "0:1",
                                    "Container": {
                                        "Image": "public.ecr.aws/example/render:3",
                                        "ResourceRequirements": [
                                            { "Type": "VCPU", "Value": "2" },
                                            { "Type": "MEMORY", "Value": "4096" }
                                        ]
                                    }
                                }]
                            },
                            "Tags": { "Owner": "render" }
                        }
                    }
                },
                "Outputs": {
                    "DefinitionRef": { "Value": { "Ref": "JobDefinition" } }
                }
            })
            .to_string()
        },
        definition_name: Some("render-jd"),
        revision: 1,
        definition_type: "multinode",
        outputs: &["DefinitionRef"],
        image: "public.ecr.aws/example/render:3",
        multinode: true,
        parameters: &[],
        tags: &[("Owner", "render")],
    },
    Row {
        case: "a second stack registering the SAME name, which AWS Batch gives the next \
               revision",
        region: "us-east-1",
        template: || {
            json!({
                "Resources": {
                    "JobDefinition": {
                        "Type": "AWS::Batch::JobDefinition",
                        "Properties": {
                            "JobDefinitionName": "orders-jd",
                            "Type": "container",
                            "ContainerProperties": {
                                "Image": "public.ecr.aws/example/orders:2",
                                "ResourceRequirements": [
                                    { "Type": "VCPU", "Value": "1" },
                                    { "Type": "MEMORY", "Value": "1024" }
                                ]
                            }
                        }
                    }
                },
                "Outputs": {
                    "DefinitionRef": { "Value": { "Ref": "JobDefinition" } },
                    "DefinitionArn": {
                        "Value": { "Fn::GetAtt": ["JobDefinition", "JobDefinitionArn"] }
                    }
                }
            })
            .to_string()
        },
        definition_name: Some("orders-jd"),
        // The first row registered revision 1 of this name in this Region, and AWS Batch
        // allocates "the revision of the job definition" one past the highest it holds — so
        // nothing a template says could have made this 1.
        revision: 2,
        definition_type: "container",
        outputs: &["DefinitionRef", "DefinitionArn"],
        image: "public.ecr.aws/example/orders:2",
        multinode: false,
        parameters: &[],
        tags: &[],
    },
];

/// Every row: the handle the stack publishes for its `AWS::Batch::JobDefinition` is the
/// revision-qualified ARN of a revision AWS Batch really holds, that service resolves the
/// handle to the revision the template declared, and deleting the stack deregisters it.
#[tokio::test]
async fn a_stack_registers_the_batch_job_definition_it_declares_under_the_arn_it_publishes() {
    let router = build_router();

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let region = row.region;
        let stack_name = format!("batch-job-definition-{index}");
        let template = (row.template)();

        let described =
            create_and_settle(&router, region, case, &stack_name, &template, &[]).await;
        assert_eq!(
            xml_text(&described, "StackStatus").as_deref(),
            Some("CREATE_COMPLETE"),
            "case {case:?}: expected the stack to reach CREATE_COMPLETE, got {described}"
        );

        let outputs = outputs_of(&described);
        let published = outputs
            .get(row.outputs[0])
            .unwrap_or_else(|| panic!("case {case:?}: no job-definition output in {described}"))
            .clone();

        // "Ref returns the job definition ARN": the published handle is that ARN, carrying
        // the name the template asked for — or, for a template that named none, the one
        // CloudFormation generated — and the revision AWS Batch allocated.
        let name = match row.definition_name {
            Some(declared) => declared.to_string(),
            None => published
                .rsplit('/')
                .next()
                .unwrap_or_default()
                .rsplit_once(':')
                .map(|(name, _revision)| name.to_string())
                .unwrap_or_default(),
        };
        let expected = job_definition_arn(region, &name, row.revision);
        assert_eq!(
            published, expected,
            "case {case:?}: expected the published handle to be {expected}, got {published:?}"
        );
        assert!(
            !name.is_empty() && name.len() <= 128,
            "case {case:?}: expected a job definition name inside the documented 1..128, got \
             {name:?}"
        );
        for key in row.outputs {
            assert_eq!(
                outputs.get(*key),
                Some(&published),
                "case {case:?}: expected output {key:?} to be {published}, got {:?}",
                outputs.get(*key)
            );
        }

        // And AWS Batch itself resolves the handle — by the ARN and by the NAME, both of
        // which its reader documents — to the revision the template declared.
        for reference in [published.clone(), format!("{name}:{}", row.revision)] {
            let definition = described_definition(&router, region, case, &reference).await;
            assert_eq!(
                definition["jobDefinitionArn"].as_str(),
                Some(published.as_str()),
                "case {case:?}: expected {reference} to resolve to {published}, got {definition}"
            );
            assert_eq!(
                definition["jobDefinitionName"].as_str(),
                Some(name.as_str()),
                "case {case:?}: expected the declared name {name}, got {definition}"
            );
            assert_eq!(
                definition["revision"].as_i64(),
                Some(row.revision),
                "case {case:?}: expected revision {}, got {definition}",
                row.revision
            );
            assert_eq!(
                definition["status"].as_str(),
                Some(ACTIVE),
                "case {case:?}: a registered revision is {ACTIVE}, got {definition}"
            );
            assert_eq!(
                definition["type"].as_str(),
                Some(row.definition_type),
                "case {case:?}: expected the declared type, got {definition}"
            );
            // The declaration's own properties reached that service under its member names,
            // nested objects and lists included.
            let image = if row.multinode {
                definition["nodeProperties"]["nodeRangeProperties"][0]["container"]["image"]
                    .as_str()
            } else {
                definition["containerProperties"]["image"].as_str()
            };
            assert_eq!(
                image,
                Some(row.image),
                "case {case:?}: expected the declared image {}, got {definition}",
                row.image
            );
            for (key, value) in row.parameters {
                assert_eq!(
                    definition["parameters"][*key].as_str(),
                    Some(*value),
                    "case {case:?}: expected parameter {key:?} to be {value:?}, got {definition}"
                );
            }
        }

        // The tags the declaration asked for, plus the three CloudFormation records on every
        // resource it provisions, read through Batch's own tagging call.
        let (status, listed) = batch_tags(&router, region, &published).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from ListTagsForResource, got {status} with body \
             {listed}"
        );
        let reported = listed["tags"].as_object().cloned().unwrap_or_default();
        let stack_id = xml_text(&described, "StackId")
            .unwrap_or_else(|| panic!("case {case:?}: no <StackId> in {described}"));
        let expected_tags: Vec<(&str, &str)> = row
            .tags
            .iter()
            .copied()
            .chain([
                ("aws:cloudformation:stack-name", stack_name.as_str()),
                ("aws:cloudformation:stack-id", stack_id.as_str()),
                ("aws:cloudformation:logical-id", "JobDefinition"),
            ])
            .collect();
        for (key, value) in expected_tags {
            assert_eq!(
                reported.get(key).and_then(Value::as_str),
                Some(value),
                "case {case:?}: expected tag {key:?} to be {value:?}, got {listed}"
            );
        }

        // Deleting the stack deregisters the revision in AWS Batch, which is what that
        // service does with one: the revision stays describable and is no longer ACTIVE.
        let (status, body) =
            cloudformation(&router, region, "DeleteStack", &[("StackName", &stack_name)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with body {body}"
        );
        let deregistered = described_definition(&router, region, case, &published).await;
        assert_eq!(
            deregistered["status"].as_str(),
            Some(INACTIVE),
            "case {case:?}: expected the deleted stack's revision to be {INACTIVE}, got \
             {deregistered}"
        );
    }
}

/// The published handle is an ARN wherever a template feeds it: an IAM inline policy that
/// scopes `batch:SubmitJob` to the job definition and the queue a `Ref` names is ACCEPTED —
/// IAM refuses a `Resource` element that is neither an ARN nor `*` — and AWS Batch runs a
/// submit addressed by the very values the stack published.
///
/// This is the standard way a template scopes a submitting function's permissions, so the
/// stack reaching `CREATE_COMPLETE` is the observable the rule is about.
#[tokio::test]
async fn the_published_job_definition_handle_scopes_a_policy_and_addresses_a_submit() {
    let router = build_router();
    let case = "a role scoped to the queue and the job definition a Ref names";
    let region = "us-west-2";
    let stack_name = "batch-submit-estate";

    let template = json!({
        "Resources": {
            "Environment": {
                "Type": "AWS::Batch::ComputeEnvironment",
                "Properties": { "ComputeEnvironmentName": "submit-ce", "Type": "UNMANAGED" }
            },
            "Queue": {
                "Type": "AWS::Batch::JobQueue",
                "Properties": {
                    "JobQueueName": "submit-jq",
                    "Priority": 1,
                    "State": "ENABLED",
                    "ComputeEnvironmentOrder": [
                        { "Order": 1, "ComputeEnvironment": { "Ref": "Environment" } }
                    ]
                }
            },
            "JobDefinition": {
                "Type": "AWS::Batch::JobDefinition",
                "Properties": {
                    "JobDefinitionName": "submit-jd",
                    "Type": "container",
                    "ContainerProperties": {
                        "Image": "public.ecr.aws/example/submit:1",
                        "ResourceRequirements": [
                            { "Type": "VCPU", "Value": "1" },
                            { "Type": "MEMORY", "Value": "2048" }
                        ]
                    }
                }
            },
            "SubmitJobFunctionRole": {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "RoleName": "submit-job-role",
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Principal": { "Service": "lambda.amazonaws.com" },
                            "Action": "sts:AssumeRole"
                        }]
                    },
                    "Policies": [{
                        "PolicyName": "submit",
                        "PolicyDocument": {
                            "Version": "2012-10-17",
                            "Statement": [{
                                "Effect": "Allow",
                                "Action": "batch:SubmitJob",
                                "Resource": [
                                    { "Ref": "Queue" },
                                    { "Ref": "JobDefinition" }
                                ]
                            }]
                        }
                    }]
                }
            }
        },
        "Outputs": {
            "QueueArn": { "Value": { "Ref": "Queue" } },
            "JobDefinitionArn": { "Value": { "Ref": "JobDefinition" } }
        }
    })
    .to_string();

    let described = create_and_settle(
        &router,
        region,
        case,
        stack_name,
        &template,
        &[("Capabilities.member.1", "CAPABILITY_NAMED_IAM")],
    )
    .await;
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "case {case:?}: expected the stack to reach CREATE_COMPLETE, got {described}"
    );

    let outputs: BTreeMap<String, String> = outputs_of(&described);
    let definition_arn = outputs
        .get("JobDefinitionArn")
        .unwrap_or_else(|| panic!("case {case:?}: no JobDefinitionArn output in {described}"))
        .clone();
    let queue_arn = outputs
        .get("QueueArn")
        .unwrap_or_else(|| panic!("case {case:?}: no QueueArn output in {described}"))
        .clone();
    assert_eq!(
        definition_arn,
        job_definition_arn(region, "submit-jd", 1),
        "case {case:?}: expected the job definition's own ARN, got {definition_arn:?}"
    );

    // The policy IAM really holds names both ARNs: the `Resource` elements the `Ref`s
    // resolved to, read back through IAM's own reader.
    let (status, policy) = iam(
        &router,
        region,
        "GetRolePolicy",
        &[
            ("RoleName", "submit-job-role"),
            ("PolicyName", "submit"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from GetRolePolicy, got {status} with body {policy}"
    );
    let document = ::smithy_server_runtime::percent_decode(
        &xml_text(&policy, "PolicyDocument").unwrap_or_default(),
    );
    for arn in [&definition_arn, &queue_arn] {
        assert!(
            document.contains(arn.as_str()),
            "case {case:?}: expected the policy to scope {arn} , got {document}"
        );
    }

    // And AWS Batch accepts a submit addressed by the two values the stack published.
    let (status, submitted) = batch(
        &router,
        region,
        "/v1/submitjob",
        &json!({
            "jobName": "submit-once",
            "jobQueue": queue_arn,
            "jobDefinition": definition_arn,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from SubmitJob on the published handles, got {status} \
         with body {submitted}"
    );
    assert_eq!(
        submitted["jobName"].as_str(),
        Some("submit-once"),
        "case {case:?}: expected the submitted job's name, got {submitted}"
    );
    assert!(
        submitted["jobId"].as_str().is_some_and(|id| !id.is_empty()),
        "case {case:?}: expected a job id for the accepted submit, got {submitted}"
    );
}
