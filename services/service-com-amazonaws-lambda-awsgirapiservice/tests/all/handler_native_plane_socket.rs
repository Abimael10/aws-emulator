// SPDX-License-Identifier: CC0-1.0

//! The CONNECTION a running handler opens itself to a data plane of this emulator that
//! speaks its own protocol, end to end over the wire: a `python3.x` function whose code does
//! `import socket, ssl`, connects to an address this emulator opened a resource's own
//! listener at, and speaks that plane's protocol on it.
//!
//! The rule pinned here: a handler that was handed a resource's own endpoint reaches the
//! protocol handler this emulator serves at that address — in the clear, or inside a TLS
//! session it asked for, whose handshake runs on the connection underneath — and an address
//! this emulator opened no plane at is answered the way a host with nothing listening there
//! answers it. The connection is the in-process one the register hands out
//! (`service_common::dialable_data_planes`); nothing of the protocol is short-circuited,
//! which is why the plane below answers only what it was really sent.
//!
//! The rows differ along what the rule generalises over: whether the handler asks for
//! encryption, which of two planes on the same engine it addresses, the bytes it sends, the
//! timeout it sets, and the addresses that reach no plane (another port of the same host, a
//! host nothing was opened at).

use std::future::Future;
use std::io::Write as _;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::ACTIVATION_DELAY;
use smithy_server_runtime::MultiServiceRouter;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// Where `CreateFunction` is bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// A well-formed execution role ARN, satisfying the model's `RoleArn` pattern.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// The module and attribute the package below carries its handler as.
const HANDLER: &str = "lambda_function.lambda_handler";

/// The service the planes below are recorded as served by, which is what a diagnosis of a
/// connection names: these stand for a database's own endpoint.
const SERVED_BY: &str = "rds";

/// How long past a due instant this suite waits before calling.
const SLACK: Duration = Duration::from_millis(150);

/// A PLANE of this emulator: a line protocol that answers what it was sent, saying whether
/// the client secured the connection first.
///
/// The encryption is negotiated IN BAND, as the native planes of this emulator negotiate it
/// (`service_common::in_band_tls`): the first byte says whether the client opened with a TLS
/// record, and the core's own TLS server runs the handshake when it did. So the plane serves
/// both a client that asked for a session and one that did not.
struct LineProtocol {
    /// What this plane calls itself in its answer, so two planes on one engine are told
    /// apart by what answers.
    label: &'static str,
    /// The core's TLS server, as the owner of a plaintext listener obtains it.
    upgrader: emulator_server::TlsUpgrader,
}

impl emulator_server::NativeHandler for LineProtocol {
    fn serve(
        &self,
        conn: emulator_server::ListenerConn,
    ) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        let label = self.label;
        let upgrader = self.upgrader.clone();
        Box::pin(async move {
            let Some((mut conn, encrypted)) =
                service_common::in_band_tls::negotiated_scheme(conn, Some(upgrader)).await
            else {
                return;
            };
            let mut asked = Vec::new();
            let mut byte = [0u8; 1];
            while asked.len() < 256 {
                match conn.read(&mut byte).await {
                    Ok(1) => {
                        if byte[0] == b'\n' {
                            break;
                        }
                        asked.push(byte[0]);
                    }
                    _ => return,
                }
            }
            let scheme = if encrypted { "secure" } else { "clear" };
            let answer = format!("{label} {scheme} {}\n", String::from_utf8_lossy(&asked));
            let _ = conn.write_all(answer.as_bytes()).await;
            let _ = conn.flush().await;
        })
    }
}

/// A plane that TAKES the bytes and answers nothing, which is what a plane whose protocol
/// expects a message of its own looks like to a client that starts a TLS handshake on it.
/// Nothing on either side writes or closes, so the wait has to be the HOST's to end.
struct SilentProtocol;

impl emulator_server::NativeHandler for SilentProtocol {
    fn serve(
        &self,
        mut conn: emulator_server::ListenerConn,
    ) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(async move {
            let mut taken = [0u8; 1024];
            while conn.read(&mut taken).await.unwrap_or_default() > 0 {}
        })
    }
}

/// Lambda on an engine whose register two planes are opened in, the addresses they answer at,
/// and the records that keep them open for as long as the test runs.
fn router_with_planes() -> (
    MultiServiceRouter,
    Vec<String>,
    Vec<service_common::dialable_data_planes::OpenedPlanes>,
) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let planes = service_common::dialable_data_planes::DialablePlanes::of(&*engine)
        .expect("the engine vends the register");
    let upgrader = engine
        .listeners()
        .tls_upgrader()
        .expect("the core prepares its TLS server");
    let addresses: Vec<String> = ["127.0.0.1:57001", "127.0.0.1:57002"]
        .iter()
        .map(|address| (*address).to_string())
        .collect();
    let mut opened = Vec::new();
    for (address, label) in addresses.iter().zip(["orders", "reporting"]) {
        // The plane negotiates its own encryption in band, as this emulator's engine planes
        // do, so it is handed byte 0 and upgrades the connection itself.
        let served = service_common::dialable_data_planes::served_in_process(
            Arc::new(LineProtocol {
                label,
                upgrader: upgrader.clone(),
            }),
            None,
        );
        opened.push(planes.opening([address.clone()], SERVED_BY, &served));
    }
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    (router, addresses, opened)
}

