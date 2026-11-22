// SPDX-License-Identifier: CC0-1.0

//! What a `COGNITO_USER_POOLS`-gated REST method publishes to its integration about the
//! signed-in user, driven end to end through the WHOLE assembled router.
//!
//! Two services and one engine, so this is a router suite: the identity token is minted by the
//! Amazon Cognito user pools service (signing name `cognito-idp`) and the gate that reads it is
//! API Gateway's own `execute-api` data plane (control plane signing name `apigateway`).
//!
//! The RULE the rows assert:
//!
//! > An identity token's claim set is projected from the AUTHENTICATING USER's own stored
//! > attribute set — every stored standard attribute and every developer-defined `custom:`
//! > attribute is a claim of the same name, an attribute the user does not hold is no claim at
//! > all — and a `COGNITO_USER_POOLS` authorizer publishes the verified token's claims verbatim,
//! > so each one is readable by the integration as `$context.authorizer.claims.<name>` with its
//! > value rendered as text.
//!
//! An identity token "contains claims about the identity of the authenticated user, such as
//! `name`, `family_name`, and `phone_number`" (`AuthenticationResultType$IdToken` in the user
//! pools model), and a `custom:`-prefixed attribute is a first-class stored attribute of that
//! set: `AttributeType$Name` is documented with `email` and `custom:department` as its example
//! values, and `AttributeType$Value` is text whatever data type the pool's schema declares for
//! it. So a `Number` custom attribute travels as the text it is stored as, while the two
//! verification FLAGS a pool declares as `Boolean` travel as the boolean each states — and a
//! mapping template, which reads text, renders either as `true`/`false`.
//!
//! The rows span what that rule generalises over: whether the attribute is standard or
//! developer-defined, the data type its schema declares (`String`, `Number`, the two `Boolean`
//! flags), the state a flag is in (confirmed, unconfirmed), what its value holds (plain text,
//! unicode and punctuation, a number) and WHICH user signed in — a second user of the same pool
//! holding a different, smaller attribute set, so the claim set follows the user and not the
//! pool's schema.
//!
//! Each row is asserted at BOTH layers it has to hold at: the claim the pool put in the token,
//! and the value the gated method's integration reads it back as. The claim NAMES are the
//! requirement; the values below are this suite's own fixtures.
//!
//! Assembling the router costs stack, so it runs on an explicitly sized thread for the same
//! reason the sibling Cognito suites do.

use std::sync::Arc;

use aws_emulator_router::{build_router, InMemoryEngine};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cognito_group_role_mapping::{claims_of, BOOT_STACK_SIZE, GOOD_PASSWORD};

/// The region every call is signed for, and so the region of the pool's issuer and of the api's
/// own invoke host.
const REGION: &str = "us-west-2";

/// The stage the api is deployed to.
const STAGE: &str = "prod";

/// The header the authorizer's identity source names.
const TOKEN_HEADER: &str = "Authorization";

/// The pool, its app client and the two users whose attribute sets the rows read.
const POOL_NAME: &str = "attribute-claims-pool";
const CLIENT_NAME: &str = "attribute-claims-client";
/// The user holding every attribute the rows name.
const FULL_USER: &str = "attributed-user";
/// A second user of the SAME pool holding only one of them, which is how a row observes that
/// the claim set follows the user's stored attributes and not the pool's schema.
const SPARSE_USER: &str = "sparse-user";

/// One stored attribute, and the two things that must then hold of it.
struct Row<'a> {
    case: &'a str,
    /// The schema entry the pool declares for a developer-defined attribute; `None` for a
    /// standard one, which every pool declares already.
    declared: Option<Value>,
    /// The attribute name, which is also the claim name: for a custom attribute that is
    /// already its `custom:` name.
    name: &'a str,
    /// The value as the request carries it — always text, as `AttributeType$Value` is.
    value: &'a str,
    /// The claim the identity token must carry under [`Self::name`].
    claim: Value,
    /// What the gated method's integration must read that claim back as.
    published: &'a str,
}

/// One request dispatched through the router, and what came back.
struct Answer {
    status: u16,
    body: Value,
    text: String,
}

