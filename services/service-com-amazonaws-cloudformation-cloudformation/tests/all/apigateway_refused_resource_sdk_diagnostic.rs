// SPDX-License-Identifier: CC0-1.0

//! What a resource AMAZON API GATEWAY refused reports on its `CREATE_FAILED` event, end to
//! end over the awsQuery wire.
//!
//! RULE under test: **a resource provider reports a refused call as the exception its SDK
//! raised — the owning service's own sentence followed by that SDK's diagnostic of the call,
//! naming the service as its SDK names it and the HTTP status that service answered beside
//! the id of the call — so the failure is attributed to the service that refused it, whatever
//! type of that service failed and whichever of its calls was refused.**
//!
//! Why it matters: a client triaging a rolled-back deployment reads the failed resource's
//! `ResourceStatusReason` and recovers from it which service refused the call and with what
//! HTTP status. A reason carrying only CloudFormation's own wrapper leaves both unrecoverable,
//! so the standard triage of a failed API Gateway resource — was the declaration refused, or
//! did the provider never get that far — cannot be made at all.
//!
//! The rows differ along what the rule generalises over: WHICH call of the owning service was
//! refused (the deployment of an api that has no method to snapshot, and a deployment of an
//! api that service does not hold) and the HTTP STATUS it answered — each from the fault's own
//! `httpError` in API Gateway's model
//! (`smithy_query(query="operation",operation="CreateDeployment")`: `BadRequestException` is
//! 400 and `NotFoundException` is 404) — which is also what puts the two refusals in
//! different `HandlerErrorCode` classes.

use std::time::Duration;

use crate::stack_lifecycle::{create_stack, resource_event_reason, router, wait_for_the_transition};

/// The SDK's own name for Amazon API Gateway, as the exception its SDK raises states it:
/// that model's `awsService.cloudFormationName`.
const API_GATEWAY_SERVICE: &str = "ApiGateway";

/// How long a row waits for the failed resource's event to be there before giving up.
///
/// Generous on purpose: the only thing being waited FOR is the stack reaching its terminal
/// status, which is on the stack's own clock, and the wait ends as soon as the event is
/// readable rather than after a fixed time.
const EVENT_DEADLINE: Duration = Duration::from_secs(60);

/// How long one look-again waits before asking the event stream once more.
const EVENT_POLL_INTERVAL: Duration = Duration::from_millis(200);

/// One row: the stack to deploy, the resource the owning service refuses, and what its reason
/// must report.
struct Row {
    case: &'static str,
    stack: &'static str,
    template: &'static str,
    logical_id: &'static str,
    /// The HTTP status the SDK diagnostic must state.
    status: u16,
    /// A fragment of API Gateway's OWN sentence the quoted message must open with.
    sentence: &'static str,
    /// The `HandlerErrorCode` class the refusal falls in.
    class: &'static str,
}

/// An api with a resource under its root and no METHOD on it, and a deployment of it: a
/// deployment is the snapshot of the api's methods that a stage serves, so API Gateway
/// refuses the create of one for an api that has none.
const NO_METHOD_TEMPLATE: &str = r#"{"Resources":{
    "Api":{"Type":"AWS::ApiGateway::RestApi","Properties":{
      "Name":"apigw-diag-no-method-api",
      "EndpointConfiguration":{"Types":["REGIONAL"]}}},
    "Items":{"Type":"AWS::ApiGateway::Resource","Properties":{
      "RestApiId":{"Ref":"Api"},
      "ParentId":{"Fn::GetAtt":["Api","RootResourceId"]},
      "PathPart":"items"}},
    "Deployment":{"Type":"AWS::ApiGateway::Deployment","DependsOn":["Items"],"Properties":{
      "RestApiId":{"Ref":"Api"}}}}}"#;

/// The same type over an api id of the right FORM that API Gateway holds no api under, which
/// that service refuses at the status it answers for a resource it does not hold.
const ABSENT_API_TEMPLATE: &str = r#"{"Resources":{
    "Deployment":{"Type":"AWS::ApiGateway::Deployment","Properties":{
      "RestApiId":"abcde12345"}}}}"#;

/// The class a refusal of the REQUEST falls in, and the class of one about a resource that is
/// not there — both values of the model's `HandlerErrorCode` enum.
const INVALID_REQUEST: &str = "InvalidRequest";
const NOT_FOUND: &str = "NotFound";

const ROWS: &[Row] = &[
    Row {
        case: "a deployment of an api that has no method to snapshot",
        stack: "apigw-diag-no-method",
        template: NO_METHOD_TEMPLATE,
        logical_id: "Deployment",
        status: 400,
        sentence: "contain any methods",
        class: INVALID_REQUEST,
    },
    Row {
        case: "a deployment of an api that service does not hold",
        stack: "apigw-diag-absent-api",
        template: ABSENT_API_TEMPLATE,
        logical_id: "Deployment",
        status: 404,
        sentence: "abcde12345",
        class: NOT_FOUND,
    },
];

