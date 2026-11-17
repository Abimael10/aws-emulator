// SPDX-License-Identifier: CC0-1.0

//! A REST API a CloudFormation TEMPLATE deployed, whose method is gated by the
//! `COGNITO_USER_POOLS` authorizer the same template declared, SERVES its integration at the
//! stage invoke URL the stack published — and the admitted token's claims reach that
//! integration.
//!
//! The rule under test: **CloudFormation creates every resource a template declares in the
//! service that OWNS that resource type, through that service's own public API, and publishes
//! the identifier that service assigned — so the api, its authorizer, its resources, its
//! methods, the deployment of them and the stage that serves it all exist in API Gateway
//! under the ids it minted, every one of those ids resolves through that service's own read,
//! and a request to the deployed stage's invoke URL is decided by that api's data plane: the
//! authorizer the template declared admits a token one of its `providerARNs` pools signed and
//! the integration behind the method answers it, with the token's claims available as
//! `$context.authorizer.claims.<name>`.** Three crates have to agree for that to hold
//! (CloudFormation expands the template and makes the calls, Amazon Cognito user pools mint
//! the tokens and publish the key set, API Gateway serves the control-plane calls and then the
//! invoke), so the suite lives here rather than in any one of them, and it drives the
//! emulator's own assembled service set.
//!
//! `cloudformation_rest_api_serves_its_stage.rs` owns the five types a REST API is built out
//! of and `cloudformation_apigateway_access_control.rs` owns the records that decide who
//! reaches it; this suite owns the one request a template-deployed stage is REACHED by when a
//! user pool decides it.
//!
//! The rows differ along what the rule generalises over: WHICH pool minted the presented token
//! (the one the declaration's `ProviderARNs` names, a second pool the stack never named, none
//! at all), WHICH kind of token it is (the identity token and the access token of one
//! sign-in), HOW it is presented (bare, with the RFC 6750 bearer scheme, absent, not a token
//! at all), WHICH claim the integration's template reads (a bare-identifier claim and the
//! namespaced one only a bracketed member access can read), and WHICH method of the deployed
//! stage is addressed (a gated `GET` one path segment deep, a gated `POST` two segments deep
//! carrying a body, and an open `GET` the authorizer does not decide).
//!
//! Every expected value is computed from what the row declares or from the token the pool
//! minted — the api id, the authorizer id and the invoke URL come out of the stack's own
//! outputs, which is what a client deploying the stack reads — never out of the answer being
//! checked.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{elements, form_encode, outputs_of};

/// Keep this at or above the size the sibling suites that assemble the whole router use.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The Region every request here is signed for, and so the Region the stack's resources, the
/// pools' issuers and the api's invoke host belong to. It is the Region the shared stack poll
/// helper signs its reads for, so the stack and its resources are all this one's.
const REGION: &str = "us-east-1";

/// The `StackStatus` the rule is stated about.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The stage the template deploys the api to.
const STAGE: &str = "live";

/// The permanent password this suite's fixture users sign in with — a value written for this
/// test and used nowhere else. It satisfies a pool's default password policy.
const GOOD_PASSWORD: &str = "Str0ng-Passw0rd!";

/// The header the declared authorizer's identity source names.
const TOKEN_HEADER: &str = "Authorization";

/// The status and sentence a request the declared authorizer does not admit earns, and that
/// nothing the method fronts ran.
const UNAUTHORIZED: u16 = 401;
const UNAUTHORIZED_MESSAGE: &str = "Unauthorized";

/// The sentence the `API_CONFIGURATION_ERROR` / `AUTHORIZER_CONFIGURATION_ERROR` gateway
/// responses carry — the `500` that must never answer a request to a stage a template
/// deployed.
const CONFIGURATION_ERROR: &str = "Internal server error";

