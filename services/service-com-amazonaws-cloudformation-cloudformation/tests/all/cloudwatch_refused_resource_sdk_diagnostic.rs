// SPDX-License-Identifier: CC0-1.0

//! What a resource AMAZON CLOUDWATCH refused reports on its `CREATE_FAILED` event, end to end
//! over the awsQuery wire.
//!
//! RULE under test: **a resource provider reports a refused call as the exception its SDK
//! raised — the owning service's own sentence followed by that SDK's diagnostic of the call —
//! so the status and the error CODE that service answered are readable out of the resource's
//! `ResourceStatusReason`. Amazon CloudWatch's SDK words that diagnostic as `(Service:
//! AmazonCloudWatch; Status Code: <status>; Error Code: <code>; Request ID: <id>; Proxy:
//! null)`, whatever type of that service failed and whichever of its calls was refused — and
//! the code it states is the LEGACY query code that `awsQueryCompatible` service reports for
//! the class, which is the code a client of it branches on.**
//!
//! Why it matters: tooling parses a rolled-back deployment's reason for the SERVICE error code
//! to decide whether the rollback was caused by a client-side validation error — a declaration
//! to fix — or by something transient. The resource provider's own `HandlerErrorCode` is not
//! that code: it is one class over many of the owning service's codes, so a reason carrying
//! only the handler's class leaves CloudWatch's own `ValidationError` unrecoverable from the
//! event.
//!
//! The rows differ along what the rule generalises over: WHICH declaration that service refused
//! and WHY — a composite alarm whose `AlarmRule` references an alarm that does not exist, one
//! whose `ActionsSuppressor` names an alarm that does not exist, and a metric alarm whose
//! declared `ComparisonOperator` is outside the set the member admits — and so WHICH of its
//! calls was refused. The `Proxy` field is the same for every row because every one of these
//! calls is made directly, with no proxy between.

use service_com_amazonaws_cloudformation_cloudformation::state::status;

use crate::harness::{failed_resource_reason, is_a_request_id, quoted_and_class, REGION};
use crate::stack_lifecycle::{create_stack, router, settled_stack, xml_text, ACCOUNT_ID};

/// The SDK's own name for Amazon CloudWatch, as the exception its SDK raises states it.
const CLOUDWATCH_SERVICE: &str = "AmazonCloudWatch";

/// The HTTP status that service answers a call it will not carry out with.
const REFUSED_STATUS: u16 = 400;

/// The legacy query code that `awsQueryCompatible` service reports for the
/// request-validation class, which is what a client of it branches on.
const VALIDATION_ERROR: &str = "ValidationError";

/// The `HandlerErrorCode` class those refusals fall in — a value of the model's own
/// `HandlerErrorCode` enum (`smithy_query(query="shape",shape="HandlerErrorCode")`).
const INVALID_REQUEST: &str = "InvalidRequest";

/// What the diagnostic states for a call made with no proxy configured, which every call this
/// emulator makes in process is.
const DIRECT_CALL_PROXY: &str = "Proxy: null";

/// The status a resource its provider could not create records.
const CREATE_FAILED: &str = "CREATE_FAILED";

/// The alarm an `AWS::CloudWatch::CompositeAlarm` references that no stack creates, and the ARN
/// Amazon CloudWatch names it by: "The ARN format of an alarm is
/// arn:aws:cloudwatch:Region:account-id:alarm:alarm-name" (`TagResourceInput$ResourceARN`).
const MISSING_ALARM: &str = "cw-reason-missing-alarm";

/// One row: the stack to deploy, the resource that service refuses, and what its reason must
/// report.
struct Row {
    case: &'static str,
    stack: &'static str,
    template: &'static str,
    logical_id: &'static str,
    /// The error code that service answered, which the diagnostic must state.
    code: &'static str,
    /// A fragment of that service's own sentence the quoted message must carry — what says the
    /// refusal is the one the row asked for rather than another.
    sentence: &'static str,
}

