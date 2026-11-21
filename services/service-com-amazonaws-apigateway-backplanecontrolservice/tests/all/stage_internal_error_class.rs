// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the exception CLASS a deployed stage names on the `500` it answers
//! when it cannot serve a call at all.
//!
//! THE RULE UNDER TEST, over a SET of such calls rather than one recorded request: every
//! refusal the `execute-api` endpoint answers through one of the API's gateway responses
//! names that refusal's exception class in the `x-amzn-errortype` header a client classifies
//! an answer by, and WHICH class it is follows the part of the configuration that failed: an
//! integration the stage cannot perform is an `InternalServerErrorException`, while a call
//! its AUTHORIZER produced no decision for — one it cannot invoke at all, and one whose run
//! produced none — is an `AuthorizerConfigurationException`. A 5xx left without the header is
//! an untyped failure, and retry or alarm logic keyed on the class never fires.
//!
//! The BODIES follow the same split, and the rows state each: an integration the stage cannot
//! perform carries the `Internal server error` sentence, while a call its authorizer left
//! undecided carries the `{"message":null}` body AWS answers that class with, because the
//! gateway sets no error message for it ([`crate::stage_authorizer_failure_body`] pins that
//! half).
//!
//! The BODIES differ, and the rows state each: an integration the stage cannot perform carries
//! the `Internal server error` sentence, while a call its authorizer left undecided carries
//! the `{"message":null}` body AWS answers that class with, because the gateway sets no error
//! message for it ([`crate::stage_authorizer_failure_body`] pins that half).
//!
//! The rows span what the rule generalises over: WHICH gateway response the refusal is
//! answered through (`API_CONFIGURATION_ERROR` for the integration half,
//! `AUTHORIZER_CONFIGURATION_ERROR` for the authorizer half), WHICH integration type and
//! cause leaves the call unservable (a Lambda proxy whose function nothing answers for, a
//! proxy `uri` that names no function invocation path at all, a passed-through `uri` carrying
//! a placeholder nothing maps, a `MOCK` with no integration response, a `MOCK` whose request
//! template is not one this emulator renders), WHICH authorizer type gates the call (`TOKEN`
//! and `REQUEST`), and — in the second test — whether the developer has CUSTOMIZED the status
//! code and the body of the gateway response, which changes what the caller reads and not
//! which exception the refusal is.
//!
//! The controls pin that the class is each refusal's OWN: a request the same stage SERVES
//! carries no class at all, and a `4xx` of the same api carries its own class rather than the
//! 5xx one.
//!
//! Every case drives the real wire path — a request to the api's own invoke host, with the
//! control plane driven through the same emulator ([`Gateway`]) — so what is asserted is what
//! a client of the published invoke URL sees.

use serde_json::{json, Value};

use crate::execute_api_data_plane::{
    deploy, integration_response, invoke, json_template, method_response, put_integration,
    put_method, Invoked,
};
use crate::gateway_response::put_gateway_response;
use crate::rest_api_lifecycle::{create_api, resource_under, string_member, Gateway};
use crate::test_invoke_method::lambda_uri;

/// The stage every api below is deployed to.
const STAGE: &str = "live";

/// The header the `x-amzn-errortype` class is read from, lowercased as HTTP names compare.
const ERROR_TYPE_HEADER: &str = "x-amzn-errortype";

/// The class a refusal whose cause is the method's own configuration is the wire form of.
const INTERNAL_SERVER_ERROR_EXCEPTION: &str = "InternalServerErrorException";

/// The class a refusal of a call its AUTHORIZER produced no decision for is the wire form of.
const AUTHORIZER_CONFIGURATION_EXCEPTION: &str = "AuthorizerConfigurationException";

/// The class of the refusal whose body is `Missing Authentication Token`, which the control
/// row asserts is unchanged by this rule.
const MISSING_TOKEN_EXCEPTION: &str = "MissingAuthenticationTokenException";

/// The sentence the uncustomized `Internal server error` envelope carries, which is the body
/// of every row whose INTEGRATION cannot be performed.
const CONFIGURATION_ERROR: &str = "Internal server error";

/// The body a call its AUTHORIZER left undecided carries instead: the response type's own
/// default template rendered with no message set, so the `message` member is the JSON literal
/// `null` ([`crate::stage_authorizer_failure_body`]).
const NO_AUTHORIZER_DECISION: &str = r#"{"message":null}"#;

/// The body the served `MOCK` route answers with: what proves a request reached an
/// integration, so no refusal may carry it.
const SERVED_BODY: &str = r#"{"served":true}"#;

/// The media type the request-template row sends its call as.
const JSON: &str = "application/json";

