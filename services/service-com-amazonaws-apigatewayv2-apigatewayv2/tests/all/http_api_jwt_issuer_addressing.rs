// SPDX-License-Identifier: CC0-1.0

//! Which NAME of an identity provider a JWT authorizer's `Issuer` may be written as, driven
//! with tokens a user pool really minted and a function that really ran.
//!
//! The rule these rows assert, one level above any single deployment:
//!
//! > A route of a deployed HTTP api whose `AuthorizationType` is `JWT` admits a token whose
//! > `iss` names THE SAME identity provider as its authorizer's recorded `Issuer` — "the base
//! > domain of the identity provider that issues JSON Web Tokens" — and a provider this
//! > emulator itself serves is one provider however the recorded value addresses it, because
//! > such a provider answers at every address this emulator is reached at while minting its
//! > tokens under one `iss`. A token of ANOTHER provider, and a token whose `iss` is an issuer
//! > nothing here publishes, is still `401` `{"message":"Unauthorized"}` with the route's
//! > integration never invoked.
//!
//! Why the rows exist: a relying party composes a user pool's issuer the way
//! `JWTConfiguration$Issuer` documents it — "an Amazon Cognito user pool has the following
//! format: `https://cognito-idp.{region}.amazonaws.com/{userPoolId}`" — or from the endpoint it
//! dialled the pool at, while the pool itself publishes its issuer on the address THIS emulator
//! answers on, so the two names of one pool differ by an authority and nothing else. An
//! authorizer configured from either name has to accept that pool's tokens, or an api fronted
//! by a pool of this emulator can never be invoked at all.
//!
//! The rows span what the rule generalises over: how the authorizer's `Issuer` addresses the
//! provider (the documented AWS form, that form with the trailing separator an issuer URL
//! carries or omits, the emulator address the pool was created through, and the exact `iss` the
//! pool mints under), which pool the presented token was minted for (the authorizer's own pool,
//! a SECOND pool of this same emulator), and an `Issuer` naming a provider this emulator does
//! not serve at all.
//!
//! `http_api_jwt_data_plane` owns the gate's clauses over a configuration whose issuer this
//! emulator serves nothing for, `http_api_jwt_scope_gate` the scope intersection and
//! `http_api_jwt_claims_reach_the_integration` the shape of what an admitted token hands on;
//! this suite owns the ADDRESSING of the provider both sides name.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice as user_pools;

use crate::api_lifecycle::{as_str, call_at_host};
use crate::http_api_backend_dispatch::{emulator_with_lambda, given_function, lambda_integration};
use crate::http_api_jwt_scope_gate::claims_of;
use crate::websocket_handshake_data_plane::{message, send};

/// The Region every request below is signed for: the Region of the pools and of the api.
const REGION: &str = "us-east-1";

/// This service's own control-plane host in [`REGION`].
fn apigateway_host() -> String {
    format!("apigateway.{REGION}.amazonaws.com")
}

/// The user pools service's own published host in [`REGION`] — the base of the issuer a relying
/// party composes the documented way.
fn cognito_host() -> String {
    format!("cognito-idp.{REGION}.amazonaws.com")
}

/// The EMULATOR's own address the pools below are created through: what a client configured
/// with one local endpoint URL sends as the `Host` for every service, since nothing on this
/// machine resolves an AWS hostname. A pool created through it publishes its issuer on an
/// address its tokens' holders can reach, which is the whole reason the two names of one pool
/// can differ.
const EMULATOR_ADDRESS: &str = "127.0.0.1:9297";

/// The `X-Amz-Target` prefix a user pools call carries.
const COGNITO_TARGET_PREFIX: &str = "AWSCognitoIdentityProviderService";

/// The permanent password this suite's fixture users sign in with: a value written for this
/// test and used nowhere else, satisfying a pool's default password policy.
const GOOD_PASSWORD: &str = "Str0ng-Passw0rd!";

