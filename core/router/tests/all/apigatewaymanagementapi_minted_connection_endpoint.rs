// SPDX-License-Identifier: CC0-1.0

//! A client that completes an RFC 6455 handshake against a deployed WEBSOCKET stage, and then
//! addresses the connection that handshake minted over `@connections` — through the WHOLE
//! assembled router, the two services on one engine.
//!
//! The rule the tables below are slices of, one level above the recorded case (one api, one
//! stage named `dev`, one probe id):
//!
//! > A deployed WEBSOCKET stage answers a correct opening handshake `101 Switching Protocols`
//! > with `Upgrade: websocket`, `Connection: Upgrade` and the `Sec-WebSocket-Accept` the
//! > client's own key derives, and MINTS a connection the `@connections` management API serves
//! > at the owning api's invoke endpoint (`…/{stage}/@connections/{id}`). What each of the
//! > three operations answers there is decided by what the REQUEST carries, in this order: a
//! > stage the api has not deployed serves nothing (`403 ForbiddenException`, "Forbidden"); a
//! > `Data` payload past the modelled maximum is refused on its size alone, before the
//! > `ConnectionId` is read and before the registry is consulted (a bare `413`); a
//! > `ConnectionId` whose FORM is not one API Gateway could have minted is refused `400
//! > BadRequestException` naming the submitted value, before the registry is consulted; and
//! > any other id is looked up, where a hit is the modelled `200`/`200`/`204` and a miss —
//! > never minted, minted by another process, or since closed — is `410 GoneException`, which
//! > is memberless and so carries no message at all.
//!
//! WHY the split is a property of the submitted VALUE and never of what THIS process minted:
//! `410` is the "this peer went away" signal a fan-out application prunes its own record on,
//! and `400` says the request itself is wrong, so an id a sibling process minted or a client
//! kept across a restart — one of the form API Gateway issues that nothing here holds — has to
//! be the `410`, or prune logic never fires against the emulator and fires in production.
//!
//! The rows span what the rule generalises over: the `Sec-WebSocket-Key` the client offers, the
//! field spellings RFC 6455 §4.1 allows a client and whether its handshake names a user agent at
//! all, the stage name, the id's provenance (minted by this socket, of the issued form but held
//! by nothing, and refused by alphabet, padding, length and the format marker), the payload size
//! (empty, short, exactly the modelled maximum, one byte past it), the payload's bytes (text and
//! non-UTF-8), the operation (all three) and the stage's state (deployed, never deployed).
//!
//! Two services, one engine, one router, so this is a router suite: the handshake and the
//! stage are API Gateway V2's (signing name `apigateway`), and the `@connections` answers are
//! the API Gateway Management API's (signing name `execute-api`).
//!
//! Assembling the router costs stack, so each check runs on an explicitly sized thread for the
//! same reason `boot_serve.rs` does.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use aws_emulator_router::{build_router, InMemoryEngine};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use service_com_amazonaws_apigatewaymanagementapi_apigatewaymanagementapi as mgmt;

/// Keep this at or above the size `boot_serve.rs` uses.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The region every request below is signed for, and the one the invoke endpoint names.
const REGION: &str = "us-east-1";

/// A SigV4 credential scope naming the API Gateway control plane's signing name, which the V2
/// endpoint shares. The signature itself is never checked here.
const CONTROL_PLANE_AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/us-east-1/apigateway/aws4_request, \
     SignedHeaders=host, Signature=0123456789abcdef";

/// A SigV4 credential scope naming the `execute-api` signing name an `@connections` caller
/// signs with.
const EXECUTE_API_AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/us-east-1/execute-api/aws4_request, \
     SignedHeaders=host, Signature=0123456789abcdef";

/// The handshake RFC 6455 §1.3 publishes as its own worked example: the key a client offers,
/// and the accept value a server owes it. Taken from the specification rather than computed
/// here, so the answer is checked against a published value.
const RFC_EXAMPLE_KEY: &str = "dGhlIHNhbXBsZSBub25jZQ==";
const RFC_EXAMPLE_ACCEPT: &str = "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=";

/// The status line RFC 6455 §4.2.2 requires of a completed handshake.
const SWITCHING_PROTOCOLS_LINE: &str = "HTTP/1.1 101 Switching Protocols";

/// The loopback host the core binds a resource listener on, and so the only address a case
/// here ever dials: the emulator's own listener on this machine and nothing else.
const LOOPBACK_HOST: &str = "127.0.0.1";

/// The `SourceIp` a connection dialled from this machine is registered with.
const LOOPBACK_SOURCE_IP: &str = "127.0.0.1";

/// The largest `Data` payload `PostToConnection` accepts, read off the owning crate's constant
/// (the model's `@length(min: 0, max: 131072)` on `Data`) so the two cannot disagree.
const MAX_DATA_BYTES: usize = mgmt::ops::post_to_connection::MAX_DATA_BYTES;

/// How long a case waits for the socket to answer a handshake it has written, and for the
/// connection that handshake minted to appear in the registry: generous, because neither wait
/// is a limit any case tests — a loaded host may schedule the serving task late — and bounded
/// so a case that will never be answered fails instead of hanging the suite. Both wait for a
/// CONDITION, and nothing here asserts on the clock.
const ANSWER_DEADLINE: Duration = Duration::from_secs(120);

