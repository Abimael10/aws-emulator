// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that declares an `AWS::AmazonMQ::Configuration` really creates that
//! configuration in Amazon MQ, and publishes the id, the ARN and the revision number that
//! service minted as the type's return values — so every output a template writes over
//! `Ref`, `Fn::GetAtt … Id`, `Fn::GetAtt … Arn` and `Fn::GetAtt … Revision` carries a VALUE,
//! and that value is what Amazon MQ's own reads of the configuration report.
//!
//! The rule under test: **a resource type CloudFormation provisions publishes exactly the
//! `Ref` and `Fn::GetAtt` return values its type documents, and each of those values is the
//! one the service that OWNS the resource minted for the resource it really created — so a
//! stack in `CREATE_COMPLETE` reports every declared output with a resolved `OutputValue`
//! that the owning service's own read agrees with, and a declaration that service will not
//! create fails the resource instead of completing the stack.** Two crates have to agree for
//! that to hold (CloudFormation makes the calls; Amazon MQ serves them, mints the id and the
//! ARN and numbers the revisions), so the suite lives here rather than in either crate.
//!
//! The rows differ along what the rule generalises over: which engine the configuration is
//! for (`ACTIVEMQ`, `RABBITMQ`) and whether it names a version at all, whether it states the
//! `Name` or leaves CloudFormation to generate one, whether it carries a `Description`, an
//! `AuthenticationStrategy` and `Tags`, how the declared document is spelled (a self-closed
//! root, a pair of tags, a document carrying children), whether the template is written in
//! JSON or YAML, and which intrinsic each output reads the resource with (`Ref`,
//! `Fn::GetAtt` over each published attribute, `Fn::Sub`).
//!
//! Every expected value is computed here from what the row declares, never read back out of
//! the answer being checked — except the configuration id itself, which is Amazon MQ's to
//! mint: what is checked about it is that ONE value appears everywhere the stack reports the
//! resource's identity, that it has the form that service publishes, that its own read
//! resolves it, and that the configuration's NAME does not resolve in its place.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery form encoder and the XML readers this crate writes ONCE
// (`tests/all/query_wire.rs`): "Nothing new should write another one — call this module."
// The same encoder escapes a value for a non-greedy URI label, which is what an SDK sends.
use super::query_wire::{element, elements, form_encode, members_of, opt_element, outputs_of};

/// How long this suite waits for a condition it polls for before giving up and asserting on
/// what it last saw. Generous: every wait here is for a transition the emulator's own clock
/// owes (a stack's status), and the suite must pass on a loaded host, so the deadline bounds
/// a FAILURE rather than timing the emulator.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

/// How often a poll looks again. Short enough that the suite is quick on an idle host and
/// never a wait of its own.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The single account id this emulator mints every ARN under.
const ACCOUNT: &str = "000000000000";

/// The revision a configuration CloudFormation created carries: its SECOND.
///
/// `CreateConfiguration` "creates a configuration from the engine's default settings" and
/// takes no document at all, so the `Data` the type declares reaches the configuration as the
/// revision `UpdateConfiguration` adds — the one every read of the configuration then reports
/// as its latest.
const DECLARED_REVISION: &str = "2";

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
    router.register_service(service_com_amazonaws_mq_mq::EmulatedService::new(engine));
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