/// Lambda on an engine whose register holds the plane that answers NOTHING, the address it
/// answers at, and the record that keeps it open.
fn router_with_a_silent_plane() -> (
    MultiServiceRouter,
    String,
    service_common::dialable_data_planes::OpenedPlanes,
) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let planes = service_common::dialable_data_planes::DialablePlanes::of(&*engine)
        .expect("the engine vends the register");
    let address = "127.0.0.1:57003".to_string();
    let served =
        service_common::dialable_data_planes::served_in_process(Arc::new(SilentProtocol), None);
    let opened = planes.opening([address.clone()], SERVED_BY, &served);
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    (router, address, opened)
}

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request. The signature is never checked here.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=emulator-test-key/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One Lambda `restJson1` call, as its status, headers and parsed body.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: &[u8],
) -> (u16, http::HeaderMap, Value) {
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("authorization", authorization("lambda"))
        .header("content-type", "application/json")
        .body(bytes::Bytes::from(body.to_vec()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    let parsed: Value = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&body).to_string()))
    };
    (parts.status.as_u16(), parts.headers, parsed)
}

/// A .zip deployment package holding `source` as `lambda_function.py`.
fn package(source: &str) -> Vec<u8> {
    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buffer);
        let options: zip::write::FileOptions<'_, '_, ()> = zip::write::FileOptions::default();
        writer
            .start_file("lambda_function.py", options)
            .expect("the entry starts");
        writer
            .write_all(source.as_bytes())
            .expect("the entry's bytes");
        writer.finish().expect("a finished archive");
    }
    buffer.into_inner()
}

/// The handler every row runs: it connects to the address the event names, secures the
/// connection when the event asks for it, speaks the plane's line protocol and reports what
/// came back.
const HANDLER_SOURCE: &str = r#"
import socket
import ssl


def lambda_handler(event, context):
    try:
        sock = socket.create_connection((event["host"], event["port"]), 10)
    except OSError as refusal:
        return {"ok": False, "refused": str(refusal)}
    try:
        if event.get("timeout"):
            sock.settimeout(event["timeout"])
        version = None
        if event.get("tls"):
            context_ = ssl.create_default_context()
            stream = context_.wrap_socket(sock, server_hostname=event["host"])
            version = stream.version()
        else:
            stream = sock
        stream.sendall(event["send"].encode("utf-8") + b"\n")
        reader = stream.makefile("rb")
        answered = reader.readline()
        return {
            "ok": True,
            "answer": answered.decode("utf-8").strip(),
            "version": version,
        }
    finally:
        sock.close()
"#;

/// Create the function and wait out its provisioning window, with `timeout` as its own
/// `Timeout` — the bound every wait of one of its invocations is clamped to.
async fn create_function_with_timeout(
    router: &MultiServiceRouter,
    name: &str,
    timeout: u32,
) {
    let (status, _headers, created) = lambda(
        router,
        "POST",
        FUNCTIONS_URI,
        &serde_json::to_vec(&json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": HANDLER,
            "Timeout": timeout,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD.encode(package(HANDLER_SOURCE)),
            },
        }))
        .expect("the create body serializes"),
    )
    .await;
    assert_eq!(
        status, 201,
        "expected HTTP 201 from CreateFunction, got {status} ({created})"
    );
    tokio::time::sleep(ACTIVATION_DELAY + SLACK).await;
}

/// Create the function and wait out its provisioning window.
async fn create_function(router: &MultiServiceRouter, name: &str) {
    create_function_with_timeout(router, name, 30).await;
}

/// Invoke the function and answer with the payload it produced.
async fn invoke(router: &MultiServiceRouter, case: &str, name: &str, event: &Value) -> Value {
    let (status, headers, payload) = lambda(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{name}/invocations"),
        event.to_string().as_bytes(),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Invoke, got {status} ({payload})"
    );
    assert_eq!(
        headers.get("x-amz-function-error"),
        None,
        "case {case:?}: the handler must run to completion, got {payload}"
    );
    payload
}

/// One row: the address the handler connects to, whether it asks for encryption, what it
/// sends, and the line the plane is expected to answer — or, for an address that reaches no
/// plane at all, the refusal the connection takes instead.
struct Row {
    case: &'static str,
    /// Which opened address to use (an index into the opened list), or an address of
    /// its own for a row that reaches nothing.
    address: Result<usize, &'static str>,
    tls: bool,
    send: &'static str,
    /// The timeout the handler sets on its socket before it reads, where the row sets one.
    timeout: Option<f64>,
    /// The line the plane answers, or `Err` with the refusal a connection to an address no
    /// plane answers at takes: the sentence the standard library words that errno in.
    answer: Result<&'static str, &'static str>,
}