/// The identity source the authorizers extract the token from.
const IDENTITY_SOURCE: &str = "$request.header.Authorization";

/// The status and sentence a token no authorizer of the api can accept earns.
const UNAUTHORIZED: u16 = 401;
const UNAUTHORIZED_MESSAGE: &str = "Unauthorized";

/// The handler every served route's integration runs: it reports the path it was invoked at
/// and the two claims the gate decided on, so a served row is asserted on what the FUNCTION
/// was handed rather than on the gateway's own word.
const CLAIM_REPORT: &str = "import json\n\
     def lambda_handler(event, context):\n\
     \x20   claims = event['requestContext']['authorizer']['jwt']['claims']\n\
     \x20   return {'statusCode': 200, 'headers': {'content-type': 'application/json'},\n\
     \x20            'body': json.dumps({'path': event['rawPath'],\n\
     \x20                                'iss': claims['iss'],\n\
     \x20                                'aud': claims['aud']})}\n";

/// One control-plane call against this service.
async fn control(
    emulator: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (u16, Value) {
    call_at_host(emulator, &apigateway_host(), REGION, method, uri, body).await
}

/// One Amazon Cognito user pools call, addressed at `host`, that must succeed; its credential
/// scope is what routes it to that service rather than to this one.
async fn cognito_at(
    emulator: &MultiServiceRouter,
    host: &str,
    operation: &str,
    body: Value,
) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host)
        .header("content-type", "application/x-amz-json-1.1")
        .header(
            "x-amz-target",
            format!("{COGNITO_TARGET_PREFIX}.{operation}"),
        )
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTESTAPIGWV2/20261021/{REGION}/cognito-idp/\
                 aws4_request, SignedHeaders=host, Signature=0000"
            ),
        )
        .body(bytes::Bytes::from(body.to_string()))
        .expect("the user pools request builds");
    let response = emulator.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let parsed: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    assert_eq!(
        status, 200,
        "setup: cognito-idp {operation} must answer 200, got {status} {parsed}"
    );
    parsed
}

/// One user pool created through [`EMULATOR_ADDRESS`], with one app client and a signed-in
/// user: the pool's id, that client's id, and the IDENTITY token the sign-in minted.
struct Pool {
    id: String,
    client_id: String,
    id_token: String,
    /// The `iss` the pool's own tokens carry, read off the minted token rather than composed
    /// here.
    minted_issuer: String,
}

impl Pool {
    /// The issuer a relying party composes the documented way: "an Amazon Cognito user pool has
    /// the following format: https://cognito-idp.{region}.amazonaws.com/{userPoolId}"
    /// (`JWTConfiguration$Issuer`).
    fn documented_issuer(&self) -> String {
        format!("https://{}/{}", cognito_host(), self.id)
    }

    /// The issuer a relying party composes from the ADDRESS it dialled the pool at, which is
    /// the only address a client on this machine has.
    fn addressed_issuer(&self) -> String {
        format!("https://{EMULATOR_ADDRESS}/{}", self.id)
    }
}

