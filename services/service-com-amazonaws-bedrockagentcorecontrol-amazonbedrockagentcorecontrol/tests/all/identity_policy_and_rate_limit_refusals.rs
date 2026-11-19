// SPDX-License-Identifier: CC0-1.0

//! The refusal paths of the identity, policy and gateway rate-limit surfaces, end to end
//! over HTTP.
//!
//! Each test drives the service the way the router does: an `http::Request` through
//! [`MultiServiceRouter::dispatch`], so the wire protocol (`restJson1`), the boundary
//! constraint validation and the handlers are all exercised against one shared engine,
//! with Secrets Manager mounted beside the control plane because a credential provider's
//! secret is stored in a real secret there.
//!
//! The four rules under test, each asserted over a table that spans what it generalises
//! over rather than over one request:
//!
//! 1. A `CredentialProviderName` "must be unique within your account" (`smithy_query
//!    (query="doc",shape="CreateApiKeyCredentialProvider",member="name")`), and a create
//!    under a name the account already holds is refused as a failure of the REQUEST — HTTP
//!    400 `ValidationException`, `x-amzn-errortype: ValidationException`, "Credential
//!    provider with name: <name> already exists" — whichever credential-provider family
//!    of the token vault the create belongs to, leaving the held provider untouched.
//! 2. A payment vendor's secret that the service is to MANAGE has to be a key in the
//!    format that vendor issues: the Coinbase CDP `apiKeySecret` is a base64-encoded
//!    Ed25519 private key, so a value that is not one is refused HTTP 400
//!    `ValidationException` with "Invalid apiKeySecret format: Expected base64-encoded
//!    Ed25519 private key", and no provider is created.
//! 3. A policy is addressed by its own `policyId`, the `policyEngineId` in the path only
//!    routing the lookup (`smithy_query(query="operation",operation="GetPolicy")`), so a
//!    read of a policy this account does not hold is the modelled 404 naming the POLICY it
//!    asked for.
//! 4. A gateway rate limit is addressed by an id of its gateway, so a read or an update of
//!    an id the gateway does not hold is the modelled 404 naming that id ("Rate limit not
//!    found: <rateLimitId>").

use std::sync::Arc;

use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE};
use base64::Engine as _;
use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol::EmulatedService;

/// The region the rows sign for.
const REGION: &str = "us-east-1";

/// A role ARN inside `RoleArn`'s pattern `^arn:aws(-[^:]+)?:iam::([0-9]{12})?:role/.+$`.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/GatewayRole";

/// One answer: its HTTP status, the error type the protocol puts in the header, and the
/// parsed body.
struct Answer {
    status: u16,
    error_type: String,
    body: Value,
}

impl Answer {
    fn of(response: http::Response<Bytes>) -> Self {
        Self {
            status: response.status().as_u16(),
            error_type: response
                .headers()
                .get("x-amzn-errortype")
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_string(),
            body: if response.body().is_empty() {
                Value::Null
            } else {
                serde_json::from_slice(response.body()).unwrap_or(Value::Null)
            },
        }
    }

    /// The error code `restJson1` puts in the body's `__type`.
    fn error_code(&self) -> String {
        self.body
            .get("__type")
            .and_then(Value::as_str)
            .unwrap_or("<no __type>")
            .rsplit('#')
            .next()
            .unwrap_or_default()
            .to_string()
    }

    fn text(&self, member: &str) -> String {
        self.body
            .get(member)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }
}

/// The control plane with Secrets Manager beside it on one shared engine, which is the
/// shape `core/router/src/lib.rs` mounts them in.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::default();
    router.register_service(EmulatedService::new(Arc::clone(&engine)));
    router.register_service(
        service_com_amazonaws_secretsmanager_secretsmanager::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header for this service, which is what the router routes on.
fn authorization() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/bedrock-agentcore/\
         aws4_request, SignedHeaders=host, Signature=abc"
    )
}