/// The quoted provider message and the reported `HandlerErrorCode` of one
/// `ResourceStatusReason`.
fn quoted_and_class(case: &str, reason: &str) -> (String, String) {
    const OPENS: &str = "Resource handler returned message: \"";
    const TOKEN: &str = "\" (RequestToken: ";
    const CLASS: &str = "HandlerErrorCode: ";
    let rest = reason.strip_prefix(OPENS).unwrap_or_else(|| {
        panic!("case {case:?}: expected the reason to open with {OPENS:?}, got {reason:?}")
    });
    let (quoted, tail) = rest.split_once(TOKEN).unwrap_or_else(|| {
        panic!("case {case:?}: expected a quoted message and a RequestToken, got {reason:?}")
    });
    let class = tail
        .split_once(CLASS)
        .and_then(|(_, class)| class.strip_suffix(')'))
        .unwrap_or_else(|| panic!("case {case:?}: expected a HandlerErrorCode, got {reason:?}"));
    (quoted.to_string(), class.to_string())
}

/// Hold one quoted exception to the diagnostic API Gateway's SDK appends: the sentence that
/// service worded, then the service, the status and the id of the call — and nothing beyond
/// the call itself.
fn assert_diagnostic(case: &str, quoted: &str, status: u16) {
    const OPENS: &str = " (Service: ";
    let (sentence, diagnostic) = quoted.rsplit_once(OPENS).unwrap_or_else(|| {
        panic!(
            "case {case:?}: expected the SDK diagnostic after the service's own sentence, got \
             {quoted:?}"
        )
    });
    assert!(
        !sentence.trim().is_empty(),
        "case {case:?}: expected the service's own sentence ahead of the diagnostic, got \
         {quoted:?}"
    );
    let envelope = diagnostic.strip_suffix(')').unwrap_or_else(|| {
        panic!("case {case:?}: expected the diagnostic to close, got {quoted:?}")
    });
    let fields: Vec<&str> = envelope.split(", ").collect();
    assert_eq!(
        fields.len(),
        3,
        "case {case:?}: expected the service, the status and the call's id and nothing else, \
         got {envelope:?}"
    );
    assert_eq!(
        fields[0], API_GATEWAY_SERVICE,
        "case {case:?}: expected the SDK's own name for the service, got {:?}",
        fields[0]
    );
    assert_eq!(
        fields[1],
        format!("Status Code: {status}"),
        "case {case:?}: expected the status the owning service answered, got {:?}",
        fields[1]
    );
    let request_id = fields[2]
        .strip_prefix("Request ID: ")
        .unwrap_or_else(|| panic!("case {case:?}: expected a Request ID, got {:?}", fields[2]));
    assert!(
        is_a_request_id(request_id),
        "case {case:?}: expected the id of the call in the form that service's answers carry, \
         got {request_id:?}"
    );
}

/// Whether `value` has the 8-4-4-4-12 lowercase-hexadecimal form an AWS request id carries.
fn is_a_request_id(value: &str) -> bool {
    let groups: Vec<&str> = value.split('-').collect();
    groups.len() == 5
        && groups.iter().map(|group| group.len()).eq([8, 4, 4, 4, 12])
        && groups.iter().all(|group| {
            group
                .chars()
                .all(|held| held.is_ascii_hexdigit() && !held.is_ascii_uppercase())
        })
}

/// The `ResourceStatusReason` of `logical_id`'s `CREATE_FAILED` event, looked for again until
/// the stack's own clock has recorded it or [`EVENT_DEADLINE`] passes.
async fn failure_reason(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    stack: &str,
    logical_id: &str,
) -> String {
    let deadline = tokio::time::Instant::now() + EVENT_DEADLINE;
    loop {
        let reason = resource_event_reason(router, stack, logical_id, "CREATE_FAILED").await;
        if !reason.is_empty() {
            return reason;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: {logical_id} never recorded a CREATE_FAILED reason"
        );
        tokio::time::sleep(EVENT_POLL_INTERVAL).await;
    }
}

#[tokio::test]
async fn a_refused_api_gateway_resource_is_attributed_to_api_gateway() {
    for row in ROWS {
        let router = router();
        let (status_code, created) = create_stack(&router, row.stack, row.template, &[]).await;
        assert_eq!(
            status_code, 200,
            "case {:?}: CreateStack must answer 200, got {status_code} {created}",
            row.case
        );
        // The create's own terminal status, and the rollback's: both are on the stack's clock.
        wait_for_the_transition().await;
        wait_for_the_transition().await;

        let reason = failure_reason(&router, row.case, row.stack, row.logical_id).await;
        let (quoted, class) = quoted_and_class(row.case, &reason);
        assert!(
            quoted.contains(row.sentence),
            "case {:?}: expected API Gateway's own sentence carrying {:?}, got {quoted:?}",
            row.case,
            row.sentence
        );
        assert_eq!(
            class, row.class,
            "case {:?}: expected the {} class, got {class:?} in {reason:?}",
            row.case, row.class
        );
        assert_diagnostic(row.case, &quoted, row.status);
    }
}
