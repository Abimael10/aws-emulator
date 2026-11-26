// SPDX-License-Identifier: CC0-1.0

//! An `AWS::Serverless::Connector` and the `Permissions` its source/destination pair publishes,
//! end to end over the awsQuery wire.
//!
//! RULE under test: **a connector's `Permissions` are decided by the profile of the pair of
//! resource types it runs between, and a connector asking for anything that pair does not publish
//! is a document the `AWS::Serverless-2016-10-31` transform cannot process: the change set that
//! submitted it ends `FAILED`, carrying the macro's own diagnostic as the `StatusReason` —
//! `Unsupported 'Permissions' provided for connector from <source type> to <destination type>;
//! valid values are: <the values that pair publishes>.`, named by the connector's logical id and
//! counted with the other faults — and nothing is planned. A connector whose pair publishes
//! everything it asks for is processed, and the change set reaches `CREATE_COMPLETE` with no
//! reason at all.**
//!
//! Why it matters: a client creates a change set to ask whether a template is deployable. A
//! connector is pure authorization — what it expands into is read out of its profile — so a
//! connector asking for a permission its pair does not publish deploys a grant the author did not
//! express, and the authoring error surfaces as a permission failure at runtime instead of a
//! precise diagnostic at deploy time.
//!
//! The rows differ along what the rule generalises over: WHICH pair the connector runs between (a
//! pair that publishes reading and writing, and three that publish only writing), HOW the ends
//! state their types (a declaration the macro expands into another type, and a resource type
//! CloudFormation owns outright), and WHAT the connector asks for (a value no profile publishes,
//! a value another pair publishes, a supported value beside an unsupported one, no permission at
//! all, and a set the pair publishes whole).
//!
//! `src/state/sam/connector.rs` holds the same table against the validator directly; this suite
//! is about what a CLIENT reads back from the change set.

use std::sync::Arc;

use smithy_server_runtime::MultiServiceRouter;

use crate::change_set_lifecycle::settle_the_store;
use crate::elbv2_resource_provisioning::poll_to_terminal;
use crate::iam_identity_resources::xml_policy_document;
use crate::stack_lifecycle::{
    acknowledging_iam, call, create_stack, get_template, iam, member, router_with_engine,
    xml_blocks, xml_text, xml_texts,
};

/// The macro the AWS Serverless Application Model is published as.
const TRANSFORM: &str = "AWS::Serverless-2016-10-31";

/// The name every row gives its change set.
const PLAN: &str = "plan";

