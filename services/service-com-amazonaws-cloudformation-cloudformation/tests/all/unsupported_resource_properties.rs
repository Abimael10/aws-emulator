// SPDX-License-Identifier: CC0-1.0

//! The PRE-DEPLOYMENT validation a stack operation's resources are held to against the
//! properties their type HAS, end to end over the awsQuery wire.
//!
//! RULE: CloudFormation validates each declared resource against its resource type's schema
//! before provisioning anything, so a declaration carrying a member that schema states no
//! property for fails the operation before any resource is created: the stack records a
//! `PROPERTY_VALIDATION` validation result — `FAILED`, handled by failing, reading
//! `Unsupported property [<member>]` at that member's own
//! `/Resources/<logical id>/Properties/<member>` — the stack's own failure event carries
//! `DetailedStatus` `VALIDATION_FAILED` with the summary over the stage's errors, and the
//! stack rolls back to `ROLLBACK_COMPLETE` with nothing provisioned. A declaration every
//! member of which the schema states is deployed as written.
//!
//! The rows differ along what the rule generalises over: WHICH resource type the declaration
//! is of (five owning services), and what KIND of member the schema does not state (a name
//! nothing publishes, a member of the owning API that is not a property of the type, a
//! property another type of the same service has, a misspelling of one the type does have,
//! and any member at all on a type that has none) — plus the control row, whose declaration
//! carries every member its type's schema states and is really created in the service that
//! owns it.

use serde_json::{json, Value};

use crate::dynamic_references::ssm;
use crate::harness::{
    call, create_stack, member, router, settled_stack, xml_blocks, xml_text, REGION,
};
use service_com_amazonaws_cloudformation_cloudformation::state::{resource_status, status};

/// The validation CloudFormation names for the check that holds a declaration's properties to
/// its type's contract. Source: `state::property_validation::PROPERTY_VALIDATION`.
const PROPERTY_VALIDATION: &str = "PROPERTY_VALIDATION";

/// The `EventType` a validation result is reported under ("Validation errors - Failures from
/// CloudFormation Early Validations").
const VALIDATION_ERROR: &str = "VALIDATION_ERROR";

/// `StackEvent$DetailedStatus` for an operation a pre-deployment validation failed, from the
/// model's `DetailedStatus` enum (`[CONFIGURATION_COMPLETE, VALIDATION_FAILED]`).
const VALIDATION_FAILED: &str = "VALIDATION_FAILED";

/// The logical id every template below declares its resource under.
const LOGICAL_ID: &str = "Param";

/// One row: a declaration, and the member the validation must report as unsupported — `None`
/// for a declaration every member of which its type's schema states.
struct Row {
    case: &'static str,
    stack: &'static str,
    resource_type: &'static str,
    /// The property document the declaration carries.
    properties: fn() -> Value,
    unsupported: Option<&'static str>,
}

/// The parameter name the control row's declaration states, which is also the name a client
/// reads the deployed parameter back by.
const DEPLOYED_PARAMETER: &str = "unsupported-control-parameter";