/// Call one Amazon MQ operation over its restJson1 wire, signed for `region`. That service
/// signs under `mq`, which is also its endpoint prefix.
async fn mq(router: &MultiServiceRouter, region: &str, path: &str) -> (u16, Value) {
    let request = http::Request::builder()
        .method("GET")
        .uri(path)
        .header("host", format!("mq.{region}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("mq", region))
        .body(Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, payload)
}

/// The stack's status once it is TERMINAL, with the `DescribeStacks` answer it was read from.
///
/// Polled for the CONDITION — a status no further transition follows — rather than waiting a
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

/// Whether `id` has the form Amazon MQ publishes for a `ConfigurationId`: `c-` followed by
/// 128 bits written as lowercase hexadecimal in the 8-4-4-4-12 grouping that service's ids
/// carry.
fn is_configuration_id(id: &str) -> bool {
    let Some(digits) = id.strip_prefix("c-") else {
        return false;
    };
    let groups: Vec<&str> = digits.split('-').collect();
    groups.len() == 5
        && groups.iter().map(|group| group.len()).eq([8, 4, 4, 4, 12])
        && groups
            .iter()
            .all(|group| group.chars().all(|c| c.is_ascii_hexdigit()))
}

/// One row of the table: a template, and what the rule says the configuration it declares
/// must then be.
struct Row {
    case: &'static str,
    /// The Region the row deploys in — which is also the Region the configuration is created
    /// in, since the ARN carries it and Amazon MQ keys a configuration by Region.
    region: &'static str,
    /// The template body, in the form the row writes it (JSON or YAML).
    template: fn() -> String,
    /// The `Name` the template declares, or `None` where it leaves CloudFormation to generate
    /// one.
    declared_name: Option<&'static str>,
    /// The engine the configuration must report, in the spelling Amazon MQ answers an engine
    /// type in (`ActiveMQ`), not the `EngineType` token the template declares.
    expected_engine_type: &'static str,
    /// The engine version the configuration must report: the one the template declared, or
    /// the latest Amazon MQ offers for that engine where it declared none.
    expected_engine_version: &'static str,
    /// The authentication strategy the configuration must report, in the spelling that
    /// service answers one in (lower case): the declared one, or `simple` by default.
    expected_authentication_strategy: &'static str,
    /// The description the configuration's latest revision must carry — the empty string
    /// where the template states none.
    expected_description: &'static str,
    /// The document the revision must carry back, as Amazon MQ stores one: the declared XML
    /// with every content-free element written self-closed.
    expected_stored_document: &'static str,
    /// The tags the template declares, which the configuration must carry beside the three
    /// CloudFormation records on every resource it provisions.
    expected_tags: &'static [(&'static str, &'static str)],
    /// The output keys whose value must be the configuration id, each written with a
    /// different intrinsic over the same resource.
    id_outputs: &'static [&'static str],
    /// The output keys whose value must be the configuration ARN.
    arn_outputs: &'static [&'static str],
    /// The output keys whose value must be the latest revision's number.
    revision_outputs: &'static [&'static str],
}

/// The ActiveMQ document the rows declare, in the two spellings a template may write it in:
/// a root with children, a root spelled as a pair of tags, and a self-closed root.
const ACTIVEMQ_WITH_CHILDREN: &str = concat!(
    r#"<broker xmlns="http://activemq.apache.org/schema/core">"#,
    "<destinationPolicy><policyMap></policyMap></destinationPolicy>",
    "</broker>"
);
/// What Amazon MQ stores for [`ACTIVEMQ_WITH_CHILDREN`]: the same document with its
/// content-free element written self-closed, which is the one difference between a submitted
/// and a stored document that service serves.
const ACTIVEMQ_WITH_CHILDREN_STORED: &str = concat!(
    r#"<broker xmlns="http://activemq.apache.org/schema/core">"#,
    "<destinationPolicy><policyMap/></destinationPolicy>",
    "</broker>"
);
/// A root spelled as a pair of tags, which that same rule stores self-closed.
const ACTIVEMQ_PAIR: &str =
    r#"<broker xmlns="http://activemq.apache.org/schema/core" schedulePeriodForDestinationPurge="7"></broker>"#;
/// See [`ACTIVEMQ_PAIR`].
const ACTIVEMQ_PAIR_STORED: &str =
    r#"<broker xmlns="http://activemq.apache.org/schema/core" schedulePeriodForDestinationPurge="7"/>"#;
/// A self-closed root, which is already the form that service stores.
const ACTIVEMQ_SELF_CLOSED: &str =
    r#"<broker xmlns="http://activemq.apache.org/schema/core" persistent="true"/>"#;
/// A RabbitMQ configuration, whose document is Cuttlefish rather than XML — stored as sent,
/// there being no element to serialize.
const RABBITMQ_CUTTLEFISH: &str = "consumer_timeout = 1800000\n";

