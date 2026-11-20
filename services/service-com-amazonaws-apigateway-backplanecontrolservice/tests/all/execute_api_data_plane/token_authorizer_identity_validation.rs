// SPDX-License-Identifier: CC0-1.0

//! What a `TOKEN` authorizer's own `identityValidationExpression` decides about the token a
//! request presents.
//!
//! RULE: `Authorizer$identityValidationExpression` is "a validation expression for the
//! incoming identity token. For `TOKEN` authorizers, this value is a regular expression […]
//! It will invoke the authorizer's Lambda function when there is a match. Otherwise, it will
//! return a 401 Unauthorized response without calling the Lambda function. […] The validation
//! expression does not apply to the `REQUEST` authorizer"
//! (`smithy_query(query="doc", shape="Authorizer", member="identityValidationExpression")`).
//! So the expression is the authorizer's FIRST filter: a `TOKEN` authorizer's function is
//! asked nothing at all about an identity the expression does not match, and the request is
//! refused `401`.
//!
//! The rows span what the rule generalises over: each way a presented token can fail one
//! expression (a different scheme, the same scheme in another case, a character the
//! expression's class excludes), a token that matches it, an authorizer that declares NO
//! expression (every token reaches its function), and the `REQUEST` authorizer the model
//! exempts (its function is asked whatever the token looks like).
//!
//! What makes an invocation OBSERVABLE: the one authorizer function RAISES for a token that
//! ends in `raise`, and an authorizer that errors earns the `500` envelope AWS reserves for
//! one — so a token that would make the function fail and is answered `401` instead is a
//! token the function was never asked about. A token the function DID decide is told apart
//! the other way: the method's integration response reads the value the authorizer published
//! for the call back through `$context.authorizer`, so the body names the very token its
//! function saw.
//!
//! Only a cross-service suite can pin this: the authorizer's function and its grant are AWS
//! Lambda's, and the api, the stage, the authorizer record and the method are API Gateway's.
//! The wire harness is the sibling `proxy_binary_media_types` suite's, reused rather than
//! stood up a second way.

use base64::Engine as _;
use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::proxy_binary_media_types::{
    apigw, dispatch, emulator, lambda, member, package, wait_until_active, Answer,
    FUNCTION_TIMEOUT, HANDLER, REGION, ROLE, STAGE,
};

/// The function every authorizer below names.
const FUNCTION: &str = "apigw-token-authorizer";

/// The header every authorizer below declares as its identity source.
const IDENTITY_HEADER: &str = "authorization";

/// The expression the validated authorizers declare: one scheme, a space and one or more
/// unaccented alphanumerics, anchored at both ends.
const EXPRESSION: &str = "^Bearer [A-Za-z0-9]+$";

