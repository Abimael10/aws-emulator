// SPDX-License-Identifier: CC0-1.0

//! Trusted identity propagation end to end over the WHOLE assembled router: an Amazon
//! Cognito user pool registered as an IAM Identity Center trusted token issuer, and the
//! `sso-oidc` JWT-bearer exchange that turns one of its ID tokens into an identity-bound
//! Identity Center token.
//!
//! Four services and one engine, so this is a router suite: the assertion is minted by the
//! user pools service (`cognito-idp`), the application, its grant and the trusted token
//! issuer are IAM Identity Center's (`sso`), the user the assertion resolves to is the
//! identity store's (`identitystore`), and the exchange is `sso-oidc`'s
//! (`CreateTokenWithIAM`, `POST /token?aws_iam=t`). Nothing about an assertion is written
//! here that a pool did not mint: every admitted leg presents the ID token
//! `AdminInitiateAuth` answered with, and the refused legs present another pool's token,
//! that same token re-encoded with a claim changed, a value that is not a token, or the
//! same token twice.
//!
//! The rule the rows assert, one level above the recorded case:
//!
//! > `CreateTokenWithIAM` with `grantType`
//! > `urn:ietf:params:oauth:grant-type:jwt-bearer` and a `clientId` naming an Identity
//! > Center application vends an identity-bound token when the `assertion` is a JWT inside
//! > its own validity window, signed by a key the issuer its `iss` names publishes, where
//! > that issuer is a trusted token issuer of the application's instance which the
//! > application's JWT-bearer grant authorizes for the audience the assertion carries, and
//! > the claim at that issuer's `ClaimAttributePath` equals the attribute at its
//! > `IdentityStoreAttributePath` of a user of the application's identity store. The token
//! > carries `accessToken`, `tokenType` `Bearer`, `expiresIn`, the granted `scope`,
//! > `issuedTokenType` `urn:ietf:params:oauth:token-type:access_token`, an `idToken` whose
//! > `sub` is that user's `UserId`, and `awsAdditionalDetails.identityContext` — the same
//! > value the identity token carries as its `sts:identity_context` claim. An assertion
//! > that verifies and resolves no such user is refused `InvalidGrantException` naming THAT
//! > case, which RFC 7523 §3.1 is the invalid grant of a JWT identifying no resource owner;
//! > an assertion this exchange may not spend at all — not a token, from an issuer the
//! > grant does not authorize, re-encoded, or already spent — is `InvalidGrantException`
//! > naming the assertion, so the two outcomes are told apart by their class text; an
//! > application that configures no JWT-bearer grant or no IAM authentication method is
//! > `UnauthorizedClientException`; a `clientId` naming no application is
//! > `InvalidClientException`.
//!
//! The rows span what that rule generalises over: which claim of the assertion names the
//! identity (`email`, `sub`), which attribute of the stored user it is matched against
//! (`emails.value`, `userName`), and, for the refusals, what is wrong and which record it
//! is wrong about.
//!
//! Assembling the router costs stack, so it runs on an explicitly sized thread for the
//! same reason `boot_serve.rs` does.

use std::sync::Arc;

use aws_emulator_router::{build_router, InMemoryEngine};
use serde_json::{json, Value};
use service_com_amazonaws_ssooidc_awsssooidcservice as ssooidc;
use smithy_server_runtime::MultiServiceRouter;

/// Keep this at or above the size `boot_serve.rs` uses.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The region every call is signed for, and so the region of the pool's issuer and of the
/// Identity Center instance the exchange is made against.
const REGION: &str = "us-west-2";

/// The permanent password this suite's fixture users sign in with — a value written for
/// this test and used nowhere else. It satisfies a pool's default password policy.
const GOOD_PASSWORD: &str = "Str0ng-Passw0rd!";

/// The route `CreateTokenWithIAM` is bound to: `CreateToken`'s path with the `aws_iam=t`
/// query literal the model's `@http` URI carries.
const IAM_TOKEN_PATH: &str = "/token?aws_iam=t";

/// `GrantType` of the JWT-bearer grant (`CreateTokenWithIAMRequest$grantType`).
const JWT_BEARER: &str = "urn:ietf:params:oauth:grant-type:jwt-bearer";

/// The `TokenTypeURI` of an access token (`CreateTokenWithIAMRequest$requestedTokenType`).
const ACCESS_TOKEN_TYPE: &str = "urn:ietf:params:oauth:token-type:access_token";

/// The claim an identity token carries its trusted context assertion in
/// (`AwsAdditionalDetails$identityContext`).
const IDENTITY_CONTEXT_CLAIM: &str = "sts:identity_context";