/// ...and the name the rows that FAIL state: nothing is provisioned, so Systems Manager must
/// hold no parameter under it.
const REFUSED_PARAMETER: &str = "unsupported-refused-parameter";

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a parameter carrying a member no resource type publishes",
            stack: "unsupported-unknown-member",
            resource_type: "AWS::SSM::Parameter",
            properties: || {
                json!({
                    "Name": REFUSED_PARAMETER,
                    "Type": "String",
                    "Value": "x",
                    "ThisPropertyDoesNotExist": "x",
                })
            },
            unsupported: Some("ThisPropertyDoesNotExist"),
        },
        Row {
            case: "a parameter carrying a member of the owning API rather than of the type",
            stack: "unsupported-api-member",
            resource_type: "AWS::SSM::Parameter",
            properties: || {
                json!({
                    "Name": REFUSED_PARAMETER,
                    "Type": "String",
                    "Value": "x",
                    "Overwrite": true,
                })
            },
            unsupported: Some("Overwrite"),
        },
        Row {
            case: "a queue policy carrying the member the topic policy type has",
            stack: "unsupported-policy-member",
            resource_type: "AWS::SQS::QueuePolicy",
            properties: || {
                json!({
                    "Queues": ["https://example.invalid/queue"],
                    "PolicyDocument": {"Version": "2012-10-17", "Statement": []},
                    "Topics": ["arn:aws:sns:us-east-1:000000000000:t"],
                })
            },
            unsupported: Some("Topics"),
        },
        Row {
            case: "a subscription misspelling the endpoint it is delivered to",
            stack: "unsupported-misspelled-member",
            resource_type: "AWS::SNS::Subscription",
            properties: || {
                json!({
                    "TopicArn": "arn:aws:sns:us-east-1:000000000000:t",
                    "Protocol": "email",
                    "EndPoint": "ops@example.com",
                })
            },
            unsupported: Some("EndPoint"),
        },
        Row {
            case: "a log stream carrying the retention its log group has",
            stack: "unsupported-sibling-member",
            resource_type: "AWS::Logs::LogStream",
            properties: || {
                json!({
                    "LogGroupName": "unsupported-group",
                    "LogStreamName": "unsupported-stream",
                    "RetentionInDays": 7,
                })
            },
            unsupported: Some("RetentionInDays"),
        },
        Row {
            case: "a wait condition handle, a type that has no property at all",
            stack: "unsupported-no-properties",
            resource_type: "AWS::CloudFormation::WaitConditionHandle",
            properties: || json!({"Ready": "true"}),
            unsupported: Some("Ready"),
        },
        Row {
            case: "a parameter carrying every member its type's schema states",
            stack: "unsupported-control",
            resource_type: "AWS::SSM::Parameter",
            properties: || {
                json!({
                    "Name": DEPLOYED_PARAMETER,
                    "Type": "String",
                    "Value": "on",
                    "Description": "the control row's parameter",
                    "AllowedPattern": "^[a-z]+$",
                    "Tier": "Standard",
                    "DataType": "text",
                    "Tags": {"team": "ops"},
                })
            },
            unsupported: None,
        },
    ]
}

/// The template one row deploys.
fn template(row: &Row) -> String {
    json!({
        "Resources": {
            LOGICAL_ID: {"Type": row.resource_type, "Properties": (row.properties)()},
        },
    })
    .to_string()
}

/// How this service summarises a failed pre-deployment property validation on the stack: the
/// count of errors and where their detail is. Source:
/// `state::property_validation::validation_failure_summary`.
fn summary(errors: usize) -> String {
    format!(
        "Validation failed with {errors} error(s). Call DescribeEvents to retrieve the full \
         list of issues with resource and property details, resolve each error, then retry \
         the operation."
    )
}