/// The status and body an uncustomized `UNAUTHORIZED` gateway response carries, which is what
/// a request refused before the authorizer is invoked is answered with.
const UNAUTHORIZED: (u16, &str) = (401, r#"{"message":"Unauthorized"}"#);

/// The status and body an authorizer that produced no policy earns: the envelope AWS reserves
/// for an authorizer that errors, times out or returns a malformed policy.
const AUTHORIZER_FAILED: (u16, &str) = (500, r#"{"message":"Internal server error"}"#);

/// The ONE authorizer function every row is decided by: it fails for a token that ends in
/// `raise` — which is what makes an invocation observable — and otherwise allows the method
/// the event names, publishing the token it was handed.
const HANDLER_SOURCE: &str = r#"
def lambda_handler(event, context):
    token = event.get("authorizationToken")
    if token is None:
        token = (event.get("headers") or {}).get("Authorization") or ""
    if token.endswith("raise"):
        raise Exception("the authorizer itself failed")
    return {
        "principalId": "principal",
        "policyDocument": {
            "Version": "2012-10-17",
            "Statement": [
                {
                    "Action": "execute-api:Invoke",
                    "Effect": "Allow",
                    "Resource": event["methodArn"],
                }
            ],
        },
        "context": {"token": token},
    }
"#;

/// One method of the api, by the path it sits on: which kind of authorizer decides it and
/// whether that authorizer declares the validation expression.
struct Protected {
    /// The path part within the stage.
    part: &'static str,
    /// `Authorizer$type`.
    kind: &'static str,
    /// Whether the authorizer declares [`EXPRESSION`].
    validated: bool,
}

/// The methods the api carries, each on its own path so one deployment serves every row.
const PROTECTED: &[Protected] = &[
    Protected {
        part: "validated",
        kind: "TOKEN",
        validated: true,
    },
    Protected {
        part: "unvalidated",
        kind: "TOKEN",
        validated: false,
    },
    // "The validation expression does not apply to the REQUEST authorizer": the same
    // expression on the kind the model exempts.
    Protected {
        part: "request-validated",
        kind: "REQUEST",
        validated: true,
    },
];

/// Stand the authorizer function and the api up, and deploy the api to [`STAGE`]. Answers the
/// api id.
async fn deployed(router: &MultiServiceRouter) -> String {
    let function = lambda(
        router,
        "POST",
        "/2015-03-31/functions",
        Some(json!({
            "FunctionName": FUNCTION,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": HANDLER,
            "Timeout": FUNCTION_TIMEOUT,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(HANDLER_SOURCE)),
            },
        })),
    )
    .await;
    assert_eq!(
        function.status,
        201,
        "setup CreateFunction: expected HTTP 201, got {} with {:?}",
        function.status,
        function.text()
    );
    let function_arn = member("CreateFunction", &function.json(), "FunctionArn");

    // "Grants a principal permission to use a function" (`AddPermission`): the grant that
    // admits every authorizer invocation below. It names no `SourceArn`, so one statement
    // admits the call from every authorizer of the api.
    let granted = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{FUNCTION}/policy"),
        Some(json!({
            "StatementId": "apigw-token-authorizer",
            "Action": "lambda:InvokeFunction",
            "Principal": "apigateway.amazonaws.com",
        })),
    )
    .await;
    assert_eq!(
        granted.status,
        201,
        "setup AddPermission: expected HTTP 201, got {} with {:?}",
        granted.status,
        granted.text()
    );

    let created = apigw(
        router,
        "CreateRestApi",
        "POST",
        "/restapis",
        Some(json!({ "name": "token-authorizer-validation" })),
        201,
    )
    .await;
    let api = member("CreateRestApi", &created, "id");
    let root = member("CreateRestApi", &created, "rootResourceId");

    for protected in PROTECTED {
        let mut authorizer = json!({
            "name": format!("{}-authorizer", protected.part),
            "type": protected.kind,
            "authorizerUri": format!(
                "arn:aws:apigateway:{REGION}:lambda:path/2015-03-31/functions/\
                 {function_arn}/invocations"
            ),
            "identitySource": format!("method.request.header.{IDENTITY_HEADER}"),
            // "If it equals 0, authorization caching is disabled": every row asks the
            // authorizer afresh, so no row is decided by what another one left held.
            "authorizerResultTtlInSeconds": 0,
        });
        if protected.validated {
            authorizer["identityValidationExpression"] = json!(EXPRESSION);
        }
        let authorizer = apigw(
            router,
            "CreateAuthorizer",
            "POST",
            &format!("/restapis/{api}/authorizers"),
            Some(authorizer),
            201,
        )
        .await;
        let authorizer = member("CreateAuthorizer", &authorizer, "id");
        let resource = apigw(
            router,
            "CreateResource",
            "POST",
            &format!("/restapis/{api}/resources/{root}"),
            Some(json!({ "pathPart": protected.part })),
            201,
        )
        .await;
        let resource = member("CreateResource", &resource, "id");
        apigw(
            router,
            "PutMethod",
            "PUT",
            &format!("/restapis/{api}/resources/{resource}/methods/GET"),
            Some(json!({ "authorizationType": "CUSTOM", "authorizerId": authorizer })),
            201,
        )
        .await;
        // A `MOCK` integration, so what an ADMITTED call is answered with is the api's own
        // records and what the authorizer published, and nothing else.
        apigw(
            router,
            "PutIntegration",
            "PUT",
            &format!("/restapis/{api}/resources/{resource}/methods/GET/integration"),
            Some(json!({
                "type": "MOCK",
                "requestTemplates": { "application/json": r#"{"statusCode": 200}"# },
            })),
            201,
        )
        .await;
        apigw(
            router,
            "PutMethodResponse",
            "PUT",
            &format!("/restapis/{api}/resources/{resource}/methods/GET/responses/200"),
            Some(json!({})),
            201,
        )
        .await;
        // The response template reads back the token the AUTHORIZER published for this call,
        // which is how a row tells what its function saw.
        apigw(
            router,
            "PutIntegrationResponse",
            "PUT",
            &format!(
                "/restapis/{api}/resources/{resource}/methods/GET/integration/responses/200"
            ),
            Some(json!({
                "responseTemplates": {
                    "application/json": "{\"token\":\"$context.authorizer.token\"}",
                },
            })),
            201,
        )
        .await;
    }

    apigw(
        router,
        "CreateDeployment",
        "POST",
        &format!("/restapis/{api}/deployments"),
        Some(json!({ "stageName": STAGE })),
        201,
    )
    .await;

    wait_until_active(router, FUNCTION).await;
    api
}

