// SPDX-License-Identifier: CC0-1.0

//! WHICH recorded event of a stack carries `StackEvent$DetailedStatus`, end to end over the
//! awsQuery wire.
//!
//! RULE: a recorded event reports `DetailedStatus` `VALIDATION_FAILED` exactly when the status
//! it reports was reached because a PRE-DEPLOYMENT VALIDATION failed the operation ("template
//! validation failed because of invalid properties in the template. The ResourceStatusReason
//! field shows what properties are defined incorrectly"), and reports no `DetailedStatus` at
//! all on every other event — so a client can tell a deployment CloudFormation refused at its
//! validation stage from one that failed for any other reason. The decision belongs to the
//! event and not to the reader, so `DescribeStackEvents` and `DescribeEvents` report the same
//! value for the same event.
//!
//! The rows differ along what the rule generalises over: WHICH stage ended the operation — the
//! property validation, the existence check, the owning service's own refusal of a resource,
//! the template's processing — and, for the two that are validations, whether the stack RESTS
//! in the failed status or rolls back. Two rows share one template and one collision and differ
//! only in whether the validation stage ran (`DisableValidation`), which is the pair that pins
//! the discrimination: the same `CREATE_FAILED`, carrying the detailed status only where a
//! validation is what reached it. The control row deploys.
//!
//! The suites that pin the POSITIVE half across resource types and operations are
//! `unsupported_resource_properties.rs` (the property validation) and
//! `stack_name_conflict_validation.rs` (the existence check); what this one adds is which
//! events do NOT carry it.

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{
    call, create_stack, member, router, settled_stack_status, xml_blocks, xml_text, REGION,
};
use crate::logs_group_children_resources::logs;
use service_com_amazonaws_cloudformation_cloudformation::state::{resource_status, status};

/// `StackEvent$DetailedStatus` / `OperationEvent$DetailedStatus` for an operation a
/// pre-deployment validation failed, from the model's `DetailedStatus` enum
/// (`smithy_query(query="shape", shape="DetailedStatus")`, whose values are
/// `[CONFIGURATION_COMPLETE, VALIDATION_FAILED]`).
const VALIDATION_FAILED: &str = "VALIDATION_FAILED";

/// The two `EventType` values the rows below read a detailed status off, from the model's
/// `EventType` enum: the stack's own status change, and the result of a pre-deployment
/// validation ("Validation errors - Failures from CloudFormation Early Validations").
const STACK_EVENT: &str = "STACK_EVENT";
const VALIDATION_ERROR: &str = "VALIDATION_ERROR";

/// The log-group name the collision rows are about: held by CloudWatch Logs itself before the
/// create, so it is a name held OUTSIDE CloudFormation.
const HELD_GROUP: &str = "/detailed-status/held";

/// A declaration carrying a member its type's schema states no property for, which is what the
/// property validation fails an operation on.
const UNSUPPORTED_PROPERTY_TEMPLATE: &str = r#"{"Resources":{"Param":{
    "Type":"AWS::SSM::Parameter","Properties":{"Name":"detailed-status-parameter",
    "Type":"String","Value":"x","ThisPropertyDoesNotExist":"x"}}}}"#;

/// A declaration of a log group at a name something else already holds, which is what the
/// existence check fails an operation on.
fn colliding_group_template() -> String {
    json!({
        "Resources": {
            "Group": {
                "Type": "AWS::Logs::LogGroup",
                "Properties": {"LogGroupName": HELD_GROUP},
            },
        },
    })
    .to_string()
}

/// A FIFO topic whose name does not carry the suffix Amazon SNS requires: the declaration
/// reaches the owning service, which refuses it — a failure no validation of
/// CloudFormation's own reached.
const REFUSED_TOPIC_TEMPLATE: &str = r#"{"Resources":{"Notifications":{
    "Type":"AWS::SNS::Topic","Properties":{"TopicName":"detailed-status-fifo",
    "FifoTopic":true}}}}"#;