/// A composite alarm whose rule references an alarm no stack creates, beside a metric alarm
/// that IS created: the valid alarm is what makes the refusal attributable to the reference
/// alone.
const DANGLING_RULE_TEMPLATE: &str = r#"{"Resources":{
    "ValidAlarm":{"Type":"AWS::CloudWatch::Alarm","Properties":{
      "Namespace":"MyApp/Reason","MetricName":"Requests","Statistic":"Sum","Period":"60",
      "EvaluationPeriods":"1","Threshold":"1","ComparisonOperator":"GreaterThanThreshold"}},
    "Composite":{"Type":"AWS::CloudWatch::CompositeAlarm","Properties":{
      "AlarmRule":"ALARM(\"cw-reason-missing-alarm\")"}}}}"#;

/// The same composite alarm, refused for its SUPPRESSOR instead: another member of the same
/// call, refused under the same class.
const DANGLING_SUPPRESSOR_TEMPLATE: &str = r#"{"Resources":{
    "ValidAlarm":{"Type":"AWS::CloudWatch::Alarm","Properties":{
      "Namespace":"MyApp/Reason","MetricName":"Requests","Statistic":"Sum","Period":"60",
      "EvaluationPeriods":"1","Threshold":"1","ComparisonOperator":"GreaterThanThreshold"}},
    "Composite":{"Type":"AWS::CloudWatch::CompositeAlarm","Properties":{
      "AlarmRule":{"Fn::Sub":"ALARM(\"${ValidAlarm}\")"},
      "ActionsSuppressor":"cw-reason-missing-alarm",
      "ActionsSuppressorWaitPeriod":"120",
      "ActionsSuppressorExtensionPeriod":"180"}}}}"#;

/// A metric alarm whose declared comparison is outside the set that member admits: a different
/// type, a different call of the same service, and a refusal its own boundary makes.
const UNKNOWN_COMPARISON_TEMPLATE: &str = r#"{"Resources":{
    "Alarm":{"Type":"AWS::CloudWatch::Alarm","Properties":{
      "Namespace":"MyApp/Reason","MetricName":"Requests","Statistic":"Sum","Period":"60",
      "EvaluationPeriods":"1","Threshold":"1","ComparisonOperator":"NotAnOperator"}}}}"#;

const ROWS: &[Row] = &[
    Row {
        case: "a composite alarm whose rule references an alarm that does not exist",
        stack: "cw-reason-dangling-rule",
        template: DANGLING_RULE_TEMPLATE,
        logical_id: "Composite",
        code: VALIDATION_ERROR,
        sentence: "in the alarm rule do not exist",
    },
    Row {
        case: "a composite alarm whose suppressor names an alarm that does not exist",
        stack: "cw-reason-dangling-suppressor",
        template: DANGLING_SUPPRESSOR_TEMPLATE,
        logical_id: "Composite",
        code: VALIDATION_ERROR,
        sentence: "actions suppressor alarm",
    },
    Row {
        case: "a metric alarm whose declared comparison is outside the set the member admits",
        stack: "cw-reason-unknown-comparison",
        template: UNKNOWN_COMPARISON_TEMPLATE,
        logical_id: "Alarm",
        code: VALIDATION_ERROR,
        sentence: "NotAnOperator",
    },
];

/// The owning service's own sentence and the diagnostic its SDK appended, split apart.
fn sentence_and_diagnostic(case: &str, quoted: &str) -> (String, String) {
    const OPENS: &str = " (Service: ";
    let (sentence, diagnostic) = quoted.rsplit_once(OPENS).unwrap_or_else(|| {
        panic!(
            "case {case:?}: expected the SDK diagnostic after the service's own sentence, got \
             {quoted:?}"
        )
    });
    let envelope = diagnostic.strip_suffix(')').unwrap_or_else(|| {
        panic!("case {case:?}: expected the diagnostic to close, got {quoted:?}")
    });
    (sentence.to_string(), envelope.to_string())
}

