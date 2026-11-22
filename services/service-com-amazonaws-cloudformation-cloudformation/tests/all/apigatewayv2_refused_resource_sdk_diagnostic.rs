// SPDX-License-Identifier: CC0-1.0

//! What a resource AMAZON API GATEWAY V2 refused reports on its `CREATE_FAILED` event, end to
//! end: the CloudFormation calls over its awsQuery wire, the refused creates over API Gateway
//! v2's own `restJson1` wire.
//!
//! RULE under test: **the SDK diagnostic a failed resource's reason carries names the service
//! that SERVED the refused call, in the shape that service's own SDK words; where one signing
//! name is shared by two services, which of them served the call is decided by the call
//! itself — every operation of the API Gateway v2 model is bound under that model's own `/v2`
//! URI prefix and no operation of the API Gateway REST control plane is — so a refused call
//! made under it is attributed to API Gateway v2.**
//!
//! Why it matters: a client triaging a rolled-back deployment reads the failed resource's
//! `ResourceStatusReason` and recovers from it which service refused the call, with what HTTP
//! status and under which id. A reason naming the REST control plane for a call the v2 plane
//! refused sends that triage — and any pipeline keyed on the originating service — to the
//! wrong service's logs, and one that stops at the envelope hides the attempt count that SDK
//! reports.
//!
//! The rows differ along what the rule generalises over: WHICH call of the v2 plane was
//! refused and why — an integration whose service-action subtype leaves out a parameter that
//! action requires, a second subtype whose required set is a different one, and a route inside
//! an api that service does not hold — and therefore the HTTP STATUS it answered, each from
//! the fault's own `httpError` in that model
//! (`smithy_query(query="operation",operation="CreateIntegration,CreateRoute")`:
//! `BadRequestException` is 400 and `NotFoundException` is 404), which is also what puts the
//! refusals in different `HandlerErrorCode` classes. The last case is the other side of the
//! same rule: a call on the REST plane over the SAME signing name is still attributed to it.

use serde_json::{json, Value};

use service_com_amazonaws_cloudformation_cloudformation::state::status;

use crate::harness::{
    assert_sdk_diagnostic, assert_sdk_diagnostic_with_attempts, failed_resource_reason,
    quoted_and_class,
};
use crate::stack_lifecycle::{create_stack, router, settled_stack, xml_text, REGION};

/// The SDK's own name for Amazon API Gateway v2, as the exception its SDK raises states it:
/// that model's `sdkId`, which is also the name its `AWS::ApiGatewayV2::*` types carry.
const APIGATEWAYV2_SERVICE: &str = "ApiGatewayV2";

/// The SDK's own name for the Amazon API Gateway REST control plane, which shares API Gateway
/// v2's signing name and is a DIFFERENT service: that model's `awsService.cloudFormationName`.
const API_GATEWAY_SERVICE: &str = "ApiGateway";

/// The status a resource its provider could not create records.
const CREATE_FAILED: &str = "CREATE_FAILED";

/// The `HandlerErrorCode` classes these refusals fall in, both values of the model's own
/// `HandlerErrorCode` enum.
const INVALID_REQUEST: &str = "InvalidRequest";
const NOT_FOUND: &str = "NotFound";

/// The protocol of an api whose integrations may name a service action: "Supported only for
/// HTTP API AWS_PROXY integrations" (`CreateIntegration$IntegrationSubtype`).
const HTTP: &str = "HTTP";

/// An api identifier of the right FORM that API Gateway v2 holds no api under.
const ABSENT_API_ID: &str = "abcde12345";

/// One row: the stack to deploy, the resource API Gateway v2 refuses, and what its reason must
/// report.
struct Row {
    case: &'static str,
    stack: &'static str,
    /// The template's resources, as the declaration states them.
    resources: fn() -> Value,
    logical_id: &'static str,
    /// The HTTP status the SDK diagnostic must state.
    status: u16,
    /// A fragment of API Gateway v2's OWN sentence the quoted message must carry.
    sentence: &'static str,
    /// The `HandlerErrorCode` class the refusal falls in.
    class: &'static str,
}

/// An HTTP api and one integration of it, with the properties the case needs beside the ones
/// every subtype integration carries.
fn subtype_integration(name: &str, subtype: &str, request_parameters: Value) -> Value {
    json!({
        "Api": {
            "Type": "AWS::ApiGatewayV2::Api",
            "Properties": { "Name": name, "ProtocolType": HTTP },
        },
        "Integration": {
            "Type": "AWS::ApiGatewayV2::Integration",
            "Properties": {
                "ApiId": { "Ref": "Api" },
                "IntegrationType": "AWS_PROXY",
                "IntegrationSubtype": subtype,
                "PayloadFormatVersion": "1.0",
                "RequestParameters": request_parameters,
            },
        },
    })
}

/// An `SQS-SendMessage` integration that maps the queue but not the message: `SendMessage`
/// cannot be called without the body it sends.
fn missing_message_body() -> Value {
    subtype_integration(
        "apigwv2-diag-sendmessage-api",
        "SQS-SendMessage",
        json!({ "QueueUrl": "$request.body.queueUrl" }),
    )
}