/// Every row: a declaration carrying a member its type's schema does not state fails before
/// anything is provisioned, and the one whose members the schema all states is deployed.
#[tokio::test]
async fn a_member_the_type_does_not_have_rolls_the_stack_back() {
    for row in rows() {
        let case = row.case;
        // A router of its own per row: what each case is about is its own declaration, not
        // what another row left behind.
        let router = router();

        let (code, created) = create_stack(&router, row.stack, &template(&row), &[]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: CreateStack must answer 200 — the request is valid and the stack \
             exists — got {code} {created}"
        );
        let described = settled_stack(case, &router, REGION, row.stack).await;
        let expected_status = match row.unsupported {
            Some(_) => status::ROLLBACK_COMPLETE,
            None => status::CREATE_COMPLETE,
        };
        let settled = xml_text(&described, "StackStatus").unwrap_or_default();
        assert_eq!(
            settled, expected_status,
            "case {case:?}: expected the stack to settle {expected_status}, got {settled:?} \
             ({described})"
        );

        // The detail of the failure: an event about the resource NOTHING was done to, naming
        // the validation, its status, how its failure was handled, the member the type does
        // not have and the path that member was carried at.
        let (code, events) = call(&router, "DescribeEvents", &[member("StackName", row.stack)]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: the operation's events must be readable, got {code} {events}"
        );
        let results: Vec<Vec<String>> = xml_blocks(&events, "member")
            .iter()
            .filter(|event| xml_text(event, "ValidationName").is_some())
            .map(|event| {
                [
                    "LogicalResourceId",
                    "ResourceType",
                    "EventType",
                    "ValidationName",
                    "ValidationStatus",
                    "ValidationFailureMode",
                    "ValidationStatusReason",
                    "ValidationPath",
                ]
                .iter()
                .map(|field| xml_text(event, field).unwrap_or_default())
                .collect()
            })
            .collect();
        let expected_results: Vec<Vec<String>> = row
            .unsupported
            .map(|property| {
                vec![vec![
                    LOGICAL_ID.to_string(),
                    row.resource_type.to_string(),
                    VALIDATION_ERROR.to_string(),
                    PROPERTY_VALIDATION.to_string(),
                    "FAILED".to_string(),
                    "FAIL".to_string(),
                    format!("Unsupported property [{property}]"),
                    format!("/Resources/{LOGICAL_ID}/Properties/{property}"),
                ]]
            })
            .unwrap_or_default();
        assert_eq!(
            results, expected_results,
            "case {case:?}: expected the validation results {expected_results:?}, got \
             {results:?}"
        );

        // ...and what the STACK reports for it: its own failure event, with the detailed
        // status the model gives a validation failure and the summary over the stage's
        // errors, which is what points a client at the detail above.
        let (code, stack_events) = call(
            &router,
            "DescribeStackEvents",
            &[member("StackName", row.stack)],
        )
        .await;
        assert_eq!(
            code, 200,
            "case {case:?}: the stack's events must be readable, got {code} {stack_events}"
        );
        let failed: Vec<(String, String)> = xml_blocks(&stack_events, "member")
            .iter()
            .filter(|event| {
                xml_text(event, "LogicalResourceId").as_deref() == Some(row.stack)
                    && xml_text(event, "ResourceStatus").as_deref()
                        == Some(resource_status::CREATE_FAILED)
            })
            .map(|event| {
                (
                    xml_text(event, "DetailedStatus").unwrap_or_default(),
                    xml_text(event, "ResourceStatusReason").unwrap_or_default(),
                )
            })
            .collect();
        let expected_failed: Vec<(String, String)> = row
            .unsupported
            .map(|_| vec![(VALIDATION_FAILED.to_string(), summary(1))])
            .unwrap_or_default();
        assert_eq!(
            failed, expected_failed,
            "case {case:?}: expected the stack's failure event {expected_failed:?}, got \
             {stack_events}"
        );

        // No resource-level event was recorded for the resource either: the operation failed
        // before provisioning, so nothing was asked of the service that owns it. The
        // declaration that passes the stage IS deployed, so its resource reports its own
        // transitions.
        let about_the_resource = xml_blocks(&stack_events, "member")
            .iter()
            .filter(|event| xml_text(event, "LogicalResourceId").as_deref() == Some(LOGICAL_ID))
            .count();
        assert_eq!(
            about_the_resource > 0,
            row.unsupported.is_none(),
            "case {case:?}: expected resource events for the resource only where the \
             declaration was deployed, got {stack_events}"
        );

        // What the owning service holds is the proof: the refused declarations left no
        // parameter in Systems Manager at all, and the deployed one is read back there by the
        // name it declared, with the value it declared.
        let (code, parameter) = ssm(
            &router,
            "GetParameter",
            &json!({"Name": match row.unsupported {
                Some(_) => REFUSED_PARAMETER,
                None => DEPLOYED_PARAMETER,
            }}),
        )
        .await;
        match row.unsupported {
            Some(_) => assert_ne!(
                code, 200,
                "case {case:?}: a refused declaration must leave no parameter behind, got \
                 {code} {parameter}"
            ),
            None => {
                assert_eq!(
                    code, 200,
                    "case {case:?}: the deployed parameter must be readable, got {code} \
                     {parameter}"
                );
                assert_eq!(
                    parameter
                        .pointer("/Parameter/Value")
                        .and_then(Value::as_str),
                    Some("on"),
                    "case {case:?}: expected the declared value, got {parameter}"
                );
            }
        }
    }
}

/// A stack operation that turns the pre-deployment validations OFF deploys the declaration as
/// written: the member the type does not have is then the owning service's to report, so no
/// validation result is recorded for it at all.
#[tokio::test]
async fn disabling_pre_deployment_validation_defers_the_unsupported_member() {
    let router = router();
    let case = "a parameter carrying a member no type publishes, with the validations disabled";
    let template = json!({
        "Resources": {
            LOGICAL_ID: {
                "Type": "AWS::SSM::Parameter",
                "Properties": {
                    "Name": "unsupported-deferred-parameter",
                    "Type": "String",
                    "Value": "x",
                    "ThisPropertyDoesNotExist": "x",
                },
            },
        },
    })
    .to_string();
    let (code, created) = create_stack(
        &router,
        "unsupported-deferred",
        &template,
        &[member("DisableValidation", "true")],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: CreateStack must answer 200, got {code} {created}"
    );
    let _ = settled_stack(case, &router, REGION, "unsupported-deferred").await;

    let (code, events) = call(
        &router,
        "DescribeEvents",
        &[member("StackName", "unsupported-deferred")],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: the operation's events must be readable, got {code} {events}"
    );
    assert!(
        !events.contains(PROPERTY_VALIDATION),
        "case {case:?}: expected no pre-deployment validation result, got {events}"
    );
}