/// The body the `MOCK` integration behind the gated `GET` answers: the claims of the token the
/// authorizer accepted, which is how a row observes that the gate let the request THROUGH and
/// that the integration could read the signed-in user.
const CLAIMS_TEMPLATE: &str =
    "{\"sub\":\"$context.authorizer.claims.sub\",\"use\":\"$context.authorizer.claims.token_use\"}";

/// The same for the gated `POST`, reading the claim whose NAME is no bare identifier: a
/// Cognito authorizer publishes namespaced claims, and the bracketed member access is how a
/// template reads one.
const NAMESPACED_CLAIM_TEMPLATE: &str =
    "{\"user\":\"$context.authorizer.claims['cognito:username']\",\
     \"use\":\"$context.authorizer.claims.token_use\"}";

/// The body the open method's `MOCK` integration answers: no authorizer decided it, so there
/// are no claims to read.
const OPEN_TEMPLATE: &str = "{\"open\":true}";

/// The request template every `MOCK` integration here maps, which selects the `200` response.
/// It carries no Velocity DIRECTIVE, as a mock integration's template need not.
const REQUEST_TEMPLATE: &str = "{\"statusCode\": 200}";

/// One answer from the emulator: the status, the body parsed as JSON where it is JSON, and the
/// body as it arrived.
struct Answer {
    status: u16,
    body: Value,
    text: String,
}

impl Answer {
    /// One string member of a data-plane body, or `"<no member>"`.
    fn member(&self, name: &str) -> String {
        self.body
            .get(name)
            .and_then(Value::as_str)
            .unwrap_or("<no member>")
            .to_string()
    }

    fn summary(&self) -> String {
        format!("{} {}", self.status, self.text)
    }
}

/// Dispatch one request with the host, path, headers and body given.
async fn dispatch(
    router: &MultiServiceRouter,
    host: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Vec<u8>,
) -> Answer {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder
        .body(bytes::Bytes::from(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let text = String::from_utf8_lossy(&bytes).to_string();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    Answer { status, body, text }
}

/// The credential scope a signed SDK request carries, which is what routes the request and
/// what each handler reads its Region from. The signature is never checked — this router runs
/// with authentication off, as in every other suite here.
fn credential_scope(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 \
         Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One awsJson1_1 call to the Amazon Cognito user pools service, which must succeed.
async fn cognito(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let target = format!("AWSCognitoIdentityProviderService.{operation}");
    let authorization = credential_scope("cognito-idp");
    let answer = dispatch(
        router,
        &format!("cognito-idp.{REGION}.amazonaws.com"),
        "POST",
        "/",
        &[
            ("content-type", "application/x-amz-json-1.1"),
            ("x-amz-target", &target),
            ("authorization", &authorization),
        ],
        body.to_string().into_bytes(),
    )
    .await;
    assert_eq!(
        answer.status,
        200,
        "setup: cognito-idp {operation} must answer 200, got {}",
        answer.summary()
    );
    answer.body
}

/// Call one CloudFormation operation over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> Answer {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let authorization = credential_scope("cloudformation");
    dispatch(
        router,
        &format!("cloudformation.{REGION}.amazonaws.com"),
        "POST",
        "/",
        &[
            ("content-type", "application/x-www-form-urlencoded"),
            ("authorization", &authorization),
        ],
        body.into_bytes(),
    )
    .await
}

/// Read one API Gateway control-plane resource over its restJson1 wire: that service signs as
/// `apigateway` and binds every REST operation under its own URI.
async fn apigateway(router: &MultiServiceRouter, what: &str, path: &str) -> Value {
    let authorization = credential_scope("apigateway");
    let answer = dispatch(
        router,
        &format!("apigateway.{REGION}.amazonaws.com"),
        "GET",
        path,
        &[
            ("content-type", "application/json"),
            ("authorization", &authorization),
        ],
        Vec::new(),
    )
    .await;
    assert_eq!(
        answer.status,
        200,
        "{what} must answer HTTP 200 for the identifier the stack published, got {}",
        answer.summary()
    );
    answer.body
}

/// A required string member of a response body.
fn as_str(what: &str, body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| panic!("{what} must carry a non-empty {member:?}, got {body}"))
}

/// One base64url segment, decoded: "base64url without padding" (RFC 7515 §2), which is
/// RFC 4648 §5 — the two alphabet substitutions and the restored padding turn it back into the
/// standard alphabet this workspace already decodes.
fn decode_segment(segment: &str) -> Vec<u8> {
    let mut standard = segment.replace('-', "+").replace('_', "/");
    while !standard.len().is_multiple_of(4) {
        standard.push('=');
    }
    aws_smithy_types::base64::decode(&standard)
        .unwrap_or_else(|e| panic!("a minted token's segment must be base64url ({e})"))
}

/// One claim of a minted token, as a string: the claim set is the second segment of a compact
/// JWS.
fn claim(token: &str, name: &str) -> String {
    let segments: Vec<&str> = token.split('.').collect();
    assert_eq!(
        segments.len(),
        3,
        "a minted token must be a compact JWS of three segments, got {} segment(s)",
        segments.len()
    );
    let claims: Value = serde_json::from_slice(&decode_segment(segments[1]))
        .unwrap_or_else(|e| panic!("a minted token's claim set must be JSON ({e})"));
    claims
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("a minted token must carry a string {name:?} claim, got {claims}"))
        .to_string()
}