impl Answer {
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

/// The credential scope a signed SDK request carries. The signature is never checked — this
/// router runs with authentication off.
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

/// One API Gateway REST control-plane call, which must answer `expected_status`.
async fn control_plane(
    router: &MultiServiceRouter,
    what: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
    expected_status: u16,
) -> Value {
    let authorization = credential_scope("apigateway");
    let answer = dispatch(
        router,
        &format!("apigateway.{REGION}.amazonaws.com"),
        method,
        path,
        &[
            ("content-type", "application/json"),
            ("authorization", &authorization),
        ],
        body.map(|body| body.to_string().into_bytes())
            .unwrap_or_default(),
    )
    .await;
    assert_eq!(
        answer.status,
        expected_status,
        "{what} must answer HTTP {expected_status}, got {}",
        answer.summary()
    );
    answer.body
}

/// A required, non-empty string member of a response body.
fn as_str(what: &str, body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| panic!("{what} must carry a non-empty {member:?}, got {body}"))
}

/// Create `username` holding `attributes`, give it a permanent password, sign it in and answer
/// the identity token the sign-in produced.
async fn signed_in_id_token(
    router: &MultiServiceRouter,
    user_pool_id: &str,
    client_id: &str,
    username: &str,
    attributes: Vec<Value>,
) -> String {
    cognito(
        router,
        "AdminCreateUser",
        &json!({
            "UserPoolId": user_pool_id,
            "Username": username,
            "MessageAction": "SUPPRESS",
            "UserAttributes": attributes
        }),
    )
    .await;
    cognito(
        router,
        "AdminSetUserPassword",
        &json!({
            "UserPoolId": user_pool_id,
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
            "UserPoolId": user_pool_id,
            "ClientId": client_id,
            "AuthFlow": "ADMIN_USER_PASSWORD_AUTH",
            "AuthParameters": { "USERNAME": username, "PASSWORD": GOOD_PASSWORD }
        }),
    )
    .await;
    as_str(
        "AdminInitiateAuth",
        &signed_in["AuthenticationResult"],
        "IdToken",
    )
}

/// The response template the gated method answers with: one JSON member per claim the rows
/// name, read through the BRACKETED member access — the only spelling that reaches a claim whose
/// name is no bare identifier, which every `custom:` claim's is.
fn claims_template(names: &[&str]) -> String {
    let members: Vec<String> = names
        .iter()
        .map(|name| format!("\"{name}\":\"$context.authorizer.claims['{name}']\""))
        .collect();
    format!("{{{}}}", members.join(","))
}

/// GET the gated method presenting `token`, which a token the pool minted reaches the
/// integration through, and answer what that integration served.
async fn gated_claims(router: &MultiServiceRouter, invoke_host: &str, token: &str) -> Answer {
    let answer = dispatch(
        router,
        invoke_host,
        "GET",
        &format!("/{STAGE}/secure"),
        &[(TOKEN_HEADER, token)],
        Vec::new(),
    )
    .await;
    assert_eq!(
        answer.status,
        200,
        "a token the pool minted must reach the integration, got {}",
        answer.summary()
    );
    answer
}