/// `Data` as a template carries it: "the base64-encoded XML configuration" (ActiveMQ) or the
/// base64-encoded Cuttlefish configuration (RabbitMQ).
fn encoded(document: &str) -> String {
    ::aws_smithy_types::base64::encode(document.as_bytes())
}

const ROWS: &[Row] = &[
    Row {
        case: "a named ActiveMQ configuration with a description and tags, JSON, Ref and \
               three Fn::GetAtts",
        region: "us-east-1",
        template: || {
            json!({
                "AWSTemplateFormatVersion": "2010-09-09",
                "Description": "an Amazon MQ ActiveMQ configuration",
                "Resources": {
                    "Cfg": {
                        "Type": "AWS::AmazonMQ::Configuration",
                        "Properties": {
                            "Name": "orders-config",
                            "Description": "the destination policy the orders brokers share",
                            "EngineType": "ACTIVEMQ",
                            "EngineVersion": "5.18",
                            "Data": encoded(ACTIVEMQ_WITH_CHILDREN),
                            "Tags": [
                                { "Key": "Owner", "Value": "payments" },
                                { "Key": "Tier", "Value": "prod" }
                            ]
                        }
                    }
                },
                "Outputs": {
                    "CfgId": { "Value": { "Ref": "Cfg" } },
                    "CfgIdAttribute": { "Value": { "Fn::GetAtt": ["Cfg", "Id"] } },
                    "CfgArn": { "Value": { "Fn::GetAtt": ["Cfg", "Arn"] } },
                    "CfgRevision": { "Value": { "Fn::GetAtt": ["Cfg", "Revision"] } }
                }
            })
            .to_string()
        },
        declared_name: Some("orders-config"),
        expected_engine_type: "ActiveMQ",
        expected_engine_version: "5.18",
        expected_authentication_strategy: "simple",
        expected_description: "the destination policy the orders brokers share",
        expected_stored_document: ACTIVEMQ_WITH_CHILDREN_STORED,
        expected_tags: &[("Owner", "payments"), ("Tier", "prod")],
        id_outputs: &["CfgId", "CfgIdAttribute"],
        arn_outputs: &["CfgArn"],
        revision_outputs: &["CfgRevision"],
    },
    Row {
        case: "an ActiveMQ configuration declaring LDAP authentication and a root spelled as \
               a pair of tags, YAML, Fn::GetAtt and Fn::Sub over Ref",
        region: "us-west-2",
        template: || {
            [
                "Resources:".to_string(),
                "  Cfg:".to_string(),
                "    Type: AWS::AmazonMQ::Configuration".to_string(),
                "    Properties:".to_string(),
                "      Name: events_active.config~1".to_string(),
                "      EngineType: ACTIVEMQ".to_string(),
                "      EngineVersion: '5.19'".to_string(),
                "      AuthenticationStrategy: LDAP".to_string(),
                format!("      Data: {}", encoded(ACTIVEMQ_PAIR)),
                "Outputs:".to_string(),
                "  CfgArn:".to_string(),
                "    Value: !GetAtt Cfg.Arn".to_string(),
                "  CfgRevision:".to_string(),
                "    Value: !GetAtt Cfg.Revision".to_string(),
                "  CfgHandle:".to_string(),
                "    Value: !Sub".to_string(),
                "      - 'configuration=${Id}'".to_string(),
                "      - Id: !Ref Cfg".to_string(),
                String::new(),
            ]
            .join("\n")
        },
        declared_name: Some("events_active.config~1"),
        expected_engine_type: "ActiveMQ",
        expected_engine_version: "5.19",
        expected_authentication_strategy: "ldap",
        expected_description: "",
        expected_stored_document: ACTIVEMQ_PAIR_STORED,
        expected_tags: &[],
        id_outputs: &[],
        arn_outputs: &["CfgArn"],
        revision_outputs: &["CfgRevision"],
    },
    Row {
        case: "a RabbitMQ configuration CloudFormation names, no EngineVersion, a Cuttlefish \
               document, JSON, Ref twice and Fn::GetAtt Revision",
        region: "eu-west-1",
        template: || {
            json!({
                "Resources": {
                    "Cfg": {
                        "Type": "AWS::AmazonMQ::Configuration",
                        "Properties": {
                            "EngineType": "RABBITMQ",
                            "Data": encoded(RABBITMQ_CUTTLEFISH)
                        }
                    }
                },
                "Outputs": {
                    "CfgId": { "Value": { "Ref": "Cfg" } },
                    "CfgIdAgain": { "Value": { "Ref": "Cfg" } },
                    "CfgRevision": { "Value": { "Fn::GetAtt": ["Cfg", "Revision"] } }
                }
            })
            .to_string()
        },
        declared_name: None,
        expected_engine_type: "RabbitMQ",
        // The create names no `EngineVersion`, so the configuration carries the latest
        // version Amazon MQ offers for RabbitMQ.
        expected_engine_version: "4.3",
        expected_authentication_strategy: "simple",
        expected_description: "",
        expected_stored_document: RABBITMQ_CUTTLEFISH,
        expected_tags: &[],
        id_outputs: &["CfgId", "CfgIdAgain"],
        arn_outputs: &[],
        revision_outputs: &["CfgRevision"],
    },
    Row {
        case: "an ActiveMQ configuration with a self-closed root and one tag, JSON, every \
               published attribute read at once",
        region: "ap-northeast-1",
        template: || {
            json!({
                "Resources": {
                    "Cfg": {
                        "Type": "AWS::AmazonMQ::Configuration",
                        "Properties": {
                            "Name": "audit-single",
                            "EngineType": "ACTIVEMQ",
                            "AuthenticationStrategy": "SIMPLE",
                            "Data": encoded(ACTIVEMQ_SELF_CLOSED),
                            "Tags": [{ "Key": "Team", "Value": "platform" }]
                        }
                    }
                },
                "Outputs": {
                    "CfgId": { "Value": { "Fn::GetAtt": ["Cfg", "Id"] } },
                    "CfgRef": { "Value": { "Ref": "Cfg" } },
                    "CfgArn": { "Value": { "Fn::GetAtt": ["Cfg", "Arn"] } },
                    "CfgArnComposed": { "Value": { "Fn::Sub": "${Cfg.Arn}" } },
                    "CfgRevision": { "Value": { "Fn::GetAtt": ["Cfg", "Revision"] } },
                    "CfgRevisionComposed": { "Value": { "Fn::Sub": "${Cfg.Revision}" } }
                }
            })
            .to_string()
        },
        declared_name: Some("audit-single"),
        expected_engine_type: "ActiveMQ",
        // No `EngineVersion`: the latest ActiveMQ version Amazon MQ offers.
        expected_engine_version: "5.19",
        expected_authentication_strategy: "simple",
        expected_description: "",
        expected_stored_document: ACTIVEMQ_SELF_CLOSED,
        expected_tags: &[("Team", "platform")],
        id_outputs: &["CfgId", "CfgRef"],
        arn_outputs: &["CfgArn", "CfgArnComposed"],
        revision_outputs: &["CfgRevision", "CfgRevisionComposed"],
    },
];