/// The scopes an exchange naming no `scope` is authorized for
/// (`CreateTokenWithIAMRequest$scope`).
const DEFAULT_SCOPES: &[&str] = &["openid", "aws", IDENTITY_CONTEXT_CLAIM];

/// The OAuth 2.0 application provider a customer managed application is created under
/// (`ListApplicationProviders`' `custom` entry).
const CUSTOM_PROVIDER_ARN: &str = "arn:aws:sso::aws:applicationProvider/custom";

/// The only `TrustedTokenIssuerType` the model admits, and the only `JwksRetrievalOption`.
const OIDC_JWT: &str = "OIDC_JWT";
const OPEN_ID_DISCOVERY: &str = "OPEN_ID_DISCOVERY";

/// The only `AuthenticationMethodType` the model admits.
const IAM_METHOD: &str = "IAM";

/// The `error_description` of each refusal class, read from the service so a row states
/// the class and not a second copy of its text
/// (`service_com_amazonaws_ssooidc_awsssooidcservice::state`).
const INVALID_ASSERTION: &str = ssooidc::state::DESCRIPTION_INVALID_ASSERTION;
const UNRESOLVED_IDENTITY: &str = ssooidc::state::DESCRIPTION_UNRESOLVED_IDENTITY;
const UNAUTHORIZED_CLIENT: &str = ssooidc::state::DESCRIPTION_UNAUTHORIZED_CLIENT;
const INVALID_CLIENT: &str = ssooidc::state::DESCRIPTION_INVALID_CLIENT;

/// One request dispatched through the router, and what came back.
struct Answer {
    status: u16,
    error_type: Option<String>,
    body: Value,
}

impl Answer {
    /// The modelled error shape this answer names, from the `x-amzn-errortype` header or
    /// the body's `__type`.
    fn shape(&self) -> String {
        let named = self.error_type.clone().unwrap_or_else(|| {
            self.body
                .get("__type")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        });
        named
            .rsplit('#')
            .next()
            .unwrap_or_default()
            .split(':')
            .next()
            .unwrap_or_default()
            .to_string()
    }

    /// Everything an assertion needs to say about an answer that was not expected.
    fn summary(&self) -> String {
        format!(
            "{} {} (x-amzn-errortype: {:?})",
            self.status, self.body, self.error_type
        )
    }