/// Mint one pool through the emulator's own address, one app client of it and one user with a
/// permanent password, then sign that user in and answer the pool.
async fn pool_with_signed_in_user(
    emulator: &MultiServiceRouter,
    pool_name: &str,
    username: &str,
) -> Pool {
    let created = cognito_at(
        emulator,
        EMULATOR_ADDRESS,
        "CreateUserPool",
        json!({ "PoolName": pool_name }),
    )
    .await;
    let id = as_str(&created["UserPool"], "Id");

    let client = cognito_at(
        emulator,
        EMULATOR_ADDRESS,
        "CreateUserPoolClient",
        json!({
            "UserPoolId": id,
            "ClientName": format!("{pool_name}-client"),
            // The flow the sign-in below uses, plus the refresh flow AWS requires beside any
            // other `ALLOW_` flow.
            "ExplicitAuthFlows": ["ALLOW_ADMIN_USER_PASSWORD_AUTH", "ALLOW_REFRESH_TOKEN_AUTH"]
        }),
    )
    .await;
    let client_id = as_str(&client["UserPoolClient"], "ClientId");

    cognito_at(
        emulator,
        EMULATOR_ADDRESS,
        "AdminCreateUser",
        json!({ "UserPoolId": id, "Username": username }),
    )
    .await;
    cognito_at(
        emulator,
        EMULATOR_ADDRESS,
        "AdminSetUserPassword",
        json!({
            "UserPoolId": id,
            "Username": username,
            "Password": GOOD_PASSWORD,
            "Permanent": true
        }),
    )
    .await;
    let signed_in = cognito_at(
        emulator,
        EMULATOR_ADDRESS,
        "AdminInitiateAuth",
        json!({
            "UserPoolId": id,
            "ClientId": client_id,
            "AuthFlow": "ADMIN_USER_PASSWORD_AUTH",
            "AuthParameters": { "USERNAME": username, "PASSWORD": GOOD_PASSWORD }
        }),
    )
    .await;
    let id_token = as_str(&signed_in["AuthenticationResult"], "IdToken");

    // The claims the rows are stated about are the POOL's, not values written here: an identity
    // token names its app client in `aud`, and the issuer it names is the one the pool publishes
    // for the address it was created through.
    let claims = claims_of(&id_token);
    assert_eq!(
        claims.get("aud").and_then(Value::as_str),
        Some(client_id.as_str()),
        "the identity token must name its app client in aud, got {claims}"
    );
    let minted_issuer = claims
        .get("iss")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("the identity token must name an issuer, got {claims}"))
        .to_string();
    assert!(
        minted_issuer.ends_with(&format!("/{id}")),
        "a pool's issuer is its own id under the endpoint it publishes, got {minted_issuer:?}"
    );

    Pool {
        id,
        client_id,
        id_token,
        minted_issuer,
    }
}

/// What the gate owes one row.
enum Expected {
    /// The route was served: the integration ran, having been handed a token of this pool's
    /// app client.
    Served {
        issuer: String,
        client_id: String,
    },
    /// The authorizer could not accept the presented token at all.
    Unauthorized,
}

/// One request to the deployed stage: the path within it, the token presented, and what the
/// gate owes it.
struct Row {
    case: &'static str,
    path: &'static str,
    presented: String,
    expected: Expected,
}

