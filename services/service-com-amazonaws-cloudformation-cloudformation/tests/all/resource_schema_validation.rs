// SPDX-License-Identifier: CC0-1.0

//! What a stack operation reports when CLOUDFORMATION ITSELF refuses its template before
//! anything is provisioned — the resource-schema validation and the existence check beside it —
//! end to end over the awsQuery wire.
//!
//! RULE: CloudFormation validates each declared resource against its resource type's schema
//! before invoking any provider, so a declaration carrying a member that schema states no
//! property for, or a scalar that spells no value of the type the schema states for the
//! property it is carried under, fails the operation at that stage; and what the STACK reports
//! for a failure of ANY of those pre-deployment stages is CloudFormation's own summary over the
//! stage's errors — their count and the pointer at `DescribeEvents` — while each failure's own
//! diagnosis is carried on the validation result recorded for the resource it failed on, which
//! `DescribeEvents` reports. A scalar whose TEXT spells a value of the stated type is coerced
//! and deployed.
//!
//! The rows of the first table differ along what the rule generalises over: WHICH question of
//! the schema the declaration fails (a member the type does not have, a value of another type),
//! which stated TYPE the value misses (integer, boolean), the FORM of the value (text that
//! spells no number, text that spells no flag, a fraction where a whole number is stated) and,
//! as the controls, the two forms a value of the stated type takes — the value itself and the
//! text of it, both deployed and read back in the service that owns the queue.
//!
//! The second test is the OTHER pre-deployment stage, the existence check, over a resource type
//! of another service: the stack reports the same summary, and the collision is the detail on
//! its own validation result. The third is the boundary of the type question: a scalar that is
//! still a DYNAMIC REFERENCE is not the value the owning service will be given, so the stage
//! leaves it alone and the declaration deploys with the value the reference stands for.
//!
//! Nothing here waits out a span: every stack is polled to its terminal status.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The Amazon SQS caller this target already writes ONCE, in the suite that first needed it:
// one spelling of that service's awsJson1_0 wire for every suite that reads a queue back.
use crate::declared_topic_policy_decides_the_alarm_action::sqs;
use crate::dynamic_references::{put_parameter, ssm};
use crate::harness::{call, create_stack, member, settled_stack, xml_blocks, xml_text, REGION};
use service_com_amazonaws_cloudformation_cloudformation::state::{resource_status, status};

/// The validation CloudFormation names for the check that holds a declaration's properties to
/// its type's schema. Source: `state::property_validation::PROPERTY_VALIDATION`.
const PROPERTY_VALIDATION: &str = "PROPERTY_VALIDATION";

/// ...and the one it names for the check that a resource it would create does not already
/// exist. Source: `state::early_validation::NAME_CONFLICT_VALIDATION`.
const NAME_CONFLICT_VALIDATION: &str = "NAME_CONFLICT_VALIDATION";

/// The `EventType` a validation result is reported under, from the model's `EventType` enum
/// (`smithy_query(query="shape", shape="EventType")`).
const VALIDATION_ERROR: &str = "VALIDATION_ERROR";

/// `StackEvent$DetailedStatus` for an operation a pre-deployment validation failed, from the
/// model's `DetailedStatus` enum (`[CONFIGURATION_COMPLETE, VALIDATION_FAILED]`).
const VALIDATION_FAILED: &str = "VALIDATION_FAILED";

/// The logical id every template below declares its resource under.
const LOGICAL_ID: &str = "Q";

/// A router carrying CloudFormation and the two services the resources below really live in,
/// all on ONE engine — the shape `core/router/src/lib.rs` mounts every service in.
///
/// Its own router rather than the shared harness's, which does not mount Amazon SQS: mounting
/// it for every suite of this target would change what the accounts those suites build hold.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(service_com_amazonaws_ssm_amazonssm::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router
}

/// What a stack reports for a failure of ANY of CloudFormation's pre-deployment validations:
/// the count of errors the stage found and where their detail is read. Source:
/// `state::early_validation::validation_failure_summary`.
fn summary(errors: usize) -> String {
    format!(
        "Validation failed with {errors} error(s). Call DescribeEvents to retrieve the full \
         list of issues with resource and property details, resolve each error, then retry \
         the operation."
    )
}