    /// A string member of the body, or the empty string.
    fn text(&self, member: &str) -> String {
        self.body
            .get(member)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
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
    let error_type = response
        .headers()
        .get("x-amzn-errortype")
        .map(|value| String::from_utf8_lossy(value.as_bytes()).to_string());
    let bytes = response.into_body();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    Answer {
        status,
        error_type,
        body,
    }
}

/// The credential scope a signed SDK request carries: the key id, the region a handler
/// builds its records in, and the service it is for. The signature is never checked —
/// this router runs with authentication off.
fn credential_scope(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 \
         Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One `awsJson1_1` call to the service whose signing name and target prefix are given.
async fn json_rpc(
    router: &MultiServiceRouter,
    signing_name: &str,
    endpoint: &str,
    target_prefix: &str,
    operation: &str,
    body: &Value,
) -> Answer {
    let target = format!("{target_prefix}.{operation}");
    let authorization = credential_scope(signing_name);
    dispatch(
        router,
        &format!("{endpoint}.{REGION}.amazonaws.com"),
        "POST",
        "/",
        &[
            ("content-type", "application/x-amz-json-1.1"),
            ("x-amz-target", &target),
            ("authorization", &authorization),
        ],
        body.to_string().into_bytes(),
    )
    .await
}

/// One call to the Amazon Cognito user pools service, which must succeed.
async fn cognito(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let answer = json_rpc(
        router,
        "cognito-idp",
        "cognito-idp",
        "AWSCognitoIdentityProviderService",
        operation,
        body,
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

/// One call to IAM Identity Center's own control plane, which must succeed.
async fn sso_admin(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let answer = json_rpc(router, "sso", "sso", "SWBExternalService", operation, body).await;
    assert_eq!(
        answer.status,
        200,
        "setup: sso-admin {operation} must answer 200, got {}",
        answer.summary()
    );
    answer.body
}

/// One call to the identity store, which must succeed.
async fn identity_store(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let answer = json_rpc(
        router,
        "identitystore",
        "identitystore",
        "AWSIdentityStore",
        operation,
        body,
    )
    .await;
    assert_eq!(
        answer.status,
        200,
        "setup: identitystore {operation} must answer 200, got {}",
        answer.summary()
    );
    answer.body
}

/// One `CreateTokenWithIAM` call for the JWT-bearer grant.
async fn exchange(router: &MultiServiceRouter, client_id: &str, assertion: &str) -> Answer {
    let authorization = credential_scope("sso-oauth");
    dispatch(
        router,
        &format!("oidc.{REGION}.amazonaws.com"),
        "POST",
        IAM_TOKEN_PATH,
        &[
            ("content-type", "application/json"),
            ("authorization", &authorization),
        ],
        json!({
            "clientId": client_id,
            "grantType": JWT_BEARER,
            "assertion": assertion,
        })
        .to_string()
        .into_bytes(),
    )
    .await
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
/// RFC 4648 §5 — the two alphabet substitutions and the restored padding turn it back into
/// the standard alphabet this workspace already decodes.
fn decode_segment(segment: &str) -> Vec<u8> {
    let mut standard = segment.replace('-', "+").replace('_', "/");
    while !standard.len().is_multiple_of(4) {
        standard.push('=');
    }
    aws_smithy_types::base64::decode(&standard)
        .unwrap_or_else(|e| panic!("a minted token's segment must be base64url ({e})"))
}

/// `value` as one base64url segment of a compact JWS.
fn encode_segment(value: &Value) -> String {
    aws_smithy_types::base64::encode(value.to_string().as_bytes())
        .replace('+', "-")
        .replace('/', "_")
        .trim_end_matches('=')
        .to_string()
}

/// The claim set of a compact JWS: its second segment.
fn claims_of(what: &str, token: &str) -> Value {
    let segments: Vec<&str> = token.split('.').collect();
    assert_eq!(
        segments.len(),
        3,
        "{what} must be a compact JWS of three segments, got {} segment(s) in {token:?}",
        segments.len()
    );
    serde_json::from_slice(&decode_segment(segments[1]))
        .unwrap_or_else(|e| panic!("{what} must carry a JSON claim set ({e})"))
}

/// The token re-encoded with the claim set given and its signature segment kept — so the
/// only difference from the minted token is the claim the row changed, which no key of the
/// issuer signed.
fn rebuilt(token: &str, claims: &Value) -> String {
    let segments: Vec<&str> = token.split('.').collect();
    format!(
        "{}.{}.{}",
        segments.first().copied().unwrap_or_default(),
        encode_segment(claims),
        segments.get(2).copied().unwrap_or_default()
    )
}

/// The claim `path` names in a claim set, as the dot-separated path into it that
/// `OidcJwtConfiguration$ClaimAttributePath` is written as.
fn claim_at(what: &str, claims: &Value, path: &str) -> String {
    let mut value = claims;
    for segment in path.split('.') {
        value = value
            .get(segment)
            .unwrap_or_else(|| panic!("{what} must carry the claim {path:?}, got {claims}"));
    }
    value
        .as_str()
        .unwrap_or_else(|| panic!("{what}'s claim {path:?} must be a string, got {value}"))
        .to_string()
}

/// What the whole fixture shares: the account's Identity Center instance and identity
/// store, and one user pool with one app client whose ID tokens the assertions are.
struct Fixture {
    instance_arn: String,
    identity_store_id: String,
    pool_id: String,
    app_client_id: String,
    /// The `iss` the pool's own tokens carry, read off a token it minted rather than
    /// rebuilt here.
    issuer: String,
}

/// Create a user pool with one app client that can sign a user in, and answer
/// `(pool id, app client id)`.
async fn user_pool(router: &MultiServiceRouter, pool_name: &str) -> (String, String) {
    let pool = cognito(router, "CreateUserPool", &json!({ "PoolName": pool_name })).await;
    let pool_id = as_str("CreateUserPool", &pool["UserPool"], "Id");
    let client = cognito(
        router,
        "CreateUserPoolClient",
        &json!({
            "UserPoolId": pool_id,
            "ClientName": format!("{pool_name}-client"),
            // The flow the sign-ins below use, plus the refresh flow AWS requires beside
            // any other `ALLOW_` flow.
            "ExplicitAuthFlows": ["ALLOW_ADMIN_USER_PASSWORD_AUTH", "ALLOW_REFRESH_TOKEN_AUTH"],
        }),
    )
    .await;
    let client_id = as_str("CreateUserPoolClient", &client["UserPoolClient"], "ClientId");
    (pool_id, client_id)
}

/// Create a confirmed pool user carrying `email`, and answer the ID token of its sign-in.
async fn pool_user_id_token(
    router: &MultiServiceRouter,
    pool_id: &str,
    app_client_id: &str,
    username: &str,
    email: &str,
) -> String {
    cognito(
        router,
        "AdminCreateUser",
        &json!({
            "UserPoolId": pool_id,
            "Username": username,
            "UserAttributes": [
                {"Name": "email", "Value": email},
                {"Name": "email_verified", "Value": "true"},
            ],
        }),
    )
    .await;
    cognito(
        router,
        "AdminSetUserPassword",
        &json!({
            "UserPoolId": pool_id,
            "Username": username,
            "Password": GOOD_PASSWORD,
            "Permanent": true,
        }),
    )
    .await;
    sign_in(router, pool_id, app_client_id, username).await
}

/// Sign an existing pool user in and answer the ID token, which is the assertion a row
/// presents. Called afresh wherever a row needs an assertion of its own, since a pool
/// mints a new one per sign-in.
async fn sign_in(
    router: &MultiServiceRouter,
    pool_id: &str,
    app_client_id: &str,
    username: &str,
) -> String {
    let signed_in = cognito(
        router,
        "AdminInitiateAuth",
        &json!({
            "UserPoolId": pool_id,
            "ClientId": app_client_id,
            "AuthFlow": "ADMIN_USER_PASSWORD_AUTH",
            "AuthParameters": {"USERNAME": username, "PASSWORD": GOOD_PASSWORD},
        }),
    )
    .await;
    as_str(
        "AdminInitiateAuth",
        &signed_in["AuthenticationResult"],
        "IdToken",
    )
}

/// Register `issuer` as a trusted token issuer of the instance, mapping the assertion's
/// `claim_path` onto the stored user's `store_path`, and answer its ARN.
async fn trusted_token_issuer(
    router: &MultiServiceRouter,
    instance_arn: &str,
    name: &str,
    issuer: &str,
    claim_path: &str,
    store_path: &str,
) -> String {
    let created = sso_admin(
        router,
        "CreateTrustedTokenIssuer",
        &json!({
            "InstanceArn": instance_arn,
            "Name": name,
            "TrustedTokenIssuerType": OIDC_JWT,
            "TrustedTokenIssuerConfiguration": {
                "OidcJwtConfiguration": {
                    "IssuerUrl": issuer,
                    "ClaimAttributePath": claim_path,
                    "IdentityStoreAttributePath": store_path,
                    "JwksRetrievalOption": OPEN_ID_DISCOVERY,
                },
            },
        }),
    )
    .await;
    as_str(
        "CreateTrustedTokenIssuer",
        &created,
        "TrustedTokenIssuerArn",
    )
}

/// What an application is configured with, so a row can leave one piece out.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Configured {
    /// The JWT-bearer grant authorizing the issuer, and an IAM authentication method: the
    /// whole configuration an exchange needs.
    GrantAndActor,
    /// The IAM authentication method alone — no JWT-bearer grant.
    ActorOnly,
    /// The JWT-bearer grant alone — no authentication method for the IAM actor.
    GrantOnly,
}

/// Create an application under the instance, configure it as `configured` says, and answer
/// its `ApplicationArn`.
async fn application(
    router: &MultiServiceRouter,
    instance_arn: &str,
    name: &str,
    issuer_arn: &str,
    audiences: Option<Vec<String>>,
    configured: Configured,
) -> String {
    let created = sso_admin(
        router,
        "CreateApplication",
        &json!({
            "InstanceArn": instance_arn,
            "ApplicationProviderArn": CUSTOM_PROVIDER_ARN,
            "Name": name,
        }),
    )
    .await;
    let application_arn = as_str("CreateApplication", &created, "ApplicationArn");
    if configured != Configured::ActorOnly {
        let mut authorized = json!({"TrustedTokenIssuerArn": issuer_arn});
        if let Some(audiences) = audiences {
            authorized["AuthorizedAudiences"] = json!(audiences);
        }
        sso_admin(
            router,
            "PutApplicationGrant",
            &json!({
                "ApplicationArn": application_arn,
                "GrantType": JWT_BEARER,
                "Grant": {"JwtBearer": {"AuthorizedTokenIssuers": [authorized]}},
            }),
        )
        .await;
    }
    if configured != Configured::GrantOnly {
        sso_admin(
            router,
            "PutApplicationAuthenticationMethod",
            &json!({
                "ApplicationArn": application_arn,
                "AuthenticationMethodType": IAM_METHOD,
                "AuthenticationMethod": {"Iam": {"ActorPolicy": {
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Effect": "Allow",
                        "Principal": {"AWS": "arn:aws:iam::123456789012:role/propagator"},
                        "Action": "sso-oauth:CreateTokenWithIAM",
                    }],
                }}},
            }),
        )
        .await;
    }
    application_arn
}

/// Build the shared fixture on a router: the instance, its identity store and one pool
/// whose tokens are the assertions.
async fn fixture(router: &MultiServiceRouter, pool_name: &str, probe_user: &str) -> Fixture {
    let instances = sso_admin(router, "ListInstances", &json!({})).await;
    let instance = instances["Instances"]
        .get(0)
        .cloned()
        .unwrap_or_else(|| panic!("ListInstances must report the account's instance: {instances}"));
    let instance_arn = as_str("ListInstances", &instance, "InstanceArn");
    let identity_store_id = as_str("ListInstances", &instance, "IdentityStoreId");

    let (pool_id, app_client_id) = user_pool(router, pool_name).await;
    // The issuer the pool announces, read off a token it minted: the `iss` claim is what a
    // trusted token issuer is configured with and what the exchange resolves.
    let probe = pool_user_id_token(
        router,
        &pool_id,
        &app_client_id,
        probe_user,
        &format!("{probe_user}@example.test"),
    )
    .await;
    let issuer = claim_at("the pool's ID token", &claims_of("the pool's ID token", &probe), "iss");

    Fixture {
        instance_arn,
        identity_store_id,
        pool_id,
        app_client_id,
        issuer,
    }
}

/// A router with every service mounted on one engine, as the emulator serves it.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
    let mut router = build_router(engine);
    // Signature verification belongs to the emulator, not to this check.
    router.set_request_authentication(false);
    router
}

/// Run `body` on a thread with enough stack to assemble the router.
fn on_a_sized_thread<F>(name: &str, body: F)
where
    F: FnOnce(MultiServiceRouter) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()>>>
        + Send
        + 'static,
{
    let name = name.to_string();
    std::thread::Builder::new()
        .name(name)
        .stack_size(BOOT_STACK_SIZE)
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .expect("build a current-thread runtime");
            runtime.block_on(async {
                let router = emulator();
                body(router).await;
            });
        })
        .expect("spawn the sized thread")
        .join()
        .expect("the suite's thread must not panic");
}