/// One row: the declarations the document carries, and what the change set must report.
struct Row {
    case: &'static str,
    stack: &'static str,
    /// The `Resources` section, as a JSON fragment.
    resources: &'static str,
    /// The two resource types the diagnostic must name and the values their profile publishes,
    /// or `None` for a document the transform processes.
    unsupported: Option<(&'static str, &'static str, &'static str)>,
}

fn rows() -> Vec<Row> {
    vec![
        // The pair that publishes both values, asked for a third: a function and a table, each
        // declared as the macro's own input type, so what the diagnostic names is what those
        // declarations BECOME.
        Row {
            case: "a permission no profile publishes, between declarations the macro expands",
            stack: "connector-unsupported-value",
            resources: r#"{
                "Fn": {"Type": "AWS::Serverless::Function", "Properties": {
                    "Runtime": "python3.12", "Handler": "index.handler",
                    "InlineCode": "def handler(event, context): pass"}},
                "Items": {"Type": "AWS::Serverless::SimpleTable", "Properties": {}},
                "Connector": {"Type": "AWS::Serverless::Connector", "Properties": {
                    "Source": {"Id": "Fn"},
                    "Destination": {"Id": "Items"},
                    "Permissions": ["Delete"]}}
            }"#,
            unsupported: Some((
                "AWS::Lambda::Function",
                "AWS::DynamoDB::Table",
                "Read, Write",
            )),
        },
        // READING a destination its pair only publishes writing to: the value is one AWS
        // publishes, just not for this pair.
        Row {
            case: "reading a destination whose pair publishes only writing",
            stack: "connector-read-a-topic",
            resources: r#"{
                "Fn": {"Type": "AWS::Serverless::Function", "Properties": {
                    "Runtime": "python3.12", "Handler": "index.handler",
                    "InlineCode": "def handler(event, context): pass"}},
                "Topic": {"Type": "AWS::SNS::Topic", "Properties": {}},
                "Connector": {"Type": "AWS::Serverless::Connector", "Properties": {
                    "Source": {"Id": "Fn"},
                    "Destination": {"Id": "Topic"},
                    "Permissions": ["Read"]}}
            }"#,
            unsupported: Some(("AWS::Lambda::Function", "AWS::SNS::Topic", "Write")),
        },
        // One supported value beside one the pair does not publish, between two resource types
        // CloudFormation owns outright — no expansion involved on either end.
        Row {
            case: "a supported value beside one the pair does not publish",
            stack: "connector-partly-supported",
            resources: r#"{
                "Rule": {"Type": "AWS::Events::Rule", "Properties": {
                    "ScheduleExpression": "rate(1 day)"}},
                "Queue": {"Type": "AWS::SQS::Queue", "Properties": {}},
                "Connector": {"Type": "AWS::Serverless::Connector", "Properties": {
                    "Source": {"Id": "Rule"},
                    "Destination": {"Id": "Queue"},
                    "Permissions": ["Write", "Read"]}}
            }"#,
            unsupported: Some(("AWS::Events::Rule", "AWS::SQS::Queue", "Write")),
        },
        // NO permission at all: no profile publishes an empty set either, so there is nothing to
        // expand for this connector.
        Row {
            case: "a connector asking for no permission at all",
            stack: "connector-no-permission",
            resources: r#"{
                "Flow": {"Type": "AWS::Serverless::StateMachine", "Properties": {
                    "Definition": {"StartAt": "Done", "States": {"Done": {"Type": "Succeed"}}}}},
                "Bucket": {"Type": "AWS::S3::Bucket", "Properties": {}},
                "Connector": {"Type": "AWS::Serverless::Connector", "Properties": {
                    "Source": {"Id": "Flow"},
                    "Destination": {"Id": "Bucket"},
                    "Permissions": []}}
            }"#,
            unsupported: Some((
                "AWS::StepFunctions::StateMachine",
                "AWS::S3::Bucket",
                "Read, Write",
            )),
        },
        // The pair publishes both values and the connector asks for both: processed, so the
        // change set plans the template and reports no reason.
        Row {
            case: "both values of a pair that publishes both",
            stack: "connector-read-and-write",
            resources: r#"{
                "Fn": {"Type": "AWS::Serverless::Function", "Properties": {
                    "Runtime": "python3.12", "Handler": "index.handler",
                    "InlineCode": "def handler(event, context): pass"}},
                "Items": {"Type": "AWS::Serverless::SimpleTable", "Properties": {}},
                "Connector": {"Type": "AWS::Serverless::Connector", "Properties": {
                    "Source": {"Id": "Fn"},
                    "Destination": {"Id": "Items"},
                    "Permissions": ["Read", "Write"]}}
            }"#,
            unsupported: None,
        },
        // The one value a write-only pair publishes.
        Row {
            case: "the one value a write-only pair publishes",
            stack: "connector-write-only",
            resources: r#"{
                "Rule": {"Type": "AWS::Events::Rule", "Properties": {
                    "ScheduleExpression": "rate(1 day)"}},
                "Flow": {"Type": "AWS::Serverless::StateMachine", "Properties": {
                    "Definition": {"StartAt": "Done", "States": {"Done": {"Type": "Succeed"}}}}},
                "Connector": {"Type": "AWS::Serverless::Connector", "Properties": {
                    "Source": {"Id": "Rule"},
                    "Destination": {"Id": "Flow"},
                    "Permissions": ["Write"]}}
            }"#,
            unsupported: None,
        },
    ]
}

/// One row's template body, with the Serverless transform declared.
fn template(row: &Row) -> String {
    format!(
        r#"{{"AWSTemplateFormatVersion":"2010-09-09","Transform":"{TRANSFORM}","Resources":{}}}"#,
        row.resources
    )
}

/// The whole `StatusReason` an unsupported row's change set must carry: the macro's aggregated
/// wrapper around the one fault the connector is invalid for.
fn expected_reason(row: &Row) -> Option<String> {
    let (source, destination, published) = row.unsupported?;
    Some(format!(
        "Transform {TRANSFORM} failed with: Invalid Serverless Application Specification \
         document. Number of errors found: 1. Resource with id [Connector] is invalid. \
         Unsupported 'Permissions' provided for connector from {source} to {destination}; valid \
         values are: {published}."
    ))
}