/// Every row: the stack publishes the id, the ARN and the revision Amazon MQ minted for the
/// configuration it declared, every declared output carries a VALUE, that service's own reads
/// resolve what the stack published, and deleting the stack deletes the configuration there.
#[tokio::test]
async fn a_stack_provisions_the_mq_configuration_its_template_declares() {
    let router = build_router();

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let region = row.region;
        let stack_name = format!("mq-config-stack-{index}");
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
            "case {case:?}: expected the stack to reach CREATE_COMPLETE, got {stack_status:?} \
             with reasons {:?}",
            failure_reasons(&router, region, &stack_name).await
        );

        // The identity of the declared resource: what a client reads out of
        // DescribeStackResources, which is the id Amazon MQ minted.
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
        let configuration_id = opt_element(&resources, "PhysicalResourceId")
            .unwrap_or_else(|| panic!("case {case:?}: no <PhysicalResourceId> in {resources}"));
        assert!(
            is_configuration_id(&configuration_id),
            "case {case:?}: expected a configuration id of Amazon MQ's own form, got \
             {configuration_id:?}"
        );
        if let Some(name) = row.declared_name {
            assert_ne!(
                configuration_id, name,
                "case {case:?}: expected the configuration id rather than its name, got \
                 {configuration_id:?}"
            );
        }
        // The ARN Amazon MQ builds around that id: `configuration:<configuration id>`.
        let configuration_arn =
            format!("arn:aws:mq:{region}:{ACCOUNT}:configuration:{configuration_id}");

        // EVERY declared output carries both halves: a value AWS never omits.
        for block in members_of(&described) {
            if let Some(key) = opt_element(block, "OutputKey") {
                let value = opt_element(block, "OutputValue");
                assert!(
                    value.as_deref().is_some_and(|value| !value.is_empty()),
                    "case {case:?}: output {key:?} must carry an OutputValue, got {block:?}"
                );
            }
        }
        let outputs = outputs_of(&described);
        for key in row.id_outputs {
            assert_eq!(
                outputs.get(*key),
                Some(&configuration_id),
                "case {case:?}: expected output {key:?} to be the configuration id \
                 {configuration_id:?}, got {:?}",
                outputs.get(*key)
            );
        }
        for key in row.arn_outputs {
            assert_eq!(
                outputs.get(*key),
                Some(&configuration_arn),
                "case {case:?}: expected output {key:?} to be the configuration ARN \
                 {configuration_arn:?}, got {:?}",
                outputs.get(*key)
            );
        }
        for key in row.revision_outputs {
            assert_eq!(
                outputs.get(*key).map(String::as_str),
                Some(DECLARED_REVISION),
                "case {case:?}: expected output {key:?} to be the revision the declared \
                 document is stored as, got {:?}",
                outputs.get(*key)
            );
        }
        if outputs.contains_key("CfgHandle") {
            assert_eq!(
                outputs.get("CfgHandle").map(String::as_str),
                Some(format!("configuration={configuration_id}").as_str()),
                "case {case:?}: expected the composed handle to carry the minted id, got {:?}",
                outputs.get("CfgHandle")
            );
        }

        // And Amazon MQ itself resolves that id to the configuration the template declared.
        let (status, read) = mq(
            &router,
            region,
            &format!("/v1/configurations/{}", form_encode(&configuration_id)),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from the Amazon MQ read of the id the stack \
             published, got {status} with body {read}"
        );
        let reported_name = read
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        match row.declared_name {
            Some(name) => assert_eq!(
                reported_name, name,
                "case {case:?}: expected the declared name, got {read}"
            ),
            // A declaration that names no configuration is created under the name
            // CloudFormation generated for the resource, which carries the stack's own name.
            None => assert!(
                reported_name.starts_with(&stack_name),
                "case {case:?}: expected a generated name carrying the stack's name, got \
                 {reported_name:?}"
            ),
        }
        for (member, expected) in [
            ("id", configuration_id.as_str()),
            ("arn", configuration_arn.as_str()),
            ("engineType", row.expected_engine_type),
            ("engineVersion", row.expected_engine_version),
            (
                "authenticationStrategy",
                row.expected_authentication_strategy,
            ),
        ] {
            assert_eq!(
                read.get(member).and_then(Value::as_str),
                Some(expected),
                "case {case:?}: expected {member} {expected:?}, got {read}"
            );
        }
        assert_eq!(
            read.pointer("/latestRevision/revision")
                .and_then(Value::as_i64),
            DECLARED_REVISION.parse::<i64>().ok(),
            "case {case:?}: expected the stack's Revision output and this read to report one \
             revision number, got {read}"
        );
        assert_eq!(
            read.pointer("/latestRevision/description")
                .and_then(Value::as_str),
            Some(row.expected_description),
            "case {case:?}: expected the declared description on the revision, got {read}"
        );

        // Beside what the template declared, CloudFormation records three tags of its own on
        // every resource it provisions — `aws:cloudformation:stack-name`,
        // `aws:cloudformation:stack-id` and `aws:cloudformation:logical-id`.
        let reported_tags = read.get("tags").cloned().unwrap_or(Value::Null);
        for (key, value) in row.expected_tags {
            assert_eq!(
                reported_tags.get(*key).and_then(Value::as_str),
                Some(*value),
                "case {case:?}: expected tag {key:?} to be {value:?}, got {reported_tags}"
            );
        }
        assert_eq!(
            reported_tags
                .get("aws:cloudformation:stack-name")
                .and_then(Value::as_str),
            Some(stack_name.as_str()),
            "case {case:?}: expected the stack-name tag CloudFormation records, got \
             {reported_tags}"
        );
        assert_eq!(
            reported_tags
                .get("aws:cloudformation:logical-id")
                .and_then(Value::as_str),
            Some("Cfg"),
            "case {case:?}: expected the logical-id tag CloudFormation records, got \
             {reported_tags}"
        );
        assert_eq!(
            reported_tags.as_object().map(serde_json::Map::len),
            Some(row.expected_tags.len() + 3),
            "case {case:?}: expected the {} declared tag(s) beside CloudFormation's three, \
             got {reported_tags}",
            row.expected_tags.len()
        );

        // The revision the stack's output names carries the document the template declared,
        // as Amazon MQ stores one.
        let (status, revision) = mq(
            &router,
            region,
            &format!(
                "/v1/configurations/{}/revisions/{DECLARED_REVISION}",
                form_encode(&configuration_id)
            ),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeConfigurationRevision, got {status} \
             with body {revision}"
        );
        let stored = revision.get("data").and_then(Value::as_str).unwrap_or_default();
        let stored = ::aws_smithy_types::base64::decode(stored).unwrap_or_default();
        assert_eq!(
            String::from_utf8_lossy(&stored),
            row.expected_stored_document,
            "case {case:?}: expected the declared document, got {revision}"
        );

        // The listing resolves the same configuration: exactly one entry carries the name,
        // and its id is what the stack reported.
        let (status, listed) = mq(&router, region, "/v1/configurations").await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from ListConfigurations, got {status} with body \
             {listed}"
        );
        let matching: Vec<&Value> = listed
            .get("configurations")
            .and_then(Value::as_array)
            .map(|entries| {
                entries
                    .iter()
                    .filter(|entry| {
                        entry.get("name").and_then(Value::as_str) == Some(reported_name.as_str())
                    })
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            matching.len(),
            1,
            "case {case:?}: expected exactly one listed configuration named \
             {reported_name:?}, got {listed}"
        );
        assert_eq!(
            matching[0].get("id").and_then(Value::as_str),
            Some(configuration_id.as_str()),
            "case {case:?}: expected the listed configuration's id to be the one the stack \
             reported, got {:?}",
            matching[0]
        );

        // The NAME the template asked for is not an identifier Amazon MQ answers to: that is
        // the whole point of reporting the minted id.
        let (status, by_name) = mq(
            &router,
            region,
            &format!("/v1/configurations/{}", form_encode(&reported_name)),
        )
        .await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected HTTP 404 reading the configuration by its NAME, got \
             {status} with body {by_name}"
        );

        // Deleting the stack deletes the configuration Amazon MQ holds for it.
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
        let (status, gone) = mq(
            &router,
            region,
            &format!("/v1/configurations/{}", form_encode(&configuration_id)),
        )
        .await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected the deleted configuration's id to resolve to nothing \
             once the stack's delete had finished, got {status} with body {gone}"
        );
    }
}

