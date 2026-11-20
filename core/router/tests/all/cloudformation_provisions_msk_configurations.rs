// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that declares an `AWS::MSK::Configuration` really creates that
//! configuration in Amazon MSK, publishes the ARN and the revision that service minted as
//! its return values, and rolls back carrying that service's own refusal when Amazon MSK
//! refuses the declaration.
//!
//! The rule under test: **a resource a stack declares is created by calling the service that
//! owns it, so what the stack publishes for the resource is what that service answered — the
//! identifiers it minted, resolvable by its own reads — and what the stack reports when that
//! service REFUSES is the failure, never a resource nothing created.** Two crates have to
//! agree for that to hold (CloudFormation makes the call; Amazon MSK serves it, mints the
//! ARN and admits the Apache Kafka versions), so the suite lives here rather than in either
//! crate.
//!
//! The rows differ along what the rule generalises over: whether the template is written in
//! JSON or YAML, whether it states the configuration's `Name` or leaves CloudFormation to
//! generate one, whether it carries a `Description`, how many Apache Kafka versions it lists,
//! how many lines its `ServerProperties` document carries, and which intrinsic each output
//! reads the resource with (`Ref`, `Fn::GetAtt` over the ARN and over each published member
//! of the latest revision, `Fn::Sub`).
//!
//! Every expected value is computed here from what the row declares, except the ARN and the
//! revision's creation instant, which are Amazon MSK's to mint: what is checked about those
//! is that ONE value appears everywhere the stack reports the resource's identity, that it has
//! the form that service publishes, and that that service's own reads resolve it.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery form encoder and the XML element readers this crate writes ONCE
// (`tests/all/query_wire.rs`). The same encoder escapes an ARN for a non-greedy URI label,
// which is what an SDK sends.
use super::query_wire::{element, elements, form_encode, members_of, opt_element};

/// How long this suite waits for a condition it polls for before giving up and asserting on
/// what it last saw. Generous: every wait here is for a span the emulator's own clock owes
/// (a stack's status transition, a configuration's deletion), and the test must pass on a
/// loaded host, so the deadline bounds a FAILURE rather than timing the emulator.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

/// How often a poll looks again. Short enough that the suite is quick on an idle host and
/// never a wait of its own.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The single account id this emulator mints every ARN under.
const ACCOUNT: &str = "000000000000";

