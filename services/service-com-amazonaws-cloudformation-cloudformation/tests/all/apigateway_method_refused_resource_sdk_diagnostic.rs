// SPDX-License-Identifier: CC0-1.0

//! What an `AWS::ApiGateway::Method` the owning service REFUSED reports on its
//! `CREATE_FAILED` event, end to end over the awsQuery wire.
//!
//! RULE under test: **the diagnostic CloudFormation quotes in a failed resource's
//! `ResourceStatusReason` is the one the exception that resource's OWN provider's SDK raised:
//! the owning service's sentence, then that SDK's account of the call — the service as its SDK
//! names it, the HTTP status that service answered and the id of the call, and then how many
//! attempts the SDK made at it where that provider's SDK words a count — and the whole of it
//! inside the quotes, with the resource request's `RequestToken` and the `HandlerErrorCode`
//! class outside them.** A method's provider is one that counts attempts, whichever of the
//! calls it makes was refused and whatever class the refusal falls in.
//!
//! Why it matters: a deployment tool triaging a rolled-back stack parses the reason to recover
//! WHICH service refused the call, with what HTTP status and under which request id — that is
//! how a failure is correlated with the called service's own logs and classified as a client
//! fault against a service fault. A reason whose quoted message stops at the service's sentence
//! carries none of it, so the failure is reported as unparseable rather than as a 400 from API
//! Gateway.
//!
//! The rows differ along what the rule generalises over: WHICH of the provider's calls the
//! refusal answered (`PutMethod`, `PutIntegration`, `PutMethodResponse`), WHICH member of the
//! declaration it refused, the HTTP STATUS that refusal carried — each the fault's own
//! `httpError` in API Gateway's model
//! (`smithy_query(query="operation",operation="PutMethod,PutIntegration,PutMethodResponse")`:
//! `BadRequestException` is 400 and `NotFoundException` is 404) — and the `HandlerErrorCode`
//! class those statuses put the refusal in.
//!
//! The POSITIVE CONTROL deploys the same api with the model its methods name really declared,
//! and reads the method back through API Gateway's own read: the four calls the method's
//! provider makes still create what the template declared.
//!
//! `apigateway_refused_resource_sdk_diagnostic.rs` owns the same question for the api's
//! DEPLOYMENT, whose provider words the bare envelope with no attempt count after it, and
//! `apigateway_rest_api_refused_create_reason.rs` for the api itself.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::apigateway_vpc_link_resources::apigateway;
use crate::harness::{
    assert_sdk_diagnostic_with_attempts, failed_resource_reason, quoted_and_class, settled_stack,
    settled_stack_status, REGION,
};
use crate::stack_lifecycle::{create_stack, output_value, router};

/// The status a resource its provider could not create records, and the one the stack settles
/// in once that failed create has been rolled back.
const CREATE_FAILED: &str = "CREATE_FAILED";
const ROLLBACK_COMPLETE: &str = "ROLLBACK_COMPLETE";
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The SDK's own name for Amazon API Gateway, as the exception its SDK raises states it: that
/// model's `awsService.cloudFormationName`.
const API_GATEWAY_SERVICE: &str = "ApiGateway";

/// The `HandlerErrorCode` classes these rows fall in, both values of the model's own
/// `HandlerErrorCode` enum: a refusal of the REQUEST, and one about a record that is not there.
const INVALID_REQUEST: &str = "InvalidRequest";
const NOT_FOUND: &str = "NotFound";

/// The content type every declared map of models is keyed by.
const JSON: &str = "application/json";

/// The model names the refused rows reference, neither of which any row declares: a
/// `requestModels` value and a `responseModels` value, so the two refusals are about two
/// different members.
const ABSENT_REQUEST_MODEL: &str = "AbsentRequestPayload";
const ABSENT_RESPONSE_MODEL: &str = "AbsentResponsePayload";

/// The `Uri` the unroutable-integration row declares: a well-formed URL of a scheme that is no
/// HTTP(S) endpoint, which is what an integration of the HTTP family takes.
const UNROUTABLE_URI: &str = "ftp://backend.example.test/items";

/// An identifier of the FORM API Gateway assigns a resource of an api — ten lowercase
/// alphanumeric characters — that no resource of the declared api answers to.
const ABSENT_RESOURCE_ID: &str = "abcde12345";

/// The model the positive control declares, named in the alphabet `CreateModel$name` admits
/// ("must be alphanumeric").
const CONTROL_MODEL: &str = "ControlPayload";

/// The status the positive control's method response and integration response are keyed by.
const CONTROL_STATUS: &str = "200";

/// Which member of the declaration the owning service refuses, and so which of the method
/// provider's calls answers the refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fault {
    /// A `RequestModels` value naming no `Model` of the api: refused by `PutMethod`.
    UnknownRequestModel,
    /// A `MethodResponses` entry whose `ResponseModels` names no `Model` of the api: refused by
    /// `PutMethodResponse`, after the method itself was put.
    UnknownResponseModel,
    /// An `Integration` of the HTTP family whose `Uri` names no HTTP(S) endpoint: refused by
    /// `PutIntegration`.
    UnroutableIntegrationUri,
    /// A `ResourceId` naming no resource of the api: refused by `PutMethod` at the status that
    /// service answers for a record it does not hold.
    UnknownResource,
}