async fn send(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> Answer {
    let payload = body.map_or_else(Bytes::new, |value| {
        Bytes::from(serde_json::to_vec(value).expect("the request body serializes"))
    });
    let mut request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", authorization())
        .header("host", "bedrock-agentcore-control.us-east-1.amazonaws.com");
    if body.is_some() {
        request = request.header("content-type", "application/json");
    }
    let request = request.body(payload).expect("a well-formed request");
    Answer::of(router.dispatch(request).await)
}

async fn post(router: &MultiServiceRouter, uri: &str, body: &Value) -> Answer {
    send(router, "POST", uri, Some(body)).await
}

/// The base64 of `length` bytes, in the spelling `encode` renders: a stand-in for a
/// vendor-issued key of that byte length.
fn base64_key(length: usize, filler: u8, encode: fn(&[u8]) -> String) -> String {
    encode(&vec![filler; length])
}

fn standard(bytes: &[u8]) -> String {
    STANDARD.encode(bytes)
}

fn standard_no_pad(bytes: &[u8]) -> String {
    STANDARD_NO_PAD.encode(bytes)
}

fn url_safe(bytes: &[u8]) -> String {
    URL_SAFE.encode(bytes)
}

// ---------------------------------------------------------------------------------------
// 1. A credential-provider name the account already holds
// ---------------------------------------------------------------------------------------

/// Which credential-provider family of the token vault a row creates in, and the one
/// request member each needs besides the name.
#[derive(Debug, Clone, Copy)]
enum Family {
    ApiKey,
    Oauth2,
    Payment,
}

impl Family {
    fn create_uri(self) -> &'static str {
        match self {
            Self::ApiKey => "/identities/CreateApiKeyCredentialProvider",
            Self::Oauth2 => "/identities/CreateOauth2CredentialProvider",
            Self::Payment => "/identities/CreatePaymentCredentialProvider",
        }
    }

    fn get_uri(self) -> &'static str {
        match self {
            Self::ApiKey => "/identities/GetApiKeyCredentialProvider",
            Self::Oauth2 => "/identities/GetOauth2CredentialProvider",
            Self::Payment => "/identities/GetPaymentCredentialProvider",
        }
    }

    /// A create body for `name` carrying the credential this family needs. `variant`
    /// distinguishes the second create from the first, so a refusal cannot be mistaken
    /// for the first request being replayed.
    fn create_body(self, name: &str, variant: &str) -> Value {
        match self {
            Self::ApiKey => json!({"name": name, "apiKey": format!("api-key-{variant}")}),
            Self::Oauth2 => json!({
                "name": name,
                "credentialProviderVendor": "GoogleOauth2",
                "oauth2ProviderConfigInput": {"googleOauth2ProviderConfig": {
                    "clientId": format!("client-{variant}"),
                    "clientSecret": format!("client-secret-{variant}"),
                }},
            }),
            Self::Payment => json!({
                "name": name,
                "credentialProviderVendor": "CoinbaseCDP",
                "providerConfigurationInput": {"coinbaseCdpConfiguration": {
                    "apiKeyId": format!("cdp-api-key-{variant}"),
                    "apiKeySecret": base64_key(32, b'a', standard),
                    "walletSecret": format!("d2FsbGV0-{variant}"),
                }},
            }),
        }
    }
}