/// Submit one row's document as a CREATE change set on a shell stack of its own — which is
/// ACCEPTED either way, because a transform's failure is not a bad request — and answer what
/// `DescribeChangeSet` reports for it.
async fn planned(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    row: &Row,
) -> String {
    let (code, body) = call(
        router,
        "CreateChangeSet",
        &[
            member("StackName", row.stack),
            member("ChangeSetName", PLAN),
            member("ChangeSetType", "CREATE"),
            member("TemplateBody", &template(row)),
            member("Capabilities.member.1", "CAPABILITY_AUTO_EXPAND"),
            member("Capabilities.member.2", "CAPABILITY_IAM"),
        ],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {:?}: a transform failure is no bad request, got {code} {body}",
        row.case
    );
    settle_the_store(router, engine).await;
    let (code, body) = call(
        router,
        "DescribeChangeSet",
        &[
            member("StackName", row.stack),
            member("ChangeSetName", PLAN),
        ],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {:?}: DescribeChangeSet must answer 200, got {code} {body}",
        row.case
    );
    body
}

#[tokio::test]
async fn a_connector_is_judged_against_the_permissions_its_pair_publishes() {
    for row in rows() {
        let case = row.case;
        let (router, engine) = router_with_engine();
        let described = planned(&router, &engine, &row).await;

        match expected_reason(&row) {
            Some(expected) => {
                assert_eq!(
                    xml_text(&described, "Status").as_deref(),
                    Some("FAILED"),
                    "case {case:?}: expected the change set to fail, got {described}"
                );
                assert_eq!(
                    xml_text(&described, "StatusReason").as_deref(),
                    Some(expected.as_str()),
                    "case {case:?}: expected the diagnostic {expected:?}, got {described}"
                );
                // A document the transform failed on never reached the stage that computes a
                // change, so the change set plans nothing at all.
                let changes = xml_blocks(&described, "Changes");
                assert!(
                    changes.iter().all(|block| !block.contains("<member")),
                    "case {case:?}: expected a failed change set to plan nothing, got {changes:?}"
                );
            }
            None => {
                assert_eq!(
                    xml_text(&described, "Status").as_deref(),
                    Some("CREATE_COMPLETE"),
                    "case {case:?}: expected the change set to be planned, got {described}"
                );
                assert_eq!(
                    xml_text(&described, "StatusReason"),
                    None,
                    "case {case:?}: expected no reason on a processed document, got {described}"
                );
            }
        }
    }
}

/// A pair NO published profile connects is a pair the macro is asked nothing about here: the
/// declaration expands as written rather than being refused in words nothing published.
#[tokio::test]
async fn a_pair_no_profile_publishes_is_planned_as_declared() {
    let row = Row {
        case: "a pair no published profile connects",
        stack: "connector-unprofiled-pair",
        resources: r#"{
            "Items": {"Type": "AWS::Serverless::SimpleTable", "Properties": {}},
            "Bucket": {"Type": "AWS::S3::Bucket", "Properties": {}},
            "Connector": {"Type": "AWS::Serverless::Connector", "Properties": {
                "Source": {"Id": "Items"},
                "Destination": {"Id": "Bucket"},
                "Permissions": ["Read"]}}
        }"#,
        unsupported: None,
    };
    let case = row.case;
    let (router, engine) = router_with_engine();
    let described = planned(&router, &engine, &row).await;
    assert_eq!(
        xml_text(&described, "Status").as_deref(),
        Some("CREATE_COMPLETE"),
        "case {case:?}: expected the change set to be planned, got {described}"
    );
    assert_eq!(
        xml_text(&described, "StatusReason"),
        None,
        "case {case:?}: expected no reason for a pair nothing publishes, got {described}"
    );
}

/// The resources a deployed connector row declares: a function, the table it is connected to,
/// and the connector between them asking for both permissions that pair publishes.
const DEPLOYED_RESOURCES: &str = r#"{
    "Reader": {"Type": "AWS::Serverless::Function", "Properties": {
        "Handler": "index.handler", "Runtime": "python3.12",
        "InlineCode": "def handler(event, context):\n    return {}\n"}},
    "Items": {"Type": "AWS::Serverless::SimpleTable", "Properties": {
        "PrimaryKey": {"Name": "id", "Type": "String"}}},
    "Connector": {"Type": "AWS::Serverless::Connector", "Properties": {
        "Source": {"Id": "Reader"},
        "Destination": {"Id": "Items"},
        "Permissions": ["Read", "Write"]}}
}"#;

