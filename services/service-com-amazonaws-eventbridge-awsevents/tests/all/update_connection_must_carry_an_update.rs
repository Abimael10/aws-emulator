// SPDX-License-Identifier: CC0-1.0

//! `UpdateConnection` has to be given something to write, end to end over the
//! wire (`awsJson1_1`, `POST /` with `X-Amz-Target: AWSEvents.<Operation>`)
//! through the real `MultiServiceRouter`.
//!
//! The RULE asserted here: `Name` is the only member `UpdateConnection`
//! requires, and every other member is something the update WRITES
//! (`smithy_query(query="operation",operation="UpdateConnection")`), with the
//! authorization counting only as the pair of an `AuthorizationType` — "the type
//! of authorization to use for the connection", the member that says which block
//! of `AuthParameters` carries the credentials in force — and that block. A
//! request carrying none of those alternatives asks for no change: it is refused
//! with HTTP 400 and the wire code `ValidationException`, under one FIXED
//! sentence that lists the alternatives and interpolates nothing from the
//! request — no connection name, no member name, no submitted value — and it
//! writes nothing, so the connection keeps the credentials, the timestamps and
//! the authorization state it had.
//!
//! The table spans the class rather than the one recorded case: the refused rows
//! differ along which member arrives on its own (none at all, each of the three
//! credential blocks without a type, a type without credentials), the
//! authorization type the connection already holds, and whether that connection
//! holds authorization or has had it stripped. The accepted rows are the other
//! side of the same rule: each member that IS an update, on its own.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_eventbridge_awsevents::state;
use service_com_amazonaws_eventbridge_awsevents::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region every request below is signed for.
const REGION: &str = "ap-south-1";

/// The authorization endpoint of the OAuth rows: an `https` URL this emulator
/// serves nothing at, which is not what these rows are about.
const OAUTH_ENDPOINT: &str = "https://authorization.example.com/oauth/token";

/// The whole refusal, as the rule says it reads: the clause naming the call that
/// failed, then the fixed sentence listing what an update has to carry.
const EXPECTED_REFUSAL: &str = "Failed to update the connection(s). At least one of the following \
                                is required: Description, KmsKeyIdentifier, AuthorizationType \
                                with BasicAuthParameters, ApiKeyAuthParameters, or \
                                OAuthParameters.";

/// `ConnectionAuthorizationType`'s values, from
/// `smithy_query(query="shape",shape="ConnectionAuthorizationType")`.
const AUTHORIZATION_TYPES: [&str; 3] = ["API_KEY", "BASIC", "OAUTH_CLIENT_CREDENTIALS"];

/// How long a test waits for a state the clock owes a connection before it calls
/// the wait itself the failure: generous, because the assertion is about the
/// state reached and not about how fast this host reaches it.
const SETTLED_BY: Duration = Duration::from_secs(60);

/// A VPC Lattice resource configuration ARN of the form
/// `ConnectivityResourceConfigurationArn$ResourceConfigurationArn` publishes
/// (`^arn:[a-z0-9\-]+:vpc-lattice:[a-zA-Z0-9\-]+:\d{12}:resourceconfiguration/rcfg-[0-9a-z]{17}$`).
const RESOURCE_CONFIGURATION_ARN: &str =
    "arn:aws:vpc-lattice:ap-south-1:123456789012:resourceconfiguration/rcfg-0a1b2c3d4e5f6a7b8";

/// The emulator: one router carrying only this service over one shared engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(engine));
    router
}

/// One `awsJson1_1` call to EventBridge over the router.
async fn events(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("events.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("AWSEvents.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTESTTESTTEST/20260101/{REGION}/events/\
                 aws4_request, SignedHeaders=host, Signature=0000"
            ),
        )
        .body(bytes::Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice::<Value>(&payload).unwrap_or_else(|error| {
            panic!(
                "{operation} answered a body that is not JSON ({error}): {:?}",
                String::from_utf8_lossy(&payload)
            )
        })
    };
    (status, parsed)
}

/// The wire error code (`__type`), without its namespace; the empty string on a
/// success.
fn error_code(body: &Value) -> &str {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .rsplit('#')
        .next()
        .unwrap_or_default()
}