/// A template whose declared transform cannot be applied to it: the document the stack would
/// have deployed could not be produced, so the stack's own failure event carries that
/// diagnostic — which is CloudFormation's, not a validation's verdict.
const UNPROCESSABLE_TEMPLATE: &str = r#"{"Transform":"AWS::LanguageExtensions",
    "Mappings":{"Fn::ForEach::Envs":["E",["a"],{"M${E}":{"k":{"v":"x"}}}]},
    "Resources":{"Topic":{"Type":"AWS::SNS::Topic"}}}"#;

/// A template that deploys: the control row, whose events report no detailed status either.
const DEPLOYABLE_TEMPLATE: &str = r#"{"Resources":{"Topic":{"Type":"AWS::SNS::Topic",
    "Properties":{"TopicName":"detailed-status-control"}}}}"#;

/// One row: what the create submits, and which of its recorded events must carry a detailed
/// status.
struct Row {
    case: &'static str,
    stack: &'static str,
    /// Whether CloudWatch Logs is given [`HELD_GROUP`] before the create, so the name the
    /// template states is already taken outside CloudFormation.
    holds_the_group: bool,
    template: TemplateOf,
    /// What the request states beyond its template.
    extra: &'static [(&'static str, &'static str)],
    /// The status the stack settles in.
    settles_in: &'static str,
    /// The events that must carry [`VALIDATION_FAILED`] as `DescribeStackEvents` reports
    /// them, `(LogicalResourceId, ResourceStatus)` — the stack's own name for its own event.
    stack_events: &'static [(&'static str, &'static str)],
    /// ...and as `DescribeEvents` reports them, `(LogicalResourceId, EventType)`.
    operation_events: &'static [(&'static str, &'static str)],
    /// The logical id of the resource whose OWN event reports a `CREATE_FAILED`, or `None`
    /// where nothing was asked of any owning service: a validation stage runs before
    /// provisioning, so no resource of the template has an event that could carry the
    /// failure.
    refused_resource: Option<&'static str>,
}

/// How a row's template body is produced: one of the constants above, or the collision the
/// two paired rows share.
enum TemplateOf {
    Fixed(&'static str),
    CollidingGroup,
}

impl TemplateOf {
    fn body(&self) -> String {
        match self {
            TemplateOf::Fixed(body) => (*body).to_string(),
            TemplateOf::CollidingGroup => colliding_group_template(),
        }
    }
}

/// The rows, in the order the test drives them.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a declaration carrying a member its type has no property for",
            stack: "detailed-status-property",
            holds_the_group: false,
            template: TemplateOf::Fixed(UNSUPPORTED_PROPERTY_TEMPLATE),
            extra: &[],
            settles_in: status::ROLLBACK_COMPLETE,
            stack_events: &[("detailed-status-property", resource_status::CREATE_FAILED)],
            operation_events: &[
                ("detailed-status-property", STACK_EVENT),
                ("Param", VALIDATION_ERROR),
            ],
            refused_resource: None,
        },
        Row {
            case: "the same declaration on a create that keeps its failed state",
            stack: "detailed-status-kept",
            holds_the_group: false,
            template: TemplateOf::Fixed(UNSUPPORTED_PROPERTY_TEMPLATE),
            extra: &[("DisableRollback", "true")],
            settles_in: status::CREATE_FAILED,
            stack_events: &[("detailed-status-kept", resource_status::CREATE_FAILED)],
            operation_events: &[
                ("detailed-status-kept", STACK_EVENT),
                ("Param", VALIDATION_ERROR),
            ],
            refused_resource: None,
        },
        Row {
            case: "a log group declared at a name the owning service already holds",
            stack: "detailed-status-conflict",
            holds_the_group: true,
            template: TemplateOf::CollidingGroup,
            extra: &[],
            settles_in: status::ROLLBACK_COMPLETE,
            stack_events: &[("detailed-status-conflict", resource_status::CREATE_FAILED)],
            operation_events: &[
                ("detailed-status-conflict", STACK_EVENT),
                ("Group", VALIDATION_ERROR),
            ],
            refused_resource: None,
        },
        Row {
            case: "the same collision on a create that disables the validations",
            stack: "detailed-status-deferred",
            holds_the_group: true,
            template: TemplateOf::CollidingGroup,
            extra: &[("DisableValidation", "true")],
            settles_in: status::ROLLBACK_COMPLETE,
            stack_events: &[],
            operation_events: &[],
            refused_resource: Some("Group"),
        },
        Row {
            case: "a topic the owning service refuses to create",
            stack: "detailed-status-refused",
            holds_the_group: false,
            template: TemplateOf::Fixed(REFUSED_TOPIC_TEMPLATE),
            extra: &[],
            settles_in: status::ROLLBACK_COMPLETE,
            stack_events: &[],
            operation_events: &[],
            refused_resource: Some("Notifications"),
        },
        Row {
            case: "a template whose declared transform cannot be applied to it",
            stack: "detailed-status-transform",
            holds_the_group: false,
            template: TemplateOf::Fixed(UNPROCESSABLE_TEMPLATE),
            // The transform the template declares has to be acknowledged for the create to
            // be accepted at all, which is what lets it reach the processing stage.
            extra: &[("Capabilities.member.1", "CAPABILITY_AUTO_EXPAND")],
            settles_in: status::ROLLBACK_COMPLETE,
            stack_events: &[],
            operation_events: &[],
            refused_resource: None,
        },
        Row {
            case: "a template that deploys",
            stack: "detailed-status-control",
            holds_the_group: false,
            template: TemplateOf::Fixed(DEPLOYABLE_TEMPLATE),
            extra: &[],
            settles_in: status::CREATE_COMPLETE,
            stack_events: &[],
            operation_events: &[],
            refused_resource: None,
        },
    ]
}