/// The `(ResourceType, PhysicalResourceId)` the stack reports for one resource.
async fn stack_resource(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    logical_id: &str,
) -> (String, String) {
    let (code, body) = call(
        router,
        "DescribeStackResources",
        &[
            member("StackName", stack),
            member("LogicalResourceId", logical_id),
        ],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: the stack must hold {logical_id:?}, got {code} {body}"
    );
    (
        xml_text(&body, "ResourceType").unwrap_or_default(),
        xml_text(&body, "PhysicalResourceId").unwrap_or_default(),
    )
}

/// RULE (the other half): the grant a connector stands for is a resource the stack really
/// holds — the managed policy the profile publishes for its pair, attached in IAM to the role
/// the connector's source executes as, granting that pair's actions on the destination's own
/// ARN — and the connector's own declaration is gone from the processed template, as every type
/// the macro expands is.
#[tokio::test]
async fn a_connector_deploys_the_policy_its_profile_publishes_attached_to_the_sources_role() {
    let case = "a function connected to the table it reads and writes";
    let stack = "connector-deployed-grant";
    let (router, engine) = router_with_engine();
    let body = format!(
        r#"{{"AWSTemplateFormatVersion":"2010-09-09","Transform":"{TRANSFORM}","Resources":{DEPLOYED_RESOURCES}}}"#
    );
    let (code, created) = create_stack(
        &router,
        stack,
        &body,
        &acknowledging_iam(&[member("Capabilities.member.2", "CAPABILITY_AUTO_EXPAND")]),
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: CreateStack must answer 200, got {code} {created}"
    );
    settle_the_store(&router, &engine).await;
    let described = poll_to_terminal(&router, case, stack).await;
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "case {case:?}: the connector's grant must deploy, got {described}"
    );

    // The grant itself: a managed policy under the connector's own logical id.
    let (kind, policy_arn) = stack_resource(&router, case, stack, "ConnectorPolicy").await;
    assert_eq!(
        kind, "AWS::IAM::ManagedPolicy",
        "case {case:?}: expected the connector's grant to be a managed policy, got {kind:?}"
    );

    // IAM holds it, and the document it holds grants this pair's actions on the table the
    // connector names — the table's own ARN and the indexes of it.
    let (code, policy) = iam(&router, "GetPolicy", &[("PolicyArn", &policy_arn)]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected IAM to hold {policy_arn:?}, got {code} {policy}"
    );
    let version = xml_text(&policy, "DefaultVersionId").unwrap_or_default();
    let (code, held) = iam(
        &router,
        "GetPolicyVersion",
        &[("PolicyArn", &policy_arn), ("VersionId", &version)],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected GetPolicyVersion to answer, got {code} {held}"
    );
    let document = xml_policy_document(&held, "Document").unwrap_or_default();
    let table = stack_resource(&router, case, stack, "Items").await.1;
    for granted in [
        "dynamodb:GetItem",
        "dynamodb:PutItem",
        &format!("table/{table}"),
        "/index/*",
    ] {
        assert!(
            document.contains(granted),
            "case {case:?}: expected the connector's grant to carry {granted:?}, got {document:?}"
        );
    }

    // ...and it is attached to the role the source executes as, which is what makes the
    // connector a grant at all.
    let role = stack_resource(&router, case, stack, "ReaderRole").await.1;
    let (code, attached) = iam(&router, "ListAttachedRolePolicies", &[("RoleName", &role)]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected ListAttachedRolePolicies({role:?}) to answer, got {code} \
         {attached}"
    );
    assert!(
        xml_texts(&attached, "PolicyArn").contains(&policy_arn),
        "case {case:?}: expected {policy_arn:?} attached to {role:?}, got {attached}"
    );

    // The connector is the macro's INPUT: the processed template carries the grant and no
    // `AWS::Serverless::*` type at all.
    let (code, template) = get_template(&router, stack, None).await;
    assert_eq!(
        code, 200,
        "case {case:?}: the processed template must be readable, got {code} {template}"
    );
    let processed = xml_text(&template, "TemplateBody").unwrap_or_default();
    assert!(
        !processed.contains("AWS::Serverless::") && processed.contains("AWS::IAM::ManagedPolicy"),
        "case {case:?}: expected the connector expanded into its grant, got {processed:?}"
    );
}
