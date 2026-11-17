// SPDX-License-Identifier: CC0-1.0

//! A Cognito-backed JWT authorizer in front of an HTTP api's route, driven at the ADDRESS
//! the client dials — the loopback endpoint a client of this emulator is configured with,
//! which is where the reported sequence created its pool, its api and its authorizer.
//!
//! Two services on one engine, so this is a router suite: the tokens are minted by the
//! Amazon Cognito user pools service (signing name `cognito-idp`) and the gate that reads
//! them is API Gateway V2's own `execute-api` data plane (control plane signing name
//! `apigateway`). Nothing about a token is written here that a pool did not mint.
//!
//! The rule the rows assert, one level above the recorded case:
//!
//! > A user pool announces its token issuer at the endpoint it was CREATED through, so the
//! > issuer a relying party composes from the endpoint it dialled and the pool's id — the
//! > documented `https://cognito-idp.{region}.amazonaws.com/{userPoolId}` for a caller that
//! > resolves AWS's own hostnames, and this emulator's own address for a caller that
//! > resolves only it — is the `iss` claim that pool's tokens carry, and the verification
//! > material (the OpenID Connect discovery document and the key set) is published under
//! > it. A route whose `AuthorizationType` is `JWT` therefore admits a request to its
//! > integration exactly when the token at the authorizer's identity source is a signed JWT
//! > whose `iss` is that authorizer's `Issuer` (compared as a base URI, so a trailing `/`
//! > names the same provider) and whose `aud` — or, for an access token, `client_id` —
//! > matches one entry of its `Audience`, inside the token's validity window, WHEREVER the
//! > pool and the api were addressed. Anything else is `401` `{"message":"Unauthorized"}`;
//! > a token the gate accepts whose claimed scopes meet none of the route's
//! > `AuthorizationScopes` is `403` `{"message":"Forbidden"}` ("otherwise, the invocation is
//! > not authorized", `Route$AuthorizationScopes`). A refused request never reaches the
//! > route's integration.
//!
//! The rows span what the rule generalises over: the ADDRESS the pool, the api and the
//! invoke were reached at (a loopback address with a port, a named local endpoint with a
//! port, each service's own published regional host), where the token came from (the api's
//! own pool and client, a second client of the same pool, a second pool), how it is
//! presented (RFC 6750 bearer credentials, bare), whether a claim was changed (`exp`), how
//! the authorizer spells its issuer (with and without a trailing `/`) and what the route
//! requires (no scope, a scope the token does not claim, a list one of whose entries it
//! does).
//!
//! `apigatewayv2_cognito_jwt_authorizer` owns the same gate at the published addressing and
//! varies the token's own form there — a malformed value, an unsecured token, a token
//! absent altogether, an `Audience` of two entries — so the rows here are only the ones
//! whose outcome the ADDRESS can bear on, which is this suite's axis.
//!
//! What an ADMITTED request is answered with is its integration's answer. An `HTTP_PROXY`
//! upstream is a URI the caller can reach, so the caller is sent there (`307`, with the
//! stored `IntegrationUri` in `Location`) — an answer that names the integration that gave
//! it, which is how these rows observe that the gate let the request THROUGH. What an
//! admitted token then HANDS the integration — `requestContext.authorizer.jwt.claims` and
//! `.scopes` — is only observable through a function that ran, so no row here asserts it:
//! that leg is `http_api_jwt_claims_reach_the_integration`'s, in the api gateway v2 crate,
//! where AWS Lambda is mounted beside the data plane.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The region every call is signed for, and so the region of each pool and api.
const REGION: &str = "us-west-2";

/// The permanent password this suite's fixture users sign in with — a value written for
/// this test and used nowhere else. It satisfies a pool's default password policy (a
/// length, an upper and a lower case letter, a digit and a symbol).
const GOOD_PASSWORD: &str = "Str0ng-Passw0rd!";

/// The upstream an `HTTP_PROXY` integration names: a URI the caller is sent to, so the
/// answer to an admitted request names the integration that gave it.
const UPSTREAM: &str = "https://upstream.example.com/guarded";