/// The `(LogicalResourceId, ResourceStatus)` of every event `DescribeStackEvents` reports a
/// non-empty `DetailedStatus` on, with the value it reports — sorted, since what the rule is
/// about is WHICH events carry one and not the order the log is paged in.
async fn stack_events_carrying_a_detailed_status(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
) -> Vec<(String, String, String)> {
    let (code, body) = call(router, "DescribeStackEvents", &[member("StackName", stack)]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: DescribeStackEvents must answer 200, got {code} {body}"
    );
    let mut carrying: Vec<(String, String, String)> = xml_blocks(&body, "member")
        .iter()
        .filter_map(|event| {
            let detailed = xml_text(event, "DetailedStatus").unwrap_or_default();
            (!detailed.is_empty()).then(|| {
                (
                    xml_text(event, "LogicalResourceId").unwrap_or_default(),
                    xml_text(event, "ResourceStatus").unwrap_or_default(),
                    detailed,
                )
            })
        })
        .collect();
    carrying.sort();
    carrying
}

/// The same scan over `DescribeEvents`, which reports the events of the OPERATIONS that caused
/// them: `(LogicalResourceId, EventType)` and the detailed status, sorted.
async fn operation_events_carrying_a_detailed_status(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
) -> Vec<(String, String, String)> {
    let (code, body) = call(router, "DescribeEvents", &[member("StackName", stack)]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: DescribeEvents must answer 200, got {code} {body}"
    );
    let mut carrying: Vec<(String, String, String)> = xml_blocks(&body, "member")
        .iter()
        .filter_map(|event| {
            let detailed = xml_text(event, "DetailedStatus").unwrap_or_default();
            (!detailed.is_empty()).then(|| {
                (
                    xml_text(event, "LogicalResourceId").unwrap_or_default(),
                    xml_text(event, "EventType").unwrap_or_default(),
                    detailed,
                )
            })
        })
        .collect();
    carrying.sort();
    carrying
}

/// The logical ids whose OWN event reports a `CREATE_FAILED` — the resources an owning service
/// refused, which is what a failure reached at provisioning leaves behind.
async fn resources_reporting_a_failure(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
) -> Vec<String> {
    let (code, body) = call(router, "DescribeStackEvents", &[member("StackName", stack)]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: DescribeStackEvents must answer 200, got {code} {body}"
    );
    let mut failed: Vec<String> = xml_blocks(&body, "member")
        .iter()
        .filter(|event| {
            xml_text(event, "ResourceStatus").as_deref() == Some(resource_status::CREATE_FAILED)
                && xml_text(event, "LogicalResourceId").as_deref() != Some(stack)
        })
        .map(|event| xml_text(event, "LogicalResourceId").unwrap_or_default())
        .collect();
    failed.sort();
    failed.dedup();
    failed
}