/// The stored identity-store user one row matches its claim against: the body that puts the
/// claim value at the row's `IdentityStoreAttributePath`.
type StoredUser = fn(&str, &str) -> Value;

/// The claim value as the user's single `Emails` entry (`emails.value`).
fn user_carrying_the_email(store: &str, claimed: &str) -> Value {
    json!({
        "IdentityStoreId": store,
        "UserName": format!("by-email-{claimed}"),
        "Emails": [{"Value": claimed, "Type": "work", "Primary": true}],
    })
}

/// The claim value as the user's `UserName`.
fn user_carrying_the_user_name(store: &str, claimed: &str) -> Value {
    json!({"IdentityStoreId": store, "UserName": claimed})
}

/// One row of the admitted table: which claim of the assertion names the identity, and
/// which attribute of the stored user it is matched against.
struct Admitted {
    case: &'static str,
    claim_path: &'static str,
    store_path: &'static str,
    stored_user: StoredUser,
    pool_username: &'static str,
}

/// RULE (above): the exchange vends a token bound to the user whose
/// `IdentityStoreAttributePath` attribute equals the assertion's `ClaimAttributePath`
/// claim, whichever pair of paths the trusted token issuer is configured with.
#[test]
fn a_trusted_token_issuer_assertion_vends_an_identity_bound_token() {
    on_a_sized_thread("sso-oidc-jwt-bearer-admitted", |router| {
        Box::pin(async move {
            let fixture = fixture(&router, "tti-pool", "probe-user").await;
            // The rows differ along both halves of the mapping a trusted token issuer
            // configures: the claim of the assertion, and the attribute of the stored user.
            let rows: Vec<Admitted> = vec![
                Admitted {
                    case: "the email claim matched against a stored email",
                    claim_path: "email",
                    store_path: "emails.value",
                    stored_user: user_carrying_the_email,
                    pool_username: "ada",
                },
                Admitted {
                    case: "the email claim matched against the stored user name",
                    claim_path: "email",
                    store_path: "userName",
                    stored_user: user_carrying_the_user_name,
                    pool_username: "grace",
                },
                Admitted {
                    case: "the subject claim matched against the stored user name",
                    claim_path: "sub",
                    store_path: "userName",
                    stored_user: user_carrying_the_user_name,
                    pool_username: "alan",
                },
            ];

            for row in rows {
                let case = row.case;
                let assertion = pool_user_id_token(
                    &router,
                    &fixture.pool_id,
                    &fixture.app_client_id,
                    row.pool_username,
                    &format!("{}@example.test", row.pool_username),
                )
                .await;
                let claimed = claim_at(
                    "the presented assertion",
                    &claims_of("the presented assertion", &assertion),
                    row.claim_path,
                );
                // The Identity Store user the claim must resolve to, created through the
                // identity store's own API so the exchange matches against a stored user.
                let created = identity_store(
                    &router,
                    "CreateUser",
                    &(row.stored_user)(&fixture.identity_store_id, &claimed),
                )
                .await;
                let user_id = as_str("CreateUser", &created, "UserId");

                let issuer_arn = trusted_token_issuer(
                    &router,
                    &fixture.instance_arn,
                    &format!("tti-{}", row.pool_username),
                    &fixture.issuer,
                    row.claim_path,
                    row.store_path,
                )
                .await;
                let application_arn = application(
                    &router,
                    &fixture.instance_arn,
                    &format!("app-{}", row.pool_username),
                    &issuer_arn,
                    None,
                    Configured::GrantAndActor,
                )
                .await;

                let answer = exchange(&router, &application_arn, &assertion).await;
                assert_eq!(
                    answer.status,
                    200,
                    "case {case:?}: the exchange must be served, got {}",
                    answer.summary()
                );
                let access_token = answer.text("accessToken");
                assert!(
                    !access_token.is_empty(),
                    "case {case:?}: expected an accessToken, got {}",
                    answer.summary()
                );
                assert_eq!(
                    answer.text("tokenType"),
                    "Bearer",
                    "case {case:?}: expected the Bearer token type, got {}",
                    answer.summary()
                );
                assert_eq!(
                    answer.text("issuedTokenType"),
                    ACCESS_TOKEN_TYPE,
                    "case {case:?}: expected an access token to be issued, got {}",
                    answer.summary()
                );
                let expires_in = answer.body["expiresIn"].as_i64().unwrap_or_default();
                assert!(
                    expires_in > 0,
                    "case {case:?}: expected a positive expiresIn, got {}",
                    answer.summary()
                );
                let granted: Vec<String> = answer.body["scope"]
                    .as_array()
                    .map(|scopes| {
                        scopes
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default();
                for scope in DEFAULT_SCOPES {
                    assert!(
                        granted.iter().any(|held| held == scope),
                        "case {case:?}: expected the default scope {scope:?} to be granted, \
                         got {granted:?}"
                    );
                }
                let identity_context = answer.body["awsAdditionalDetails"]["identityContext"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                assert!(
                    !identity_context.is_empty(),
                    "case {case:?}: expected awsAdditionalDetails.identityContext, got {}",
                    answer.summary()
                );
                // The identity token is what makes the answer identity-BOUND: its subject
                // is the Identity Store user the claim resolved to, read back from the id
                // the identity store's own create answered with.
                let id_token = answer.text("idToken");
                let claims = claims_of("the vended identity token", &id_token);
                assert_eq!(
                    claim_at("the vended identity token", &claims, "sub"),
                    user_id,
                    "case {case:?}: the identity token must name the stored user, got {claims}"
                );
                assert_eq!(
                    claim_at("the vended identity token", &claims, "aud"),
                    application_arn,
                    "case {case:?}: the identity token must name the application it was \
                     issued for, got {claims}"
                );
                assert_eq!(
                    claim_at("the vended identity token", &claims, "aws:application_arn"),
                    application_arn,
                    "case {case:?}: the identity token must name the application the \
                     exchange was made for, got {claims}"
                );
                assert_eq!(
                    claim_at("the vended identity token", &claims, "aws:identity_store_id"),
                    fixture.identity_store_id,
                    "case {case:?}: the identity token must name the directory its subject \
                     lives in, got {claims}"
                );
                assert!(
                    !claim_at("the vended identity token", &claims, "aws:instance_account")
                        .is_empty(),
                    "case {case:?}: the identity token must name the account the instance \
                     belongs to, got {claims}"
                );
                assert_eq!(
                    claim_at("the vended identity token", &claims, IDENTITY_CONTEXT_CLAIM),
                    identity_context,
                    "case {case:?}: the identity token's context claim and \
                     awsAdditionalDetails.identityContext must be the same value, got {claims}"
                );
                let issuer_arn_claim = claims["act"]["sub"].as_str().unwrap_or_default();
                assert_eq!(
                    issuer_arn_claim, issuer_arn,
                    "case {case:?}: the identity token must name the trusted token issuer \
                     that acted for the user, got {claims}"
                );
                let instance_id = fixture
                    .instance_arn
                    .rsplit('/')
                    .next()
                    .unwrap_or_default()
                    .to_string();
                let issued_by = claim_at("the vended identity token", &claims, "iss");
                assert!(
                    issued_by.contains(&instance_id),
                    "case {case:?}: the identity token must be issued by the instance that \
                     holds the application, got {issued_by:?}"
                );
                let window = claims["exp"].as_i64().unwrap_or_default()
                    - claims["iat"].as_i64().unwrap_or_default();
                assert_eq!(
                    window, expires_in,
                    "case {case:?}: the identity token's window must be the access token's \
                     expiresIn, got {window} against {expires_in}"
                );
                assert_eq!(
                    answer.body.get("refreshToken").and_then(Value::as_str),
                    None,
                    "case {case:?}: the exchange vends no refresh token, got {}",
                    answer.summary()
                );
            }
        })
    });
}

/// One row of the refused table: what is presented, and the refusal it earns.
struct Refused {
    case: String,
    client_id: String,
    assertion: String,
    status: u16,
    shape: &'static str,
    error: &'static str,
    /// The `error_description` of the refusal's own class, which is what tells two
    /// outcomes of one error shape apart.
    description: &'static str,
}

/// RULE (above): an assertion that verifies and resolves no user is refused as the invalid
/// grant of THAT case; one this exchange may not spend at all is refused as the invalid
/// grant of the assertion, so the two are distinguishable; an application that configures
/// no JWT-bearer grant or no IAM actor is `UnauthorizedClientException`; a `clientId`
/// naming no application is `InvalidClientException`.
#[test]
fn an_assertion_or_client_the_exchange_cannot_accept_is_refused_by_its_class() {
    on_a_sized_thread("sso-oidc-jwt-bearer-refused", |router| {
        Box::pin(async move {
            assert_ne!(
                UNRESOLVED_IDENTITY, INVALID_ASSERTION,
                "case \"the two refusals of one shape\": an assertion that resolves no \
                 identity and an assertion this exchange may not spend must not answer the \
                 same text, or a client cannot tell them apart"
            );
            let fixture = fixture(&router, "refusal-pool", "refusal-probe").await;

            // The configuration a working exchange needs, so every row below differs from
            // it in exactly one way.
            let issuer_arn = trusted_token_issuer(
                &router,
                &fixture.instance_arn,
                "tti-refusals",
                &fixture.issuer,
                "email",
                "emails.value",
            )
            .await;
            let application_arn = application(
                &router,
                &fixture.instance_arn,
                "app-refusals",
                &issuer_arn,
                None,
                Configured::GrantAndActor,
            )
            .await;
            let assertion = pool_user_id_token(
                &router,
                &fixture.pool_id,
                &fixture.app_client_id,
                "resolvable",
                "resolvable@example.test",
            )
            .await;
            identity_store(
                &router,
                "CreateUser",
                &user_carrying_the_email(&fixture.identity_store_id, "resolvable@example.test"),
            )
            .await;

            // A pool user whose email no Identity Store user carries.
            let unresolvable = pool_user_id_token(
                &router,
                &fixture.pool_id,
                &fixture.app_client_id,
                "unresolvable",
                "unresolvable@example.test",
            )
            .await;

            // A trusted token issuer of the same pool whose claim the assertion does not
            // carry at all, so nothing can be matched against the identity store.
            let absent_claim_issuer = trusted_token_issuer(
                &router,
                &fixture.instance_arn,
                "tti-absent-claim",
                &fixture.issuer,
                // A claim an Amazon Cognito identity token does not mint; the member's
                // pattern admits letters and `.`/`_` separators only.
                "upn",
                "emails.value",
            )
            .await;
            let absent_claim_application = application(
                &router,
                &fixture.instance_arn,
                "app-absent-claim",
                &absent_claim_issuer,
                None,
                Configured::GrantAndActor,
            )
            .await;

            // An application whose grant lists an audience the assertion was not issued
            // for.
            let other_audience_application = application(
                &router,
                &fixture.instance_arn,
                "app-other-audience",
                &issuer_arn,
                Some(vec!["another-app-client-id".to_string()]),
                Configured::GrantAndActor,
            )
            .await;

            // An application missing one half of the configuration.
            let no_grant_application = application(
                &router,
                &fixture.instance_arn,
                "app-no-grant",
                &issuer_arn,
                None,
                Configured::ActorOnly,
            )
            .await;
            let no_actor_application = application(
                &router,
                &fixture.instance_arn,
                "app-no-actor",
                &issuer_arn,
                None,
                Configured::GrantOnly,
            )
            .await;

            // A second pool, registered as a trusted token issuer of nothing.
            let (other_pool, other_client) = user_pool(&router, "unregistered-pool").await;
            let foreign = pool_user_id_token(
                &router,
                &other_pool,
                &other_client,
                "foreign",
                "resolvable@example.test",
            )
            .await;

            // The admitted assertion with its email claim changed, so no key of the issuer
            // signed what it now carries.
            let mut rewritten_claims = claims_of("the presented assertion", &assertion);
            rewritten_claims["email"] = json!("resolvable@example.test");
            rewritten_claims["nickname"] = json!("rewritten");
            let rewritten = rebuilt(&assertion, &rewritten_claims);

            // A well-formed application ARN this account holds no application for.
            let unknown_application = format!("{application_arn}-absent");

            let rows: Vec<Refused> = vec![
                Refused {
                    case: "an assertion whose claim resolves no Identity Store user".to_string(),
                    client_id: application_arn.clone(),
                    assertion: unresolvable.clone(),
                    status: 400,
                    shape: "InvalidGrantException",
                    error: "invalid_grant",
                    description: UNRESOLVED_IDENTITY,
                },
                Refused {
                    case: "an issuer whose claim the assertion does not carry".to_string(),
                    client_id: absent_claim_application.clone(),
                    assertion: assertion.clone(),
                    status: 400,
                    shape: "InvalidGrantException",
                    error: "invalid_grant",
                    description: UNRESOLVED_IDENTITY,
                },
                Refused {
                    case: "a value that is not a JSON Web Token".to_string(),
                    client_id: application_arn.clone(),
                    assertion: "not-a-token".to_string(),
                    status: 400,
                    shape: "InvalidGrantException",
                    error: "invalid_grant",
                    description: INVALID_ASSERTION,
                },
                Refused {
                    case: "a token from a pool no trusted token issuer names".to_string(),
                    client_id: application_arn.clone(),
                    assertion: foreign,
                    status: 400,
                    shape: "InvalidGrantException",
                    error: "invalid_grant",
                    description: INVALID_ASSERTION,
                },
                Refused {
                    case: "the assertion with a claim rewritten after signing".to_string(),
                    client_id: application_arn.clone(),
                    assertion: rewritten,
                    status: 400,
                    shape: "InvalidGrantException",
                    error: "invalid_grant",
                    description: INVALID_ASSERTION,
                },
                Refused {
                    case: "an audience the application's grant does not list".to_string(),
                    client_id: other_audience_application,
                    assertion: assertion.clone(),
                    status: 400,
                    shape: "InvalidGrantException",
                    error: "invalid_grant",
                    description: INVALID_ASSERTION,
                },
                Refused {
                    case: "an application with no JWT-bearer grant".to_string(),
                    client_id: no_grant_application,
                    assertion: assertion.clone(),
                    status: 400,
                    shape: "UnauthorizedClientException",
                    error: "unauthorized_client",
                    description: UNAUTHORIZED_CLIENT,
                },
                Refused {
                    case: "an application with no IAM authentication method".to_string(),
                    client_id: no_actor_application,
                    assertion: assertion.clone(),
                    status: 400,
                    shape: "UnauthorizedClientException",
                    error: "unauthorized_client",
                    description: UNAUTHORIZED_CLIENT,
                },
                Refused {
                    case: "a clientId naming no application of this account".to_string(),
                    client_id: unknown_application,
                    assertion: assertion.clone(),
                    status: 401,
                    shape: "InvalidClientException",
                    error: "invalid_client",
                    description: INVALID_CLIENT,
                },
            ];

            for row in rows {
                let case = &row.case;
                let answer = exchange(&router, &row.client_id, &row.assertion).await;
                assert_eq!(
                    answer.status, row.status,
                    "case {case:?}: expected HTTP {}, got {}",
                    row.status,
                    answer.summary()
                );
                assert_eq!(
                    answer.shape(),
                    row.shape,
                    "case {case:?}: expected the {} shape, got {}",
                    row.shape,
                    answer.summary()
                );
                assert_eq!(
                    answer.text("error"),
                    row.error,
                    "case {case:?}: expected the OAuth error {}, got {}",
                    row.error,
                    answer.summary()
                );
                assert_eq!(
                    answer.text("error_description"),
                    row.description,
                    "case {case:?}: expected the fixed text of its class, got {}",
                    answer.summary()
                );
                assert_eq!(
                    answer.body.get("accessToken").and_then(Value::as_str),
                    None,
                    "case {case:?}: a refused exchange vends no token, got {}",
                    answer.summary()
                );
            }

            // The configuration the rows above each differ from is itself served, so no row
            // passes because the fixture was broken — and the assertion it spends is good
            // for ONE exchange (RFC 7523 §3), so presenting it again is the invalid grant
            // it has become.
            let served = exchange(&router, &application_arn, &assertion).await;
            assert_eq!(
                served.status,
                200,
                "case \"the configuration every row differs from\": expected the exchange to \
                 be served, got {}",
                served.summary()
            );
            let replayed = exchange(&router, &application_arn, &assertion).await;
            assert_eq!(
                replayed.status,
                400,
                "case \"the same assertion presented twice\": expected HTTP 400, got {}",
                replayed.summary()
            );
            assert_eq!(
                replayed.shape(),
                "InvalidGrantException",
                "case \"the same assertion presented twice\": expected the \
                 InvalidGrantException shape, got {}",
                replayed.summary()
            );
            assert_eq!(
                replayed.text("error"),
                "invalid_grant",
                "case \"the same assertion presented twice\": expected the OAuth error \
                 invalid_grant, got {}",
                replayed.summary()
            );
        })
    });
}