/// The status and sentence a token the authorizer cannot accept earns.
const UNAUTHORIZED: u16 = 401;
const UNAUTHORIZED_MESSAGE: &str = "Unauthorized";

/// The status and sentence a token the authorizer ACCEPTS earns at a route requiring a
/// scope it does not claim.
const FORBIDDEN: u16 = 403;
const FORBIDDEN_MESSAGE: &str = "Forbidden";

/// The status an admitted request to a route fronting an `HTTP_PROXY` upstream earns.
const TEMPORARY_REDIRECT: u16 = 307;

/// The error code the front door answers when NO service claims a request.
const UNKNOWN_SERVICE: &str = "UnknownService";

/// A scope no Amazon Cognito access token claims, used as the route scope a token cannot
/// meet. The form is a resource server's `{identifier}/{scope}`.
const UNCLAIMED_SCOPE: &str = "things/read";

/// The path a pool publishes its OpenID Connect discovery document under its issuer (RFC
/// 8414 §3), and the one its key set sits at (Amazon Cognito Developer Guide, "Verifying a
/// JSON web token").
const DISCOVERY_PATH: &str = "/.well-known/openid-configuration";
const KEY_SET_PATH: &str = "/.well-known/jwks.json";

/// One address the pool, the api and the invoke of a case are reached at.
struct Addressing {
    case: &'static str,
    /// The authority the user pools service is dialled at, which is also the base of the
    /// issuer a relying party composes.
    cognito_host: &'static str,
    /// The authority the API Gateway V2 control plane is dialled at.
    control_host: &'static str,
}

/// The addresses the rows run at, each named so the test that drives it says which one it
/// is: the two forms of this emulator's own endpoint a client on this machine dials, and
/// each service's own published regional host.
const LOOPBACK_ENDPOINT: Addressing = Addressing {
    case: "a loopback address with a port, which is how this emulator is reached",
    cognito_host: "127.0.0.1:9297",
    control_host: "127.0.0.1:9297",
};
const NAMED_LOCAL_ENDPOINT: Addressing = Addressing {
    case: "a named local endpoint with a port",
    cognito_host: "localhost:4566",
    control_host: "localhost:4566",
};
const PUBLISHED_REGIONAL_HOSTS: Addressing = Addressing {
    case: "each service's own published regional host",
    cognito_host: "cognito-idp.us-west-2.amazonaws.com",
    control_host: "apigateway.us-west-2.amazonaws.com",
};

/// One request dispatched through the router, and what came back.
struct Answer {
    status: u16,
    text: String,
    body: Value,
    error_type: Option<String>,
    location: Option<String>,
}

impl Answer {
    /// The `message` member of a JSON body, which is how this data plane words its own
    /// answers.
    fn message(&self) -> String {
        self.body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    /// One line naming everything an assertion needs about the answer.
    fn summary(&self) -> String {
        format!(
            "{} {} (x-amzn-errortype: {:?}, location: {:?})",
            self.status, self.text, self.error_type, self.location
        )
    }
}

/// This suite's emulator: the two services the rule spans, on ONE engine, with request
/// authentication off (signature verification belongs to the emulator, not to this check).
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice::
            EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_apigatewayv2_apigatewayv2::EmulatedService::new(Arc::clone(&engine)),
    );
    router
}

/// A credential scope naming `signing_name` in [`REGION`], which is what routes a call to
/// that service whatever host it addressed.
fn credential_scope(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 \
         Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
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
    let error_type = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let location = response
        .headers()
        .get("location")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let bytes = response.into_body();
    let text = String::from_utf8_lossy(&bytes).to_string();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    Answer {
        status,
        text,
        body,
        error_type,
        location,
    }
}

/// One `awsJson1_1` call to the Amazon Cognito user pools service at `host`, which must
/// succeed.
async fn cognito(
    router: &MultiServiceRouter,
    host: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let target = format!("AWSCognitoIdentityProviderService.{operation}");
    let authorization = credential_scope("cognito-idp");
    let answer = dispatch(
        router,
        host,
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
        "setup: cognito-idp {operation} at {host} must answer 200, got {}",
        answer.summary()
    );
    answer.body
}