/// A `StepFunctions-StartExecution` integration that maps the execution's input but not the
/// state machine it runs: another subtype, with a required set of its own.
fn missing_state_machine_arn() -> Value {
    subtype_integration(
        "apigwv2-diag-startexecution-api",
        "StepFunctions-StartExecution",
        json!({ "Input": "$request.body" }),
    )
}

/// A route declared inside an api API Gateway v2 does not hold, which that service refuses at
/// the status it answers for a resource it has no record of.
fn route_in_an_absent_api() -> Value {
    json!({
        "Route": {
            "Type": "AWS::ApiGatewayV2::Route",
            "Properties": {
                "ApiId": ABSENT_API_ID,
                "RouteKey": "GET /items",
            },
        },
    })
}

/// A deployment of a REST api API Gateway does not hold: a call of the OTHER service that
/// shares this signing name, on a URI outside the v2 prefix.
fn deployment_in_an_absent_rest_api() -> Value {
    json!({
        "Deployment": {
            "Type": "AWS::ApiGateway::Deployment",
            "Properties": { "RestApiId": ABSENT_API_ID },
        },
    })
}

const ROWS: &[Row] = &[
    Row {
        case: "an integration whose SQS-SendMessage mapping leaves out the message body",
        stack: "apigwv2-diag-sendmessage",
        resources: missing_message_body,
        logical_id: "Integration",
        status: 400,
        sentence: "SQS-SendMessage requires enabling passthrough",
        class: INVALID_REQUEST,
    },
    Row {
        case: "an integration whose StepFunctions-StartExecution mapping leaves out the machine",
        stack: "apigwv2-diag-startexecution",
        resources: missing_state_machine_arn,
        logical_id: "Integration",
        status: 400,
        sentence: "StepFunctions-StartExecution requires enabling passthrough",
        class: INVALID_REQUEST,
    },
    Row {
        case: "a route inside an api that service does not hold",
        stack: "apigwv2-diag-absent-api",
        resources: route_in_an_absent_api,
        logical_id: "Route",
        status: 404,
        sentence: ABSENT_API_ID,
        class: NOT_FOUND,
    },
];

/// One template out of the resources a row declares.
fn template(resources: Value) -> String {
    json!({ "AWSTemplateFormatVersion": "2010-09-09", "Resources": resources }).to_string()
}

/// Hold one quoted exception to the diagnostic API Gateway v2's SDK appends: that service's own
/// sentence, then the service as its SDK names it, the status it answered, the id of the call
/// and the attempts the SDK made at it — each of the FORM that SDK words rather than one
/// recorded value.
///
/// The whole reading is the shape every SDK that counts its attempts words, so it is held to
/// the one assertion this target has for it
/// ([`assert_sdk_diagnostic_with_attempts`]) under this service's own name.
fn assert_apigatewayv2_diagnostic(case: &str, quoted: &str, status: u16) {
    assert_sdk_diagnostic_with_attempts(case, quoted, APIGATEWAYV2_SERVICE, status);
}

/// Deploy `stack`, wait for it to settle rolled back, and answer the quoted provider message
/// and the `HandlerErrorCode` class of `logical_id`'s failed event.
async fn refused_resource(
    case: &str,
    stack: &str,
    resources: Value,
    logical_id: &str,
) -> (String, String) {
    let router = router();
    let (code, body) = create_stack(&router, stack, &template(resources), &[]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: CreateStack must answer 200, got {code} {body}"
    );
    let described = settled_stack(&router, case, stack).await;
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some(status::ROLLBACK_COMPLETE),
        "case {case:?}: expected the refused create to roll back, got {described}"
    );
    let reason =
        failed_resource_reason(case, &router, REGION, stack, logical_id, CREATE_FAILED).await;
    let (quoted, class) = quoted_and_class(case, &reason);
    (quoted, class)
}

#[tokio::test]
async fn a_refused_api_gateway_v2_resource_is_attributed_to_api_gateway_v2() {
    for row in ROWS {
        let case = row.case;
        let (quoted, class) = refused_resource(case, row.stack, (row.resources)(), row.logical_id)
            .await;
        assert!(
            quoted.contains(row.sentence),
            "case {case:?}: expected API Gateway v2's own sentence carrying {:?}, got {quoted:?}",
            row.sentence
        );
        assert_eq!(
            class, row.class,
            "case {case:?}: expected the {} class, got {class:?} in {quoted:?}",
            row.class
        );
        assert_apigatewayv2_diagnostic(case, &quoted, row.status);
    }
}

#[tokio::test]
async fn a_refused_rest_api_gateway_resource_is_still_attributed_to_api_gateway() {
    // The other side of the rule: the REST control plane signs the SAME name as API Gateway v2
    // and is a different service, so a refusal of a call made OUTSIDE the v2 prefix keeps that
    // service's own name and its own diagnostic shape — the bare envelope, with no attempt
    // count after it.
    let case = "a deployment of a REST api that service does not hold";
    let (quoted, class) = refused_resource(
        case,
        "apigw-diag-shared-name",
        deployment_in_an_absent_rest_api(),
        "Deployment",
    )
    .await;
    assert_eq!(
        class, NOT_FOUND,
        "case {case:?}: expected the {NOT_FOUND} class, got {class:?} in {quoted:?}"
    );
    assert_sdk_diagnostic(case, &quoted, API_GATEWAY_SERVICE, 404);
}
