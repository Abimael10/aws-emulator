// SPDX-License-Identifier: CC0-1.0

//! RUNNING a custom authorizer: the token-signature check AWS IoT performs before it
//! calls one, the event it calls the authorizer's Lambda function with, and the
//! authorization answer it reads back out of that function's response.
//!
//! An authorizer is "the Lambda function that authorizes for it plus how its token is
//! validated" ([`super::RegisteredAuthorizer`]), and running one is two steps in that
//! order:
//!
//! 1. **The token signature.** `TestInvokeAuthorizer$tokenSignature` is "the signature
//!    made with the token and your custom authentication service's private key. This
//!    value must be Base-64-encoded", and `CreateAuthorizer$tokenSigningPublicKeys` is
//!    "the public keys used to verify the digital signature returned by your custom
//!    authentication service" — so the signature is checked against those keys, and
//!    the authorizer carries a MAP of them rather than one, so a signature made with
//!    ANY of the registered keys verifies. That is what key rotation is for: a service
//!    publishes the new key beside the old one and signs with either while its fleet
//!    moves over. An authorizer whose `signingDisabled` is true "specifies whether IoT
//!    validates the token signature in an authorization request" as false, so no
//!    signature is required and none is checked. How many keys an authorizer may
//!    register is bounded ([`super::MAX_TOKEN_SIGNING_PUBLIC_KEYS`]), because every
//!    run of that authorizer checks a presented signature against each of them.
//! 2. **The function.** The authorizer's Lambda function is invoked synchronously with
//!    the token, whether the signature verified, and the protocol context of the
//!    authorization request; what it returns IS the authorization answer — the
//!    `isAuthenticated`, `principalId`, `policyDocuments`, `refreshAfterInSeconds` and
//!    `disconnectAfterInSeconds` of the response — so nothing here decides any of them.
//!
//! The signature algorithm is RSASSA-PKCS1-v1_5 over SHA-256, which is what signing a
//! token with an RSA private key and `sha256` produces and what AWS IoT's custom
//! authentication documentation has a service sign its token with; the check is
//! `aws-lc-rs`'s, over the key the authorizer's PEM `SubjectPublicKeyInfo` holds.

use std::collections::BTreeMap;

use base64::Engine as _;
use serde_json::{json, Map, Value};
use x509_cert::der::DecodePem as _;

use crate::ops::types::{HttpContext, MqttContext, TlsContext};

/// How this service states that a presented token signature verifies against none of
/// the authorizer's registered token signing public keys.
///
/// The fixed text names what did not check out, and the variable part is the
/// authorizer the request named: every key the authorizer holds is tried, so a
/// mismatch is a statement about the authorizer as a whole and not about one key.
pub fn signature_mismatch_message(authorizer_name: &str) -> String {
    format!("Token signature mismatch for authorizer {authorizer_name}")
}

/// How this service states that an authorizer which validates token signatures was
/// given no token to check.
///
/// Same family as [`signature_mismatch_message`]: the fixed text names the member the
/// request left out, the variable part is the authorizer it named.
pub fn missing_token_message(authorizer_name: &str) -> String {
    format!("Token is required for authorizer {authorizer_name}")
}

/// How this service states that an authorizer which validates token signatures was
/// given a token with no signature over it.
pub fn missing_token_signature_message(authorizer_name: &str) -> String {
    format!("Token signature is required for authorizer {authorizer_name}")
}

/// How this service states that what the authorizer's function answered is not an
/// authorization answer: `detail` says which part of it is not.
pub fn invalid_response_message(authorizer_name: &str, detail: &str) -> String {
    format!("Invalid response from authorizer {authorizer_name}: {detail}")
}

/// How this service states that the authorizer's function was never reached, so there
/// is no answer to read at all: `detail` is what AWS Lambda said about the invocation.
///
/// Worded apart from [`invalid_response_message`] on purpose — a function the account
/// does not hold answered nothing, and saying its response was invalid would assert an
/// answer that never existed.
pub fn not_invoked_message(authorizer_name: &str, detail: &str) -> String {
    format!("Authorizer {authorizer_name} could not be invoked: {detail}")
}