/// One row of the refused-configuration table: a declaration Amazon MQ will not create, and
/// the FIXED text of the rule it refuses it by.
struct RefusedRow {
    case: &'static str,
    /// The `Name` the template declares, which no configuration must end up carrying.
    name: &'static str,
    /// The properties the template declares beside that name.
    properties: fn() -> Value,
    /// A fragment of the sentence that service's refusal has to carry.
    expected_reason: &'static str,
}

const REFUSED_ROWS: &[RefusedRow] = &[
    RefusedRow {
        case: "a Data that is not base64 at all",
        name: "unencoded-data-config",
        properties: || {
            json!({
                "EngineType": "ACTIVEMQ",
                "Data": "<broker/> not base64"
            })
        },
        expected_reason: "Make sure your configuration data is base64-encoded.",
    },
    RefusedRow {
        case: "a Data that decodes to no document of the declared engine",
        name: "unparsable-data-config",
        properties: || {
            json!({
                "EngineType": "ACTIVEMQ",
                "Data": encoded("<broker><unclosed></broker>")
            })
        },
        expected_reason: "The specified configuration data could not be parsed for reason [",
    },
    RefusedRow {
        case: "an EngineType outside the set the service offers",
        name: "unknown-engine-config",
        properties: || {
            json!({
                "EngineType": "KAFKAMQ",
                "Data": encoded(ACTIVEMQ_SELF_CLOSED)
            })
        },
        expected_reason: "Broker engine type [KAFKAMQ] is invalid.",
    },
];