/// What one row expects, as the tuples the scans above produce.
fn expected(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    let mut expected: Vec<(String, String)> = pairs
        .iter()
        .map(|(left, right)| (left.to_string(), right.to_string()))
        .collect();
    expected.sort();
    expected
}

/// Every row: the events that carry `VALIDATION_FAILED` are exactly the ones a pre-deployment
/// validation failure reached, in both readers of the log.
#[tokio::test]
async fn only_a_validation_failure_carries_a_detailed_status() {
    for row in rows() {
        let case = row.case;
        // A router of its own per row: what each case is about is its own create, not what
        // another row left behind.
        let router = router();

        if row.holds_the_group {
            let (code, body) = logs(
                &router,
                "CreateLogGroup",
                &json!({"logGroupName": HELD_GROUP}),
            )
            .await;
            assert_eq!(
                code, 200,
                "case {case:?}: the setup CreateLogGroup must answer 200, got {code} {body}"
            );
        }

        let extra: Vec<(String, String)> = row
            .extra
            .iter()
            .map(|(key, value)| member(key, value))
            .collect();
        let (code, created) =
            create_stack(&router, row.stack, &row.template.body(), &extra).await;
        assert_eq!(
            code, 200,
            "case {case:?}: CreateStack must be accepted — the stack reports the outcome — got \
             {code} {created}"
        );
        let settled = settled_stack_status(case, &router, REGION, row.stack).await;
        assert_eq!(
            settled, row.settles_in,
            "case {case:?}: expected the stack to settle {}, got {settled:?}",
            row.settles_in
        );

        // What `DescribeStackEvents` reports: the stack's own failure event carries the
        // detailed status where a validation is what reached that status, and nothing carries
        // one otherwise.
        let carrying = stack_events_carrying_a_detailed_status(&router, case, row.stack).await;
        let as_pairs: Vec<(String, String)> = carrying
            .iter()
            .map(|(logical_id, event_status, _)| (logical_id.clone(), event_status.clone()))
            .collect();
        assert_eq!(
            as_pairs,
            expected(row.stack_events),
            "case {case:?}: expected exactly those stack events to carry a detailed status, \
             got {carrying:?}"
        );
        for (logical_id, event_status, detailed) in &carrying {
            assert_eq!(
                detailed, VALIDATION_FAILED,
                "case {case:?}: expected the {logical_id}/{event_status} event to report \
                 {VALIDATION_FAILED}, got {detailed:?}"
            );
        }

        // ...and the OTHER reader of the same log agrees, event for event: the detailed status
        // belongs to the event, so the stack's own failure event and the validation result
        // recorded beside it both report it there too.
        let reported = operation_events_carrying_a_detailed_status(&router, case, row.stack).await;
        let as_pairs: Vec<(String, String)> = reported
            .iter()
            .map(|(logical_id, event_type, _)| (logical_id.clone(), event_type.clone()))
            .collect();
        assert_eq!(
            as_pairs,
            expected(row.operation_events),
            "case {case:?}: expected exactly those operation events to carry a detailed \
             status, got {reported:?}"
        );
        for (logical_id, event_type, detailed) in &reported {
            assert_eq!(
                detailed, VALIDATION_FAILED,
                "case {case:?}: expected the {logical_id}/{event_type} event to report \
                 {VALIDATION_FAILED}, got {detailed:?}"
            );
        }

        // The row really failed where it says it did: a validation stage runs before anything
        // is asked of any owning service, so no resource of the template reports a failure of
        // its own, while a deployment that reached provisioning reports the refusal on the
        // resource that was refused.
        let refused = resources_reporting_a_failure(&router, case, row.stack).await;
        let expected_refusals: Vec<String> = row
            .refused_resource
            .map(|logical_id| vec![logical_id.to_string()])
            .unwrap_or_default();
        assert_eq!(
            refused, expected_refusals,
            "case {case:?}: expected {expected_refusals:?} to report a failure of their own, \
             got {refused:?}"
        );
    }
}