#[test]
fn a_gated_methods_claims_are_the_signed_in_users_stored_attributes() {
    std::thread::Builder::new()
        .name("apigateway-cognito-attribute-claims".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                // The IO and TIME drivers a service's background work needs, as the sibling
                // router suites build it.
                .enable_all()
                .build()
                .expect("build a current-thread runtime");
            runtime.block_on(async {
                let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
                let mut router = build_router(engine);
                // Signature verification belongs to the emulator, not to this check.
                router.set_request_authentication(false);

                let rows: &[Row<'_>] = &[
                    Row {
                        case: "a standard attribute whose value is text",
                        declared: None,
                        name: "email",
                        value: "attributed@example.test",
                        claim: json!("attributed@example.test"),
                        published: "attributed@example.test",
                    },
                    Row {
                        case: "the email verification flag, confirmed",
                        declared: None,
                        name: "email_verified",
                        value: "true",
                        claim: json!(true),
                        published: "true",
                    },
                    Row {
                        case: "the phone verification flag, unconfirmed",
                        declared: None,
                        name: "phone_number_verified",
                        value: "false",
                        claim: json!(false),
                        published: "false",
                    },
                    Row {
                        case: "another standard attribute, under its own name",
                        declared: None,
                        name: "given_name",
                        value: "Ada",
                        claim: json!("Ada"),
                        published: "Ada",
                    },
                    Row {
                        case: "a developer-defined String attribute",
                        declared: Some(json!({
                            "Name": "tier",
                            "AttributeDataType": "String",
                            "Mutable": true
                        })),
                        name: "custom:tier",
                        value: "platinum",
                        claim: json!("platinum"),
                        published: "platinum",
                    },
                    Row {
                        case: "a developer-defined Number attribute, which is still text",
                        declared: Some(json!({
                            "Name": "level",
                            "AttributeDataType": "Number",
                            "Mutable": true
                        })),
                        name: "custom:level",
                        value: "7",
                        claim: json!("7"),
                        published: "7",
                    },
                    Row {
                        case: "a developer-defined attribute carrying unicode and punctuation",
                        declared: Some(json!({
                            "Name": "team",
                            "AttributeDataType": "String",
                            "Mutable": true
                        })),
                        name: "custom:team",
                        value: "équipe/platform-1",
                        claim: json!("équipe/platform-1"),
                        published: "équipe/platform-1",
                    },
                ];

                // 1. One pool declaring every developer-defined attribute the rows name, one app
                //    client, and the two users.
                let schema: Vec<Value> =
                    rows.iter().filter_map(|row| row.declared.clone()).collect();
                let pool = cognito(
                    &router,
                    "CreateUserPool",
                    &json!({ "PoolName": POOL_NAME, "Schema": schema }),
                )
                .await;
                let pool_id = as_str("CreateUserPool", &pool["UserPool"], "Id");
                let pool_arn = as_str("CreateUserPool", &pool["UserPool"], "Arn");
                let client = cognito(
                    &router,
                    "CreateUserPoolClient",
                    &json!({
                        "UserPoolId": &pool_id,
                        "ClientName": CLIENT_NAME,
                        "GenerateSecret": false,
                        // The flow the sign-ins below use, plus the refresh flow AWS requires
                        // beside any other `ALLOW_` flow.
                        "ExplicitAuthFlows": [
                            "ALLOW_ADMIN_USER_PASSWORD_AUTH",
                            "ALLOW_REFRESH_TOKEN_AUTH"
                        ]
                    }),
                )
                .await;
                let client_id = as_str("CreateUserPoolClient", &client["UserPoolClient"], "ClientId");

                let full_attributes: Vec<Value> = rows
                    .iter()
                    .map(|row| json!({ "Name": row.name, "Value": row.value }))
                    .collect();
                let full_token = signed_in_id_token(
                    &router,
                    &pool_id,
                    &client_id,
                    FULL_USER,
                    full_attributes,
                )
                .await;
                // The second user of the same pool holds ONE attribute of the set, in the other
                // state of the flag.
                let sparse_token = signed_in_id_token(
                    &router,
                    &pool_id,
                    &client_id,
                    SPARSE_USER,
                    vec![json!({ "Name": "email_verified", "Value": "false" })],
                )
                .await;

                // 2. The api: one method gated by a `COGNITO_USER_POOLS` authorizer over that
                //    pool, answering the claims the authorizer published.
                let api = control_plane(
                    &router,
                    "CreateRestApi",
                    "POST",
                    "/restapis",
                    Some(json!({ "name": "attribute-claims-api" })),
                    201,
                )
                .await;
                let api_id = as_str("CreateRestApi", &api, "id");
                let root = as_str("CreateRestApi", &api, "rootResourceId");

                let authorizer = control_plane(
                    &router,
                    "CreateAuthorizer",
                    "POST",
                    &format!("/restapis/{api_id}/authorizers"),
                    Some(json!({
                        "name": "attribute-claims-authorizer",
                        "type": "COGNITO_USER_POOLS",
                        "providerARNs": [pool_arn],
                        "identitySource": format!("method.request.header.{TOKEN_HEADER}")
                    })),
                    201,
                )
                .await;
                let authorizer_id = as_str("CreateAuthorizer", &authorizer, "id");

                let resource = control_plane(
                    &router,
                    "CreateResource",
                    "POST",
                    &format!("/restapis/{api_id}/resources/{root}"),
                    Some(json!({ "pathPart": "secure" })),
                    201,
                )
                .await;
                let resource_id = as_str("CreateResource", &resource, "id");
                control_plane(
                    &router,
                    "PutMethod",
                    "PUT",
                    &format!("/restapis/{api_id}/resources/{resource_id}/methods/GET"),
                    Some(json!({
                        "authorizationType": "COGNITO_USER_POOLS",
                        "authorizerId": authorizer_id
                    })),
                    201,
                )
                .await;
                control_plane(
                    &router,
                    "PutIntegration",
                    "PUT",
                    &format!(
                        "/restapis/{api_id}/resources/{resource_id}/methods/GET/integration"
                    ),
                    Some(json!({ "type": "MOCK" })),
                    201,
                )
                .await;
                control_plane(
                    &router,
                    "PutMethodResponse",
                    "PUT",
                    &format!(
                        "/restapis/{api_id}/resources/{resource_id}/methods/GET/responses/200"
                    ),
                    Some(json!({})),
                    201,
                )
                .await;
                let names: Vec<&str> = rows.iter().map(|row| row.name).collect();
                control_plane(
                    &router,
                    "PutIntegrationResponse",
                    "PUT",
                    &format!(
                        "/restapis/{api_id}/resources/{resource_id}/methods/GET/integration/\
                         responses/200"
                    ),
                    Some(json!({
                        "responseTemplates": {
                            "application/json": claims_template(&names)
                        }
                    })),
                    201,
                )
                .await;
                control_plane(
                    &router,
                    "CreateDeployment",
                    "POST",
                    &format!("/restapis/{api_id}/deployments"),
                    Some(json!({ "stageName": STAGE })),
                    201,
                )
                .await;

                // 3. The claims the pool put in the token, and the claims the gated method
                //    publishes to its integration.
                let invoke_host = format!("{api_id}.execute-api.{REGION}.amazonaws.com");

                let full_claims = claims_of(&full_token);
                let full_published = gated_claims(&router, &invoke_host, &full_token).await;
                for row in rows {
                    let case = row.case;
                    let minted = full_claims.get(row.name).cloned().unwrap_or(Value::Null);
                    assert_eq!(
                        minted, row.claim,
                        "case {case:?}: the identity token must claim {:?} as {}, got {minted}",
                        row.name, row.claim
                    );
                    let published = full_published.body.get(row.name).and_then(Value::as_str);
                    assert_eq!(
                        published,
                        Some(row.published),
                        "case {case:?}: the gated method must publish the claim {:?} as {:?}, \
                         got {:?} in {}",
                        row.name,
                        row.published,
                        published,
                        full_published.summary()
                    );
                }

                // An attribute the user does NOT hold is no claim at all: the pool's schema
                // declares it and the second user stores none of it, so its claim is absent
                // from that user's token and only the flag it does hold is published.
                let sparse_claims = claims_of(&sparse_token);
                let sparse_published = gated_claims(&router, &invoke_host, &sparse_token).await;
                for row in rows {
                    let case = row.case;
                    if row.name == "email_verified" {
                        assert_eq!(
                            sparse_claims.get(row.name).cloned(),
                            Some(json!(false)),
                            "case {case:?}: the second user's flag states what it stores, got \
                             {sparse_claims}"
                        );
                        let published = sparse_published.body.get(row.name).and_then(Value::as_str);
                        assert_eq!(
                            published,
                            Some("false"),
                            "case {case:?}: the second user's flag must be published as \
                             \"false\", got {:?} in {}",
                            published,
                            sparse_published.summary()
                        );
                        continue;
                    }
                    let held = sparse_claims.get(row.name);
                    assert!(
                        held.is_none(),
                        "case {case:?}: an attribute the signed-in user does not hold is no \
                         claim of its token, got {held:?} in {sparse_claims}"
                    );
                }
            });
        })
        .expect("spawn the sized thread")
        .join()
        .expect("the suite thread must not panic");
}