/// A create under a name the account already holds is refused as an input validation
/// failure naming that name, whichever credential-provider family it belongs to, and the
/// provider holding the name is left exactly as it was.
///
/// The rows differ along the family (so along the operation, its request shape and the
/// secret it stores) and along the `CredentialProviderName` spelling the name takes
/// (`smithy_query(query="shape",shape="CredentialProviderName")` — length 1..128,
/// `^[a-zA-Z0-9\-_]+$`): one character, hyphens, underscores, mixed case and digits, and
/// the longest name the length admits.
#[tokio::test]
async fn a_credential_provider_name_the_account_holds_is_refused_as_a_validation_failure() {
    let longest = "n".repeat(128);
    let names = [
        ("one character", "a"),
        ("hyphens", "held-provider-name"),
        ("underscores", "held_provider_name"),
        ("mixed case and digits", "Held9Provider0"),
        ("the longest name the length admits", longest.as_str()),
    ];
    for family in [Family::ApiKey, Family::Oauth2, Family::Payment] {
        for (spelling, name) in names {
            let case = format!("{family:?} under a {spelling} name");
            let router = router();
            let first = post(&router, family.create_uri(), &family.create_body(name, "first")).await;
            assert_eq!(
                first.status, 201,
                "case {case:?}: the first create must be served, got {} ({})",
                first.status, first.body
            );
            let held = post(&router, family.get_uri(), &json!({"name": name})).await;
            assert_eq!(
                held.status, 200,
                "case {case:?}: the held provider must read back, got {} ({})",
                held.status, held.body
            );

            let refused =
                post(&router, family.create_uri(), &family.create_body(name, "second")).await;
            assert_eq!(
                (
                    refused.status,
                    refused.error_code(),
                    refused.error_type.clone(),
                    refused.text("message")
                ),
                (
                    400,
                    "ValidationException".to_string(),
                    "ValidationException".to_string(),
                    format!("Credential provider with name: {name} already exists")
                ),
                "case {case:?}: expected the modelled 400 naming the held name, got {} ({})",
                refused.status,
                refused.body
            );
            // The identity plane's refusal carries that sentence and nothing besides.
            assert!(
                refused.body.get("reason").is_none() && refused.body.get("fieldList").is_none(),
                "case {case:?}: the refusal carries the sentence alone, got {}",
                refused.body
            );

            let after = post(&router, family.get_uri(), &json!({"name": name})).await;
            assert_eq!(
                (after.status, &after.body),
                (200, &held.body),
                "case {case:?}: the refused create changed the held provider, got {} ({})",
                after.status,
                after.body
            );
        }
    }
}

// ---------------------------------------------------------------------------------------
// 2. A payment vendor secret that is not a key in the vendor's format
// ---------------------------------------------------------------------------------------

/// A `CreatePaymentCredentialProvider` body whose Coinbase CDP API key secret is
/// `api_key_secret` and whose wallet secret the service manages.
fn coinbase_create(name: &str, api_key_secret: &str) -> Value {
    json!({
        "name": name,
        "credentialProviderVendor": "CoinbaseCDP",
        "providerConfigurationInput": {"coinbaseCdpConfiguration": {
            "apiKeyId": "cdp-api-key",
            "apiKeySecret": api_key_secret,
            "walletSecret": "bWFuYWdlZC13YWxsZXQ=",
        }},
    })
}