#[tokio::test]
async fn a_jwt_route_admits_the_pool_token_however_its_authorizer_addresses_the_pool() {
    // This service, AWS Lambda and Amazon Cognito user pools on ONE engine: the tokens the
    // gate decides are minted by that service, and a served route's answer is the answer of a
    // function AWS Lambda holds and runs.
    let (mut emulator, engine) = emulator_with_lambda();
    emulator.register_service(user_pools::EmulatedService::new(Arc::clone(&engine)));

    // 1. Two pools of this emulator, each created through the emulator's own address, each with
    //    one app client and one signed-in user.
    let home = pool_with_signed_in_user(&emulator, "issuer-addressing-home", "home-user").await;
    let second = pool_with_signed_in_user(&emulator, "issuer-addressing-second", "second-user").await;

    // The condition every row below is about: the pool mints its tokens under the issuer it
    // publishes for the address it was created through, which is NOT the documented AWS form a
    // relying party composes — so an authorizer configured from either name has to accept them.
    assert_ne!(
        home.minted_issuer,
        home.documented_issuer(),
        "a pool created through an address of this emulator publishes its issuer there"
    );

    // 2. The api: one authorizer per spelling of the provider's name, each on its own route.
    let function = given_function(
        &emulator,
        &engine,
        "the issuer-addressing routes",
        "jwt-issuer-addressing",
        CLAIM_REPORT,
    )
    .await;
    let authorizers: Vec<(&str, String, String)> = vec![
        (
            "/documented",
            home.documented_issuer(),
            home.client_id.clone(),
        ),
        (
            "/trailing-separator",
            format!("{}/", home.documented_issuer()),
            home.client_id.clone(),
        ),
        (
            "/addressed",
            home.addressed_issuer(),
            home.client_id.clone(),
        ),
        (
            "/minted",
            home.minted_issuer.clone(),
            home.client_id.clone(),
        ),
        (
            "/second-pool",
            second.documented_issuer(),
            second.client_id.clone(),
        ),
        (
            "/unserved",
            // A provider this emulator serves nothing for: its path segment names no pool
            // here, so the recorded value is the only name the gate has for it.
            "https://accounts.unserved.example/not-a-pool-of-this-emulator".to_string(),
            home.client_id.clone(),
        ),
    ];
    let host = deploy(&emulator, &function, &authorizers).await;

    // 3. The table.
    let rows = vec![
        Row {
            case: "the documented AWS form of the pool's issuer",
            path: "/documented",
            presented: format!("Bearer {}", home.id_token),
            expected: Expected::Served {
                issuer: home.minted_issuer.clone(),
                client_id: home.client_id.clone(),
            },
        },
        Row {
            case: "that same form written with a trailing separator",
            path: "/trailing-separator",
            presented: format!("Bearer {}", home.id_token),
            expected: Expected::Served {
                issuer: home.minted_issuer.clone(),
                client_id: home.client_id.clone(),
            },
        },
        Row {
            case: "the emulator address the pool was created through",
            path: "/addressed",
            presented: format!("Bearer {}", home.id_token),
            expected: Expected::Served {
                issuer: home.minted_issuer.clone(),
                client_id: home.client_id.clone(),
            },
        },
        Row {
            case: "the exact issuer the pool mints its tokens under",
            path: "/minted",
            presented: format!("Bearer {}", home.id_token),
            expected: Expected::Served {
                issuer: home.minted_issuer.clone(),
                client_id: home.client_id.clone(),
            },
        },
        Row {
            case: "the second pool's own token at the route its own authorizer gates",
            path: "/second-pool",
            presented: format!("Bearer {}", second.id_token),
            expected: Expected::Served {
                issuer: second.minted_issuer.clone(),
                client_id: second.client_id.clone(),
            },
        },
        Row {
            case: "a token of ANOTHER pool of this emulator, presented bare",
            path: "/second-pool",
            presented: home.id_token.clone(),
            expected: Expected::Unauthorized,
        },
        Row {
            case: "a pool token at an authorizer naming a provider this emulator does not serve",
            path: "/unserved",
            presented: format!("Bearer {}", home.id_token),
            expected: Expected::Unauthorized,
        },
        Row {
            case: "a value that is no token at all",
            path: "/documented",
            presented: "Bearer not-a-token".to_string(),
            expected: Expected::Unauthorized,
        },
    ];

    for row in &rows {
        let case = row.case;
        let answer = send(
            &emulator,
            &host,
            "GET",
            row.path,
            &[("authorization", row.presented.as_str())],
        )
        .await;
        match &row.expected {
            Expected::Served { issuer, client_id } => {
                assert_eq!(
                    answer.status,
                    200,
                    "case {case:?} at {}: an admitted request must be answered by the route's \
                     own function, got {}",
                    row.path,
                    answer.summary()
                );
                let reported: Value = serde_json::from_str(&answer.text).unwrap_or(Value::Null);
                assert_eq!(
                    reported.get("path").and_then(Value::as_str),
                    Some(row.path),
                    "case {case:?}: the function must have been invoked for {}, got {}",
                    row.path,
                    answer.text
                );
                assert_eq!(
                    reported.get("iss").and_then(Value::as_str),
                    Some(issuer.as_str()),
                    "case {case:?}: the event must carry the admitted token's own issuer \
                     {issuer:?}, got {}",
                    answer.text
                );
                assert_eq!(
                    reported.get("aud").and_then(Value::as_str),
                    Some(client_id.as_str()),
                    "case {case:?}: the event must carry the admitted token's own app client \
                     {client_id:?}, got {}",
                    answer.text
                );
            }
            Expected::Unauthorized => {
                assert_eq!(
                    (answer.status, message(&answer).as_str()),
                    (UNAUTHORIZED, UNAUTHORIZED_MESSAGE),
                    "case {case:?} at {}: expected ({UNAUTHORIZED}, {UNAUTHORIZED_MESSAGE:?}), \
                     got {}",
                    row.path,
                    answer.summary()
                );
            }
        }
    }
}