/// The header every authorizer below declares as its identity source, and the value the rows
/// carry for it — a request carrying none is refused `401` before the authorizer is reached,
/// which is a different refusal and not what these rows are about.
const IDENTITY_HEADER: &str = "Auth";
const IDENTITY_VALUE: &str = "a-token";

/// A route under `root` whose method is open and whose integration is `integration`.
async fn route(
    gateway: &Gateway,
    api: &str,
    root: &str,
    path_part: &str,
    integration: Value,
) -> String {
    let resource = resource_under(gateway, api, root, path_part).await;
    put_method(
        gateway,
        api,
        &resource,
        "GET",
        json!({ "authorizationType": "NONE" }),
    )
    .await;
    put_integration(gateway, api, &resource, "GET", integration).await;
    resource
}

/// A `MOCK` route under `root` that answers [`SERVED_BODY`], gated by `authorizer` when one
/// is named: the integration would serve the call, so a `500` is the authorizer half's.
async fn mock_route(
    gateway: &Gateway,
    api: &str,
    root: &str,
    path_part: &str,
    authorizer: Option<&str>,
) {
    let resource = resource_under(gateway, api, root, path_part).await;
    let method = match authorizer {
        Some(authorizer) => {
            json!({ "authorizationType": "CUSTOM", "authorizerId": authorizer })
        }
        None => json!({ "authorizationType": "NONE" }),
    };
    put_method(gateway, api, &resource, "GET", method).await;
    put_integration(gateway, api, &resource, "GET", json!({ "type": "MOCK" })).await;
    method_response(gateway, api, &resource, "GET", "200", json!({})).await;
    integration_response(
        gateway,
        api,
        &resource,
        "GET",
        "200",
        json!({ "responseTemplates": json_template(SERVED_BODY) }),
    )
    .await;
}

/// An `Authorizer` of `api` of `kind` (`TOKEN` or `REQUEST`) naming `uri`, answering its id.
async fn authorizer(gateway: &Gateway, api: &str, name: &str, kind: &str, uri: &str) -> String {
    let answer = gateway
        .call(
            "POST",
            &format!("/restapis/{api}/authorizers"),
            Some(json!({
                "name": name,
                "type": kind,
                "identitySource": format!("method.request.header.{IDENTITY_HEADER}"),
                "authorizerUri": uri,
            })),
        )
        .await;
    assert_eq!(
        answer.status, 201,
        "case CreateAuthorizer({name}): expected 201, got {} with {:?}",
        answer.status, answer.body
    );
    string_member(&answer.body, "id", &format!("CreateAuthorizer({name})"))
}

/// The `uri` of an `AWS` integration ARN that names an action other than the Lambda
/// function-invoking one, so it names no function to invoke: `Integration$uri` gives the AWS
/// form as `arn:aws:apigateway:{region}:{subdomain.service|service}:path|action/{path}`, and
/// this one names the S3 subdomain's path form.
fn not_a_function_uri() -> String {
    "arn:aws:apigateway:us-east-1:s3:path/bucket/key".to_string()
}

/// One call the stage cannot serve: what the request carries, and the answer it earns.
struct Unservable<'c> {
    case: &'c str,
    path: String,
    headers: Vec<(&'c str, &'c str)>,
    /// The body the refusal's own class carries: the `message` sentence of an integration
    /// that cannot be performed, and `None` for the authorizer class, whose body carries no
    /// message at all.
    message: Option<&'c str>,
}

/// Assert one unservable call: its status, its body, its class, and that nothing of an
/// integration's own answer reached the caller.
async fn assert_internal_error(gateway: &Gateway, api: &str, call: &Unservable<'_>) {
    let answered: Invoked = invoke(gateway, api, "GET", &call.path, &call.headers).await;
    assert_eq!(
        answered.status, 500,
        "case {:?}: expected 500, got {} with {:?}",
        call.case, answered.status, answered.body
    );
    match call.message {
        Some(message) => assert_eq!(
            answered.message().as_deref(),
            Some(message),
            "case {:?}: expected the message {message:?}, got {:?}",
            call.case,
            answered.body
        ),
        None => assert_eq!(
            answered.body, NO_AUTHORIZER_DECISION,
            "case {:?}: expected the body {NO_AUTHORIZER_DECISION:?}, got {:?}",
            call.case, answered.body
        ),
    }
    // WHICH class follows the same split as the body: the authorizer's own class for a call
    // it left undecided, and the server-fault one for a method's own configuration.
    let class = if call.message.is_some() {
        INTERNAL_SERVER_ERROR_EXCEPTION
    } else {
        AUTHORIZER_CONFIGURATION_EXCEPTION
    };
    assert_eq!(
        answered.header(ERROR_TYPE_HEADER),
        Some(class),
        "case {:?}: expected the refusal to name the class {class:?}, got {:?}",
        call.case,
        answered.header(ERROR_TYPE_HEADER)
    );
    assert!(
        !answered.body.contains("served"),
        "case {:?}: expected nothing of an integration's answer, got {:?}",
        call.case,
        answered.body
    );
}