/// The `message` an error body carries.
fn message(body: &Value) -> &str {
    body.get("message")
        .or_else(|| body.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
}

/// The `AuthParameters` block each `ConnectionAuthorizationType` carries its
/// credentials in, as
/// `smithy_query(query="shape",shape="UpdateConnectionAuthRequestParameters")`
/// names the member of each type.
fn auth_parameters(authorization_type: &str, value: &str) -> Value {
    match authorization_type {
        "API_KEY" => json!({
            "ApiKeyAuthParameters": { "ApiKeyName": value, "ApiKeyValue": "sekrit-key" },
        }),
        "BASIC" => json!({
            "BasicAuthParameters": { "Username": value, "Password": "sekrit-password" },
        }),
        _ => json!({
            "OAuthParameters": {
                "ClientParameters": { "ClientID": value, "ClientSecret": "sekrit-secret" },
                "AuthorizationEndpoint": OAUTH_ENDPOINT,
                "HttpMethod": "POST",
            },
        }),
    }
}

/// `DescribeConnection`, which must answer.
async fn read_connection(router: &MultiServiceRouter, case: &str, name: &str) -> Value {
    let (status, body) = events(router, "DescribeConnection", &json!({ "Name": name })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeConnection answered {status} {body}"
    );
    body
}

/// Read the connection until it reports `wanted`, looking again until it does or
/// [`SETTLED_BY`] passes: what is waited for is the STATE, never a span of the
/// clock.
async fn connection_in_state(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    wanted: &str,
) -> Value {
    let deadline = Instant::now() + SETTLED_BY;
    loop {
        let read = read_connection(router, case, name).await;
        if read["ConnectionState"] == json!(wanted) {
            return read;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: connection {name:?} never reached {wanted}, last read {read}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A settled connection of `authorization_type`, deauthorized first when the row
/// asks for a connection whose authorization was stripped.
async fn given_connection(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    authorization_type: &str,
    deauthorized: bool,
) -> Value {
    let (status, body) = events(
        router,
        "CreateConnection",
        &json!({
            "Name": name,
            "AuthorizationType": authorization_type,
            "AuthParameters": auth_parameters(authorization_type, "the-stored-credential"),
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateConnection answered {status} {body}"
    );
    // An OAuth connection obtains a token at an endpoint nothing answers at, so
    // the state it settles in is the deauthorized one; the other two hold
    // credentials that ARE the authorization.
    let settled = if authorization_type == "OAUTH_CLIENT_CREDENTIALS" {
        state::CONNECTION_STATE_DEAUTHORIZED
    } else {
        state::CONNECTION_STATE_AUTHORIZED
    };
    connection_in_state(router, case, name, settled).await;
    if deauthorized {
        let (status, body) =
            events(router, "DeauthorizeConnection", &json!({ "Name": name })).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DeauthorizeConnection answered {status} {body}"
        );
        connection_in_state(
            router,
            case,
            name,
            state::CONNECTION_STATE_DEAUTHORIZED,
        )
        .await
    } else {
        read_connection(router, case, name).await
    }
}

/// One refused row: the case, the type the connection already holds, whether its
/// authorization was stripped first, and the update members the request carries
/// beside `Name`.
type RefusedRow = (String, &'static str, bool, Value);

/// An `UpdateConnection` carrying nothing to write is refused with the fixed
/// sentence and writes nothing.
#[tokio::test]
async fn an_update_that_carries_no_change_is_refused_and_writes_nothing() {
    let mut rows: Vec<RefusedRow> = vec![
        (
            "nothing but the connection's name".to_string(),
            "API_KEY",
            false,
            json!({}),
        ),
        (
            "nothing but the name, on a connection whose authorization was stripped".to_string(),
            "BASIC",
            true,
            json!({}),
        ),
        (
            "credentials of the type the connection already holds, with no type to select them"
                .to_string(),
            "API_KEY",
            false,
            json!({ "AuthParameters": auth_parameters("API_KEY", "the-submitted-credential") }),
        ),
        (
            "basic credentials with no type to select them, after a deauthorize".to_string(),
            "BASIC",
            true,
            json!({ "AuthParameters": auth_parameters("BASIC", "the-submitted-credential") }),
        ),
        (
            "oauth client parameters with no type to select them".to_string(),
            "BASIC",
            false,
            json!({
                "AuthParameters":
                    auth_parameters("OAUTH_CLIENT_CREDENTIALS", "the-submitted-credential"),
            }),
        ),
    ];
    // A type with no credentials of that type is the other half of the pair
    // missing: asserted for EVERY value of the enum, the type the connection
    // already holds included.
    for authorization_type in AUTHORIZATION_TYPES {
        rows.push((
            format!("the authorization type {authorization_type} with no credentials for it"),
            "API_KEY",
            false,
            json!({ "AuthorizationType": authorization_type }),
        ));
    }

    for (case, held_type, deauthorized, update) in rows {
        let router = emulator();
        let name = "the-connection";
        let case = case.as_str();
        let before = given_connection(&router, case, name, held_type, deauthorized).await;

        let mut request = update.clone();
        request["Name"] = json!(name);
        let (status, body) = events(&router, "UpdateConnection", &request).await;
        assert_eq!(
            status, 400,
            "case {case:?}: an update carrying no change is refused, got {status} {body}"
        );
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {body}"
        );
        assert_eq!(
            message(&body),
            EXPECTED_REFUSAL,
            "case {case:?}: expected {EXPECTED_REFUSAL:?}, got {:?}",
            message(&body)
        );
        // The sentence is FIXED: no name, member or value of the request is
        // interpolated into it, and it diagnoses no single parameter.
        for named in [name, "the-submitted-credential", "is not valid"] {
            assert!(
                !message(&body).contains(named),
                "case {case:?}: the refusal must not name {named:?}, got {:?}",
                message(&body)
            );
        }

        // The refused update wrote nothing: the connection is the one the read
        // before it reported, authorization state, credentials and timestamps
        // included.
        let after = read_connection(&router, case, name).await;
        assert_eq!(
            after, before,
            "case {case:?}: a refused update must leave the connection exactly as it was, got \
             {after}"
        );
    }
}

/// One accepted row: the case, the type the connection already holds, the update
/// members the request carries beside `Name`, and what a read must then report —
/// each as the path into `DescribeConnection`'s body and the value at it. A row
/// whose member no read reports back carries no expectation.
type AcceptedRow = (
    &'static str,
    &'static str,
    Value,
    Vec<(Vec<&'static str>, Value)>,
);

/// The key the kms row names: an ARN form, which `KmsKeyIdentifier`'s pattern
/// (`^[a-zA-Z0-9_\-/:]*$`) admits and a read reports back as it arrived.
const KEY_ARN: &str = "arn:aws:kms:ap-south-1:123456789012:key/the-connection-key";

/// The value at `path` in a read's body.
fn reported(read: &Value, path: &[&str]) -> Value {
    let mut cursor = read;
    for step in path {
        cursor = &cursor[*step];
    }
    cursor.clone()
}

/// Each member that IS an update is accepted on its own, and lands.
#[tokio::test]
async fn an_update_carrying_one_thing_to_write_is_accepted() {
    let mut rows: Vec<AcceptedRow> = vec![
        (
            "a description alone",
            "API_KEY",
            json!({ "Description": "the updated connection" }),
            vec![(vec!["Description"], json!("the updated connection"))],
        ),
        (
            "a kms key identifier alone",
            "BASIC",
            json!({ "KmsKeyIdentifier": KEY_ARN }),
            vec![(vec!["KmsKeyIdentifier"], json!(KEY_ARN))],
        ),
        (
            // The parameters for invoking a private API: written by this call,
            // and reported back by no read of this service, so this row asserts
            // that it is an update and nothing more.
            "connectivity parameters alone",
            "API_KEY",
            json!({
                "InvocationConnectivityParameters": {
                    "ResourceParameters": {
                        "ResourceConfigurationArn": RESOURCE_CONFIGURATION_ARN,
                    },
                },
            }),
            vec![],
        ),
    ];
    // The authorization as the PAIR: a type together with the block that carries
    // its credentials, for every value of the enum.
    for authorization_type in AUTHORIZATION_TYPES {
        rows.push((
            "an authorization type with the credentials it selects",
            "API_KEY",
            json!({
                "AuthorizationType": authorization_type,
                "AuthParameters": auth_parameters(authorization_type, "the-submitted-credential"),
            }),
            vec![
                (vec!["AuthorizationType"], json!(authorization_type)),
                (
                    reported_credential_path(authorization_type),
                    json!("the-submitted-credential"),
                ),
            ],
        ));
    }

    for (case, held_type, update, expected) in rows {
        let router = emulator();
        let name = "the-connection";
        let before = given_connection(&router, case, name, held_type, false).await;

        let mut request = update.clone();
        request["Name"] = json!(name);
        let (status, body) = events(&router, "UpdateConnection", &request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: an update carrying a change is accepted, got {status} {body}"
        );
        assert_eq!(
            body["ConnectionArn"], before["ConnectionArn"],
            "case {case:?}: an updated connection keeps its ARN, got {body}"
        );
        // Read back through the implemented reader: what the update carried is
        // what a later caller sees.
        let after = read_connection(&router, case, name).await;
        for (path, value) in expected {
            assert_eq!(
                reported(&after, &path),
                value,
                "case {case:?}: expected {value} at {path:?}, got {}",
                reported(&after, &path)
            );
        }
    }
}

/// The member path a read reports the non-secret half of each type's credentials
/// at, as `DescribeConnectionOutput$AuthParameters` carries them.
fn reported_credential_path(authorization_type: &str) -> Vec<&'static str> {
    match authorization_type {
        "API_KEY" => vec!["AuthParameters", "ApiKeyAuthParameters", "ApiKeyName"],
        "BASIC" => vec!["AuthParameters", "BasicAuthParameters", "Username"],
        _ => vec![
            "AuthParameters",
            "OAuthParameters",
            "ClientParameters",
            "ClientID",
        ],
    }
}