/// One API Gateway V2 control-plane call at `host`, which must answer `expected_status`.
async fn control_plane(
    router: &MultiServiceRouter,
    host: &str,
    what: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
    expected_status: u16,
) -> Value {
    let authorization = credential_scope("apigateway");
    let answer = dispatch(
        router,
        host,
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
        "setup: {what} at {host} must answer HTTP {expected_status}, got {}",
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

/// `value` as one base64url segment of a compact JWS.
fn encode_segment(value: &Value) -> String {
    aws_smithy_types::base64::encode(value.to_string().as_bytes())
        .replace('+', "-")
        .replace('/', "_")
        .trim_end_matches('=')
        .to_string()
}

/// The JOSE header and the claim set of a compact JWS: its first two segments.
fn parts_of(token: &str) -> (Value, Value) {
    let segments: Vec<&str> = token.split('.').collect();
    assert_eq!(
        segments.len(),
        3,
        "a minted token must be a compact JWS of three segments, got {} segment(s)",
        segments.len()
    );
    // The decoder is this test binary's own ([`crate::cognito_group_role_mapping`]), so the
    // base64url reading of a JWS segment is not written twice.
    let read = |segment: &str| -> Value {
        serde_json::from_slice(&crate::cognito_group_role_mapping::decode_base64url(segment))
            .unwrap_or_else(|error| panic!("a minted token's segment must be JSON ({error})"))
    };
    (read(segments[0]), read(segments[1]))
}

/// The token the pool minted, re-encoded with the JOSE header and claim set given and its
/// signature segment kept — so the only difference from the admitted token is what the row
/// changed.
fn rebuilt(token: &str, header: &Value, claims: &Value) -> String {
    let signature = token
        .split('.')
        .next_back()
        .expect("a compact JWS has a signature segment");
    format!(
        "{}.{}.{signature}",
        encode_segment(header),
        encode_segment(claims)
    )
}

/// Mint one pool at `host`, one app client per entry of `client_names` and one confirmed
/// user, and answer the pool id, the client ids and the ACCESS TOKEN each client's sign-in
/// produced.
async fn pool_with_signed_in_user(
    router: &MultiServiceRouter,
    host: &str,
    pool_name: &str,
    client_names: &[&str],
    username: &str,
) -> (String, Vec<String>, Vec<String>) {
    let pool = cognito(
        router,
        host,
        "CreateUserPool",
        &json!({ "PoolName": pool_name }),
    )
    .await;
    let pool_id = as_str("CreateUserPool", &pool["UserPool"], "Id");

    let mut client_ids = Vec::new();
    for client_name in client_names {
        let client = cognito(
            router,
            host,
            "CreateUserPoolClient",
            &json!({
                "UserPoolId": pool_id,
                "ClientName": client_name,
                // The flow the sign-in below uses, plus the refresh flow AWS requires
                // beside any other `ALLOW_` flow.
                "ExplicitAuthFlows": [
                    "ALLOW_ADMIN_USER_PASSWORD_AUTH",
                    "ALLOW_REFRESH_TOKEN_AUTH"
                ]
            }),
        )
        .await;
        client_ids.push(as_str(
            "CreateUserPoolClient",
            &client["UserPoolClient"],
            "ClientId",
        ));
    }

    cognito(
        router,
        host,
        "AdminCreateUser",
        &json!({ "UserPoolId": pool_id, "Username": username }),
    )
    .await;
    cognito(
        router,
        host,
        "AdminSetUserPassword",
        &json!({
            "UserPoolId": pool_id,
            "Username": username,
            "Password": GOOD_PASSWORD,
            "Permanent": true
        }),
    )
    .await;

    let mut access_tokens = Vec::new();
    for client_id in &client_ids {
        let signed_in = cognito(
            router,
            host,
            "AdminInitiateAuth",
            &json!({
                "UserPoolId": pool_id,
                "ClientId": client_id,
                "AuthFlow": "ADMIN_USER_PASSWORD_AUTH",
                "AuthParameters": { "USERNAME": username, "PASSWORD": GOOD_PASSWORD }
            }),
        )
        .await;
        access_tokens.push(as_str(
            "AdminInitiateAuth",
            &signed_in["AuthenticationResult"],
            "AccessToken",
        ));
    }

    (pool_id, client_ids, access_tokens)
}

/// What the gate owes one row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expected {
    /// The gate admitted the request and the route's `HTTP_PROXY` integration answered it,
    /// sending the caller to the upstream it names.
    Redirected,
    /// The authorizer could not accept the presented token at all.
    Unauthorized,
    /// The authorizer accepted the token and the route requires a scope it does not claim.
    Forbidden,
}

/// One request to the deployed stage: the token presented at the authorizer's identity
/// source, the route it is presented to, and what the gate owes it.
struct Row {
    case: String,
    path: &'static str,
    /// The value the `Authorization` header carries.
    presented: String,
    expected: Expected,
}

/// The verification material a relying party bootstraps from `issuer` must be served under
/// it, and the key set must hold the key the minted token's header names: an issuer that
/// leads nowhere is an issuer no verifier can use.
async fn assert_issuer_publishes_its_keys(
    router: &MultiServiceRouter,
    case: &str,
    host: &str,
    issuer: &str,
    token: &str,
) {
    let path = issuer
        .strip_prefix("https://")
        .and_then(|rest| rest.split_once('/'))
        .map(|(_authority, path)| format!("/{path}"))
        .unwrap_or_else(|| panic!("case {case:?}: an issuer carries a path, got {issuer:?}"));

    let discovery = dispatch(
        router,
        host,
        "GET",
        &format!("{path}{DISCOVERY_PATH}"),
        &[],
        Vec::new(),
    )
    .await;
    assert_eq!(
        discovery.status,
        200,
        "case {case:?}: the discovery document under the issuer must be served, got {}",
        discovery.summary()
    );
    assert_eq!(
        discovery.body.get("issuer").and_then(Value::as_str),
        Some(issuer),
        "case {case:?}: the discovery document must name the issuer the authorizer records \
         ({issuer:?}), got {}",
        discovery.text
    );
    let key_set_uri = format!("{issuer}{KEY_SET_PATH}");
    assert_eq!(
        discovery.body.get("jwks_uri").and_then(Value::as_str),
        Some(key_set_uri.as_str()),
        "case {case:?}: the key set must sit under the issuer, got {}",
        discovery.text
    );

    let key_set = dispatch(
        router,
        host,
        "GET",
        &format!("{path}{KEY_SET_PATH}"),
        &[],
        Vec::new(),
    )
    .await;
    assert_eq!(
        key_set.status,
        200,
        "case {case:?}: the key set under the issuer must be served, got {}",
        key_set.summary()
    );
    let (header, _claims) = parts_of(token);
    let kid = header
        .get("kid")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: a minted token names its key, got {header}"));
    let published: Vec<&str> = key_set
        .body
        .get("keys")
        .and_then(Value::as_array)
        .map(|keys| {
            keys.iter()
                .filter_map(|key| key.get("kid").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_default();
    assert!(
        published.contains(&kid),
        "case {case:?}: the key set under the issuer must hold the key the token names \
         ({kid:?}), got {published:?}"
    );
}

/// The whole rule at ONE address: the pool and its app clients, the api and its JWT routes,
/// and the table of requests to the deployed stage.
///
/// One test per address rather than one test walking them all — each address is an
/// independent fixture, so they run in parallel and no single test carries all three.
async fn the_gate_at(addressing: &Addressing) {
    let router = router();
    {
        let case = addressing.case;

        // A pool of ANOTHER issuer: its tokens name a pool no authorizer here records, so
        // they are foreign however they are presented.
        let (_foreign_pool, _foreign_clients, foreign_tokens) = pool_with_signed_in_user(
            &router,
            addressing.cognito_host,
            "jwt-foreign-pool",
            &["foreign-client"],
            "foreign-user",
        )
        .await;
        let foreign_token = foreign_tokens[0].clone();

        // 1. The pool whose tokens the api accepts, with two app clients, created and
        //    signed in at THIS address.
        let (pool_id, client_ids, access_tokens) = pool_with_signed_in_user(
            &router,
            addressing.cognito_host,
            "jwt-gate-pool",
            &["admitted-client", "second-client"],
            "gate-user",
        )
        .await;
        let admitted_token = access_tokens[0].clone();
        let second_client_token = access_tokens[1].clone();

        // The issuer a relying party composes: the endpoint it dialled, with the pool's id
        // as the path — which for a caller that resolves AWS's own hostnames is the
        // documented `https://cognito-idp.{region}.amazonaws.com/{userPoolId}`
        // (`JWTConfiguration$Issuer`), and for a client of this emulator is the address it
        // can actually reach.
        let issuer = format!("https://{}/{pool_id}", addressing.cognito_host);
        let (header, claims) = parts_of(&admitted_token);
        assert_eq!(
            claims.get("iss").and_then(Value::as_str),
            Some(issuer.as_str()),
            "case {case:?}: the pool's access token must name the issuer a client composes \
             from the endpoint it created the pool through, got {claims}"
        );
        // An ACCESS token names its client in `client_id` rather than in `aud`, which is
        // the audience a JWT authorizer of a user pool matches.
        assert_eq!(
            claims.get("client_id").and_then(Value::as_str),
            Some(client_ids[0].as_str()),
            "case {case:?}: the access token must name the client it was minted for, got \
             {claims}"
        );
        // The scope claim is "a JSON string containing a space-separated list of scopes"
        // (RFC 8693 §4.2); a route below requires one of its entries, so the matching scope
        // is the token's own rather than a value written here.
        let claimed_scope = claims
            .get("scope")
            .and_then(Value::as_str)
            .and_then(|scope| scope.split_whitespace().next())
            .unwrap_or_else(|| {
                panic!("case {case:?}: the access token must carry a scope claim, got {claims}")
            })
            .to_string();
        assert_ne!(
            claimed_scope, UNCLAIMED_SCOPE,
            "case {case:?}: the unmatched route scope must be one the token does not claim"
        );

        // The issuer is a URL a verifier FETCHES, so it has to lead back to the material
        // that verifies these tokens.
        assert_issuer_publishes_its_keys(
            &router,
            case,
            addressing.cognito_host,
            &issuer,
            &admitted_token,
        )
        .await;

        // 2. The api, its authorizers, its integration, its routes and the stage that
        //    serves its root — all created at this address too.
        let api = control_plane(
            &router,
            addressing.control_host,
            "CreateApi",
            "POST",
            "/v2/apis",
            Some(json!({ "name": "jwt-gated-api", "protocolType": "HTTP" })),
            201,
        )
        .await;
        let api_id = as_str("CreateApi", &api, "apiId");
        let endpoint = as_str("CreateApi", &api, "apiEndpoint");
        let (invoke_host, invoke_base) = endpoint
            .strip_prefix("https://")
            .map(|rest| match rest.split_once('/') {
                Some((host, path)) => (host.to_string(), format!("/{path}")),
                None => (rest.to_string(), String::new()),
            })
            .unwrap_or_else(|| {
                panic!("case {case:?}: CreateApi must report an https endpoint, got {endpoint:?}")
            });

        // The two authorizers differ in how the issuer is spelled.
        let mut authorizer_ids = Vec::new();
        for (name, configured_issuer, audience) in [
            ("one-client", issuer.clone(), json!([client_ids[0]])),
            // The same provider, written as a base URI ending in `/`: an endpoint URL a
            // client composes from a base that carries a trailing separator.
            (
                "trailing-separator",
                format!("{issuer}/"),
                json!([client_ids[0]]),
            ),
        ] {
            let request = json!({
                "name": name,
                "authorizerType": "JWT",
                "identitySource": ["$request.header.Authorization"],
                "jwtConfiguration": { "issuer": configured_issuer, "audience": audience }
            });
            let created = control_plane(
                &router,
                addressing.control_host,
                &format!("CreateAuthorizer({name})"),
                "POST",
                &format!("/v2/apis/{api_id}/authorizers"),
                Some(request.clone()),
                201,
            )
            .await;
            let authorizer_id = as_str("CreateAuthorizer", &created, "authorizerId");
            // Read back through the implemented reader: what the create answered is what a
            // later reader reports, so the authorizer was persisted rather than echoed.
            let fetched = control_plane(
                &router,
                addressing.control_host,
                &format!("GetAuthorizer({name})"),
                "GET",
                &format!("/v2/apis/{api_id}/authorizers/{authorizer_id}"),
                None,
                200,
            )
            .await;
            assert_eq!(
                fetched, created,
                "case {case:?} / {name:?}: GetAuthorizer must report the created authorizer \
                 unchanged, got {fetched}"
            );
            authorizer_ids.push(authorizer_id);
        }

        let upstream = control_plane(
            &router,
            addressing.control_host,
            "CreateIntegration(HTTP_PROXY)",
            "POST",
            &format!("/v2/apis/{api_id}/integrations"),
            Some(json!({
                "integrationType": "HTTP_PROXY",
                "integrationMethod": "GET",
                "integrationUri": UPSTREAM,
                "payloadFormatVersion": "1.0"
            })),
            201,
        )
        .await;
        let upstream_id = as_str("CreateIntegration", &upstream, "integrationId");

        // Each route names the authorizer and the scopes one group of rows is about.
        let routes: Vec<(&str, &str, Vec<String>)> = vec![
            ("GET /guarded", &authorizer_ids[0], Vec::new()),
            ("GET /trailing", &authorizer_ids[1], Vec::new()),
            (
                "GET /scoped",
                &authorizer_ids[0],
                vec![UNCLAIMED_SCOPE.to_string()],
            ),
            (
                "GET /pool-scope",
                &authorizer_ids[0],
                vec![UNCLAIMED_SCOPE.to_string(), claimed_scope.clone()],
            ),
        ];
        for (route_key, authorizer_id, scopes) in &routes {
            let mut request = json!({
                "routeKey": route_key,
                "target": format!("integrations/{upstream_id}"),
                "authorizationType": "JWT",
                "authorizerId": authorizer_id
            });
            if !scopes.is_empty() {
                request["authorizationScopes"] = json!(scopes);
            }
            control_plane(
                &router,
                addressing.control_host,
                &format!("CreateRoute({route_key})"),
                "POST",
                &format!("/v2/apis/{api_id}/routes"),
                Some(request),
                201,
            )
            .await;
        }

        control_plane(
            &router,
            addressing.control_host,
            "CreateStage",
            "POST",
            &format!("/v2/apis/{api_id}/stages"),
            Some(json!({ "stageName": "$default", "autoDeploy": true })),
            201,
        )
        .await;

        // 3. The table: one request per row to the stage that serves the api's root,
        //    addressed at the endpoint `CreateApi` itself reported.
        let bearer = |token: &str| format!("Bearer {token}");
        // The one claim the expired row changes: the instant the token was ISSUED at, which
        // is in the past, so a token carrying it as its `exp` is one no gate may accept.
        // Absent, the mint shape changed and the row would be asserting nothing.
        let issued_at = claims.get("iat").and_then(Value::as_i64).unwrap_or_else(|| {
            panic!("case {case:?}: the access token must carry an iat claim, got {claims}")
        });
        let mut expired_claims = claims.clone();
        expired_claims["exp"] = json!(issued_at);
        let rows = vec![
            Row {
                case: "the pool's own access token, as RFC 6750 bearer credentials".to_string(),
                path: "/guarded",
                presented: bearer(&admitted_token),
                expected: Expected::Redirected,
            },
            Row {
                case: "the same token presented bare, with no scheme prefix".to_string(),
                path: "/guarded",
                presented: admitted_token.clone(),
                expected: Expected::Redirected,
            },
            Row {
                case: "the same token at the route whose authorizer spells the issuer with a \
                       trailing separator"
                    .to_string(),
                path: "/trailing",
                presented: bearer(&admitted_token),
                expected: Expected::Redirected,
            },
            Row {
                case: "a second client of the same pool, outside the authorizer's one-entry \
                       Audience"
                    .to_string(),
                path: "/guarded",
                presented: bearer(&second_client_token),
                expected: Expected::Unauthorized,
            },
            Row {
                case: "an access token of another user pool: a foreign issuer".to_string(),
                path: "/guarded",
                presented: bearer(&foreign_token),
                expected: Expected::Unauthorized,
            },
            Row {
                case: "the pool's own token with its exp no longer in the future".to_string(),
                path: "/guarded",
                presented: bearer(&rebuilt(&admitted_token, &header, &expired_claims)),
                expected: Expected::Unauthorized,
            },
            Row {
                case: "a route scope the token's scope claim does not carry".to_string(),
                path: "/scoped",
                presented: bearer(&admitted_token),
                expected: Expected::Forbidden,
            },
            Row {
                case: "a route scope list one of whose entries the token claims".to_string(),
                path: "/pool-scope",
                presented: bearer(&admitted_token),
                expected: Expected::Redirected,
            },
        ];

        for row in &rows {
            let full_case = format!("{case} / {}", row.case);
            let headers: Vec<(&str, &str)> = vec![("authorization", row.presented.as_str())];
            let answer = dispatch(
                &router,
                &invoke_host,
                "GET",
                &format!("{invoke_base}{}", row.path),
                &headers,
                Vec::new(),
            )
            .await;
            assert_ne!(
                answer.error_type.as_deref(),
                Some(UNKNOWN_SERVICE),
                "case {full_case:?}: the invoke URL must be served by the api's own data \
                 plane, got {}",
                answer.summary()
            );
            match row.expected {
                Expected::Redirected => {
                    assert_eq!(
                        answer.status,
                        TEMPORARY_REDIRECT,
                        "case {full_case:?}: an admitted request must reach the route's \
                         integration, got {}",
                        answer.summary()
                    );
                    assert_eq!(
                        answer.location.as_deref(),
                        Some(UPSTREAM),
                        "case {full_case:?}: the integration's own upstream must be where the \
                         caller is sent, got {}",
                        answer.summary()
                    );
                }
                Expected::Unauthorized | Expected::Forbidden => {
                    let (status, message) = if row.expected == Expected::Unauthorized {
                        (UNAUTHORIZED, UNAUTHORIZED_MESSAGE)
                    } else {
                        (FORBIDDEN, FORBIDDEN_MESSAGE)
                    };
                    assert_eq!(
                        answer.status,
                        status,
                        "case {full_case:?}: expected HTTP {status}, got {}",
                        answer.summary()
                    );
                    assert_eq!(
                        answer.message(),
                        message,
                        "case {full_case:?}: expected the {message:?} body, got {}",
                        answer.summary()
                    );
                    assert_eq!(
                        answer.location,
                        None,
                        "case {full_case:?}: a refused request must not reach the route's \
                         integration, got {}",
                        answer.summary()
                    );
                }
            }
        }
    }
}

#[tokio::test]
async fn a_jwt_route_admits_a_token_minted_at_a_loopback_endpoint() {
    the_gate_at(&LOOPBACK_ENDPOINT).await;
}

#[tokio::test]
async fn a_jwt_route_admits_a_token_minted_at_a_named_local_endpoint() {
    the_gate_at(&NAMED_LOCAL_ENDPOINT).await;
}

#[tokio::test]
async fn a_jwt_route_admits_a_token_minted_at_the_published_regional_hosts() {
    the_gate_at(&PUBLISHED_REGIONAL_HOSTS).await;
}