#[tokio::test]
async fn every_internal_error_a_deployed_stage_answers_names_its_exception_class() {
    let gateway = Gateway::new();
    let (api, root) = create_api(&gateway, "internal-error-classes").await;

    // The integration half: five ways one stored configuration leaves the call unservable.
    route(
        &gateway,
        &api,
        &root,
        "proxy",
        json!({ "type": "AWS_PROXY", "httpMethod": "POST", "uri": lambda_uri() }),
    )
    .await;
    route(
        &gateway,
        &api,
        &root,
        "elsewhere",
        json!({ "type": "AWS_PROXY", "httpMethod": "POST", "uri": not_a_function_uri() }),
    )
    .await;
    route(
        &gateway,
        &api,
        &root,
        "passed-on",
        json!({
            "type": "HTTP_PROXY",
            "httpMethod": "GET",
            "uri": "https://backend.example.com/{item}",
        }),
    )
    .await;
    route(&gateway, &api, &root, "unmapped", json!({ "type": "MOCK" })).await;
    let templated = route(
        &gateway,
        &api,
        &root,
        "templated",
        json!({
            "type": "MOCK",
            "requestTemplates": { JSON: "#foreach($i in $input.path('$.ids'))\n{\"held\": 1}\n#end" },
        }),
    )
    .await;
    method_response(&gateway, &api, &templated, "GET", "200", json!({})).await;
    integration_response(
        &gateway,
        &api,
        &templated,
        "GET",
        "200",
        json!({ "responseTemplates": json_template(SERVED_BODY) }),
    )
    .await;

    // The authorizer half: each authorizer type, in front of an integration that WOULD serve
    // the call, so the 500 is the authorizer's and not the integration's.
    let token = authorizer(
        &gateway,
        &api,
        "token-authorizer",
        "TOKEN",
        &not_a_function_uri(),
    )
    .await;
    let request = authorizer(
        &gateway,
        &api,
        "request-authorizer",
        "REQUEST",
        &lambda_uri(),
    )
    .await;
    mock_route(&gateway, &api, &root, "token-guarded", Some(&token)).await;
    mock_route(&gateway, &api, &root, "request-guarded", Some(&request)).await;
    // And one route the stage serves in full, so a refusal is distinguishable from an answer.
    mock_route(&gateway, &api, &root, "open", None).await;
    deploy(&gateway, &api, STAGE).await;

    let identity = vec![(IDENTITY_HEADER, IDENTITY_VALUE)];
    let unservable = [
        Unservable {
            case: "an AWS_PROXY integration whose function nothing answers for",
            path: format!("/{STAGE}/proxy"),
            headers: Vec::new(),
            message: Some(CONFIGURATION_ERROR),
        },
        Unservable {
            case: "an AWS_PROXY uri that names no Lambda function invocation path",
            path: format!("/{STAGE}/elsewhere"),
            headers: Vec::new(),
            message: Some(CONFIGURATION_ERROR),
        },
        Unservable {
            case: "an HTTP_PROXY uri carrying a placeholder nothing maps a path parameter onto",
            path: format!("/{STAGE}/passed-on"),
            headers: Vec::new(),
            message: Some(CONFIGURATION_ERROR),
        },
        Unservable {
            case: "a MOCK integration with no integration response for the call",
            path: format!("/{STAGE}/unmapped"),
            headers: Vec::new(),
            message: Some(CONFIGURATION_ERROR),
        },
        Unservable {
            case: "a MOCK request template this emulator does not render",
            path: format!("/{STAGE}/templated"),
            headers: vec![("content-type", JSON)],
            message: Some(CONFIGURATION_ERROR),
        },
        Unservable {
            case: "a TOKEN authorizer whose authorizerUri names no function to invoke",
            path: format!("/{STAGE}/token-guarded"),
            headers: identity.clone(),
            message: None,
        },
        Unservable {
            case: "a REQUEST authorizer whose function nothing answers for",
            path: format!("/{STAGE}/request-guarded"),
            headers: identity.clone(),
            message: None,
        },
    ];
    for call in &unservable {
        assert_internal_error(&gateway, &api, call).await;
    }

    // The controls. A request the stage SERVES carries no class at all, so the header above is
    // the refusal's own and not something every response carries.
    let served = invoke(&gateway, &api, "GET", &format!("/{STAGE}/open"), &[]).await;
    assert_eq!(
        served.status, 200,
        "case \"a served request\": expected 200, got {} with {:?}",
        served.status, served.body
    );
    assert_eq!(
        served.body, SERVED_BODY,
        "case \"a served request\": expected the integration's own body, got {:?}",
        served.body
    );
    assert_eq!(
        served.header(ERROR_TYPE_HEADER),
        None,
        "case \"a served request\": expected no exception class, got {:?}",
        served.header(ERROR_TYPE_HEADER)
    );

    // And a 4xx of the same api names ITS own class, so the class is each refusal's rather
    // than a label every failure carries.
    let unroutable = invoke(&gateway, &api, "GET", &format!("/{STAGE}/absent"), &[]).await;
    assert_eq!(
        unroutable.status, 403,
        "case \"a path the deployment holds no resource for\": expected 403, got {} with {:?}",
        unroutable.status, unroutable.body
    );
    assert_eq!(
        unroutable.header(ERROR_TYPE_HEADER),
        Some(MISSING_TOKEN_EXCEPTION),
        "case \"a path the deployment holds no resource for\": expected the class \
         {MISSING_TOKEN_EXCEPTION:?}, got {:?}",
        unroutable.header(ERROR_TYPE_HEADER)
    );
}