/// Why running an authorizer produced no authorization answer.
///
/// Both are the operation's modelled `InvalidResponseException` ("The response is
/// invalid", HTTP 400) — the authorization answer the caller asked for does not exist
/// either way — but they are WORDED apart, because a function that was never reached
/// answered nothing and a function that answered answered something wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthorizerFailure {
    /// The invocation never reached a function, or was never answered at all.
    NotInvoked(String),
    /// The function answered, and what it answered is not an authorization answer.
    AnsweredWrongly(String),
}

impl AuthorizerFailure {
    /// The message this failure carries for the authorizer the request named.
    pub fn message(&self, authorizer_name: &str) -> String {
        match self {
            Self::NotInvoked(detail) => not_invoked_message(authorizer_name, detail),
            Self::AnsweredWrongly(detail) => invalid_response_message(authorizer_name, detail),
        }
    }
}

/// Whether `token_signature` — base-64 as the member documents — is a valid
/// RSASSA-PKCS1-v1_5 SHA-256 signature over `token` under ANY of `public_keys`.
///
/// The work is one PEM read plus one verification per key, stopping at the first key
/// that verifies, so it is bounded by how many keys an authorizer may register
/// ([`super::MAX_TOKEN_SIGNING_PUBLIC_KEYS`], enforced by every writer of one) and not
/// by anything this request chooses.
///
/// A map with no keys verifies nothing, and a key this library cannot read is simply a
/// key that verifies nothing: it is not grounds to reject a signature another
/// registered key does verify.
pub fn token_signature_verifies(
    public_keys: &BTreeMap<String, String>,
    token: &str,
    token_signature: &str,
) -> bool {
    let Ok(signature) = base64::engine::general_purpose::STANDARD.decode(token_signature) else {
        return false;
    };
    public_keys.values().any(|public_key| {
        rsa_public_key(public_key).is_some_and(|key| {
            ::aws_lc_rs::signature::UnparsedPublicKey::new(
                &::aws_lc_rs::signature::RSA_PKCS1_2048_8192_SHA256,
                key,
            )
            .verify(token.as_bytes(), &signature)
            .is_ok()
        })
    })
}

/// The RSA public key of a PEM `SubjectPublicKeyInfo` block, as the DER PKCS#1
/// `RSAPublicKey` a verifier takes.
///
/// A `SubjectPublicKeyInfo`'s `subjectPublicKey` IS the PKCS#1 `RSAPublicKey` for an
/// RSA key (RFC 3279 section 2.3.1), so the structure is read out of the block rather
/// than rebuilt from parts.
fn rsa_public_key(public_key_pem: &str) -> Option<Vec<u8>> {
    let info = ::x509_cert::spki::SubjectPublicKeyInfoOwned::from_pem(public_key_pem.trim())
        .ok()?;
    Some(info.subject_public_key.raw_bytes().to_vec())
}

/// The event an authorizer's Lambda function is invoked with.
///
/// The members are the ones AWS IoT's custom authentication documentation gives a
/// token-based authorizer's function: the `token` presented (when one was), whether
/// its signature was verified, the `protocols` of the authorization request, the
/// `protocolData` of each of them, and the `connectionMetadata` of the connection being
/// authorized. Which protocols are in play is which contexts the request carried — an
/// `httpContext` "specifies a test HTTP authorization request", an `mqttContext` a test
/// MQTT one and a `tlsContext` a test TLS one — so a request that carries none names
/// none.
pub fn authorizer_event(
    token: Option<&str>,
    signature_verified: bool,
    http_context: Option<&HttpContext>,
    mqtt_context: Option<&MqttContext>,
    tls_context: Option<&TlsContext>,
    connection_id: &str,
) -> Value {
    let mut protocols: Vec<Value> = Vec::new();
    let mut protocol_data = Map::new();
    if let Some(context) = tls_context {
        protocols.push(Value::String("tls".to_string()));
        protocol_data.insert("tls".to_string(), tls_protocol_data(context));
    }
    if let Some(context) = http_context {
        protocols.push(Value::String("http".to_string()));
        protocol_data.insert("http".to_string(), http_protocol_data(context));
    }
    if let Some(context) = mqtt_context {
        protocols.push(Value::String("mqtt".to_string()));
        protocol_data.insert("mqtt".to_string(), mqtt_protocol_data(context));
    }

    let mut event = Map::new();
    if let Some(token) = token {
        event.insert("token".to_string(), Value::String(token.to_string()));
    }
    event.insert(
        "signatureVerified".to_string(),
        Value::Bool(signature_verified),
    );
    event.insert("protocols".to_string(), Value::Array(protocols));
    event.insert("protocolData".to_string(), Value::Object(protocol_data));
    event.insert(
        "connectionMetadata".to_string(),
        json!({ "id": connection_id }),
    );
    Value::Object(event)
}

