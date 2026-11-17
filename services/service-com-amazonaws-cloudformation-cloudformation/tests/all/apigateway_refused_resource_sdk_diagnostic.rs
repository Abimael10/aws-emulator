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

use crate::harness::{assert_sdk_diagnostic, failed_resource_reason, quoted_and_class, REGION};
use crate::stack_lifecycle::{create_stack, router, wait_for_the_transition};

/// The SDK's own name for Amazon API Gateway, as the exception its SDK raises states it:
/// that model's `awsService.cloudFormationName`.
const API_GATEWAY_SERVICE: &str = "ApiGateway";

/// The status a resource its provider could not create records.
const CREATE_FAILED: &str = "CREATE_FAILED";

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

        let reason = failed_resource_reason(
            row.case,
            &router,
            REGION,
            row.stack,
            row.logical_id,
            CREATE_FAILED,
        )
        .await;
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
        assert_sdk_diagnostic(row.case, &quoted, API_GATEWAY_SERVICE, row.status);
    }
}