#[tokio::test]
async fn a_customized_five_xx_gateway_response_still_names_the_refusals_class() {
    // A developer's `PutGatewayResponse` decides the status code and the body a refusal is
    // answered with; it does not decide which exception the refusal IS. Both ways a
    // customization applies are covered: the response type's own, and the `DEFAULT_5XX`
    // fallback for the whole class.
    let gateway = Gateway::new();
    let (api, root) = create_api(&gateway, "internal-error-classes-customized").await;
    route(
        &gateway,
        &api,
        &root,
        "proxy",
        json!({ "type": "AWS_PROXY", "httpMethod": "POST", "uri": lambda_uri() }),
    )
    .await;
    let token = authorizer(
        &gateway,
        &api,
        "token-authorizer",
        "TOKEN",
        &not_a_function_uri(),
    )
    .await;
    mock_route(&gateway, &api, &root, "guarded", Some(&token)).await;

    // Each customization moves the status to one the response type does not document and
    // replaces the body, so neither the status nor the prose can be what the class is read
    // from. `GatewayResponse$statusCode` admits any three-digit code.
    for (response_type, status, message) in [
        ("API_CONFIGURATION_ERROR", "503", "the backend is not wired"),
        ("DEFAULT_5XX", "521", "something of ours broke"),
    ] {
        let answer = put_gateway_response(
            &gateway,
            &api,
            response_type,
            json!({
                "statusCode": status,
                "responseTemplates": json_template(
                    &json!({ "message": message, "customized": true }).to_string(),
                ),
            }),
        )
        .await;
        assert_eq!(
            answer.status, 201,
            "case PutGatewayResponse({response_type}): expected 201, got {} with {:?}",
            answer.status, answer.body
        );
    }
    deploy(&gateway, &api, STAGE).await;

    for (case, path, headers, status, message, class) in [
        (
            "a customized API_CONFIGURATION_ERROR response",
            format!("/{STAGE}/proxy"),
            Vec::new(),
            503,
            "the backend is not wired",
            INTERNAL_SERVER_ERROR_EXCEPTION,
        ),
        (
            "an authorizer failure answered through the customized DEFAULT_5XX",
            format!("/{STAGE}/guarded"),
            vec![(IDENTITY_HEADER, IDENTITY_VALUE)],
            521,
            "something of ours broke",
            AUTHORIZER_CONFIGURATION_EXCEPTION,
        ),
    ] {
        let answered = invoke(&gateway, &api, "GET", &path, &headers).await;
        assert_eq!(
            answered.status, status,
            "case {case:?}: expected the customized {status}, got {} with {:?}",
            answered.status, answered.body
        );
        assert_eq!(
            answered.message().as_deref(),
            Some(message),
            "case {case:?}: expected the customized body, got {:?}",
            answered.body
        );
        assert!(
            answered.body.contains("customized"),
            "case {case:?}: expected the customized template, got {:?}",
            answered.body
        );
        assert_eq!(
            answered.header(ERROR_TYPE_HEADER),
            Some(class),
            "case {case:?}: expected the refusal to name the class {class:?}, got {:?}",
            answered.header(ERROR_TYPE_HEADER)
        );
    }
}