/// How often a case looks again for a condition that is already owed.
const LOOK_INTERVAL: Duration = Duration::from_millis(25);

/// How many bytes of response head a case reads before giving up: far above the few hundred of
/// a server's handshake.
const MAX_HEAD_BYTES: usize = 16 * 1024;

/// Run one check on the assembled emulator: the whole router over one engine, on a thread
/// sized for it, with request authentication left to the emulator rather than to the check.
fn on_the_assembled_emulator(
    name: &'static str,
    body: impl FnOnce(MultiServiceRouter, Arc<InMemoryEngine>) -> Pin<Box<dyn Future<Output = ()>>>
        + Send
        + 'static,
) {
    std::thread::Builder::new()
        .name(name.to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(move || {
            // `enable_all` for the io a WEBSOCKET api's own socket needs — creating one opens a
            // listener — and the timers the waits below use.
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build a current-thread runtime");
            runtime.block_on(async move {
                let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
                let mut router = build_router(Arc::clone(&engine));
                router.set_request_authentication(false);
                body(router, engine).await;
            });
        })
        .expect("spawn the suite thread")
        .join()
        .unwrap_or_else(|_| panic!("the {name} check must pass"));
}

/// One API Gateway V2 control-plane call, as JSON.
async fn control_plane(
    router: &MultiServiceRouter,
    case: &str,
    what: &str,
    method: &str,
    path: &str,
    body: &Value,
    expected_status: u16,
) -> Value {
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://apigateway.{REGION}.amazonaws.com{path}"))
        .header("host", format!("apigateway.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", CONTROL_PLANE_AUTHORIZATION)
        .body(bytes::Bytes::from(body.to_string().into_bytes()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body().as_ref()).to_string();
    assert_eq!(
        status, expected_status,
        "case {case:?}: expected HTTP {expected_status} from {what}, got {status} {text}"
    );
    serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("case {case:?}: {what} must answer JSON ({error}): {text}"))
}

/// A required string member of a response body.
fn as_str(case: &str, what: &str, body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| panic!("case {case:?}: {what} must carry {member:?}, got {body}"))
}

/// One deployed WEBSOCKET api: the id its invoke endpoint is named after, and the `host:port`
/// of the socket it announced as its `ApiEndpoint`.
struct Deployed {
    api_id: String,
    authority: String,
}

/// Create a WEBSOCKET api with a `$default` route over one MOCK integration, release a
/// deployment to `stage`, and answer the socket the api announced.
///
/// The integration's response renders `$context.connectionId`, which is what AWS gives a
/// backend for the connection — the same id the `@connections` calls below address.
async fn given_deployed_api(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    stage: &str,
) -> Deployed {
    let created = control_plane(
        router,
        case,
        "CreateApi",
        "POST",
        "/v2/apis",
        &json!({
            "name": name,
            "protocolType": "WEBSOCKET",
            "routeSelectionExpression": "$request.body.action"
        }),
        201,
    )
    .await;
    let api_id = as_str(case, "CreateApi", &created, "apiId");
    let announced = as_str(case, "CreateApi", &created, "apiEndpoint");
    let authority = announced
        .strip_prefix("wss://")
        .unwrap_or_else(|| {
            panic!("case {case:?}: a WEBSOCKET api must announce a wss:// socket, got {announced:?}")
        })
        .to_string();

    let integration = control_plane(
        router,
        case,
        "CreateIntegration",
        "POST",
        &format!("/v2/apis/{api_id}/integrations"),
        &json!({
            "integrationType": "MOCK",
            "requestTemplates": { "$default": "{\"statusCode\": 200}" }
        }),
        201,
    )
    .await;
    let integration_id = as_str(case, "CreateIntegration", &integration, "integrationId");
    control_plane(
        router,
        case,
        "CreateIntegrationResponse",
        "POST",
        &format!("/v2/apis/{api_id}/integrations/{integration_id}/integrationresponses"),
        &json!({
            "integrationResponseKey": "$default",
            "responseTemplates": { "$default": "$context.connectionId" }
        }),
        201,
    )
    .await;

    // The `$default` route, and deliberately no `$connect` route: what a connection REQUEST
    // is decided by is the subject of the apigatewayv2 crate's own
    // `websocket_transport_data_plane` suite — a MOCK integration gives that request no
    // decision, so an api whose `$connect` route fronts one is refused — while the subject
    // here is what follows a handshake that WAS switched.
    control_plane(
        router,
        case,
        "CreateRoute",
        "POST",
        &format!("/v2/apis/{api_id}/routes"),
        &json!({
            "routeKey": "$default",
            "target": format!("integrations/{integration_id}")
        }),
        201,
    )
    .await;

    control_plane(
        router,
        case,
        "CreateStage",
        "POST",
        &format!("/v2/apis/{api_id}/stages"),
        &json!({ "stageName": stage }),
        201,
    )
    .await;
    control_plane(
        router,
        case,
        "CreateDeployment",
        "POST",
        &format!("/v2/apis/{api_id}/deployments"),
        &json!({ "stageName": stage }),
        201,
    )
    .await;

    Deployed { api_id, authority }
}

/// The head the socket answered one handshake with, as the client read it off the wire.
struct Head {
    status: u16,
    /// The status line as written.
    line: String,
    /// Every field, under its lower-cased name.
    fields: BTreeMap<String, String>,
}

impl Head {
    /// The value the answer carries for `name`, read case-insensitively as a field name is.
    fn field(&self, name: &str) -> &str {
        self.fields
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
            .unwrap_or_default()
    }
}

/// The port of the api's announced socket, having checked the address is [`LOOPBACK_HOST`].
fn loopback_port(case: &str, authority: &str) -> u16 {
    let (host, port) = authority
        .rsplit_once(':')
        .unwrap_or_else(|| panic!("case {case:?}: expected a host:port, got {authority:?}"));
    assert_eq!(
        host, LOOPBACK_HOST,
        "case {case:?}: a case dials only the emulator's own loopback listener, got \
         {authority:?}"
    );
    port.parse()
        .unwrap_or_else(|_| panic!("case {case:?}: expected a port, got {authority:?}"))
}

/// Write one opening handshake to the api's announced socket as raw bytes and read the head it
/// is answered with, keeping the socket open — the connection it minted lives as long as it.
///
/// The bytes are written and read directly rather than through a client library because what
/// this suite pins is the head itself: a library checks the accept value and then hides it.
async fn opening_handshake(case: &str, authority: &str, stage: &str, client: &Client) -> (TcpStream, Head) {
    let port = loopback_port(case, authority);
    // The literal loopback address, on this line: [`loopback_port`] has already checked the
    // announced authority IS that address, so this dial reaches the emulator's own listener on
    // this machine and nothing else.
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap_or_else(|error| {
            panic!(
                "case {case:?}: the announced socket {authority:?} must accept a connection, got \
                 {error}"
            )
        });
    let mut request = format!("GET /{stage} HTTP/1.1\r\n");
    let mut fields: Vec<(&str, &str)> = vec![
        ("Host", authority),
        ("Connection", client.connection),
        ("Upgrade", client.upgrade),
        (client.key_field, client.key),
        ("Sec-WebSocket-Version", "13"),
    ];
    if let Some(user_agent) = client.user_agent {
        fields.push(("User-Agent", user_agent));
    }
    for (name, value) in fields {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: the handshake must be sent, got {error}"));
    stream
        .flush()
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: the handshake must be flushed, got {error}"));

    let head = tokio::time::timeout(ANSWER_DEADLINE, read_head(case, &mut stream))
        .await
        .unwrap_or_else(|_elapsed| {
            panic!(
                "case {case:?}: the opening handshake must be answered — a client that reads no \
                 status at all cannot tell a refusal from an endpoint that serves nothing"
            )
        });
    let line = head
        .lines()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: expected a status line, got {head:?}"))
        .to_string();
    let status = line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or_else(|| panic!("case {case:?}: expected a status in {line:?}"));
    let mut fields: BTreeMap<String, String> = BTreeMap::new();
    for field in head.lines().skip(1) {
        if let Some((name, value)) = field.split_once(':') {
            fields.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }
    (stream, Head { status, line, fields })
}

/// Read bytes until the response HEAD is complete, and answer it as text: a completed
/// handshake is followed by frames rather than by a body, so reading stops at the blank line
/// that ends the head (RFC 9112 §2.1).
async fn read_head(case: &str, stream: &mut TcpStream) -> String {
    /// The blank line that ends an HTTP response head.
    const HEAD_TERMINATOR: &[u8] = b"\r\n\r\n";
    let mut bytes: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 512];
    loop {
        let read = stream
            .read(&mut chunk)
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: the answer must be readable: {error}"));
        assert!(
            read > 0,
            "case {case:?}: the socket closed before answering the handshake, got {:?}",
            String::from_utf8_lossy(&bytes)
        );
        bytes.extend_from_slice(chunk.get(..read).unwrap_or_default());
        if let Some(end) = bytes
            .windows(HEAD_TERMINATOR.len())
            .position(|window| window == HEAD_TERMINATOR)
        {
            return String::from_utf8_lossy(bytes.get(..end).unwrap_or_default()).to_string();
        }
        assert!(
            bytes.len() < MAX_HEAD_BYTES,
            "case {case:?}: the socket wrote {} bytes without ending a response head",
            bytes.len()
        );
    }
}

/// How many bytes the nonce a client offers carries: "a base64-encoded value that, when
/// decoded, is 16 bytes in length" (RFC 6455 §4.1).
const CLIENT_NONCE_BYTES: usize = 16;

/// How many bytes the accept value decodes to: the length of the digest RFC 6455 §1.3 derives
/// it from, so an accept of any other length cannot be that digest at all.
const ACCEPT_DIGEST_BYTES: usize = 20;

/// Assert `head` is the answer RFC 6455 owes the client's OWN key, and nothing less: the status
/// line, both upgrade fields, and an accept value that is the digest the key derives — compared
/// against the value the specification publishes where it publishes one, and otherwise read for
/// what it must be whatever the key: a digest of its own length, and never the value a
/// DIFFERENT key owes, which is what tells a derived accept from a remembered one.
fn switched(case: &str, head: &Head, client: &Client) {
    assert_eq!(
        head.status, 101,
        "case {case:?}: a correct handshake against a deployed stage must be switched, got \
         {status} with head {line:?} {fields:?}",
        status = head.status,
        line = head.line,
        fields = head.fields
    );
    assert_eq!(
        head.line, SWITCHING_PROTOCOLS_LINE,
        "case {case:?}: expected the status line {SWITCHING_PROTOCOLS_LINE:?}, got {:?}",
        head.line
    );
    let upgrade = head.field("upgrade");
    assert!(
        upgrade.eq_ignore_ascii_case("websocket"),
        "case {case:?}: expected the Upgrade field to name websocket, got {upgrade:?}"
    );
    let connection = head.field("connection");
    assert!(
        connection.eq_ignore_ascii_case("upgrade"),
        "case {case:?}: expected the Connection field to name Upgrade, got {connection:?}"
    );
    let accept = head.field("sec-websocket-accept");
    match client.published_accept {
        Some(published) => assert_eq!(
            accept, published,
            "case {case:?}: expected the accept RFC 6455 publishes for the offered key {:?}, got \
             {accept:?}",
            client.key
        ),
        None => {
            assert_ne!(
                accept, RFC_EXAMPLE_ACCEPT,
                "case {case:?}: the key {:?} is not the specification's example key, so its \
                 accept cannot be the value that key owes: an accept that does not follow the \
                 key is no accept at all",
                client.key
            );
            let digest = aws_smithy_types::base64::decode(accept).unwrap_or_else(|error| {
                panic!("case {case:?}: the accept must be base64, got {accept:?}: {error}")
            });
            assert_eq!(
                digest.len(),
                ACCEPT_DIGEST_BYTES,
                "case {case:?}: the accept must be the base64 of the digest the key derives, so \
                 {ACCEPT_DIGEST_BYTES} bytes, got {} from {accept:?}",
                digest.len()
            );
        }
    }
}

/// The `ConnectionId` of every connection the registry holds for the api reached at
/// `authority` — the connections that api's own socket minted.
fn registered_at(engine: &Arc<InMemoryEngine>, authority: &str) -> Vec<String> {
    let registry =
        mgmt::state::registry(&**engine).expect("the engine carries the connection registry");
    let mut ids: Vec<String> = registry
        .lock()
        .connections
        .values()
        .filter(|connection| connection.endpoint.as_deref() == Some(authority))
        .map(|connection| connection.connection_id.clone())
        .collect();
    ids.sort();
    ids
}

/// The `ConnectionId` the handshake against `authority` minted, waited for because the
/// registration is owed by a handshake that has ALREADY been answered rather than by any
/// clock: this looks again until the registry holds it.
async fn minted_at(case: &str, engine: &Arc<InMemoryEngine>, authority: &str) -> String {
    let deadline = Instant::now() + ANSWER_DEADLINE;
    loop {
        let ids = registered_at(engine, authority);
        if let Some(id) = ids.first() {
            assert_eq!(
                ids.len(),
                1,
                "case {case:?}: one handshake must mint exactly one connection, got {ids:?}"
            );
            return id.clone();
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the completed handshake must register its connection, so \
             @connections can resolve it; the registry holds none for {authority:?}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// What one `@connections` call was answered: the status, the error SHAPE a restJson1 client
/// resolves from `x-amzn-errortype`, and the body.
struct Answer {
    status: u16,
    /// The `x-amzn-errortype` the answer named, or `"<absent>"`.
    error_type: String,
    body: Vec<u8>,
}

impl Answer {
    /// The `message` member the body carries, or `"<no message>"` — which is what a memberless
    /// error shape's body answers, since there is nothing to serialize a sentence into.
    fn message(&self) -> String {
        self.json()
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("<no message>")
            .to_string()
    }

    /// The body as JSON, or `Value::Null` when it is not JSON at all.
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    /// The body rendered for an assertion message.
    fn rendered(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }
}

/// The URI one `@connections` call is sent to: the owning api's invoke endpoint, the stage,
/// and the `ConnectionId` as a path SEGMENT.
///
/// Built by the maintained URL implementation rather than by formatting the path here, because
/// a minted `ConnectionId` is standard base64, whose alphabet carries `/`: the segment has to
/// be encoded, and which characters that takes is that crate's to decide.
fn connections_uri(host: &str, stage: &str, connection_id: &str) -> String {
    let mut url =
        reqwest::Url::parse(&format!("https://{host}/")).expect("the invoke endpoint is a URL");
    url.path_segments_mut()
        .expect("an https URL has path segments")
        .pop_if_empty()
        .push(stage)
        .push("@connections")
        .push(connection_id);
    url.to_string()
}

/// Make one `@connections` call at the owning api's INVOKE ENDPOINT, as a backend's SDK client
/// configured with that endpoint URL does: `…/{stage}/@connections/{id}`, signed for
/// `execute-api`.
async fn connections_call(
    router: &MultiServiceRouter,
    api_id: &str,
    stage: &str,
    method: &str,
    connection_id: &str,
    body: Vec<u8>,
) -> Answer {
    let host = format!("{api_id}.execute-api.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(connections_uri(&host, stage, connection_id))
        .header("host", &host)
        .header("authorization", EXECUTE_API_AUTHORIZATION)
        .body(bytes::Bytes::from(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let error_type = response
        .headers()
        .get("x-amzn-errortype")
        .map(|value| String::from_utf8_lossy(value.as_bytes()).to_string())
        .unwrap_or_else(|| "<absent>".to_string());
    Answer {
        status,
        error_type,
        body: response.into_body().to_vec(),
    }
}

/// One client's opening handshake: the key it offers, the spellings it writes the upgrade
/// fields in, and the `User-Agent` it names itself with where it names one.
struct Client {
    /// The `Sec-WebSocket-Key` the client offers: "a base64-encoded value that, when decoded,
    /// is 16 bytes in length" (RFC 6455 §4.1).
    key: &'static str,
    /// The accept value RFC 6455 publishes for that key, where it publishes one.
    published_accept: Option<&'static str>,
    /// The `Connection` field's value, as written.
    connection: &'static str,
    /// The `Upgrade` field's value, as written.
    upgrade: &'static str,
    /// The spelling of the key field's NAME, which a server reads case-insensitively.
    key_field: &'static str,
    user_agent: Option<&'static str>,
}

/// One row of the lifecycle table: the client, the stage it dials, and the payload pushed to
/// the connection it opened.
struct Row {
    case: &'static str,
    stage: &'static str,
    client: Client,
    payload: &'static [u8],
}

/// The rows differ along the key the client offers, the field spellings RFC 6455 §4.1 allows a
/// client, whether the handshake names a user agent, the stage name, and the payload's size
/// and bytes.
const ROWS: &[Row] = &[
    Row {
        case: "the spellings RFC 6455 writes its own example in, and a short text payload",
        stage: "dev",
        client: Client {
            key: RFC_EXAMPLE_KEY,
            published_accept: Some(RFC_EXAMPLE_ACCEPT),
            connection: "Upgrade",
            upgrade: "websocket",
            key_field: "Sec-WebSocket-Key",
            user_agent: Some("fan-out-client/1.0"),
        },
        payload: b"hello",
    },
    Row {
        case: "a client offering a nonce of its own, which the accept must follow",
        stage: "another-nonce",
        client: Client {
            // The base64 of the sixteen bytes 0x00..=0x0f; the check below decodes it to
            // confirm the row really offers a nonce of the length RFC 6455 §4.1 requires.
            key: "AAECAwQFBgcICQoLDA0ODw==",
            published_accept: None,
            connection: "Upgrade",
            upgrade: "websocket",
            key_field: "Sec-WebSocket-Key",
            user_agent: Some("fan-out-client/2.0"),
        },
        payload: b"a second nonce",
    },
    Row {
        case: "a client that writes keep-alive beside the upgrade token, names no user agent, \
               and pushes an empty payload",
        stage: "prod",
        client: Client {
            key: RFC_EXAMPLE_KEY,
            published_accept: Some(RFC_EXAMPLE_ACCEPT),
            connection: "keep-alive, Upgrade",
            upgrade: "WebSocket",
            key_field: "sec-websocket-key",
            user_agent: None,
        },
        payload: b"",
    },
    Row {
        case: "a lower-cased upgrade token, a stage name carrying a hyphen and a payload that \
               is not text",
        stage: "beta-1",
        client: Client {
            key: RFC_EXAMPLE_KEY,
            published_accept: Some(RFC_EXAMPLE_ACCEPT),
            connection: "upgrade",
            upgrade: "websocket",
            key_field: "Sec-WebSocket-KEY",
            user_agent: Some("agent/2"),
        },
        payload: &[0x00, 0xff, 0x10, 0x80],
    },
];

/// The whole journey of the client this issue is about: the handshake is switched, the
/// connection it minted is of the form API Gateway issues, the `@connections` endpoint of that
/// api REPORTS it, a push is DELIVERED to it, and deleting it makes every operation answer the
/// memberless `GoneException`.
#[test]
fn a_switched_handshake_mints_a_connection_the_connections_endpoint_serves() {
    on_the_assembled_emulator("minted-connection-lifecycle", |router, engine| {
        Box::pin(async move {
            for (index, row) in ROWS.iter().enumerate() {
                let case = row.case;
                let api = given_deployed_api(
                    &router,
                    case,
                    &format!("minted-connection-{index}"),
                    row.stage,
                )
                .await;

                // 1. The handshake itself, read off the wire. Each row's own key is checked to
                //    be a nonce of the length RFC 6455 §4.1 requires, so the accept below is
                //    judged against a key a client could really have offered.
                let nonce = aws_smithy_types::base64::decode(row.client.key)
                    .unwrap_or_else(|error| {
                        panic!(
                            "case {case:?}: the offered key must be base64, got {:?}: {error}",
                            row.client.key
                        )
                    });
                assert_eq!(
                    nonce.len(),
                    CLIENT_NONCE_BYTES,
                    "case {case:?}: a Sec-WebSocket-Key carries a nonce of \
                     {CLIENT_NONCE_BYTES} bytes, got {}",
                    nonce.len()
                );
                let (_socket, head) =
                    opening_handshake(case, &api.authority, row.stage, &row.client).await;
                switched(case, &head, &row.client);

                // 2. The connection it minted, which is what makes the live half of
                //    `@connections` reachable at all.
                let connection_id = minted_at(case, &engine, &api.authority).await;
                assert!(
                    mgmt::state::is_well_formed_connection_id(&connection_id),
                    "case {case:?}: a minted id must be of the form API Gateway issues, got \
                     {connection_id:?}"
                );

                // 3. `GetConnection` at the api's own invoke endpoint reports that record: the
                //    identity its handshake arrived with, and a `LastActiveAt` no read moves.
                let reported =
                    connections_call(&router, &api.api_id, row.stage, "GET", &connection_id, Vec::new())
                        .await;
                assert_eq!(
                    reported.status, 200,
                    "case {case:?}: GetConnection on the minted id must answer 200, got {} {}",
                    reported.status,
                    reported.rendered()
                );
                let report = reported.json();
                assert_eq!(
                    report.pointer("/identity/sourceIp").and_then(Value::as_str),
                    Some(LOOPBACK_SOURCE_IP),
                    "case {case:?}: expected the address the client dialled from, got {report}"
                );
                assert_eq!(
                    report.pointer("/identity/userAgent").and_then(Value::as_str),
                    row.client.user_agent,
                    "case {case:?}: expected the user agent the handshake named, got {report}"
                );
                let connected_at = report
                    .get("connectedAt")
                    .and_then(Value::as_str)
                    .unwrap_or_else(|| panic!("case {case:?}: expected connectedAt in {report}"))
                    .to_string();
                let last_active_at = report
                    .get("lastActiveAt")
                    .and_then(Value::as_str)
                    .unwrap_or_else(|| panic!("case {case:?}: expected lastActiveAt in {report}"))
                    .to_string();
                assert_eq!(
                    last_active_at, connected_at,
                    "case {case:?}: no data has flowed, so the connection was last active when \
                     it was established, got {report}"
                );

                // A report changes nothing a later caller sees.
                let again =
                    connections_call(&router, &api.api_id, row.stage, "GET", &connection_id, Vec::new())
                        .await;
                assert_eq!(
                    again.json(),
                    report,
                    "case {case:?}: two reports with no traffic between them must answer \
                     identically, got {} then {}",
                    report,
                    again.json()
                );

                // 4. A push is a DELIVERY: the registry records it against that connection.
                let pushed = connections_call(
                    &router,
                    &api.api_id,
                    row.stage,
                    "POST",
                    &connection_id,
                    row.payload.to_vec(),
                )
                .await;
                assert_eq!(
                    pushed.status, 200,
                    "case {case:?}: PostToConnection to the minted id must answer 200, got {} {}",
                    pushed.status,
                    pushed.rendered()
                );
                let held = mgmt::state::connection(&*engine, &connection_id).unwrap_or_else(|| {
                    panic!("case {case:?}: the connection must still be registered")
                });
                assert_eq!(
                    held.delivered_count, 1,
                    "case {case:?}: exactly one payload was pushed, got {}",
                    held.delivered_count
                );
                assert_eq!(
                    held.messages.as_slice(),
                    &[row.payload.to_vec()],
                    "case {case:?}: expected the pushed payload to be delivered, got {:?}",
                    held.messages
                );

                // 5. `DeleteConnection` ends it, and from there every operation agrees the
                //    connection no longer exists.
                let deleted = connections_call(
                    &router,
                    &api.api_id,
                    row.stage,
                    "DELETE",
                    &connection_id,
                    Vec::new(),
                )
                .await;
                assert_eq!(
                    deleted.status, 204,
                    "case {case:?}: DeleteConnection must answer its modelled 204, got {} {}",
                    deleted.status,
                    deleted.rendered()
                );
                for (method, body) in [
                    ("GET", Vec::new()),
                    ("POST", b"after the close".to_vec()),
                    ("DELETE", Vec::new()),
                ] {
                    let gone = connections_call(
                        &router,
                        &api.api_id,
                        row.stage,
                        method,
                        &connection_id,
                        body,
                    )
                    .await;
                    assert_eq!(
                        gone.status, 410,
                        "case {case:?}: {method} on the deleted connection must answer 410, got \
                         {} {}",
                        gone.status,
                        gone.rendered()
                    );
                    assert_eq!(
                        gone.error_type, "GoneException",
                        "case {case:?}: {method} on the deleted connection must name \
                         GoneException, got {} {}",
                        gone.error_type,
                        gone.rendered()
                    );
                    assert_eq!(
                        gone.json(),
                        json!({}),
                        "case {case:?}: GoneException is memberless, so its body carries nothing, \
                         got {}",
                        gone.rendered()
                    );
                }
                assert!(
                    registered_at(&engine, &api.authority).is_empty(),
                    "case {case:?}: the deleted connection must be gone from the registry"
                );
            }
        })
    });
}

/// Ids of the form API Gateway ISSUES that nothing here holds, each with where it came from.
///
/// The first two are built by the registry's own encoder (`mgmt::state::connection_id_for`), so
/// nothing is spelled out by hand; the rest are the character classes such a token may use,
/// each carrying the format marker that encoding stamps. Standing in for the ids a client
/// really sends: one a sibling process minted, one it kept across a restart.
fn ids_of_the_issued_form() -> Vec<(String, String)> {
    vec![
        (
            "an id a sibling process minted".to_string(),
            mgmt::state::connection_id_for(b"sibling"),
        ),
        (
            "an id a client kept across a restart".to_string(),
            mgmt::state::connection_id_for(b"restarted"),
        ),
        (
            "an upper-case token with the marker in place".to_string(),
            "ZZZZZZZZZZYCZZw=".to_string(),
        ),
        (
            "a token of digits and letters".to_string(),
            "0123456789cCQ4A=".to_string(),
        ),
        (
            "an id whose encoding carries the base64 alphabet's own + and /".to_string(),
            mgmt::state::connection_id_for(&[0xFB, 0xFF]),
        ),
    ]
}

/// Values no API Gateway connection-id encoding could have produced, each with the way it
/// differs from one: too short, outside the base64 alphabet, unpadded, a base64url spelling,
/// the wrong decoded length, overlong, and a token of the right length whose decoding carries
/// no format marker.
fn ids_api_gateway_does_not_mint() -> Vec<(String, String)> {
    vec![
        ("a single character".to_string(), "x".to_string()),
        (
            "characters outside the base64 alphabet".to_string(),
            "cid-with-dashes".to_string(),
        ),
        ("unpadded base64".to_string(), "0123456789abcdef".to_string()),
        ("a base64url spelling".to_string(), "a-b_c9DEF0123A4=".to_string()),
        ("base64 of a shorter token".to_string(), "AAAAAAAAAAA=".to_string()),
        ("an overlong id".to_string(), "A".repeat(200)),
        (
            "characters of the alphabet, including its + and /, at a length no token has"
                .to_string(),
            "a/b+c".to_string(),
        ),
        (
            "eleven zero bytes, so no format marker".to_string(),
            "AAAAAAAAAAAAAAA=".to_string(),
        ),
    ]
}

/// The refusal a `@connections` call earns is read off the VALUE the request sent and then off
/// the registry — never off what this process happens to have minted: a live connection and an
/// unknown id of the same form are served on the same endpoint, one `200` and one `410`.
#[test]
fn the_connections_refusal_is_decided_by_the_submitted_id_and_the_registry() {
    on_the_assembled_emulator("connections-id-form-split", |router, engine| {
        Box::pin(async move {
            let stage = "dev";
            let case = "a deployed stage carrying one live connection";
            let api = given_deployed_api(&router, case, "id-form-split", stage).await;
            let (_socket, head) =
                opening_handshake(case, &api.authority, stage, &ROWS[0].client).await;
            switched(case, &head, &ROWS[0].client);
            let live = minted_at(case, &engine, &api.authority).await;

            // An id OF the issued form that no connection owns reaches the store, whose miss is
            // the memberless `GoneException` — the answer a sender prunes its own record on.
            for (why, id) in ids_of_the_issued_form() {
                let case = format!("{case}: {why}");
                assert!(
                    mgmt::state::is_well_formed_connection_id(&id),
                    "case {case:?}: this row must be an id of the issued form, got {id:?}"
                );
                for (method, body) in [
                    ("GET", Vec::new()),
                    ("POST", b"fan-out".to_vec()),
                    ("DELETE", Vec::new()),
                ] {
                    let answer =
                        connections_call(&router, &api.api_id, stage, method, &id, body).await;
                    assert_eq!(
                        answer.status, 410,
                        "case {case:?}: {method} with {id:?} must answer 410, got {} {}",
                        answer.status,
                        answer.rendered()
                    );
                    assert_eq!(
                        answer.error_type, "GoneException",
                        "case {case:?}: {method} with {id:?} must name GoneException, got {} {}",
                        answer.error_type,
                        answer.rendered()
                    );
                    assert_eq!(
                        answer.message(),
                        "<no message>",
                        "case {case:?}: GoneException declares no members, so it carries no \
                         message, got {}",
                        answer.rendered()
                    );
                }
            }

            // A value that encoding could never have produced is a defect in the request, and
            // the refusal names the value the request sent.
            for (why, id) in ids_api_gateway_does_not_mint() {
                let case = format!("{case}: {why}");
                for (method, body) in [
                    ("GET", Vec::new()),
                    ("POST", b"fan-out".to_vec()),
                    ("DELETE", Vec::new()),
                ] {
                    let answer =
                        connections_call(&router, &api.api_id, stage, method, &id, body).await;
                    assert_eq!(
                        answer.status, 400,
                        "case {case:?}: {method} with {id:?} must answer 400, got {} {}",
                        answer.status,
                        answer.rendered()
                    );
                    assert_eq!(
                        answer.error_type, "BadRequestException",
                        "case {case:?}: {method} with {id:?} must name BadRequestException, got \
                         {} {}",
                        answer.error_type,
                        answer.rendered()
                    );
                    assert_eq!(
                        answer.message(),
                        format!("Invalid connectionId: {id}"),
                        "case {case:?}: the refusal must name the id the request sent, got {}",
                        answer.rendered()
                    );
                }
            }

            // And none of that touched the live connection: the same endpoint still serves it.
            let served =
                connections_call(&router, &api.api_id, stage, "GET", &live, Vec::new()).await;
            assert_eq!(
                served.status, 200,
                "case {case:?}: the live connection must still be served, got {} {}",
                served.status,
                served.rendered()
            );
        })
    });
}

/// The two refusals the `@connections` ENDPOINT takes before anything reads the id: the stage
/// it was addressed through, and the size of the payload.
#[test]
fn the_payload_ceiling_and_the_stage_are_judged_before_the_connection_id() {
    on_the_assembled_emulator("connections-endpoint-ordering", |router, engine| {
        Box::pin(async move {
            let stage = "live";
            let case = "a deployed stage carrying one live connection";
            let api = given_deployed_api(&router, case, "endpoint-ordering", stage).await;
            let (_socket, head) =
                opening_handshake(case, &api.authority, stage, &ROWS[0].client).await;
            switched(case, &head, &ROWS[0].client);
            let live = minted_at(case, &engine, &api.authority).await;

            // The modelled maximum is INCLUSIVE, so a payload of exactly that size is served.
            let at_the_ceiling = connections_call(
                &router,
                &api.api_id,
                stage,
                "POST",
                &live,
                vec![b'p'; MAX_DATA_BYTES],
            )
            .await;
            assert_eq!(
                at_the_ceiling.status, 200,
                "case {case:?}: a payload of exactly {MAX_DATA_BYTES} bytes must be delivered, \
                 got {} {}",
                at_the_ceiling.status,
                at_the_ceiling.rendered()
            );

            // One byte past it is refused on the SIZE alone — whatever the id says, and without
            // the registry being consulted, which is what the three id forms below show.
            let unknown = mgmt::state::connection_id_for(b"unknown");
            for (why, id) in [
                ("the live connection's own id", live.clone()),
                ("an id of the issued form that nothing holds", unknown),
                ("an id that could never have been minted", "x".to_string()),
            ] {
                let case = format!("{case}: one byte past the ceiling with {why}");
                let refused = connections_call(
                    &router,
                    &api.api_id,
                    stage,
                    "POST",
                    &id,
                    vec![b'p'; MAX_DATA_BYTES + 1],
                )
                .await;
                assert_eq!(
                    refused.status, 413,
                    "case {case:?}: a payload past the modelled maximum must answer 413, got {} \
                     {}",
                    refused.status,
                    refused.rendered()
                );
                assert_eq!(
                    refused.error_type, "<absent>",
                    "case {case:?}: the endpoint's own ceiling names no error shape, so a client \
                     reports the status itself, got {}",
                    refused.error_type
                );
                assert!(
                    refused.body.is_empty(),
                    "case {case:?}: the refusal carries no body, got {}",
                    refused.rendered()
                );
            }

            // Nothing past the ceiling was delivered: the one payload the connection holds is
            // the one that was inside it.
            let held = mgmt::state::connection(&*engine, &live)
                .unwrap_or_else(|| panic!("case {case:?}: the connection must still be registered"));
            assert_eq!(
                held.delivered_count, 1,
                "case {case:?}: only the payload inside the ceiling was delivered, got {}",
                held.delivered_count
            );

            // A stage the api has not deployed serves none of the three operations, and says so
            // without telling the caller anything about what is behind it — the live connection
            // above is what shows the refusal is taken before the store is read.
            for undeployed in ["nope", "Live", "staging"] {
                for (method, body) in [
                    ("GET", Vec::new()),
                    ("POST", b"fan-out".to_vec()),
                    ("DELETE", Vec::new()),
                ] {
                    let case = format!("{case}: {method} through the undeployed stage \
                                        {undeployed:?}");
                    let refused =
                        connections_call(&router, &api.api_id, undeployed, method, &live, body)
                            .await;
                    assert_eq!(
                        refused.status, 403,
                        "case {case:?}: an endpoint that serves no deployed stage must answer \
                         403, got {} {}",
                        refused.status,
                        refused.rendered()
                    );
                    assert_eq!(
                        refused.error_type, "ForbiddenException",
                        "case {case:?}: expected ForbiddenException, got {} {}",
                        refused.error_type,
                        refused.rendered()
                    );
                    assert_eq!(
                        refused.json(),
                        json!({ "message": "Forbidden" }),
                        "case {case:?}: expected the sentence API Gateway refuses an endpoint \
                         with, got {}",
                        refused.rendered()
                    );
                }
            }

            // The deployed stage of the same api still serves the connection.
            let served =
                connections_call(&router, &api.api_id, stage, "GET", &live, Vec::new()).await;
            assert_eq!(
                served.status, 200,
                "case {case:?}: the deployed stage must still serve the live connection, got {} \
                 {}",
                served.status,
                served.rendered()
            );
        })
    });
}