/// `protocolData.tls` — the server name the TLS handshake asked for.
fn tls_protocol_data(context: &TlsContext) -> Value {
    let mut data = Map::new();
    if let Some(server_name) = &context.server_name {
        data.insert(
            "serverName".to_string(),
            Value::String(server_name.clone()),
        );
    }
    Value::Object(data)
}

/// `protocolData.http` — the headers and query string of the HTTP request.
fn http_protocol_data(context: &HttpContext) -> Value {
    let mut data = Map::new();
    if let Some(headers) = &context.headers {
        // `HttpHeaders` is keyed by header name, so the headers are reported in one
        // stable order whatever order the request listed them in.
        let ordered: BTreeMap<&String, &String> = headers.iter().collect();
        data.insert(
            "headers".to_string(),
            Value::Object(
                ordered
                    .into_iter()
                    .map(|(name, value)| (name.clone(), Value::String(value.clone())))
                    .collect(),
            ),
        );
    }
    if let Some(query_string) = &context.query_string {
        data.insert(
            "queryString".to_string(),
            Value::String(query_string.clone()),
        );
    }
    Value::Object(data)
}

/// `protocolData.mqtt` — the CONNECT packet's user name, password and client id. The
/// password is a blob (`MqttContext$password`), and the event is a JSON document, so it
/// travels base-64 encoded.
fn mqtt_protocol_data(context: &MqttContext) -> Value {
    let mut data = Map::new();
    if let Some(username) = &context.username {
        data.insert("username".to_string(), Value::String(username.clone()));
    }
    if let Some(password) = &context.password {
        data.insert(
            "password".to_string(),
            Value::String(base64::engine::general_purpose::STANDARD.encode(password)),
        );
    }
    if let Some(client_id) = &context.client_id {
        data.insert("clientId".to_string(), Value::String(client_id.clone()));
    }
    Value::Object(data)
}

/// The authorization answer an authorizer's function returned, as the response members
/// report it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizerVerdict {
    /// "True if the token is authenticated, otherwise false" — the one member the
    /// function has to answer, since it is the verdict itself.
    pub is_authenticated: bool,
    /// "The principal ID", when the function named one.
    pub principal_id: Option<String>,
    /// "IAM policy documents", when the function returned any.
    pub policy_documents: Option<Vec<String>>,
    /// "The number of seconds after which the temporary credentials are refreshed".
    pub refresh_after_in_seconds: Option<i32>,
    /// "The number of seconds after which the connection is terminated".
    pub disconnect_after_in_seconds: Option<i32>,
}

/// The longest a `principalId` may be (`PrincipalId`, `length 1..128`).
const PRINCIPAL_ID_MAX_LENGTH: usize = 128;

/// The authorization answer `payload` carries, or why it is not one.
///
/// The response members are modelled, so a function whose document cannot fill them is
/// a function that answered something other than an authorization answer — which is
/// what the operation's `InvalidResponseException` ("The response is invalid") is for,
/// rather than a 200 carrying members invented here.
pub fn read_verdict(payload: &[u8]) -> Result<AuthorizerVerdict, AuthorizerFailure> {
    read_answer(payload).map_err(AuthorizerFailure::AnsweredWrongly)
}

/// The answer, or the detail of what about the document is not an answer.
fn read_answer(payload: &[u8]) -> Result<AuthorizerVerdict, String> {
    let Ok(Value::Object(document)) = ::serde_json::from_slice::<Value>(payload) else {
        return Err("the authorizer function did not return a JSON object".to_string());
    };
    let Some(is_authenticated) = document.get("isAuthenticated").and_then(Value::as_bool) else {
        return Err("isAuthenticated is missing or is not a boolean".to_string());
    };
    Ok(AuthorizerVerdict {
        is_authenticated,
        principal_id: principal_id(&document)?,
        policy_documents: policy_documents(&document)?,
        refresh_after_in_seconds: seconds(&document, "refreshAfterInSeconds")?,
        disconnect_after_in_seconds: seconds(&document, "disconnectAfterInSeconds")?,
    })
}