#[tokio::test]
async fn a_handler_speaks_the_protocol_of_a_plane_this_emulator_published() {
    let rows = [
        Row {
            case: "a plane reached in the clear",
            address: Ok(0),
            tls: false,
            send: "ping",
            timeout: None,
            answer: Ok("orders clear ping"),
        },
        Row {
            case: "the same plane inside a session the handler asked for",
            address: Ok(0),
            tls: true,
            send: "ping",
            timeout: None,
            answer: Ok("orders secure ping"),
        },
        Row {
            case: "another plane of the same engine, and other bytes",
            address: Ok(1),
            tls: false,
            send: "select 1",
            timeout: None,
            answer: Ok("reporting clear select 1"),
        },
        Row {
            case: "another plane, secured",
            address: Ok(1),
            tls: true,
            send: "select 1",
            timeout: None,
            answer: Ok("reporting secure select 1"),
        },
        Row {
            // A timeout the handler set that no span can hold: the host clamps it to what the
            // invocation has left, so the read is served rather than ending the run.
            case: "a socket timeout larger than any span",
            address: Ok(0),
            tls: false,
            send: "ping",
            timeout: Some(1e30),
            answer: Ok("orders clear ping"),
        },
        Row {
            // This machine's own host, where this emulator's planes are served, at a port no
            // plane of it answers at: what a host answers for one is ECONNREFUSED.
            case: "another port of the same host",
            address: Err("127.0.0.1:57999"),
            tls: false,
            send: "ping",
            timeout: None,
            answer: Err("[Errno 111] Connection refused"),
        },
        Row {
            // Another host entirely: a function has no network to reach one on, so the
            // connection is answered as a host with no route to it answers.
            case: "a host no plane of this emulator is served on",
            address: Err("10.0.0.1:3306"),
            tls: false,
            send: "ping",
            timeout: None,
            answer: Err("[Errno 101] Network is unreachable"),
        },
    ];
    let (router, published, _opened) = router_with_planes();
    create_function(&router, "plane-client").await;
    for row in &rows {
        let case = row.case;
        let address = match row.address {
            Ok(at) => published[at].clone(),
            Err(own) => own.to_string(),
        };
        let (host, port) = address
            .rsplit_once(':')
            .expect("the published address carries a port");
        let payload = invoke(
            &router,
            case,
            "plane-client",
            &json!({
                "host": host,
                "port": port.parse::<u32>().expect("the port is a number"),
                "tls": row.tls,
                "send": row.send,
                "timeout": row.timeout,
            }),
        )
        .await;
        match row.answer {
            Ok(expected) => {
                assert_eq!(
                    payload["ok"], json!(true),
                    "case {case:?}: expected the handler to reach the plane, got {payload}"
                );
                assert_eq!(
                    payload["answer"], json!(expected),
                    "case {case:?}: expected the plane's own answer {expected:?}, got {payload}"
                );
                let version = payload["version"].as_str();
                if row.tls {
                    let secured = version.unwrap_or_default().starts_with("TLSv1");
                    assert!(
                        secured,
                        "case {case:?}: expected the session to report a TLS version, got \
                         {version:?}"
                    );
                } else {
                    assert_eq!(
                        payload["version"], Value::Null,
                        "case {case:?}: expected no session on a connection in the clear, got \
                         {version:?}"
                    );
                }
            }
            Err(expected) => {
                assert_eq!(
                    payload["ok"], json!(false),
                    "case {case:?}: expected no plane to be reached, got {payload}"
                );
                let refused = payload["refused"].as_str().unwrap_or_default();
                assert_eq!(
                    refused, expected,
                    "case {case:?}: expected the connection to {address:?} to be answered \
                     {expected:?}, got {refused:?}"
                );
            }
        }
    }
}

/// RULE: every wait a handler's connection makes is the HOST's to end, clamped to what the
/// invocation has left — so a plane that takes the bytes and answers nothing is an invocation
/// that ANSWERS, rather than one parked on a session that will never be negotiated.
///
/// The function's own `Timeout` is the bound under test, so it is the one limit here that is
/// tight; nothing is asserted about how long the host took, only that the call came back and
/// reported the failure.
#[tokio::test]
async fn a_handler_waiting_on_a_plane_that_answers_nothing_still_ends_its_invocation() {
    let case = "a plane that takes the bytes and answers nothing";
    let (router, address, _opened) = router_with_a_silent_plane();
    create_function_with_timeout(&router, "silent-plane-client", 2).await;
    let (host, port) = address
        .rsplit_once(':')
        .expect("the published address carries a port");
    let (status, headers, payload) = lambda(
        &router,
        "POST",
        &format!("{FUNCTIONS_URI}/silent-plane-client/invocations"),
        json!({
            "host": host,
            "port": port.parse::<u32>().expect("the port is a number"),
            "tls": true,
            "send": "ping",
        })
        .to_string()
        .as_bytes(),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Invoke, got {status} ({payload})"
    );
    let errored = headers.get("x-amz-function-error").is_some();
    let reported_failure = payload["ok"] == json!(false);
    assert!(
        errored || reported_failure,
        "case {case:?}: expected the invocation to report the session it could not negotiate, \
         got {payload}"
    );
}