/// A `MANAGED` Coinbase CDP `apiKeySecret` that is not a base64-encoded Ed25519 private
/// key refuses the create, and nothing is created under the name it asked for.
///
/// The rows differ along WHY the value is not such a key: too few bytes, one byte short of
/// the seed, one byte past it, one byte either side of the 64-byte form the vendor issues,
/// and base64 that does not decode at all. Every value is inside the member's own pattern
/// (`^[a-zA-Z0-9+/=\-_\s]*$`, `smithy_query(query="shape",
/// shape="CoinbaseCdpConfigurationInput")`), so the refusal is the handler's rule and not
/// the boundary constraint walk.
#[tokio::test]
async fn a_coinbase_api_key_secret_that_is_not_an_ed25519_private_key_is_refused() {
    let rows = [
        ("a handful of bytes", base64_key(6, b'z', standard)),
        ("one byte short of the seed", base64_key(31, b'q', standard)),
        ("one byte past the seed", base64_key(33, b'q', standard)),
        ("one byte short of the issued key", base64_key(63, b'k', standard)),
        ("one byte past the issued key", base64_key(65, b'k', standard)),
        ("base64 that does not decode", "=AAA=".to_string()),
    ];
    for (case, api_key_secret) in rows {
        let router = router();
        let name = "payment-malformed-secret";
        let refused = post(
            &router,
            "/identities/CreatePaymentCredentialProvider",
            &coinbase_create(name, &api_key_secret),
        )
        .await;
        assert_eq!(
            (
                refused.status,
                refused.error_code(),
                refused.error_type.clone(),
                refused.text("message")
            ),
            (
                400,
                "ValidationException".to_string(),
                "ValidationException".to_string(),
                "Invalid apiKeySecret format: Expected base64-encoded Ed25519 private key"
                    .to_string()
            ),
            "case {case:?}: expected the modelled 400 for the malformed secret, got {} ({})",
            refused.status,
            refused.body
        );
        // The identity plane's refusal carries that sentence and nothing besides.
        assert!(
            refused.body.get("reason").is_none() && refused.body.get("fieldList").is_none(),
            "case {case:?}: the refusal carries the sentence alone, got {}",
            refused.body
        );

        let read = post(
            &router,
            "/identities/GetPaymentCredentialProvider",
            &json!({"name": name}),
        )
        .await;
        assert_eq!(
            read.status, 404,
            "case {case:?}: the refused create left a provider behind, got {} ({})",
            read.status, read.body
        );
    }
}

/// A `MANAGED` Coinbase CDP `apiKeySecret` that IS a base64-encoded Ed25519 private key is
/// stored, and the provider reads back through `GetPaymentCredentialProvider`.
///
/// The rows differ along the key length the vendor hands over (the 32-byte seed, the
/// 64-byte seed-and-public-key form) and along the base64 spelling it arrives in (padded,
/// unpadded, URL-safe, wrapped across lines), because one key is one key however it is
/// spelled.
#[tokio::test]
async fn a_coinbase_api_key_secret_in_the_vendors_key_format_is_accepted() {
    let wrapped = format!(
        "{}\n{}",
        &base64_key(64, b'w', standard)[..40],
        &base64_key(64, b'w', standard)[40..]
    );
    let rows = [
        ("the 32-byte seed, padded", base64_key(32, b's', standard)),
        ("the 32-byte seed, unpadded", base64_key(32, b's', standard_no_pad)),
        ("the 32-byte seed, URL-safe", base64_key(32, b'\xfb', url_safe)),
        ("the 64-byte issued key", base64_key(64, b'i', standard)),
        ("the 64-byte issued key, unpadded", base64_key(64, b'i', standard_no_pad)),
        ("the 64-byte issued key wrapped across lines", wrapped),
    ];
    for (index, (case, api_key_secret)) in rows.into_iter().enumerate() {
        let router = router();
        let name = format!("payment-key-{index}");
        let created = post(
            &router,
            "/identities/CreatePaymentCredentialProvider",
            &coinbase_create(&name, &api_key_secret),
        )
        .await;
        assert_eq!(
            created.status, 201,
            "case {case:?}: a key in the vendor's format must be stored, got {} ({})",
            created.status, created.body
        );

        let read = post(
            &router,
            "/identities/GetPaymentCredentialProvider",
            &json!({"name": name}),
        )
        .await;
        assert_eq!(
            (read.status, read.text("name")),
            (200, name.clone()),
            "case {case:?}: the created provider must read back, got {} ({})",
            read.status,
            read.body
        );
        let reported = read.body["providerConfigurationOutput"]["coinbaseCdpConfiguration"]
            ["apiKeySecretArn"]["secretArn"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            reported.starts_with("arn:aws:secretsmanager:"),
            "case {case:?}: the stored key must be reported by its Secrets Manager ARN, got \
             {reported:?} ({})",
            read.body
        );
        assert!(
            !read.body.to_string().contains(&api_key_secret),
            "case {case:?}: no answer may carry the secret itself, got {}",
            read.body
        );
    }
}