/// `principalId` of a returned document, checked against `PrincipalId`'s modelled
/// constraints (`length 1..128, pattern ^[a-zA-Z0-9]+$`) so the response this service
/// serializes is one the shape admits.
fn principal_id(document: &Map<String, Value>) -> Result<Option<String>, String> {
    let Some(value) = document.get("principalId") else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let Some(principal_id) = value.as_str() else {
        return Err("principalId is not a string".to_string());
    };
    let admitted = (1..=PRINCIPAL_ID_MAX_LENGTH).contains(&principal_id.chars().count())
        && principal_id.chars().all(|character| character.is_ascii_alphanumeric());
    if !admitted {
        return Err(format!(
            "principalId {principal_id} is not 1 to {PRINCIPAL_ID_MAX_LENGTH} alphanumeric \
             characters"
        ));
    }
    Ok(Some(principal_id.to_string()))
}

/// `policyDocuments` of a returned document: "IAM policy documents", a list of them.
fn policy_documents(document: &Map<String, Value>) -> Result<Option<Vec<String>>, String> {
    let Some(value) = document.get("policyDocuments") else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let Some(entries) = value.as_array() else {
        return Err("policyDocuments is not a list".to_string());
    };
    let mut documents = Vec::with_capacity(entries.len());
    for entry in entries {
        let Some(document) = entry.as_str() else {
            return Err("policyDocuments holds an entry that is not a policy document".to_string());
        };
        documents.push(document.to_string());
    }
    Ok(Some(documents))
}

/// A `Seconds` member of a returned document, which is a 32-bit integer.
fn seconds(document: &Map<String, Value>, member: &str) -> Result<Option<i32>, String> {
    let Some(value) = document.get(member) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let seconds = value
        .as_i64()
        .and_then(|seconds| i32::try_from(seconds).ok())
        .ok_or_else(|| format!("{member} is not a number of seconds"))?;
    Ok(Some(seconds))
}

/// The ARN namespace, and the endpoint prefix, of AWS Lambda.
pub(crate) const LAMBDA_ARN_NAMESPACE: &str = "lambda";

/// The longest this service waits for an authorizer's function to answer.
///
/// The bound that really governs the run is the function's OWN configured timeout,
/// which AWS Lambda enforces and answers the invocation with: "The maximum allowed
/// value is 900 seconds" (`FunctionConfiguration$Timeout`). This ceiling sits a minute
/// above that maximum, so it never pre-empts a function AWS Lambda would still be
/// running, and an invocation that is never answered at all still leaves this operation
/// able to answer its caller instead of holding it for ever.
const INVOCATION_CEILING: ::std::time::Duration = ::std::time::Duration::from_secs(960);

/// What an invocation that was never answered inside [`INVOCATION_CEILING`] reports.
const INVOCATION_NOT_ANSWERED: &str = "the authorizer function did not answer";

/// Invoke the authorizer's Lambda function with `event` and hand back the payload it
/// returned, or why it produced no answer.
///
/// The invocation is SYNCHRONOUS: the authorization answer IS what the function
/// returns, so there is nothing to answer the caller with until it has returned. How
/// long that wait can be is the function's own configured timeout, which Lambda
/// enforces on the run — and [`INVOCATION_CEILING`] above it, so this operation always
/// answers.
///
/// The Region is the one the authorizer's own `authorizerFunctionArn` names, so a
/// function in another Region is reached through that Region's Lambda endpoint, and an
/// ARN that names no Region label falls back to the Region the request was signed for.
///
/// Call it with this service's state lock RELEASED: nothing here writes state, and
/// holding the lock across the invocation would make every other request of this
/// service wait on the function.
pub async fn invoke_authorizer_function(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    function_arn: &str,
    request_region: &str,
    event: &Value,
) -> Result<Vec<u8>, AuthorizerFailure> {
    let region = ::service_common::arn::region_of(function_arn, LAMBDA_ARN_NAMESPACE)
        .unwrap_or(request_region);
    let invocation = lambda_client(ctx, region)
        .invoke()
        .function_name(function_arn)
        .invocation_type(::aws_sdk_lambda::types::InvocationType::RequestResponse)
        .payload(::aws_sdk_lambda::primitives::Blob::new(
            event.to_string().into_bytes(),
        ))
        .send();
    let Ok(answer) = ::tokio::time::timeout(INVOCATION_CEILING, invocation).await else {
        return Err(AuthorizerFailure::NotInvoked(
            INVOCATION_NOT_ANSWERED.to_string(),
        ));
    };
    let output = match answer {
        Ok(output) => output,
        // Lambda refused the invocation — the authorizer names a function it does not
        // hold, or one it cannot run — so no function answered, and no answer is
        // invented for one.
        Err(error) => {
            return Err(AuthorizerFailure::NotInvoked(
                ::aws_smithy_types::error::metadata::ProvideErrorMetadata::message(&error)
                    .unwrap_or(INVOCATION_NOT_ANSWERED)
                    .to_string(),
            ));
        }
    };
    // "FunctionError: if present, indicates that an error occurred during function
    // execution. Details about the error are included in the response payload"
    // (Lambda's `Invoke` model), so a function that failed answered no authorization
    // answer either, and the error it reported is what says so.
    if let Some(function_error) = output.function_error() {
        let detail = reported_error(output.payload()).unwrap_or_else(|| function_error.to_string());
        return Err(AuthorizerFailure::AnsweredWrongly(detail));
    }
    Ok(output
        .payload()
        .map(|payload| payload.as_ref().to_vec())
        .unwrap_or_default())
}