/// How the stage reports a member the type's schema states no property for. Source:
/// `state::property_validation::declared_properties::unsupported_property_reason`.
fn unsupported_property(property: &str) -> String {
    format!("Unsupported property [{property}]")
}

/// How the stage reports a value whose type its property's schema does not state. Source:
/// `state::property_validation::typed_properties::type_failure_reason`, whose type token is
/// the one CloudFormation's own property validator renders (`Number` for a whole-number
/// property, `Boolean` for a flag).
fn wrong_type(property: &str, schema_type: &str) -> String {
    format!(
        "Property validation failure: [Value of property {{/{property}}} does not match type \
         {{{schema_type}}}]"
    )
}

/// CloudFormation's own sentence for a resource it cannot create because one already exists
/// under the identifier the declaration asked for. Source:
/// `state::early_validation::already_exists`.
fn already_exists(resource_type: &str, identifier: &str) -> String {
    format!("Resource of type '{resource_type}' with identifier '{identifier}' already exists.")
}

/// What the validation must report about one row's declaration: the reason it carries and the
/// property its path names.
type Refusal = (fn() -> String, &'static str);

/// One row: a queue declaration, and what the schema validation must report about it — `None`
/// for a declaration the schema admits, which is deployed.
struct Row {
    case: &'static str,
    stack: &'static str,
    queue: &'static str,
    /// The property document the declaration carries beside its `QueueName`.
    properties: fn() -> Value,
    /// The `(reason, the property the path names)` the validation result must carry.
    refused: Option<Refusal>,
    /// The attributes Amazon SQS must report for a declaration that IS deployed.
    deployed_attributes: &'static [(&'static str, &'static str)],
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a queue carrying a member no resource type publishes",
            stack: "schema-unknown-member",
            queue: "schema-unknown-member-queue",
            properties: || json!({"ThisPropertyDoesNotExist": "x"}),
            refused: Some((
                || unsupported_property("ThisPropertyDoesNotExist"),
                "ThisPropertyDoesNotExist",
            )),
            deployed_attributes: &[],
        },
        Row {
            case: "a queue carrying the access policy its own schema states no property for",
            stack: "schema-policy-member",
            queue: "schema-policy-member-queue",
            properties: || json!({"Policy": {"Version": "2012-10-17", "Statement": []}}),
            refused: Some((|| unsupported_property("Policy"), "Policy")),
            deployed_attributes: &[],
        },
        Row {
            case: "a queue whose visibility timeout is text that spells no number",
            stack: "schema-mistyped-integer",
            queue: "schema-mistyped-integer-queue",
            properties: || json!({"VisibilityTimeout": "not-a-number"}),
            refused: Some((
                || wrong_type("VisibilityTimeout", "Number"),
                "VisibilityTimeout",
            )),
            deployed_attributes: &[],
        },
        Row {
            case: "a queue whose delay is a fraction where a whole number is stated",
            stack: "schema-fractional-integer",
            queue: "schema-fractional-integer-queue",
            properties: || json!({"DelaySeconds": 1.5}),
            refused: Some((|| wrong_type("DelaySeconds", "Number"), "DelaySeconds")),
            deployed_attributes: &[],
        },
        Row {
            case: "a queue whose encryption flag is text that spells no flag",
            stack: "schema-mistyped-boolean",
            queue: "schema-mistyped-boolean-queue",
            properties: || json!({"SqsManagedSseEnabled": "sometimes"}),
            refused: Some((
                || wrong_type("SqsManagedSseEnabled", "Boolean"),
                "SqsManagedSseEnabled",
            )),
            deployed_attributes: &[],
        },
        Row {
            case: "a queue whose scalars are values of the types their schema states",
            stack: "schema-typed-control",
            queue: "schema-typed-control-queue",
            properties: || json!({"VisibilityTimeout": 45, "SqsManagedSseEnabled": true}),
            refused: None,
            deployed_attributes: &[("VisibilityTimeout", "45"), ("SqsManagedSseEnabled", "true")],
        },
        Row {
            case: "a queue whose scalars are the TEXT of such values, which are coerced",
            stack: "schema-coerced-control",
            queue: "schema-coerced-control-queue",
            properties: || {
                json!({
                    "VisibilityTimeout": "45",
                    "MessageRetentionPeriod": "600",
                    "SqsManagedSseEnabled": "true",
                })
            },
            refused: None,
            deployed_attributes: &[
                ("VisibilityTimeout", "45"),
                ("MessageRetentionPeriod", "600"),
                ("SqsManagedSseEnabled", "true"),
            ],
        },
    ]
}