/// One row: the declaration whose method the owning service refuses, and what its failed
/// event must report.
struct Row {
    case: &'static str,
    stack: &'static str,
    fault: Fault,
    /// The HTTP status the SDK diagnostic must state.
    status: u16,
    /// API Gateway's OWN sentence, which the quoted message must open with.
    sentence: &'static str,
    /// The `HandlerErrorCode` class the refusal falls in.
    class: &'static str,
}

const ROWS: &[Row] = &[
    Row {
        case: "a method whose RequestModels names a model the api does not hold",
        stack: "apigw-method-diag-request-model",
        fault: Fault::UnknownRequestModel,
        status: 400,
        sentence: "Invalid model identifier specified: AbsentRequestPayload",
        class: INVALID_REQUEST,
    },
    Row {
        case: "a method response whose ResponseModels names a model the api does not hold",
        stack: "apigw-method-diag-response-model",
        fault: Fault::UnknownResponseModel,
        status: 400,
        sentence: "Invalid model identifier specified: AbsentResponsePayload",
        class: INVALID_REQUEST,
    },
    Row {
        case: "a method whose HTTP integration names no HTTP endpoint",
        stack: "apigw-method-diag-integration-uri",
        fault: Fault::UnroutableIntegrationUri,
        status: 400,
        sentence: "Invalid HTTP endpoint specified for URI",
        class: INVALID_REQUEST,
    },
    Row {
        case: "a method on a resource the api does not hold",
        stack: "apigw-method-diag-absent-resource",
        fault: Fault::UnknownResource,
        status: 404,
        sentence: "Invalid Resource identifier specified",
        class: NOT_FOUND,
    },
];

/// The name the row's api is declared under.
fn api_name(stack: &str) -> String {
    format!("{stack}-api")
}

/// The api and the resource under its root that every template here declares.
fn api_and_resource(stack: &str) -> serde_json::Map<String, Value> {
    let mut resources = serde_json::Map::new();
    resources.insert(
        "Api".to_string(),
        json!({
            "Type": "AWS::ApiGateway::RestApi",
            "Properties": {
                "Name": api_name(stack),
                "EndpointConfiguration": { "Types": ["REGIONAL"] }
            }
        }),
    );
    resources.insert(
        "Items".to_string(),
        json!({
            "Type": "AWS::ApiGateway::Resource",
            "Properties": {
                "RestApiId": { "Ref": "Api" },
                "ParentId": { "Fn::GetAtt": ["Api", "RootResourceId"] },
                "PathPart": "items"
            }
        }),
    );
    resources
}

/// The template one row deploys: the api, a resource under its root and the method whose
/// declaration carries the row's fault. Nothing else can fail the stack.
fn template(row: &Row) -> String {
    let mut method = json!({
        "RestApiId": { "Ref": "Api" },
        "ResourceId": { "Ref": "Items" },
        "HttpMethod": "GET",
        "AuthorizationType": "NONE",
        "Integration": { "Type": "MOCK" }
    });
    match row.fault {
        Fault::UnknownRequestModel => {
            method["RequestModels"] = json!({ JSON: ABSENT_REQUEST_MODEL });
        }
        Fault::UnknownResponseModel => {
            method["MethodResponses"] = json!([{
                "StatusCode": CONTROL_STATUS,
                "ResponseModels": { JSON: ABSENT_RESPONSE_MODEL }
            }]);
        }
        Fault::UnroutableIntegrationUri => {
            method["Integration"] = json!({
                "Type": "HTTP",
                "IntegrationHttpMethod": "GET",
                "Uri": UNROUTABLE_URI
            });
        }
        Fault::UnknownResource => {
            method["ResourceId"] = json!(ABSENT_RESOURCE_ID);
        }
    }
    let mut resources = api_and_resource(row.stack);
    resources.insert(
        "Method".to_string(),
        json!({ "Type": "AWS::ApiGateway::Method", "Properties": method }),
    );
    json!({ "Resources": Value::Object(resources) }).to_string()
}

/// Whether API Gateway holds an api NAMED `name`: a page of that service's own collection read,
/// which reports no `item` member at all for a page that holds none.
async fn api_exists(router: &MultiServiceRouter, case: &str, name: &str) -> bool {
    let (status, page) = apigateway(router, "GET", "/restapis").await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetRestApis must answer 200, got {status} {page}"
    );
    page.get("item")
        .and_then(Value::as_array)
        .is_some_and(|items| {
            items
                .iter()
                .any(|api| api.get("name").and_then(Value::as_str) == Some(name))
        })
}