/// One request to the api's own invoke endpoint, presenting `token` as the identity source
/// every authorizer declares and carrying no credential, as a client of a custom-authorized
/// method sends none.
async fn invoke(router: &MultiServiceRouter, api: &str, part: &str, token: &str) -> Answer {
    dispatch(
        router,
        "GET",
        &format!("{api}.execute-api.{REGION}.amazonaws.com"),
        &format!("/{STAGE}/{part}"),
        &[(IDENTITY_HEADER, token)],
        Bytes::new(),
    )
    .await
}

/// What the caller must be served for one row.
#[derive(Debug)]
enum Served {
    /// The integration's answer, naming the token the authorizer's function was handed —
    /// which only an invocation that HAPPENED can carry.
    Decided,
    /// The `401` of a request refused before the authorizer is invoked.
    RefusedUnasked,
    /// The `500` of an authorizer that was invoked and errored — which is what a token the
    /// expression ADMITS earns here, since the one function fails for it.
    Asked,
}

/// One row: the method it addresses, the token it presents and what the caller must be served.
struct Row {
    case: &'static str,
    part: &'static str,
    token: &'static str,
    served: Served,
}

/// RULE: a `TOKEN` authorizer's `identityValidationExpression` is tested against the presented
/// identity BEFORE its function is invoked — a token it does not match is refused `401` with
/// the function never asked, a token it matches reaches the function, an authorizer declaring
/// no expression admits every token to its function, and a `REQUEST` authorizer's function is
/// reached whatever the token looks like.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_token_the_validation_expression_rejects_never_reaches_the_authorizer() {
    let router = emulator();
    let api = deployed(&router).await;

    let rows: &[Row] = &[
        Row {
            case: "a token the expression matches reaches the authorizer's function",
            part: "validated",
            token: "Bearer abc123",
            served: Served::Decided,
        },
        Row {
            case: "a token the expression matches, which this function fails on: the \
                   invocation happened",
            part: "validated",
            token: "Bearer raise",
            served: Served::Asked,
        },
        Row {
            case: "another scheme, which the expression does not match",
            part: "validated",
            token: "Basic abc123",
            served: Served::RefusedUnasked,
        },
        Row {
            case: "the same scheme in another case, which the expression does not match",
            part: "validated",
            token: "bearer abc123",
            served: Served::RefusedUnasked,
        },
        Row {
            case: "a token carrying a character the expression's class excludes",
            part: "validated",
            token: "Bearer abc 123",
            served: Served::RefusedUnasked,
        },
        Row {
            case: "a token that would make this function fail and does not match: refused \
                   without invoking it",
            part: "validated",
            token: "Basic raise",
            served: Served::RefusedUnasked,
        },
        Row {
            case: "an authorizer declaring no expression: the same token reaches its function",
            part: "unvalidated",
            token: "Basic raise",
            served: Served::Asked,
        },
        Row {
            case: "an authorizer declaring no expression, admitting a token no expression \
                   filtered",
            part: "unvalidated",
            token: "Basic abc123",
            served: Served::Decided,
        },
        Row {
            case: "a REQUEST authorizer, which the expression does not apply to",
            part: "request-validated",
            token: "Basic raise",
            served: Served::Asked,
        },
        Row {
            case: "a REQUEST authorizer deciding a token no expression filtered",
            part: "request-validated",
            token: "Basic abc123",
            served: Served::Decided,
        },
    ];

    for row in rows {
        let answered = invoke(&router, &api, row.part, row.token).await;
        let (status, body) = match row.served {
            Served::Decided => (
                200,
                json!({ "token": row.token }).to_string(),
            ),
            Served::RefusedUnasked => (UNAUTHORIZED.0, UNAUTHORIZED.1.to_string()),
            Served::Asked => (AUTHORIZER_FAILED.0, AUTHORIZER_FAILED.1.to_string()),
        };
        assert_eq!(
            answered.status,
            status,
            "case {:?}: expected HTTP {status} ({:?}), got {} with {:?}",
            row.case,
            row.served,
            answered.status,
            answered.text()
        );
        match row.served {
            // The decided rows are compared as the documents they are: the body is the
            // integration's own rendering of what the authorizer published.
            Served::Decided => assert_eq!(
                answered.json(),
                serde_json::from_str::<Value>(&body).unwrap_or(Value::Null),
                "case {:?}: expected the authorizer's own published token, got {:?}",
                row.case,
                answered.text()
            ),
            _ => assert_eq!(
                answered.text(),
                body,
                "case {:?}: expected the body {body:?}, got {:?}",
                row.case,
                answered.text()
            ),
        }
    }
}