/// The error a failed function reported, read from the `errorMessage` of the payload
/// Lambda returns for it. `None` for a payload that carries none.
fn reported_error(payload: Option<&::aws_sdk_lambda::primitives::Blob>) -> Option<String> {
    let parsed: Value = ::serde_json::from_slice(payload?.as_ref()).ok()?;
    Some(parsed.get("errorMessage")?.as_str()?.to_string())
}

/// This crate's ONE construction of the in-process AWS Lambda client, addressed to `region`, so
/// no two places in it configure that hop differently.
///
/// Both halves of this service that reach a function build it here: `TestInvokeAuthorizer`,
/// which RUNS the authorizer's own function, and the rules engine's `lambda` topic-rule action
/// ([`super::rules_engine::lambda_action`]), which invokes the function a rule names and reads
/// that function's resource policy. It lives beside the first of those rather than in
/// [`::service_common`] because that crate declares no Lambda SDK dependency; the copies of this
/// builder in other service crates are to be migrated onto one shared construction there, each
/// in the change that owns its service, exactly as `::service_common::kinesis_client` records
/// for the Kinesis client.
///
/// The transport is the emulator's own in-process callback
/// ([`smithy_server_runtime::OperationContext::callback`]), so a function this client addresses
/// is one this emulator holds: no socket is opened, no name is resolved, and nothing outside
/// this process is reachable from it. The endpoint URL is addressing for that transport rather
/// than a host this process connects to, and it is built from the Region alone — no part of a
/// caller's request is a destination. The credentials are the callback's own: a caller's
/// `Authorization` header is never forwarded.
///
/// Being in-process, it disables what a network client needs and this one cannot have: retries,
/// timeouts, stalled-stream protection and identity caching each need a sleep implementation the
/// callback transport is not given, and there is nothing to retry behind it anyway. A deadline
/// on one call is the caller's to apply.
pub(crate) fn lambda_client(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
) -> ::aws_sdk_lambda::Client {
    let config = ::aws_sdk_lambda::Config::builder()
        .behavior_version(::aws_sdk_lambda::config::BehaviorVersion::latest())
        .http_client(ctx.callback().http_client())
        .credentials_provider(ctx.callback().aws_credentials_provider())
        .region(::aws_sdk_lambda::config::Region::new(region.to_string()))
        .endpoint_url(format!(
            "https://{LAMBDA_ARN_NAMESPACE}.{region}.amazonaws.com"
        ))
        .retry_config(::aws_sdk_lambda::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_lambda::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_lambda::config::IdentityCache::no_cache())
        .stalled_stream_protection(
            ::aws_sdk_lambda::config::StalledStreamProtectionConfig::disabled(),
        )
        .build();
    ::aws_sdk_lambda::Client::from_conf(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A document that fills every response member is read as that answer, member for
    /// member.
    #[test]
    fn a_full_answer_is_read_member_for_member() {
        let payload = br#"{
            "isAuthenticated": true,
            "principalId": "principal01",
            "policyDocuments": ["{\"Version\":\"2012-10-17\"}"],
            "refreshAfterInSeconds": 300,
            "disconnectAfterInSeconds": 86400
        }"#;
        let verdict = read_verdict(payload).expect("the answer is an authorization answer");
        assert_eq!(
            verdict,
            AuthorizerVerdict {
                is_authenticated: true,
                principal_id: Some("principal01".to_string()),
                policy_documents: Some(vec!["{\"Version\":\"2012-10-17\"}".to_string()]),
                refresh_after_in_seconds: Some(300),
                disconnect_after_in_seconds: Some(86400),
            },
            "expected every member to come from the answered document, got {verdict:?}"
        );
    }

    /// What is NOT an authorization answer is reported as such, one row per way a
    /// document can fail to be one.
    #[test]
    fn a_document_that_is_not_an_answer_is_refused() {
        // (case, the document the function returned)
        let rows: [(&str, &[u8]); 6] = [
            ("not JSON at all", b"not json"),
            ("a JSON array", b"[]"),
            ("no isAuthenticated", br#"{"principalId": "p1"}"#),
            (
                "isAuthenticated as a string",
                br#"{"isAuthenticated": "true"}"#,
            ),
            (
                "a principalId outside the modelled pattern",
                br#"{"isAuthenticated": true, "principalId": "not a principal"}"#,
            ),
            (
                "policyDocuments holding a document object",
                br#"{"isAuthenticated": true, "policyDocuments": [{"Version": "2012-10-17"}]}"#,
            ),
        ];
        for (case, payload) in rows {
            let read = read_verdict(payload);
            assert!(
                read.is_err(),
                "case {case:?}: expected the document to be refused, got {read:?}"
            );
        }
    }

    /// Only the contexts a request carried are named as protocols, and the protocol
    /// data holds exactly those.
    #[test]
    fn the_event_names_the_protocols_the_request_carried() {
        let event = authorizer_event(
            Some("a-token"),
            true,
            Some(&HttpContext {
                headers: Some(
                    [("x-token".to_string(), "a-token".to_string())]
                        .into_iter()
                        .collect(),
                ),
                query_string: Some("?k=v".to_string()),
            }),
            None,
            Some(&TlsContext {
                server_name: Some("iot.example.test".to_string()),
            }),
            "connection-1",
        );
        assert_eq!(
            event["protocols"],
            json!(["tls", "http"]),
            "expected only the carried contexts' protocols, got {event}"
        );
        assert_eq!(
            event["protocolData"]["http"]["queryString"],
            json!("?k=v"),
            "expected the HTTP context's query string in the event, got {event}"
        );
        assert_eq!(
            event["protocolData"]["tls"]["serverName"],
            json!("iot.example.test"),
            "expected the TLS context's server name in the event, got {event}"
        );
        assert!(
            event["protocolData"].get("mqtt").is_none(),
            "expected no MQTT protocol data for a request that carried no MQTT context, got {event}"
        );
        assert_eq!(
            event["signatureVerified"],
            json!(true),
            "expected the signature verdict in the event, got {event}"
        );
    }

    /// An MQTT password is a blob, so it travels base-64 encoded.
    #[test]
    fn an_mqtt_password_travels_base64_encoded() {
        let event = authorizer_event(
            None,
            false,
            None,
            Some(&MqttContext {
                username: Some("device".to_string()),
                password: Some(b"password".to_vec()),
                client_id: Some("client-1".to_string()),
            }),
            None,
            "connection-2",
        );
        assert_eq!(
            event["protocolData"]["mqtt"]["password"],
            json!("cGFzc3dvcmQ="),
            "expected the password base-64 encoded in the event, got {event}"
        );
        assert!(
            event.get("token").is_none(),
            "expected no token member for a request that presented none, got {event}"
        );
    }

    /// A signature is checked against the authorizer's keys, and a key the library
    /// cannot read verifies nothing rather than deciding the whole check.
    #[test]
    fn an_unreadable_public_key_verifies_nothing() {
        let keys: BTreeMap<String, String> =
            [("key1".to_string(), "not a pem block".to_string())]
                .into_iter()
                .collect();
        assert!(
            !token_signature_verifies(&keys, "a-token", "c2lnbmF0dXJl"),
            "expected a key that cannot be read to verify nothing"
        );
        assert!(
            !token_signature_verifies(&BTreeMap::new(), "a-token", "c2lnbmF0dXJl"),
            "expected an authorizer with no registered keys to verify nothing"
        );
    }
}