/// The template one row deploys: one queue, under the row's own name.
fn template(row: &Row) -> String {
    let mut properties = (row.properties)();
    if let Value::Object(document) = &mut properties {
        document.insert("QueueName".to_string(), Value::String(row.queue.to_string()));
    }
    json!({
        "Resources": {LOGICAL_ID: {"Type": "AWS::SQS::Queue", "Properties": properties}},
    })
    .to_string()
}

/// The validation RESULTS `DescribeEvents` reports for `stack`: one tuple per event carrying a
/// `ValidationName`.
async fn validation_results(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
) -> Vec<(String, String, String, String, String, String)> {
    let (code, body) = call(router, "DescribeEvents", &[member("StackName", stack)]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: the operation's events must be readable, got {code} {body}"
    );
    xml_blocks(&body, "member")
        .iter()
        .filter(|event| xml_text(event, "ValidationName").is_some())
        .map(|event| {
            (
                xml_text(event, "LogicalResourceId").unwrap_or_default(),
                xml_text(event, "EventType").unwrap_or_default(),
                xml_text(event, "ValidationName").unwrap_or_default(),
                xml_text(event, "ValidationStatus").unwrap_or_default(),
                xml_text(event, "ValidationStatusReason").unwrap_or_default(),
                xml_text(event, "ValidationPath").unwrap_or_default(),
            )
        })
        .collect()
}