/// What one pool's sign-in produced: the pool's ARN (the value a declaration's `ProviderARNs`
/// lists) and the two tokens of the sign-in.
struct Pool {
    arn: String,
    id_token: String,
    access_token: String,
}

/// Mint one pool, one app client and one confirmed user, and sign that user in.
async fn pool_with_signed_in_user(
    router: &MultiServiceRouter,
    pool_name: &str,
    username: &str,
) -> Pool {
    let pool = cognito(router, "CreateUserPool", &json!({ "PoolName": pool_name })).await;
    let pool_id = as_str("CreateUserPool", &pool["UserPool"], "Id");
    let arn = as_str("CreateUserPool", &pool["UserPool"], "Arn");

    let client = cognito(
        router,
        "CreateUserPoolClient",
        &json!({
            "UserPoolId": pool_id,
            "ClientName": format!("{pool_name}-client"),
            // The flow the sign-in below uses, plus the refresh flow AWS requires beside any
            // other `ALLOW_` flow.
            "ExplicitAuthFlows": ["ALLOW_ADMIN_USER_PASSWORD_AUTH", "ALLOW_REFRESH_TOKEN_AUTH"]
        }),
    )
    .await;
    let client_id = as_str("CreateUserPoolClient", &client["UserPoolClient"], "ClientId");

    cognito(
        router,
        "AdminCreateUser",
        &json!({ "UserPoolId": pool_id, "Username": username }),
    )
    .await;
    cognito(
        router,
        "AdminSetUserPassword",
        &json!({
            "UserPoolId": pool_id,
            "Username": username,
            "Password": GOOD_PASSWORD,
            "Permanent": true
        }),
    )
    .await;
    let signed_in = cognito(
        router,
        "AdminInitiateAuth",
        &json!({
            "UserPoolId": pool_id,
            "ClientId": client_id,
            "AuthFlow": "ADMIN_USER_PASSWORD_AUTH",
            "AuthParameters": { "USERNAME": username, "PASSWORD": GOOD_PASSWORD }
        }),
    )
    .await;
    let result = &signed_in["AuthenticationResult"];
    Pool {
        arn,
        id_token: as_str("AdminInitiateAuth", result, "IdToken"),
        access_token: as_str("AdminInitiateAuth", result, "AccessToken"),
    }
}