/// Hold one diagnostic to the shape Amazon CloudWatch's SDK words: the service, the status, the
/// error code, the id of the call and the proxy it was made through — and nothing else.
fn assert_diagnostic(case: &str, envelope: &str, code: &str) {
    let fields: Vec<&str> = envelope.split("; ").collect();
    assert_eq!(
        fields.len(),
        5,
        "case {case:?}: expected the service, the status, the error code, the call's id and the \
         proxy and nothing else, got {envelope:?}"
    );
    assert_eq!(
        fields[0], CLOUDWATCH_SERVICE,
        "case {case:?}: expected the SDK's own name for the service, got {:?}",
        fields[0]
    );
    assert_eq!(
        fields[1],
        format!("Status Code: {REFUSED_STATUS}"),
        "case {case:?}: expected the status the owning service answered, got {:?}",
        fields[1]
    );
    assert_eq!(
        fields[2],
        format!("Error Code: {code}"),
        "case {case:?}: expected the error code the owning service answered, got {:?}",
        fields[2]
    );
    let request_id = fields[3]
        .strip_prefix("Request ID: ")
        .unwrap_or_else(|| panic!("case {case:?}: expected a Request ID, got {:?}", fields[3]));
    assert!(
        is_a_request_id(request_id),
        "case {case:?}: expected the id of the call in the form that service's answers carry, \
         got {request_id:?}"
    );
    assert_eq!(
        fields[4], DIRECT_CALL_PROXY,
        "case {case:?}: expected the proxy of a direct call, got {:?}",
        fields[4]
    );
}

#[tokio::test]
async fn a_resource_cloudwatch_refused_carries_that_services_status_and_error_code() {
    for row in ROWS {
        let case = row.case;
        let router = router();
        let (code, created) = create_stack(&router, row.stack, row.template, &[]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: CreateStack must answer 200, got {code} {created}"
        );
        let described = settled_stack(&router, case, row.stack).await;
        assert_eq!(
            xml_text(&described, "StackStatus").as_deref(),
            Some(status::ROLLBACK_COMPLETE),
            "case {case:?}: expected the refused create to roll back, got {described}"
        );

        let reason =
            failed_resource_reason(case, &router, REGION, row.stack, row.logical_id, CREATE_FAILED)
                .await;
        let (quoted, class) = quoted_and_class(case, &reason);
        let (sentence, envelope) = sentence_and_diagnostic(case, &quoted);
        assert!(
            sentence.contains(row.sentence),
            "case {case:?}: expected the owning service's own sentence carrying {:?}, got \
             {sentence:?}",
            row.sentence
        );
        assert_diagnostic(case, &envelope, row.code);
        assert_eq!(
            class, INVALID_REQUEST,
            "case {case:?}: expected the {INVALID_REQUEST} class, got {class:?} in {reason:?}"
        );
    }
}

#[tokio::test]
async fn the_dangling_reference_names_the_alarm_that_is_not_there() {
    // The variable part of that service's own sentence: the ARN of the alarm the rule
    // references, which is what tells a client WHICH reference to fix.
    let case = "the alarm the rule references";
    let router = router();
    let stack = "cw-reason-dangling-arn";
    let (code, created) = create_stack(&router, stack, DANGLING_RULE_TEMPLATE, &[]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: CreateStack must answer 200, got {code} {created}"
    );
    let described = settled_stack(&router, case, stack).await;
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some(status::ROLLBACK_COMPLETE),
        "case {case:?}: expected the refused create to roll back, got {described}"
    );
    let reason =
        failed_resource_reason(case, &router, REGION, stack, "Composite", CREATE_FAILED).await;
    let expected_arn = format!("arn:aws:cloudwatch:{REGION}:{ACCOUNT_ID}:alarm:{MISSING_ALARM}");
    assert!(
        reason.contains(&expected_arn),
        "case {case:?}: expected the reason to name {expected_arn:?}, got {reason:?}"
    );
    assert!(
        reason.contains(&format!("Error Code: {VALIDATION_ERROR}")),
        "case {case:?}: expected the reason to carry CloudWatch's {VALIDATION_ERROR}, got \
         {reason:?}"
    );
}