/// The emulator with the two services this suite exercises, mounted on ONE engine exactly
/// as `core/router/src/lib.rs` mounts them. Request authentication is off, as in every other
/// suite here; the in-process callback CloudFormation makes signs its request either way
/// (the credential scope is what routes it).
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_kafka_kafka::EmulatedService::new(
        engine,
    ));
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service` in `region`, which
/// is what routes the request and what each handler reads its Region from. The signature is
/// never checked: this router runs with request authentication off.
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
    let payload = String::from_utf8_lossy(response.body()).to_string();
    (status, payload)
}

/// Call one Amazon MSK operation over its restJson1 wire, signed for `region`. That service
/// signs under `kafka`, which is also its endpoint prefix.
async fn kafka(router: &MultiServiceRouter, region: &str, path: &str) -> (u16, Value) {
    let request = http::Request::builder()
        .method("GET")
        .uri(path)
        .header("host", format!("kafka.{region}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("kafka", region))
        .body(Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, payload)
}

/// The stack's status once it is TERMINAL, with the `DescribeStacks` answer it was read from.
///
/// Polled for the condition — a status no further transition follows — rather than waiting a
/// span: the stack's own transitions are the emulator's to make, and a suite that slept a
/// fixed time would pass or fail on how fast the host is.
async fn settled_stack(
    router: &MultiServiceRouter,
    region: &str,
    stack_name: &str,
) -> (String, String) {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (_, body) =
            cloudformation(router, region, "DescribeStacks", &[("StackName", stack_name)]).await;
        let status = opt_element(&body, "StackStatus").unwrap_or_default();
        if status.ends_with("_COMPLETE") || Instant::now() >= deadline {
            return (status, body);
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Every `ResourceStatusReason` the stack's events carry, which is where the refusal the
/// owning service answered travels.
async fn failure_reasons(
    router: &MultiServiceRouter,
    region: &str,
    stack_name: &str,
) -> Vec<String> {
    let (_, events) = cloudformation(
        router,
        region,
        "DescribeStackEvents",
        &[("StackName", stack_name)],
    )
    .await;
    elements(&events, "ResourceStatusReason")
}

/// One instant as CloudFormation publishes one in a return value: UTC with exactly three
/// fractional digits, the fraction TRUNCATED rather than rounded.
///
/// The spelling of the SAME instant the owning service reported, so an output read off a
/// stack and that service's own read of the revision compare as one value.
fn millisecond_utc(reported: &str) -> String {
    let body = reported.strip_suffix('Z').unwrap_or(reported);
    let (seconds, fraction) = body.split_once('.').unwrap_or((body, ""));
    let mut milliseconds: String = fraction.chars().take(3).collect();
    while milliseconds.len() < 3 {
        milliseconds.push('0');
    }
    format!("{seconds}.{milliseconds}Z")
}

/// One row of the configuration table: a template, and what the rule says the configuration
/// it declares must then be.
struct Row {
    case: &'static str,
    /// The Region the row deploys in — which is also the Region the configuration is created
    /// in, since the ARN carries it and Amazon MSK keys a configuration by Region.
    region: &'static str,
    /// The template body, in the form the row writes it (JSON or YAML).
    template: fn() -> String,
    /// The `Name` the template declares, or `None` where it leaves CloudFormation to generate
    /// one.
    declared_name: Option<&'static str>,
    /// The Apache Kafka versions the configuration must report, in the order declared.
    expected_versions: &'static [&'static str],
    /// The `server.properties` text the revision must carry back, byte for byte.
    expected_server_properties: &'static str,
    /// The description the configuration must report, where the template states one.
    expected_description: Option<&'static str>,
    /// The output keys whose value must be the configuration ARN, each written with a
    /// different intrinsic over the same resource.
    arn_outputs: &'static [&'static str],
    /// The output key carrying the latest revision's creation instant.
    creation_time_output: &'static str,
    /// The output key carrying the latest revision's number, and the number it must report —
    /// `1` for a configuration a create has just minted.
    revision_output: &'static str,
}

const ROWS: &[Row] = &[
    Row {
        case: "a named configuration, one Kafka version, JSON, Ref and three Fn::GetAtts",
        region: "us-east-1",
        template: || {
            json!({
                "Resources": {
                    "MskConfig": {
                        "Type": "AWS::MSK::Configuration",
                        "Properties": {
                            "Name": "orders-config",
                            "ServerProperties": "auto.create.topics.enable=true\n",
                            "KafkaVersionsList": ["3.9.x"]
                        }
                    }
                },
                "Outputs": {
                    "ConfigArn": { "Value": { "Ref": "MskConfig" } },
                    "ConfigArnAttribute": { "Value": { "Fn::GetAtt": ["MskConfig", "Arn"] } },
                    "ConfigCreationTime": {
                        "Value": { "Fn::GetAtt": ["MskConfig", "LatestRevision.CreationTime"] }
                    },
                    "ConfigRevision": {
                        "Value": { "Fn::GetAtt": ["MskConfig", "LatestRevision.Revision"] }
                    }
                }
            })
            .to_string()
        },
        declared_name: Some("orders-config"),
        expected_versions: &["3.9.x"],
        expected_server_properties: "auto.create.topics.enable=true\n",
        expected_description: None,
        arn_outputs: &["ConfigArn", "ConfigArnAttribute"],
        creation_time_output: "ConfigCreationTime",
        revision_output: "ConfigRevision",
    },
    Row {
        case: "a described configuration, two Kafka versions, a two-line document, YAML, \
               Ref and Fn::GetAtt",
        region: "eu-west-1",
        template: || {
            [
                "Resources:",
                "  MskConfig:",
                "    Type: AWS::MSK::Configuration",
                "    Properties:",
                "      Name: events-config",
                "      Description: the broker settings the events clusters share",
                "      ServerProperties: |",
                "        auto.create.topics.enable=true",
                "        default.replication.factor=2",
                "      KafkaVersionsList:",
                "        - 3.9.x",
                "        - 3.8.x",
                "Outputs:",
                "  ConfigArn:",
                "    Value: !Ref MskConfig",
                "  ConfigCreationTime:",
                "    Value: !GetAtt MskConfig.LatestRevision.CreationTime",
                "  ConfigRevision:",
                "    Value: !GetAtt MskConfig.LatestRevision.Revision",
                "",
            ]
            .join("\n")
        },
        declared_name: Some("events-config"),
        expected_versions: &["3.9.x", "3.8.x"],
        expected_server_properties: "auto.create.topics.enable=true\ndefault.replication.factor=2\n",
        expected_description: Some("the broker settings the events clusters share"),
        arn_outputs: &["ConfigArn"],
        creation_time_output: "ConfigCreationTime",
        revision_output: "ConfigRevision",
    },
    Row {
        case: "a configuration CloudFormation names, JSON, the ARN read through Fn::Sub",
        region: "ap-southeast-2",
        template: || {
            json!({
                "Resources": {
                    "MskConfig": {
                        "Type": "AWS::MSK::Configuration",
                        "Properties": {
                            "ServerProperties": "num.partitions=3\n",
                            "KafkaVersionsList": ["3.6.0"]
                        }
                    }
                },
                "Outputs": {
                    "ConfigArn": { "Value": { "Ref": "MskConfig" } },
                    "ConfigArnComposed": { "Value": { "Fn::Sub": "${MskConfig}" } },
                    "ConfigCreationTime": {
                        "Value": { "Fn::GetAtt": ["MskConfig", "LatestRevision.CreationTime"] }
                    },
                    "ConfigRevision": {
                        "Value": { "Fn::GetAtt": ["MskConfig", "LatestRevision.Revision"] }
                    }
                }
            })
            .to_string()
        },
        declared_name: None,
        expected_versions: &["3.6.0"],
        expected_server_properties: "num.partitions=3\n",
        expected_description: None,
        arn_outputs: &["ConfigArn", "ConfigArnComposed"],
        creation_time_output: "ConfigCreationTime",
        revision_output: "ConfigRevision",
    },
];

/// Every row: the stack publishes the ARN and the revision Amazon MSK minted for the
/// configuration it declared, every declared output carries a VALUE, that service's own reads
/// resolve what the stack published, and deleting the stack deletes the configuration there.
#[tokio::test]
async fn a_stack_provisions_the_msk_configuration_its_template_declares() {
    let router = build_router();

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let region = row.region;
        let stack_name = format!("msk-config-stack-{index}");
        let template = (row.template)();

        let (status, body) = cloudformation(
            &router,
            region,
            "CreateStack",
            &[("StackName", &stack_name), ("TemplateBody", &template)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
        );
        // The stack's own id, which is how a DELETED stack is still readable: a deleted stack
        // is no longer resolvable by name.
        let stack_id = element(&body, "StackId");

        let (stack_status, described) = settled_stack(&router, region, &stack_name).await;
        assert_eq!(
            stack_status, "CREATE_COMPLETE",
            "case {case:?}: expected the stack to reach CREATE_COMPLETE, got \
             {stack_status:?} with reasons {:?}",
            failure_reasons(&router, region, &stack_name).await
        );

        // The identity of the declared resource: what a client reads out of
        // DescribeStackResources, which is the ARN Amazon MSK minted.
        let (status, resources) = cloudformation(
            &router,
            region,
            "DescribeStackResources",
            &[("StackName", &stack_name)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStackResources, got {status} with \
             body {resources}"
        );
        let configuration_arn = opt_element(&resources, "PhysicalResourceId")
            .unwrap_or_else(|| panic!("case {case:?}: no <PhysicalResourceId> in {resources}"));
        // The form Amazon MSK publishes for a configuration ARN:
        // `arn:aws:kafka:<region>:<account>:configuration/<name>/<id>`.
        let prefix = format!("arn:aws:kafka:{region}:{ACCOUNT}:configuration/");
        assert!(
            configuration_arn.starts_with(&prefix),
            "case {case:?}: expected a configuration ARN starting {prefix:?}, got \
             {configuration_arn:?}"
        );
        let path = &configuration_arn[prefix.len()..];
        let (reported_name, minted_id) = path
            .split_once('/')
            .unwrap_or_else(|| panic!("case {case:?}: expected <name>/<id>, got {path:?}"));
        assert!(
            !minted_id.is_empty() && !minted_id.contains('/'),
            "case {case:?}: expected one service-minted identifier after the configuration \
             name, got {minted_id:?}"
        );
        match row.declared_name {
            Some(name) => assert_eq!(
                reported_name, name,
                "case {case:?}: expected the declared name in the ARN, got {reported_name:?}"
            ),
            // A declaration that names no configuration is created under the name
            // CloudFormation generated for the resource, which carries the stack's own name.
            None => assert!(
                reported_name.starts_with(&stack_name),
                "case {case:?}: expected a generated name carrying the stack's name, got \
                 {reported_name:?}"
            ),
        }

        // EVERY declared output carries both halves: a value AWS never omits.
        for block in members_of(&described) {
            let key = opt_element(block, "OutputKey");
            if let Some(key) = key {
                let value = opt_element(block, "OutputValue");
                assert!(
                    value.as_deref().is_some_and(|value| !value.is_empty()),
                    "case {case:?}: output {key:?} must carry an OutputValue, got {block:?}"
                );
            }
        }
        let outputs: std::collections::BTreeMap<String, String> =
            elements(&described, "OutputKey")
                .into_iter()
                .zip(elements(&described, "OutputValue"))
                .collect();
        for key in row.arn_outputs {
            assert_eq!(
                outputs.get(*key),
                Some(&configuration_arn),
                "case {case:?}: expected output {key:?} to be the configuration ARN \
                 {configuration_arn:?}, got {:?}",
                outputs.get(*key)
            );
        }
        assert_eq!(
            outputs.get(row.revision_output).map(String::as_str),
            Some("1"),
            "case {case:?}: a created configuration's latest revision is its first, got {:?}",
            outputs.get(row.revision_output)
        );

        // Amazon MSK itself resolves that ARN to the configuration the template declared.
        let (status, read) = kafka(
            &router,
            region,
            &format!("/v1/configurations/{}", form_encode(&configuration_arn)),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from the Amazon MSK read of the ARN the stack \
             published, got {status} with body {read}"
        );
        assert_eq!(
            read.get("state").and_then(Value::as_str),
            Some("ACTIVE"),
            "case {case:?}: expected the configuration ACTIVE, got {read}"
        );
        assert_eq!(
            read.get("name").and_then(Value::as_str),
            Some(reported_name),
            "case {case:?}: expected the configuration named as the ARN says, got {read}"
        );
        assert_eq!(
            read.get("kafkaVersions").cloned().unwrap_or(Value::Null),
            json!(row.expected_versions),
            "case {case:?}: expected the declared Kafka versions, got {read}"
        );
        assert_eq!(
            read.pointer("/latestRevision/revision")
                .and_then(Value::as_i64),
            Some(1),
            "case {case:?}: expected revision 1, got {read}"
        );
        assert_eq!(
            read.pointer("/latestRevision/description")
                .and_then(Value::as_str),
            row.expected_description,
            "case {case:?}: expected the declared description on the revision, got {read}"
        );
        // The instant the output carries IS the instant that service reports for the
        // revision, in the millisecond UTC spelling CloudFormation publishes one in.
        let reported_instant = read
            .pointer("/latestRevision/creationTime")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case:?}: no revision creationTime in {read}"));
        let published = outputs
            .get(row.creation_time_output)
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            published,
            millisecond_utc(reported_instant),
            "case {case:?}: expected the revision's creation instant to the millisecond, got \
             {published:?} against {reported_instant:?}"
        );
        assert!(
            published.len() == "2026-01-02T03:04:05.678Z".len()
                && published.ends_with('Z')
                && published.as_bytes()[10] == b'T'
                && published.as_bytes()[19] == b'.',
            "case {case:?}: expected a YYYY-MM-DDTHH:MM:SS.mmmZ instant, got {published:?}"
        );

        // The listing reports the same ARN for that name, and the revision carries the
        // document the template declared, byte for byte.
        let (status, listed) = kafka(&router, region, "/v1/configurations").await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from ListConfigurations, got {status} with body \
             {listed}"
        );
        let listed_arn = listed
            .get("configurations")
            .and_then(Value::as_array)
            .and_then(|entries| {
                entries
                    .iter()
                    .find(|entry| entry.get("name").and_then(Value::as_str) == Some(reported_name))
            })
            .and_then(|entry| entry.get("arn"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        assert_eq!(
            listed_arn, configuration_arn,
            "case {case:?}: expected the listing to report the ARN the stack published, got \
             {listed_arn:?}"
        );
        let (status, revision) = kafka(
            &router,
            region,
            &format!(
                "/v1/configurations/{}/revisions/1",
                form_encode(&configuration_arn)
            ),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeConfigurationRevision, got {status} \
             with body {revision}"
        );
        // `ServerProperties` is a blob, which the restJson1 wire carries base64-encoded.
        let stored = revision
            .get("serverProperties")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let stored = ::aws_smithy_types::base64::decode(stored).unwrap_or_default();
        assert_eq!(
            String::from_utf8_lossy(&stored),
            row.expected_server_properties,
            "case {case:?}: expected the declared server.properties document, got {revision}"
        );

        // Deleting the stack deletes the configuration Amazon MSK holds for it, and the
        // stack's deletion is not reported finished until that service has really taken the
        // configuration away — so the ARN resolves to nothing as soon as the delete has
        // settled, which that service reports as a bad request rather than as a
        // configuration in some other state.
        let (status, body) =
            cloudformation(&router, region, "DeleteStack", &[("StackName", &stack_name)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with body {body}"
        );
        let (deleted_status, deleted) = settled_stack(&router, region, &stack_id).await;
        assert_eq!(
            deleted_status, "DELETE_COMPLETE",
            "case {case:?}: expected the stack's deletion to finish, got {deleted_status:?} \
             in {deleted}"
        );
        let (status, gone) = kafka(
            &router,
            region,
            &format!("/v1/configurations/{}", form_encode(&configuration_arn)),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the deleted configuration's ARN to resolve to nothing \
             once the stack's delete had finished, got {status} with body {gone}"
        );
        assert_eq!(
            gone.get("message").and_then(Value::as_str),
            Some("Configuration ARN does not exist."),
            "case {case:?}: expected Amazon MSK's sentence for an ARN it holds nothing for, \
             got {gone}"
        );
    }
}

/// One row of the refused-configuration table: a declaration Amazon MSK refuses, and the
/// FIXED text of the rule it refuses it by.
struct RefusedRow {
    case: &'static str,
    /// The `Name` the template declares, which no configuration must end up carrying.
    name: &'static str,
    /// The `KafkaVersionsList` the template declares.
    versions: &'static [&'static str],
    /// The version the refusal must name as the one that is not supported.
    rejected: &'static str,
}

const REFUSED_ROWS: &[RefusedRow] = &[
    RefusedRow {
        case: "one Kafka version that is no version the service offers",
        name: "rejected-version-config",
        versions: &["99.99.x"],
        rejected: "99.99.x",
    },
    RefusedRow {
        case: "an offered version beside one the service does not offer",
        name: "rejected-mixed-config",
        versions: &["3.9.x", "0.0.0-not-a-version"],
        rejected: "0.0.0-not-a-version",
    },
];

/// Every row: the stack rolls back carrying Amazon MSK's own refusal of the declared Apache
/// Kafka version — the fixed text, the status, the handler envelope and the invalid-request
/// class — and that service holds no configuration under the declared name.
#[tokio::test]
async fn a_configuration_amazon_msk_refuses_rolls_the_stack_back() {
    let router = build_router();
    let region = "us-west-2";

    // The versions the service OFFERS, which the refusal's valid-value list has to carry and
    // the rejected version has to be outside of.
    let (status, versions) = kafka(&router, region, "/v1/kafka-versions").await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from ListKafkaVersions, got {status} with body {versions}"
    );
    let active: Vec<String> = versions
        .get("kafkaVersions")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter(|entry| entry.get("status").and_then(Value::as_str) == Some("ACTIVE"))
                .filter_map(|entry| entry.get("version").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    assert!(
        !active.is_empty(),
        "expected ListKafkaVersions to report at least one ACTIVE version, got {versions}"
    );

    for (index, row) in REFUSED_ROWS.iter().enumerate() {
        let case = row.case;
        let stack_name = format!("msk-config-refused-{index}");
        let template = json!({
            "Resources": {
                "MskConfig": {
                    "Type": "AWS::MSK::Configuration",
                    "Properties": {
                        "Name": row.name,
                        "ServerProperties": "auto.create.topics.enable=true\n",
                        "KafkaVersionsList": row.versions
                    }
                }
            },
            "Outputs": { "ConfigArn": { "Value": { "Ref": "MskConfig" } } }
        })
        .to_string();

        let (status, body) = cloudformation(
            &router,
            region,
            "CreateStack",
            &[("StackName", &stack_name), ("TemplateBody", &template)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
        );

        let (stack_status, described) = settled_stack(&router, region, &stack_name).await;
        assert_eq!(
            stack_status, "ROLLBACK_COMPLETE",
            "case {case:?}: expected a template Amazon MSK refuses to roll back, got \
             {stack_status:?} in {described}"
        );

        let reasons = failure_reasons(&router, region, &stack_name).await;
        let reason = reasons
            .iter()
            .find(|reason| reason.contains("Unsupported KafkaVersion"))
            .unwrap_or_else(|| {
                panic!("case {case:?}: no reason carrying the service's refusal in {reasons:?}")
            });
        assert!(
            reason.contains(&format!("Unsupported KafkaVersion [{}]. Valid values: [", row.rejected)),
            "case {case:?}: expected the service's sentence naming {:?}, got {reason:?}",
            row.rejected
        );
        assert!(
            reason.contains("(Service: Kafka, Status Code: 400,")
                && reason.contains("HandlerErrorCode: InvalidRequest"),
            "case {case:?}: expected the handler envelope for a 400 of that service, got \
             {reason:?}"
        );
        assert!(
            reason.contains("[ClientRequestToken: "),
            "case {case:?}: expected the provider's request-token prefix, got {reason:?}"
        );
        // The actionable half of the refusal: every version the service reports ACTIVE is
        // offered as a retry, and the rejected one is not.
        let listed = reason
            .split_once("Valid values: [")
            .and_then(|(_, rest)| rest.split_once(']'))
            .map(|(list, _)| list.to_string())
            .unwrap_or_default();
        for version in &active {
            assert!(
                listed.split(", ").any(|entry| entry == version),
                "case {case:?}: expected the valid-value list to offer {version:?}, got \
                 {listed:?}"
            );
        }
        assert!(
            !listed.split(", ").any(|entry| entry == row.rejected),
            "case {case:?}: the valid-value list must not offer the rejected version, got \
             {listed:?}"
        );

        // And nothing was created: Amazon MSK holds no configuration under that name.
        let (_, listed) = kafka(&router, region, "/v1/configurations").await;
        let held = listed
            .get("configurations")
            .and_then(Value::as_array)
            .map(|entries| {
                entries
                    .iter()
                    .filter(|entry| entry.get("name").and_then(Value::as_str) == Some(row.name))
                    .count()
            })
            .unwrap_or(0);
        assert_eq!(
            held, 0,
            "case {case:?}: expected no configuration named {:?}, got {listed}",
            row.name
        );
    }

    // The control: the same template with a version the service offers deploys.
    let accepted = json!({
        "Resources": {
            "MskConfig": {
                "Type": "AWS::MSK::Configuration",
                "Properties": {
                    "Name": "accepted-version-config",
                    "ServerProperties": "auto.create.topics.enable=true\n",
                    "KafkaVersionsList": [active[0].as_str()]
                }
            }
        },
        "Outputs": { "ConfigArn": { "Value": { "Ref": "MskConfig" } } }
    })
    .to_string();
    let (status, body) = cloudformation(
        &router,
        region,
        "CreateStack",
        &[
            ("StackName", "msk-config-accepted"),
            ("TemplateBody", &accepted),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"an offered version\": expected HTTP 200 from CreateStack, got {status} with \
         body {body}"
    );
    let (stack_status, described) = settled_stack(&router, region, "msk-config-accepted").await;
    assert_eq!(
        stack_status, "CREATE_COMPLETE",
        "case \"an offered version\": expected the stack to deploy, got {stack_status:?} in \
         {described} with reasons {:?}",
        failure_reasons(&router, region, "msk-config-accepted").await
    );
}

/// One row of the serverless-cluster table: how the declaration states its client
/// authentication, and whether Amazon MSK admits it.
struct ServerlessRow {
    case: &'static str,
    /// The `ClusterName` the template declares.
    cluster_name: &'static str,
    /// The `ClientAuthentication` the template declares, or `None` where it declares none.
    authentication: fn() -> Option<Value>,
    /// Whether the cluster must end up existing.
    admitted: bool,
}

const SERVERLESS_ROWS: &[ServerlessRow] = &[
    ServerlessRow {
        case: "SASL/IAM turned off",
        cluster_name: "serverless-iam-off",
        authentication: || Some(json!({ "Sasl": { "Iam": { "Enabled": false } } })),
        admitted: false,
    },
    ServerlessRow {
        case: "no ClientAuthentication at all",
        cluster_name: "serverless-no-auth",
        authentication: || None,
        admitted: false,
    },
    ServerlessRow {
        case: "a Sasl block naming no Iam",
        cluster_name: "serverless-no-iam",
        authentication: || Some(json!({ "Sasl": {} })),
        admitted: false,
    },
    ServerlessRow {
        case: "SASL/IAM turned on",
        cluster_name: "serverless-iam-on",
        authentication: || Some(json!({ "Sasl": { "Iam": { "Enabled": true } } })),
        admitted: true,
    },
];

/// Every row: a serverless cluster declared without the SASL/IAM authentication Amazon MSK
/// requires fails its resource and rolls the stack back, carrying that service's own sentence
/// and the member it attributed the refusal to, and no cluster is listed under the declared
/// name — while the same template with SASL/IAM on deploys and reports it on.
#[tokio::test]
async fn a_serverless_cluster_without_sasl_iam_rolls_the_stack_back() {
    let router = build_router();
    let region = "eu-central-1";

    for (index, row) in SERVERLESS_ROWS.iter().enumerate() {
        let case = row.case;
        let stack_name = format!("msk-serverless-{index}");
        let mut properties = json!({
            "ClusterName": row.cluster_name,
            "VpcConfigs": [
                { "SubnetIds": ["subnet-0a1b2c3d", "subnet-4e5f6a7b"] }
            ]
        });
        if let Some(authentication) = (row.authentication)() {
            properties["ClientAuthentication"] = authentication;
        }
        let template = json!({
            "Resources": {
                "MskServerless": {
                    "Type": "AWS::MSK::ServerlessCluster",
                    "Properties": properties
                }
            },
            "Outputs": { "ClusterArn": { "Value": { "Ref": "MskServerless" } } }
        })
        .to_string();

        let (status, body) = cloudformation(
            &router,
            region,
            "CreateStack",
            &[("StackName", &stack_name), ("TemplateBody", &template)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
        );
        let (stack_status, described) = settled_stack(&router, region, &stack_name).await;

        let (_, listed) = kafka(
            &router,
            region,
            &format!("/api/v2/clusters?clusterNameFilter={}", row.cluster_name),
        )
        .await;
        let held = listed
            .get("clusterInfoList")
            .and_then(Value::as_array)
            .map(|entries| {
                entries
                    .iter()
                    .filter(|entry| {
                        entry.get("clusterName").and_then(Value::as_str) == Some(row.cluster_name)
                    })
                    .count()
            })
            .unwrap_or(0);

        if !row.admitted {
            assert_eq!(
                stack_status, "ROLLBACK_COMPLETE",
                "case {case:?}: expected a cluster Amazon MSK refuses to roll the stack back, \
                 got {stack_status:?} in {described}"
            );
            let reasons = failure_reasons(&router, region, &stack_name).await;
            let reason = reasons
                .iter()
                .find(|reason| reason.contains("serverless cluster"))
                .unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: no reason carrying the service's refusal in {reasons:?}"
                    )
                });
            assert!(
                reason.contains("A serverless cluster must use SASL/IAM authentication."),
                "case {case:?}: expected the service's own sentence, got {reason:?}"
            );
            assert!(
                reason.contains("(Service: Kafka, Status Code: 400,")
                    && reason.contains("HandlerErrorCode: InvalidRequest"),
                "case {case:?}: expected the handler envelope for a 400 of that service, got \
                 {reason:?}"
            );
            assert!(
                reason.contains("'clientAuthentication'"),
                "case {case:?}: expected the member the refusal was attributed to, got \
                 {reason:?}"
            );
            assert_eq!(
                held, 0,
                "case {case:?}: expected no cluster named {:?}, got {listed}",
                row.cluster_name
            );
            continue;
        }

        assert_eq!(
            stack_status, "CREATE_COMPLETE",
            "case {case:?}: expected the stack to deploy, got {stack_status:?} in {described} \
             with reasons {:?}",
            failure_reasons(&router, region, &stack_name).await
        );
        assert_eq!(
            held, 1,
            "case {case:?}: expected exactly one cluster named {:?}, got {listed}",
            row.cluster_name
        );
        let cluster_arn = element(&described, "OutputValue");
        let (status, read) = kafka(
            &router,
            region,
            &format!("/api/v2/clusters/{}", form_encode(&cluster_arn)),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeClusterV2, got {status} with body \
             {read}"
        );
        assert_eq!(
            read.pointer("/clusterInfo/serverless/clientAuthentication/sasl/iam/enabled")
                .and_then(Value::as_bool),
            Some(true),
            "case {case:?}: expected the cluster to report SASL/IAM on, got {read}"
        );
    }
}