// ---------------------------------------------------------------------------------------
// 3. A policy read names the policy it asked for
// ---------------------------------------------------------------------------------------

/// A Cedar definition: `CedarPolicy$statement` is 35..10000 characters
/// (`smithy_query(query="shape",shape="CedarPolicy")`).
fn cedar_definition(action: &str) -> Value {
    json!({"cedar": {
        "statement": format!("permit(principal, action == Action::\"{action}\", resource);"),
    }})
}

/// Create a policy engine and hand back its `policyEngineId`.
async fn policy_engine(router: &MultiServiceRouter, name: &str) -> String {
    let created = post(router, "/policy-engines", &json!({"name": name})).await;
    assert_eq!(
        created.status, 202,
        "setup {name:?}: expected HTTP 202 from CreatePolicyEngine, got {} ({})",
        created.status, created.body
    );
    created.text("policyEngineId")
}

/// A read of a policy this account does not hold is the modelled 404 naming the POLICY the
/// request asked for, not the policy engine the path routed through.
///
/// The rows differ along why the policy does not resolve — an engine that exists and does
/// not hold it, an engine the account does not hold at all, and a policy held by ANOTHER
/// engine of the account — and along the `policyId` spelling (`ResourceId` is 12..59 with
/// `^[A-Za-z][A-Za-z0-9_]*-[a-z0-9_]{10}$`).
#[tokio::test]
async fn a_policy_read_names_the_policy_it_asked_for() {
    let router = router();
    let held_engine = policy_engine(&router, "Holding_engine").await;
    let other_engine = policy_engine(&router, "Other_engine").await;
    let created = post(
        &router,
        &format!("/policy-engines/{held_engine}/policies"),
        &json!({"name": "HeldPolicy", "definition": cedar_definition("invokeTool")}),
    )
    .await;
    assert_eq!(
        created.status, 202,
        "setup: the policy must be created, got {} ({})",
        created.status, created.body
    );
    let held_policy = created.text("policyId");

    // (case, the engine id the path carries, the policy id it asks for)
    let rows = [
        (
            "an id the holding engine carries no policy under",
            held_engine.clone(),
            "Absent_policy-0123456789".to_string(),
        ),
        (
            "an id of the shortest shape the pattern admits",
            held_engine.clone(),
            "A-0123456789".to_string(),
        ),
        (
            "an id of the longest shape the pattern admits",
            held_engine.clone(),
            format!("P{}-zz00zz00zz", "x".repeat(47)),
        ),
        (
            "an engine the account does not hold either",
            "Missing_engine-abcdefghij".to_string(),
            "Absent_policy-0123456789".to_string(),
        ),
        (
            "a policy of another engine of the account",
            other_engine.clone(),
            held_policy.clone(),
        ),
    ];

    for (case, engine_id, policy_id) in rows {
        let refused = send(
            &router,
            "GET",
            &format!("/policy-engines/{engine_id}/policies/{policy_id}"),
            None,
        )
        .await;
        assert_eq!(
            (
                refused.status,
                refused.error_code(),
                refused.text("message")
            ),
            (
                404,
                "ResourceNotFoundException".to_string(),
                format!("Policy with ID {policy_id} not found")
            ),
            "case {case:?}: expected the modelled 404 naming the policy asked for, got {} ({})",
            refused.status,
            refused.body
        );
    }

    // None of the refusals touched the policy that IS held.
    let intact = send(
        &router,
        "GET",
        &format!("/policy-engines/{held_engine}/policies/{held_policy}"),
        None,
    )
    .await;
    assert_eq!(
        (intact.status, intact.text("policyId")),
        (200, held_policy.clone()),
        "the held policy must still read back through its own engine, got {} ({})",
        intact.status,
        intact.body
    );
}

// ---------------------------------------------------------------------------------------
// 4. A gateway rate limit the gateway does not hold
// ---------------------------------------------------------------------------------------