/// The api this suite deploys: one JWT authorizer per `(path, issuer, audience)` entry, a
/// Lambda proxy integration on the reporting handler, one route per entry, and the stage that
/// serves the api's root. Answers the host its `ApiEndpoint` names.
async fn deploy(
    emulator: &MultiServiceRouter,
    function_arn: &str,
    authorizers: &[(&str, String, String)],
) -> String {
    let (status, api) = control(
        emulator,
        "POST",
        "/v2/apis",
        Some(json!({ "name": "jwt-issuer-addressing-api", "protocolType": "HTTP" })),
    )
    .await;
    assert_eq!(
        status, 201,
        "setup: CreateApi must answer 201, got {status} {api}"
    );
    let api_id = as_str(&api, "apiId");
    let host = as_str(&api, "apiEndpoint")
        .split_once("://")
        .map(|(_scheme, rest)| rest.to_string())
        .unwrap_or_default();

    let (status, integration) = control(
        emulator,
        "POST",
        &format!("/v2/apis/{api_id}/integrations"),
        Some(lambda_integration(function_arn, "2.0")),
    )
    .await;
    assert_eq!(
        status, 201,
        "setup: CreateIntegration must answer 201, got {status} {integration}"
    );
    let integration_id = as_str(&integration, "integrationId");

    for (path, issuer, audience) in authorizers {
        let (status, authorizer) = control(
            emulator,
            "POST",
            &format!("/v2/apis/{api_id}/authorizers"),
            Some(json!({
                "name": format!("authorizer{}", path.replace('/', "-")),
                "authorizerType": "JWT",
                "identitySource": [IDENTITY_SOURCE],
                "jwtConfiguration": { "issuer": issuer, "audience": [audience] }
            })),
        )
        .await;
        assert_eq!(
            status, 201,
            "setup: CreateAuthorizer({issuer}) must answer 201, got {status} {authorizer}"
        );
        let authorizer_id = as_str(&authorizer, "authorizerId");
        // The configuration the gate reads is the STORED one: what the create answered is what
        // a later reader reports.
        let (status, read) = control(
            emulator,
            "GET",
            &format!("/v2/apis/{api_id}/authorizers/{authorizer_id}"),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "setup: GetAuthorizer must answer 200, got {status} {read}"
        );
        assert_eq!(
            read["jwtConfiguration"]["issuer"].as_str(),
            Some(issuer.as_str()),
            "setup: GetAuthorizer must report the issuer the create stored, got {read}"
        );

        let (status, route) = control(
            emulator,
            "POST",
            &format!("/v2/apis/{api_id}/routes"),
            Some(json!({
                "routeKey": format!("GET {path}"),
                "target": format!("integrations/{integration_id}"),
                "authorizationType": "JWT",
                "authorizerId": authorizer_id
            })),
        )
        .await;
        assert_eq!(
            status, 201,
            "setup: CreateRoute(GET {path}) must answer 201, got {status} {route}"
        );
    }

    let (status, stage) = control(
        emulator,
        "POST",
        &format!("/v2/apis/{api_id}/stages"),
        Some(json!({ "stageName": "$default", "autoDeploy": true })),
    )
    .await;
    assert_eq!(
        status, 201,
        "setup: CreateStage must answer 201, got {status} {stage}"
    );
    host
}