/// One `AWS::ApiGateway::Method` declaration: the resource it is on, the verb it answers, the
/// authorizer that decides it (`None` for the open method) and the template its integration
/// answers with.
fn method(resource: &str, verb: &str, gated: bool, template: &str) -> Value {
    let mut properties = json!({
        "RestApiId": { "Ref": "Api" },
        "ResourceId": { "Ref": resource },
        "HttpMethod": verb,
        "AuthorizationType": "NONE",
        "MethodResponses": [{ "StatusCode": "200" }],
        "Integration": {
            "Type": "MOCK",
            "RequestTemplates": { "application/json": REQUEST_TEMPLATE },
            "IntegrationResponses": [{
                "StatusCode": "200",
                "ResponseTemplates": { "application/json": template }
            }]
        }
    });
    if gated {
        let members = properties.as_object_mut().expect("a method declaration");
        members.insert(
            "AuthorizationType".to_string(),
            json!("COGNITO_USER_POOLS"),
        );
        members.insert("AuthorizerId".to_string(), json!({ "Ref": "Auth" }));
    }
    json!({ "Type": "AWS::ApiGateway::Method", "Properties": properties })
}

/// The whole template: a REST API, the `COGNITO_USER_POOLS` authorizer over `pool_arn`, three
/// resources, the three methods on them, the deployment of all of it and the stage that serves
/// it — plus the outputs a client deploying this stack reads the api, its records and its
/// invoke URL from.
fn template(name: &str, pool_arn: &str) -> String {
    let invoke_url = format!("https://${{Api}}.execute-api.${{AWS::Region}}.amazonaws.com/{STAGE}");
    json!({
        "Resources": {
            "Api": {
                "Type": "AWS::ApiGateway::RestApi",
                "Properties": {
                    "Name": name,
                    "Description": "a user-pool gated api a template deploys",
                    "EndpointConfiguration": { "Types": ["REGIONAL"] }
                }
            },
            "Auth": {
                "Type": "AWS::ApiGateway::Authorizer",
                "Properties": {
                    "Name": format!("{name}-pool-gate"),
                    "RestApiId": { "Ref": "Api" },
                    "Type": "COGNITO_USER_POOLS",
                    "IdentitySource": format!("method.request.header.{TOKEN_HEADER}"),
                    "ProviderARNs": [pool_arn]
                }
            },
            "Items": {
                "Type": "AWS::ApiGateway::Resource",
                "Properties": {
                    "RestApiId": { "Ref": "Api" },
                    "ParentId": { "Fn::GetAtt": ["Api", "RootResourceId"] },
                    "PathPart": "items"
                }
            },
            "Detail": {
                "Type": "AWS::ApiGateway::Resource",
                "Properties": {
                    "RestApiId": { "Ref": "Api" },
                    "ParentId": { "Ref": "Items" },
                    "PathPart": "detail"
                }
            },
            "Open": {
                "Type": "AWS::ApiGateway::Resource",
                "Properties": {
                    "RestApiId": { "Ref": "Api" },
                    "ParentId": { "Fn::GetAtt": ["Api", "RootResourceId"] },
                    "PathPart": "open"
                }
            },
            "GatedGet": method("Items", "GET", true, CLAIMS_TEMPLATE),
            "GatedPost": method("Detail", "POST", true, NAMESPACED_CLAIM_TEMPLATE),
            "OpenGet": method("Open", "GET", false, OPEN_TEMPLATE),
            "Deployment": {
                "Type": "AWS::ApiGateway::Deployment",
                "DependsOn": ["GatedGet", "GatedPost", "OpenGet"],
                "Properties": { "RestApiId": { "Ref": "Api" } }
            },
            "Stage": {
                "Type": "AWS::ApiGateway::Stage",
                "Properties": {
                    "RestApiId": { "Ref": "Api" },
                    "DeploymentId": { "Ref": "Deployment" },
                    "StageName": STAGE
                }
            }
        },
        "Outputs": {
            "ApiId": { "Value": { "Ref": "Api" } },
            "AuthorizerId": { "Value": { "Ref": "Auth" } },
            "ItemsId": { "Value": { "Ref": "Items" } },
            "DeploymentId": { "Value": { "Ref": "Deployment" } },
            "InvokeUrl": { "Value": { "Fn::Sub": invoke_url } }
        }
    })
    .to_string()
}