/// `(DetailedStatus, ResourceStatusReason)` of every stack-level `CREATE_FAILED` event of
/// `stack`, and every event about the resource the template declared.
async fn failure_and_resource_events(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
) -> (Vec<(String, String)>, usize) {
    let (code, body) = call(
        router,
        "DescribeStackEvents",
        &[member("StackName", stack)],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: the stack's events must be readable, got {code} {body}"
    );
    let events = xml_blocks(&body, "member");
    let failed = events
        .iter()
        .filter(|event| {
            xml_text(event, "LogicalResourceId").as_deref() == Some(stack)
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
    let about_the_resource = events
        .iter()
        .filter(|event| xml_text(event, "LogicalResourceId").as_deref() == Some(LOGICAL_ID))
        .count();
    (failed, about_the_resource)
}

/// Every row: a declaration the resource type's schema refuses fails the stack before anything
/// is provisioned, with the summary on the stack and the diagnosis on the validation result,
/// while a declaration whose scalars are values of the stated types — written as such or as
/// their text — is deployed and read back in Amazon SQS.
#[tokio::test]
async fn a_declaration_the_resource_schema_refuses_rolls_the_stack_back() {
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
        let expected_status = match row.refused {
            Some(_) => status::ROLLBACK_COMPLETE,
            None => status::CREATE_COMPLETE,
        };
        let settled = xml_text(&described, "StackStatus").unwrap_or_default();
        assert_eq!(
            settled, expected_status,
            "case {case:?}: expected the stack to settle {expected_status}, got {settled:?} \
             ({described})"
        );

        // What the STACK reports: its own failure event, with the detailed status the model
        // gives a validation failure and the summary over the stage's one error — never the
        // owning service's refusal, which was never asked for.
        let (failed, about_the_resource) =
            failure_and_resource_events(&router, case, row.stack).await;
        let expected_failed: Vec<(String, String)> = row
            .refused
            .map(|_| vec![(VALIDATION_FAILED.to_string(), summary(1))])
            .unwrap_or_default();
        assert_eq!(
            failed, expected_failed,
            "case {case:?}: expected the stack's failure event {expected_failed:?}, got \
             {failed:?}"
        );
        assert_eq!(
            about_the_resource > 0,
            row.refused.is_none(),
            "case {case:?}: expected resource events only where the declaration was deployed, \
             got {about_the_resource} of them"
        );

        // ...and the DETAIL of the failure: an event about the resource nothing was done to,
        // naming the validation, its status, what failed it and the member it was applied at.
        let results = validation_results(&router, case, row.stack).await;
        let expected_results: Vec<(String, String, String, String, String, String)> = row
            .refused
            .map(|(reason, property)| {
                vec![(
                    LOGICAL_ID.to_string(),
                    VALIDATION_ERROR.to_string(),
                    PROPERTY_VALIDATION.to_string(),
                    "FAILED".to_string(),
                    reason(),
                    format!("/Resources/{LOGICAL_ID}/Properties/{property}"),
                )]
            })
            .unwrap_or_default();
        assert_eq!(
            results, expected_results,
            "case {case:?}: expected the validation results {expected_results:?}, got \
             {results:?}"
        );

        // What the owning service holds is the proof: a refused declaration left no queue in
        // Amazon SQS at all, and a deployed one carries the attributes its scalars state.
        let (code, located) = sqs(&router, "GetQueueUrl", &json!({"QueueName": row.queue})).await;
        if row.refused.is_some() {
            assert_ne!(
                code, 200,
                "case {case:?}: a refused declaration must leave no queue behind, got {code} \
                 {located}"
            );
            continue;
        }
        assert_eq!(
            code, 200,
            "case {case:?}: the deployed queue must be readable, got {code} {located}"
        );
        let url = located
            .get("QueueUrl")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let (code, reported) = sqs(
            &router,
            "GetQueueAttributes",
            &json!({"QueueUrl": url, "AttributeNames": ["All"]}),
        )
        .await;
        assert_eq!(
            code, 200,
            "case {case:?}: GetQueueAttributes must answer 200, got {code} {reported}"
        );
        for (attribute, value) in row.deployed_attributes {
            let held = reported
                .pointer(&format!("/Attributes/{attribute}"))
                .and_then(Value::as_str);
            assert_eq!(
                held,
                Some(*value),
                "case {case:?}: expected Amazon SQS to report {attribute}={value:?}, got \
                 {reported}"
            );
        }
    }
}

/// The OTHER pre-deployment stage reports the SAME summary on the stack: a create whose
/// declared physical name the service that owns the type already holds fails the existence
/// check, the stack carries the summary over that one error, and the collision is the diagnosis
/// on the validation result `DescribeEvents` reports.
#[tokio::test]
async fn the_existence_check_reports_the_same_summary_on_the_stack() {
    let case = "a parameter whose name Systems Manager already holds";
    let router = router();
    let held = "/schema/already-held-parameter";
    put_parameter(&router, case, held, "String", "the value a client wrote").await;

    let template = json!({
        "Resources": {
            LOGICAL_ID: {
                "Type": "AWS::SSM::Parameter",
                "Properties": {"Name": held, "Type": "String", "Value": "the stack's value"},
            },
        },
    })
    .to_string();
    let (code, created) = create_stack(&router, "schema-name-conflict", &template, &[]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: CreateStack must answer 200, got {code} {created}"
    );
    let described = settled_stack(case, &router, REGION, "schema-name-conflict").await;
    let settled = xml_text(&described, "StackStatus").unwrap_or_default();
    assert_eq!(
        settled,
        status::ROLLBACK_COMPLETE,
        "case {case:?}: expected the stack to settle {}, got {settled:?} ({described})",
        status::ROLLBACK_COMPLETE
    );
    let reported = xml_text(&described, "StackStatusReason").unwrap_or_default();
    assert_eq!(
        reported,
        summary(1),
        "case {case:?}: expected the stack to report the validation summary, got {reported:?}"
    );

    let (failed, about_the_resource) =
        failure_and_resource_events(&router, case, "schema-name-conflict").await;
    assert_eq!(
        failed,
        vec![(VALIDATION_FAILED.to_string(), summary(1))],
        "case {case:?}: expected the stack's failure event to carry the summary, got {failed:?}"
    );
    assert_eq!(
        about_the_resource, 0,
        "case {case:?}: nothing was asked of the owning service, so the resource reports no \
         event, got {about_the_resource} of them"
    );

    let results = validation_results(&router, case, "schema-name-conflict").await;
    let expected_results = vec![(
        LOGICAL_ID.to_string(),
        VALIDATION_ERROR.to_string(),
        NAME_CONFLICT_VALIDATION.to_string(),
        "FAILED".to_string(),
        already_exists("AWS::SSM::Parameter", held),
        format!("/Resources/{LOGICAL_ID}"),
    )];
    assert_eq!(
        results, expected_results,
        "case {case:?}: expected the validation results {expected_results:?}, got {results:?}"
    );

    // The parameter that was already there is untouched: nothing was asked of the service that
    // holds it.
    let (code, read) = ssm(&router, "GetParameter", &json!({"Name": held})).await;
    assert_eq!(
        code, 200,
        "case {case:?}: the parameter that was already there must still be readable, got \
         {code} {read}"
    );
    assert_eq!(
        read.pointer("/Parameter/Value").and_then(Value::as_str),
        Some("the value a client wrote"),
        "case {case:?}: expected the value the client wrote, got {read}"
    );
}

/// The boundary of the type question: a scalar that is still a DYNAMIC REFERENCE is not the
/// value the owning service will be given — CloudFormation resolves those references while it
/// provisions, after this stage has run — so the stage leaves it alone, and the declaration is
/// deployed with the value the reference stands for.
///
/// Both types this stage judges are covered: the reference stands for a whole number on one
/// property and for a flag on the other, and Amazon SQS reports the resolved value of each.
#[tokio::test]
async fn a_dynamic_reference_is_left_for_the_deployment_to_resolve() {
    let case = "a queue whose timeout and encryption flag are dynamic references";
    let router = router();
    let timeout = "/schema/queue-timeout";
    let encrypted = "/schema/queue-encrypted";
    put_parameter(&router, case, timeout, "String", "75").await;
    put_parameter(&router, case, encrypted, "String", "true").await;

    let queue = "schema-dynamic-reference-queue";
    let template = json!({
        "Resources": {
            LOGICAL_ID: {
                "Type": "AWS::SQS::Queue",
                "Properties": {
                    "QueueName": queue,
                    "VisibilityTimeout": format!("{{{{resolve:ssm:{timeout}}}}}"),
                    "SqsManagedSseEnabled": format!("{{{{resolve:ssm:{encrypted}}}}}"),
                },
            },
        },
    })
    .to_string();
    let (code, created) = create_stack(&router, "schema-dynamic-reference", &template, &[]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: CreateStack must answer 200, got {code} {created}"
    );
    let described = settled_stack(case, &router, REGION, "schema-dynamic-reference").await;
    let settled = xml_text(&described, "StackStatus").unwrap_or_default();
    assert_eq!(
        settled,
        status::CREATE_COMPLETE,
        "case {case:?}: expected the stack to settle {}, got {settled:?} ({described})",
        status::CREATE_COMPLETE
    );

    // ...and no pre-deployment validation refused anything: the stage recorded no result.
    let results = validation_results(&router, case, "schema-dynamic-reference").await;
    assert!(
        results.is_empty(),
        "case {case:?}: expected no validation result at all, got {results:?}"
    );

    // What the owning service holds is the proof: the queue carries the values the references
    // stood for.
    let (code, located) = sqs(&router, "GetQueueUrl", &json!({"QueueName": queue})).await;
    assert_eq!(
        code, 200,
        "case {case:?}: the deployed queue must be readable, got {code} {located}"
    );
    let url = located
        .get("QueueUrl")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let (code, reported) = sqs(
        &router,
        "GetQueueAttributes",
        &json!({"QueueUrl": url, "AttributeNames": ["All"]}),
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: GetQueueAttributes must answer 200, got {code} {reported}"
    );
    for (attribute, value) in [("VisibilityTimeout", "75"), ("SqsManagedSseEnabled", "true")] {
        let held = reported
            .pointer(&format!("/Attributes/{attribute}"))
            .and_then(Value::as_str);
        assert_eq!(
            held,
            Some(value),
            "case {case:?}: expected Amazon SQS to report {attribute}={value:?}, got {reported}"
        );
    }
}