/// Every row: a configuration Amazon MQ refuses fails its resource and rolls the stack back
/// carrying that service's own refusal, and that service holds no configuration under the
/// declared name — so no stack ever reports an output over a resource nothing created.
#[tokio::test]
async fn a_configuration_amazon_mq_refuses_rolls_the_stack_back() {
    let router = build_router();
    let region = "eu-central-1";

    for (index, row) in REFUSED_ROWS.iter().enumerate() {
        let case = row.case;
        let stack_name = format!("mq-config-refused-{index}");
        let mut properties = (row.properties)();
        properties["Name"] = json!(row.name);
        let template = json!({
            "Resources": {
                "Cfg": {
                    "Type": "AWS::AmazonMQ::Configuration",
                    "Properties": properties
                }
            },
            "Outputs": { "CfgId": { "Value": { "Ref": "Cfg" } } }
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
            "case {case:?}: expected a declaration Amazon MQ refuses to roll the stack back, \
             got {stack_status:?} in {described}"
        );
        let reasons = failure_reasons(&router, region, &stack_name).await;
        assert!(
            reasons
                .iter()
                .any(|reason| reason.contains(row.expected_reason)),
            "case {case:?}: expected a reason carrying {:?}, got {reasons:?}",
            row.expected_reason
        );

        // And nothing was created: Amazon MQ holds no configuration under that name.
        let (_, listed) = mq(&router, region, "/v1/configurations").await;
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

    // The control: the same template with a document that service accepts deploys, and its
    // outputs resolve.
    let accepted = json!({
        "Resources": {
            "Cfg": {
                "Type": "AWS::AmazonMQ::Configuration",
                "Properties": {
                    "Name": "accepted-data-config",
                    "EngineType": "ACTIVEMQ",
                    "Data": encoded(ACTIVEMQ_SELF_CLOSED)
                }
            }
        },
        "Outputs": {
            "CfgId": { "Value": { "Ref": "Cfg" } },
            "CfgArn": { "Value": { "Fn::GetAtt": ["Cfg", "Arn"] } },
            "CfgRevision": { "Value": { "Fn::GetAtt": ["Cfg", "Revision"] } }
        }
    })
    .to_string();
    let (status, body) = cloudformation(
        &router,
        region,
        "CreateStack",
        &[
            ("StackName", "mq-config-accepted"),
            ("TemplateBody", &accepted),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"an accepted document\": expected HTTP 200 from CreateStack, got {status} with \
         body {body}"
    );
    let (stack_status, described) = settled_stack(&router, region, "mq-config-accepted").await;
    assert_eq!(
        stack_status, "CREATE_COMPLETE",
        "case \"an accepted document\": expected the stack to deploy, got {stack_status:?} in \
         {described} with reasons {:?}",
        failure_reasons(&router, region, "mq-config-accepted").await
    );
    let outputs: BTreeMap<String, String> = outputs_of(&described);
    for key in ["CfgId", "CfgArn", "CfgRevision"] {
        assert!(
            outputs.get(key).is_some_and(|value| !value.is_empty()),
            "case \"an accepted document\": expected output {key:?} to carry a value, got {:?}",
            outputs.get(key)
        );
    }
}