#[tokio::test]
async fn a_refused_method_reports_the_exception_its_provider_raised() {
    for row in ROWS {
        let router = router();
        let (status_code, created) = create_stack(&router, row.stack, &template(row), &[]).await;
        assert_eq!(
            status_code, 200,
            "case {:?}: CreateStack must answer 200, got {status_code} {created}",
            row.case
        );

        let reason =
            failed_resource_reason(row.case, &router, REGION, row.stack, "Method", CREATE_FAILED)
                .await;
        let (quoted, class) = quoted_and_class(row.case, &reason);
        assert!(
            quoted.starts_with(row.sentence),
            "case {:?}: expected the quoted message to open with the owning service's own \
             sentence {:?}, got {quoted:?}",
            row.case,
            row.sentence
        );
        assert_eq!(
            class, row.class,
            "case {:?}: expected the {} class, got {class:?} in {reason:?}",
            row.case, row.class
        );
        assert_sdk_diagnostic_with_attempts(row.case, &quoted, API_GATEWAY_SERVICE, row.status);

        // The failed create is rolled back, and the api the declaration asked for is left
        // nowhere: the refusal is the resource's, and the rollback takes the rest away.
        let settled = settled_stack_status(row.case, &router, REGION, row.stack).await;
        assert_eq!(
            settled, ROLLBACK_COMPLETE,
            "case {:?}: expected the failed create to roll back, got {settled:?}",
            row.case
        );
        let name = api_name(row.stack);
        assert!(
            !api_exists(&router, row.case, &name).await,
            "case {:?}: expected API Gateway to hold no api named {name:?}",
            row.case
        );
    }
}

/// The positive control: the same api with the model its method and its method response name
/// really declared. Every call the method's provider makes is accepted, and the method is read
/// back through API Gateway's own read — so the refusals above are about the declarations, not
/// about the calls.
#[tokio::test]
async fn a_method_naming_a_declared_model_is_created_in_api_gateway() {
    let case = "a method naming the model the template declares";
    let stack = "apigw-method-diag-control";
    let router = router();

    let mut resources = api_and_resource(stack);
    resources.insert(
        "Model".to_string(),
        json!({
            "Type": "AWS::ApiGateway::Model",
            "Properties": {
                "RestApiId": { "Ref": "Api" },
                "Name": CONTROL_MODEL,
                "ContentType": JSON,
                "Schema": {
                    "$schema": "http://json-schema.org/draft-04/schema#",
                    "title": "Payload",
                    "type": "object",
                    "properties": { "marker": { "type": "string" } }
                }
            }
        }),
    );
    resources.insert(
        "Method".to_string(),
        json!({
            "Type": "AWS::ApiGateway::Method",
            "Properties": {
                "RestApiId": { "Ref": "Api" },
                "ResourceId": { "Ref": "Items" },
                "HttpMethod": "GET",
                "AuthorizationType": "NONE",
                "RequestModels": { JSON: { "Ref": "Model" } },
                "Integration": {
                    "Type": "MOCK",
                    "IntegrationResponses": [{ "StatusCode": CONTROL_STATUS }]
                },
                "MethodResponses": [{
                    "StatusCode": CONTROL_STATUS,
                    "ResponseModels": { JSON: { "Ref": "Model" } }
                }]
            }
        }),
    );
    let template = json!({
        "Resources": Value::Object(resources),
        "Outputs": {
            "ApiId": { "Value": { "Ref": "Api" } },
            "ResourceId": { "Value": { "Ref": "Items" } }
        }
    })
    .to_string();

    let (status_code, created) = create_stack(&router, stack, &template, &[]).await;
    assert_eq!(
        status_code, 200,
        "case {case:?}: CreateStack must answer 200, got {status_code} {created}"
    );
    let described = settled_stack(case, &router, REGION, stack).await;
    assert!(
        described.contains(CREATE_COMPLETE),
        "case {case:?}: expected the complete declaration to deploy, got {described}"
    );
    let api_id = output_value(&described, "ApiId").unwrap_or_default();
    let resource_id = output_value(&described, "ResourceId").unwrap_or_default();
    assert!(
        !api_id.is_empty() && !resource_id.is_empty(),
        "case {case:?}: expected the stack to publish the api and the resource, got \
         {api_id:?} and {resource_id:?}"
    );

    let (status, method) = apigateway(
        &router,
        "GET",
        &format!("/restapis/{api_id}/resources/{resource_id}/methods/GET"),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected API Gateway to hold the method the stack created, got {status} \
         {method}"
    );
    assert_eq!(
        method.pointer("/requestModels").and_then(|models| models.get(JSON)),
        Some(&json!(CONTROL_MODEL)),
        "case {case:?}: expected the declared request model in force, got {method}"
    );
    assert_eq!(
        method
            .pointer("/methodResponses")
            .and_then(|responses| responses.get(CONTROL_STATUS))
            .and_then(|response| response.get("responseModels"))
            .and_then(|models| models.get(JSON)),
        Some(&json!(CONTROL_MODEL)),
        "case {case:?}: expected the declared response model in force, got {method}"
    );
    assert_eq!(
        method
            .pointer("/methodIntegration/type")
            .and_then(Value::as_str),
        Some("MOCK"),
        "case {case:?}: expected the declared integration behind the method, got {method}"
    );
    assert!(
        method
            .pointer("/methodIntegration/integrationResponses")
            .and_then(|responses| responses.get(CONTROL_STATUS))
            .is_some(),
        "case {case:?}: expected the declared integration response in force, got {method}"
    );
}