/// A rate configuration: `RateConfig` needs a rate in 0..10000000 and a `Period`, whose
/// values are `second | minute` (`smithy_query(query="shape",shape="Period")`).
fn rate(value: f64, period: &str) -> Value {
    json!([{"rate": value, "period": period}])
}

/// Create a gateway and hand back its `gatewayId`.
async fn gateway(router: &MultiServiceRouter, name: &str) -> String {
    let created = post(
        router,
        "/gateways/",
        &json!({"name": name, "roleArn": ROLE_ARN, "authorizerType": "AWS_IAM"}),
    )
    .await;
    assert_eq!(
        created.status, 202,
        "setup {name:?}: expected HTTP 202 from CreateGateway, got {} ({})",
        created.status, created.body
    );
    created.text("gatewayId")
}

/// A read or an update of a `rateLimitId` the gateway does not hold is the modelled 404
/// naming that id, and leaves the limit the gateway does hold as it was.
///
/// The rows differ along the id asked for (`GatewayRateLimitId` is 2..64 with
/// `^[a-zA-Z0-9][a-zA-Z0-9\-_\.]{0,62}[a-zA-Z0-9]$`: the shortest the pattern admits, one
/// carrying the punctuation it allows, the longest, and one differing from the held id
/// only in case) and along the operation that resolves it.
#[tokio::test]
async fn a_rate_limit_the_gateway_does_not_hold_is_refused_by_its_own_id() {
    let router = router();
    let gateway_id = gateway(&router, "ratelimitrefusal").await;
    let held = post(
        &router,
        &format!("/gateways/{gateway_id}/rate-limits"),
        &json!({
            "rateLimitId": "held-limit",
            "dimensionKeys": ["toolName"],
            "entries": [{"dimensions": {"toolName": "search"}, "tokens": rate(9.0, "minute")}],
        }),
    )
    .await;
    assert_eq!(
        held.status, 201,
        "setup: the rate limit must be created, got {} ({})",
        held.status, held.body
    );

    let longest = format!("l{}f", "0".repeat(62));
    let unknown = [
        ("the shortest id the pattern admits", "ab".to_string()),
        ("an id carrying the punctuation allowed", "not.a_held-limit".to_string()),
        ("the longest id the pattern admits", longest),
        ("an id differing only in case", "Held-Limit".to_string()),
    ];
    let update = json!({
        "entries": [{"dimensions": {"toolName": "search"}, "tokens": rate(10.0, "minute")}],
    });

    for (case, rate_limit_id) in unknown {
        let read = send(
            &router,
            "GET",
            &format!("/gateways/{gateway_id}/rate-limits/{rate_limit_id}"),
            None,
        )
        .await;
        assert_eq!(
            (read.status, read.error_code(), read.text("message")),
            (
                404,
                "ResourceNotFoundException".to_string(),
                format!("Rate limit not found: {rate_limit_id}")
            ),
            "case {case:?}: the read must name the rate limit asked for, got {} ({})",
            read.status,
            read.body
        );

        let refused = send(
            &router,
            "PATCH",
            &format!("/gateways/{gateway_id}/rate-limits/{rate_limit_id}"),
            Some(&update),
        )
        .await;
        assert_eq!(
            (refused.status, refused.error_code(), refused.text("message")),
            (
                404,
                "ResourceNotFoundException".to_string(),
                format!("Rate limit not found: {rate_limit_id}")
            ),
            "case {case:?}: the update must name the rate limit asked for, got {} ({})",
            refused.status,
            refused.body
        );
    }

    let intact = send(
        &router,
        "GET",
        &format!("/gateways/{gateway_id}/rate-limits/held-limit"),
        None,
    )
    .await;
    assert_eq!(
        (intact.status, intact.text("rateLimitId")),
        (200, "held-limit".to_string()),
        "the held rate limit must still read back, got {} ({})",
        intact.status,
        intact.body
    );
}