/// What the deployed stage owes one row.
#[derive(Debug, Clone)]
enum Expected {
    /// The gate admitted the request and the `MOCK` integration behind the method answered it
    /// with the claim the method's response template reads: this member, carrying this value.
    Served {
        member: &'static str,
        value: String,
        token_use: String,
    },
    /// The authorizer the template declared does not admit what the request presented, so it
    /// authorized nobody.
    Unauthorized,
    /// The method the request addresses declares no authorizer, so the integration answers it
    /// whatever the request presents.
    Open,
}

/// One request to the stage the template deployed: the path under the published invoke URL,
/// the verb, the body, the value presented at the authorizer's identity source, and what the
/// stage owes it.
struct Row {
    case: String,
    path: &'static str,
    verb: &'static str,
    body: &'static str,
    /// The value the [`TOKEN_HEADER`] carries, or `None` to present nothing at all.
    token: Option<String>,
    expected: Expected,
}

#[test]
fn a_template_deployed_user_pool_gated_stage_serves_its_integration() {
    std::thread::Builder::new()
        .name("cfn-cognito-gated-rest-api".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                // The IO and TIME drivers the services behind a stack need for their own
                // background work, and the poll the stack wait makes.
                .enable_all()
                .build()
                .expect("build a current-thread runtime");
            runtime.block_on(async {
                let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
                let mut router = aws_emulator_router::build_router(engine);
                // Signature verification belongs to the emulator, not to this check.
                router.set_request_authentication(false);

                // 1. The pool the template's authorizer names, and a second pool the template
                //    never names, so its tokens come from an issuer the api does not trust.
                let home =
                    pool_with_signed_in_user(&router, "cfn-gate-pool", "cfn-gate-user").await;
                let foreign =
                    pool_with_signed_in_user(&router, "cfn-gate-other-pool", "other-user").await;

                // 2. The stack: everything the api is, declared in ONE template.
                let stack = "cfn-cognito-gated-rest-api";
                let declared_name = format!("{stack}-api");
                let body = template(&declared_name, &home.arn);
                let created = cloudformation(
                    &router,
                    "CreateStack",
                    &[("StackName", stack), ("TemplateBody", &body)],
                )
                .await;
                assert_eq!(
                    created.status,
                    200,
                    "expected HTTP 200 from CreateStack, got {}",
                    created.summary()
                );

                let (status, described) =
                    wait_for_stack_status(&router, stack, CREATE_COMPLETE).await;
                let failures = if status == CREATE_COMPLETE {
                    Vec::new()
                } else {
                    let events =
                        cloudformation(&router, "DescribeStackEvents", &[("StackName", stack)])
                            .await;
                    elements(&events.text, "ResourceStatusReason")
                };
                assert_eq!(
                    status, CREATE_COMPLETE,
                    "expected the stack to reach {CREATE_COMPLETE}, got {described} with \
                     resource failures {failures:?}"
                );

                let outputs = outputs_of(&described);
                let published = |name: &str| -> String {
                    outputs.get(name).cloned().unwrap_or_else(|| {
                        panic!("the stack published no {name} output in {described}")
                    })
                };
                let api_id = published("ApiId");
                let authorizer_id = published("AuthorizerId");
                let items_id = published("ItemsId");
                let deployment_id = published("DeploymentId");
                let invoke_url = published("InvokeUrl");

                // 3. Every identifier the stack published resolves through API Gateway's OWN
                //    read: the stack's records are not evidence that anything exists, the
                //    owning service's answer is.
                let api = apigateway(&router, "GetRestApi", &format!("/restapis/{api_id}")).await;
                assert_eq!(
                    api.get("name").and_then(Value::as_str),
                    Some(declared_name.as_str()),
                    "expected the api the stack published ({api_id}) to carry the name the \
                     template declared, got {api}"
                );

                let authorizer = apigateway(
                    &router,
                    "GetAuthorizer",
                    &format!("/restapis/{api_id}/authorizers/{authorizer_id}"),
                )
                .await;
                assert_eq!(
                    authorizer.get("type").and_then(Value::as_str),
                    Some("COGNITO_USER_POOLS"),
                    "expected the authorizer the stack published to carry the declared type, \
                     got {authorizer}"
                );
                assert_eq!(
                    authorizer.get("providerARNs"),
                    Some(&json!([home.arn])),
                    "expected the authorizer to list the pool ARN the template declared, got \
                     {authorizer}"
                );

                let gated = apigateway(
                    &router,
                    "GetMethod",
                    &format!("/restapis/{api_id}/resources/{items_id}/methods/GET"),
                )
                .await;
                assert_eq!(
                    gated.get("authorizationType").and_then(Value::as_str),
                    Some("COGNITO_USER_POOLS"),
                    "expected the gated method to carry the declared authorizationType, got \
                     {gated}"
                );
                assert_eq!(
                    gated.get("authorizerId").and_then(Value::as_str),
                    Some(authorizer_id.as_str()),
                    "expected the gated method to name the authorizer the stack published \
                     ({authorizer_id}), got {gated}"
                );

                let stage = apigateway(
                    &router,
                    "GetStage",
                    &format!("/restapis/{api_id}/stages/{STAGE}"),
                )
                .await;
                assert_eq!(
                    stage.get("deploymentId").and_then(Value::as_str),
                    Some(deployment_id.as_str()),
                    "expected the stage to serve the deployment the stack published \
                     ({deployment_id}), got {stage}"
                );

                // 4. The rows, each a request to the stage invoke URL the stack published.
                let sub = claim(&home.id_token, "sub");
                let username = claim(&home.id_token, "cognito:username");
                let served = |member: &'static str, value: &str, token_use: &str| {
                    Expected::Served {
                        member,
                        value: value.to_string(),
                        token_use: token_use.to_string(),
                    }
                };
                let rows: Vec<Row> = vec![
                    Row {
                        case: "the declared pool's identity token, bare".to_string(),
                        path: "/items",
                        verb: "GET",
                        body: "",
                        token: Some(home.id_token.clone()),
                        expected: served("sub", &sub, "id"),
                    },
                    Row {
                        case: "the same token with the RFC 6750 bearer scheme".to_string(),
                        path: "/items",
                        verb: "GET",
                        body: "",
                        token: Some(format!("Bearer {}", home.id_token)),
                        expected: served("sub", &sub, "id"),
                    },
                    Row {
                        case: "the declared pool's ACCESS token of the same sign-in".to_string(),
                        path: "/items",
                        verb: "GET",
                        body: "",
                        token: Some(home.access_token.clone()),
                        expected: served("sub", &sub, "access"),
                    },
                    Row {
                        case: "a token of a pool the template never named".to_string(),
                        path: "/items",
                        verb: "GET",
                        body: "",
                        token: Some(foreign.id_token.clone()),
                        expected: Expected::Unauthorized,
                    },
                    Row {
                        case: "no token at all".to_string(),
                        path: "/items",
                        verb: "GET",
                        body: "",
                        token: None,
                        expected: Expected::Unauthorized,
                    },
                    Row {
                        case: "a value that is not a token at all".to_string(),
                        path: "/items",
                        verb: "GET",
                        body: "",
                        token: Some("not-a-token".to_string()),
                        expected: Expected::Unauthorized,
                    },
                    Row {
                        case: "a gated POST two path segments deep, carrying a body".to_string(),
                        path: "/items/detail",
                        verb: "POST",
                        body: "{\"note\":\"written by the client\"}",
                        token: Some(home.id_token.clone()),
                        expected: served("user", &username, "id"),
                    },
                    Row {
                        case: "a gated POST two segments deep with no token".to_string(),
                        path: "/items/detail",
                        verb: "POST",
                        body: "{\"note\":\"written by the client\"}",
                        token: None,
                        expected: Expected::Unauthorized,
                    },
                    Row {
                        case: "the stage's open method, which no authorizer decides".to_string(),
                        path: "/open",
                        verb: "GET",
                        body: "",
                        token: None,
                        expected: Expected::Open,
                    },
                ];

                for row in &rows {
                    let case = &row.case;
                    let mut headers: Vec<(&str, &str)> =
                        vec![("content-type", "application/json")];
                    if let Some(presented) = &row.token {
                        headers.push((TOKEN_HEADER, presented.as_str()));
                    }
                    let answer = dispatch(
                        &router,
                        &host_of(&invoke_url),
                        row.verb,
                        &format!("{}{}", path_of(&invoke_url), row.path),
                        &headers,
                        row.body.as_bytes().to_vec(),
                    )
                    .await;
                    // A request to a stage a TEMPLATE deployed is answered by the api's own
                    // data plane: never the emulator's generic front door, which resolves no
                    // target service for an invoke host, and never the 500 of an api whose
                    // configuration could not be read.
                    assert_ne!(
                        answer.status,
                        404,
                        "case {case:?}: expected the api's data plane to answer the stage \
                         invoke URL {invoke_url}, got {}",
                        answer.summary()
                    );
                    assert_ne!(
                        answer.member("message"),
                        CONFIGURATION_ERROR,
                        "case {case:?}: expected the stage to decide the call, not report a \
                         configuration error; got {}",
                        answer.summary()
                    );
                    match &row.expected {
                        Expected::Unauthorized => {
                            assert_eq!(
                                (answer.status, answer.member("message")),
                                (UNAUTHORIZED, UNAUTHORIZED_MESSAGE.to_string()),
                                "case {case:?}: expected {UNAUTHORIZED} \
                                 {UNAUTHORIZED_MESSAGE:?}, got {}",
                                answer.summary()
                            );
                        }
                        Expected::Open => {
                            assert_eq!(
                                answer.status,
                                200,
                                "case {case:?}: expected the integration's 200, got {}",
                                answer.summary()
                            );
                            assert_eq!(
                                answer.body.get("open").and_then(Value::as_bool),
                                Some(true),
                                "case {case:?}: expected the open method's own template to \
                                 answer, got {}",
                                answer.summary()
                            );
                        }
                        Expected::Served {
                            member,
                            value,
                            token_use,
                        } => {
                            assert_eq!(
                                answer.status,
                                200,
                                "case {case:?}: expected the integration's 200, got {}",
                                answer.summary()
                            );
                            assert_eq!(
                                answer.body.get(*member).and_then(Value::as_str),
                                Some(value.as_str()),
                                "case {case:?}: expected the token's {member} {value:?} to \
                                 reach the integration, got {}",
                                answer.summary()
                            );
                            assert_eq!(
                                answer.body.get("use").and_then(Value::as_str),
                                Some(token_use.as_str()),
                                "case {case:?}: expected the token's token_use {token_use:?} \
                                 to reach the integration, got {}",
                                answer.summary()
                            );
                        }
                    }
                }
            });
        })
        .expect("spawn the sized thread")
        .join()
        .expect("the suite thread must not panic");
}

/// The host of the invoke URL the stack published, which is the api's own `execute-api`
/// endpoint.
fn host_of(invoke_url: &str) -> String {
    let without_scheme = invoke_url
        .strip_prefix("https://")
        .or_else(|| invoke_url.strip_prefix("http://"))
        .unwrap_or(invoke_url);
    without_scheme
        .split_once('/')
        .map_or(without_scheme, |(host, _path)| host)
        .to_string()
}

/// The path of the invoke URL the stack published, which is the stage the template deployed.
fn path_of(invoke_url: &str) -> String {
    let without_scheme = invoke_url
        .strip_prefix("https://")
        .or_else(|| invoke_url.strip_prefix("http://"))
        .unwrap_or(invoke_url);
    without_scheme
        .split_once('/')
        .map_or(String::new(), |(_host, path)| format!("/{path}"))
}
