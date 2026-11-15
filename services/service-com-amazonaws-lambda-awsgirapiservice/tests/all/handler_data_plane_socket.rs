// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the STREAM SOCKETS a running handler opens itself: `CreateFunction`
//! then `Invoke` over the router, against data-plane listeners this emulator's own core
//! opened in this process.
//!
//! The rule under test: **a running function's outbound connection to an address this emulator
//! has OPENED a resource's own data plane at succeeds and carries bytes both ways — in the
//! clear and under a TLS session negotiated with that plane — and an address this emulator has
//! opened no plane at is answered the way a host answers one, with the refused connection, the
//! absent route or the unresolvable name a client branches on.**
//!
//! Every connection here reaches the plane's own protocol handler: the listeners are opened
//! through the core (`engine.listeners()`) and recorded as the planes they serve, and the
//! handler's `socket` calls are the runtime's own — so what these tests drive is the path an
//! in-VPC client of a database, a cache or a broker takes, and not an in-process shortcut past
//! the protocol.
//!
//! The table spans what the rule generalises over: how the address is SPELLED (the literal the
//! control plane published, and the loopback name for the same plane), how much is carried (one
//! small line, and a payload larger than one write), how the bytes are read (the socket itself,
//! and a buffered file over it), how many connections one invocation opens one after another,
//! whether the session is in the clear or TLS, and the addresses that are not a plane of this
//! emulator at all (a port of this machine whose plane is gone, an address of another host, and
//! a host of AWS's own naming — the shape a control plane publishes for a plane nothing here
//! serves).

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use emulator_server::{ListenerConn, NativeHandler, ResourceListener, TlsCertificateAuthority};
use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::ACTIVATION_DELAY;
use service_com_amazonaws_lambda_awsgirapiservice::EmulatedService;
use service_common::data_plane_addressing::listener_authority;
use service_common::dialable_data_planes::{
    served_in_process, DialablePlanes, OpenedPlanes, SharedHandler,
};
use smithy_server_runtime::MultiServiceRouter;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::invoke_request_id::{is_host_bound, one_invocation_stream_at_a_time};
use crate::node_invoke_run::package;

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// Where `CreateFunction` is bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// A well-formed execution role ARN, satisfying the model's `RoleArn` pattern.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// The handler module the function ships, and the handler it declares.
const MODULE: &str = "lambda_function";
const HANDLER: &str = "lambda_function.lambda_handler";

/// How long past a due instant this suite waits before calling. The provisioning window a
/// create documents is owed by the CLOCK, so nothing here sleeps on the emulator's behalf.
const SLACK: Duration = Duration::from_millis(150);

/// The `Timeout` the function carries, in seconds: sized for this test binary's own
/// scheduling, not for the handler's work, which is immediate.
const FUNCTION_TIMEOUT_SECONDS: i32 = 25;

/// How many times the invocation is re-driven while the answer reports one of this test
/// binary's own host bounds rather than a run.
const HOST_BOUND_ATTEMPTS: usize = 6;

/// What the listeners below answer every line with, which is how an answer is told from an
/// echo: the protocol is "one line in, that line back under this prefix".
const ANSWER_PREFIX: &str = "ANSWER ";

/// What this suite names as the service answering at the addresses it opens.
const SERVED_BY: &str = "lambda-data-plane-probe";

/// One line a case sends, and how many connections the sequential case opens.
const LINE: &str = "ping\n";
const CONNECTIONS_IN_SEQUENCE: usize = 10;

/// A data plane of this emulator, as a resource's own listener: the socket, and the record
/// that says its address is one an in-process client may dial.
struct Plane {
    /// Held to keep the port open.
    listener: ResourceListener,
    /// Held to keep the address recorded as dialable.
    _dialable: OpenedPlanes,
}

impl Plane {
    /// The host the core bound it to, which is what a control plane publishes.
    fn host(&self) -> String {
        self.listener.host()
    }

    /// The port the core assigned it.
    fn port(&self) -> u16 {
        self.listener.port()
    }
}

/// The protocol handler every listener in this suite serves: one line in, the same line back
/// under [`ANSWER_PREFIX`], for as long as the client keeps the connection open.
///
/// Framed on the newline rather than on one read, so a payload that arrives in several reads
/// is answered once — which is what makes the answer to a large write assertable.
struct Answering;

impl NativeHandler for Answering {
    fn serve(&self, conn: ListenerConn) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(async move {
            let mut conn = conn;
            let mut arriving = vec![0_u8; 16 * 1024];
            let mut held: Vec<u8> = Vec::new();
            loop {
                let read = match conn.read(&mut arriving).await {
                    Ok(0) | Err(_) => break,
                    Ok(read) => read,
                };
                held.extend_from_slice(&arriving[..read]);
                while let Some(at) = held.iter().position(|byte| *byte == b'\n') {
                    let line: Vec<u8> = held.drain(..=at).collect();
                    let mut answer = ANSWER_PREFIX.as_bytes().to_vec();
                    answer.extend_from_slice(&line);
                    if conn.write_all(&answer).await.is_err() || conn.flush().await.is_err() {
                        return;
                    }
                }
            }
        })
    }
}

/// What one request this suite made came back as.
struct Answer {
    status: u16,
    headers: http::HeaderMap,
    body: bytes::Bytes,
}

impl Answer {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|value| value.to_str().ok())
    }
}

/// AWS Lambda on one engine, reached the way the production router reaches it.
fn emulator(engine: &Arc<engine::InMemoryEngine>) -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(engine)));
    router
}

/// A SigV4 `Authorization` whose credential scope names `service`. Nothing in it is
/// verified: the harness turns request authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=emulator-test-key/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One AWS Lambda call over its `restJson1` wire.
async fn lambda(router: &MultiServiceRouter, method: &str, uri: &str, body: &[u8]) -> Answer {
    let host = format!("lambda.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("http://{host}{uri}"))
        .header("host", host)
        .header("authorization", authorization("lambda"))
        .header("content-type", "application/json")
        .body(bytes::Bytes::from(body.to_vec()))
        .expect("the test request must build");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    Answer {
        status: parts.status.as_u16(),
        headers: parts.headers,
        body,
    }
}

/// A `python3.12` function running [`PROBE_SOURCE`], waited out of the provisioning window
/// the create documents.
async fn create_function(router: &MultiServiceRouter, name: &str) {
    let entry = format!("{MODULE}.py");
    let created = lambda(
        router,
        "POST",
        FUNCTIONS_URI,
        &serde_json::to_vec(&json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": HANDLER,
            "Timeout": FUNCTION_TIMEOUT_SECONDS,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(&[(entry.as_str(), PROBE_SOURCE)])),
            },
        }))
        .expect("the create body serializes"),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "setup CreateFunction {name:?}: expected HTTP 201, got {} {}",
        created.status,
        created.text()
    );
    tokio::time::sleep(ACTIVATION_DELAY + SLACK).await;
}

/// What the invocation answered, re-driven while the answer reports a bound this whole test
/// binary shares rather than a run.
async fn answered(router: &MultiServiceRouter, name: &str, event: &Value) -> Value {
    let uri = format!("{FUNCTIONS_URI}/{name}/invocations");
    let body = serde_json::to_vec(event).expect("the event serializes");
    let mut answer = lambda(router, "POST", &uri, &body).await;
    for _ in 1..HOST_BOUND_ATTEMPTS {
        if !is_host_bound(answer.status, &answer.text()) {
            break;
        }
        answer = lambda(router, "POST", &uri, &body).await;
    }
    assert!(
        !is_host_bound(answer.status, &answer.text()),
        "every attempt reported a bound this whole test binary shares, so nothing of the rule \
         was read: {}",
        answer.text()
    );
    assert_eq!(
        answer.status,
        200,
        "expected HTTP 200 from Invoke, got {} {}",
        answer.status,
        answer.text()
    );
    let failure = answer.header("x-amz-function-error");
    assert_eq!(
        failure, None,
        "the handler opens its connections and reports what each came to, so the invocation \
         carries no function error, got {failure:?} with {}",
        answer.text()
    );
    answer.json()
}

/// Open a data plane of this emulator: a listener the core binds on the interface it was
/// configured with, recorded as the plane an in-process client reaches at the address it
/// published — which is what the service owning a resource does when it publishes the
/// resource's endpoint. The SAME handler serves both ends, so a connection opened in process
/// is served exactly as one accepted on the listener.
async fn open_plane(engine: &Arc<engine::InMemoryEngine>, secured: bool) -> Plane {
    let listeners = engine.listeners();
    let answering = Arc::new(Answering);
    let listener = match secured {
        true => listeners.tcp(SharedHandler(Arc::clone(&answering))).await,
        false => {
            listeners
                .tcp_plaintext(SharedHandler(Arc::clone(&answering)))
                .await
        }
    }
    .expect("the core must open the listener this plane is served on");
    // A TLS listener is one the CORE terminates TLS on, so the plane's own end of an
    // in-process connection runs the same server handshake.
    let upgrader = match secured {
        true => Some(
            listeners
                .tls_upgrader()
                .expect("the core must prepare the TLS this plane is served over"),
        ),
        false => None,
    };
    let dialable = DialablePlanes::of(&**engine)
        .expect("the engine vends the dialable-plane records")
        .opening(
            [listener_authority(&listener)],
            SERVED_BY,
            &served_in_process(answering, upgrader),
        );
    Plane {
        listener,
        _dialable: dialable,
    }
}

/// The whole rule, driven END TO END: real listeners of this emulator's core, one real
/// `CreateFunction`, one real `Invoke`, and the handler's own sockets.
///
/// ONE invocation for the whole table, the way the sibling run-based suites do it: the memory
/// bound a run is stopped on is read from the WHOLE process and this binary runs its suites in
/// parallel, so one interpreter for the table is a cost this suite does not put on the rest of
/// the target.
#[tokio::test]
async fn a_handler_connects_to_the_data_plane_listeners_this_emulator_opened() {
    let _stream = one_invocation_stream_at_a_time().await;
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = emulator(&engine);
    let plane = open_plane(&engine, false).await;
    let secured = open_plane(&engine, true).await;
    // A plane that is GONE: its listener is closed and its address withdrawn, which is the
    // position every address of a deleted resource is in.
    let closed = open_plane(&engine, false).await;
    let closed_port = closed.port();
    let closed_host = closed.host();
    drop(closed);
    let function = "data-plane-socket-probe";
    create_function(&router, function).await;

    let large = format!("{}\n", "x".repeat(64 * 1024));
    let cases = json!([
        {
            "name": "the address as the control plane published it",
            "kind": "plain",
            "host": plane.host(),
            "port": plane.port(),
            "send": LINE,
        },
        {
            "name": "the same listener addressed by the loopback name",
            "kind": "plain",
            "host": "localhost",
            "port": plane.port(),
            "send": LINE,
        },
        {
            "name": "a payload larger than one write",
            "kind": "plain",
            "host": plane.host(),
            "port": plane.port(),
            "send": large,
        },
        {
            "name": "a buffered file over the connection",
            "kind": "makefile",
            "host": plane.host(),
            "port": plane.port(),
            "send": LINE,
        },
        {
            "name": "connection after connection in one invocation",
            "kind": "sequential",
            "host": plane.host(),
            "port": plane.port(),
            "send": LINE,
            "count": CONNECTIONS_IN_SEQUENCE,
        },
        {
            "name": "a data plane that speaks TLS",
            "kind": "tls",
            "host": secured.host(),
            "port": secured.port(),
            "send": LINE,
        },
        {
            "name": "a port of this machine whose plane is gone",
            "kind": "plain",
            "host": closed_host,
            "port": closed_port,
            "send": LINE,
        },
        {
            "name": "an address of another host",
            "kind": "plain",
            "host": "10.0.7.9",
            "port": 27017,
            "send": LINE,
        },
        {
            "name": "a host of AWS's own naming",
            "kind": "plain",
            "host": "probe-in-1.cluster-abc.us-east-1.docdb.amazonaws.com",
            "port": 27017,
            "send": LINE,
        },
    ]);
    let document = answered(&router, function, &json!({ "cases": cases })).await;

    // What each connection came to. The ANSWER rows are the rule itself: the bytes the
    // listener wrote are the bytes the handler read back.
    let answer_for = |sent: &str| format!("{ANSWER_PREFIX}{sent}");
    let served: &[(&str, String)] = &[
        (
            "the address as the control plane published it",
            answer_for(LINE),
        ),
        (
            "the same listener addressed by the loopback name",
            answer_for(LINE),
        ),
        ("a payload larger than one write", answer_for(&large)),
        ("a buffered file over the connection", answer_for(LINE)),
        ("a data plane that speaks TLS", answer_for(LINE)),
    ];
    for (case, expected) in served {
        let reported = document.get(*case).cloned().unwrap_or(Value::Null);
        assert_eq!(
            reported.get("ok").and_then(Value::as_bool),
            Some(true),
            "case {case:?}: expected the connection to be opened and answered, got {reported}"
        );
        let answer = reported.get("answer").and_then(Value::as_str).unwrap_or("");
        assert_eq!(
            answer, expected,
            "case {case:?}: expected the listener's own answer, got {answer:?}"
        );
    }
    // The connection is a connection: it reports the address it reached.
    let published = document
        .get("the address as the control plane published it")
        .cloned()
        .unwrap_or(Value::Null);
    assert_eq!(
        published.get("peer").cloned().unwrap_or(Value::Null),
        json!([plane.host(), plane.port()]),
        "case \"the address as the control plane published it\": expected the peer to be the \
         published address, got {published}"
    );

    // One invocation opens connection after connection: each is closed, so the next one has a
    // slot, and every one of them is answered.
    let sequence = document
        .get("connection after connection in one invocation")
        .cloned()
        .unwrap_or(Value::Null);
    let answers = sequence
        .get("answers")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let expected: Vec<Value> = (0..CONNECTIONS_IN_SEQUENCE)
        .map(|at| Value::String(format!("{ANSWER_PREFIX}ping-{at}\n")))
        .collect();
    assert_eq!(
        answers, expected,
        "case \"connection after connection in one invocation\": expected every connection to \
         be answered, got {sequence}"
    );

    // The TLS session is the session that handshake negotiated — with THIS emulator's own
    // listener, which is what the certificate the handler read back says.
    let tls = document
        .get("a data plane that speaks TLS")
        .cloned()
        .unwrap_or(Value::Null);
    let version = tls.get("version").and_then(Value::as_str).unwrap_or("");
    assert!(
        version.starts_with("TLSv1."),
        "case \"a data plane that speaks TLS\": expected the protocol version the handshake \
         negotiated, got {version:?}"
    );
    let cipher = tls.get("cipher").and_then(Value::as_str).unwrap_or("");
    assert!(
        !cipher.is_empty(),
        "case \"a data plane that speaks TLS\": expected the cipher suite the handshake \
         negotiated, got {cipher:?}"
    );
    let bits = tls.get("bits").and_then(Value::as_u64).unwrap_or_default();
    assert!(
        bits >= 128,
        "case \"a data plane that speaks TLS\": expected the secret bits of the negotiated \
         suite, got {bits}"
    );
    let certificate = tls
        .get("certificate")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let presented = base64::engine::general_purpose::STANDARD
        .encode(TlsCertificateAuthority::for_this_process().der());
    assert_eq!(
        certificate, presented,
        "case \"a data plane that speaks TLS\": expected the certificate this emulator's core \
         presents on its listeners, got {certificate:?}"
    );
    assert_eq!(
        tls.get("unwrapped").and_then(Value::as_bool),
        Some(true),
        "case \"a data plane that speaks TLS\": expected the session to be released back to \
         the socket it ran on, got {tls}"
    );

    // And the addresses that are no plane of this emulator: answered the way a host answers
    // one, with no connection opened at all.
    let refused: &[(&str, &str, Option<i64>)] = &[
        (
            "a port of this machine whose plane is gone",
            "ConnectionRefusedError",
            Some(111),
        ),
        ("an address of another host", "OSError", Some(101)),
        ("a host of AWS's own naming", "gaierror", Some(-2)),
    ];
    for (case, kind, errno) in refused {
        let reported = document.get(*case).cloned().unwrap_or(Value::Null);
        assert_eq!(
            reported.get("ok").and_then(Value::as_bool),
            Some(false),
            "case {case:?}: expected the connection to be refused, got {reported}"
        );
        let reported_kind = reported
            .get("error_type")
            .and_then(Value::as_str)
            .unwrap_or("<not reported>");
        assert_eq!(
            reported_kind, *kind,
            "case {case:?}: expected the error a host answers with, got {reported_kind:?} in \
             {reported}"
        );
        assert_eq!(
            reported.get("errno").and_then(Value::as_i64),
            *errno,
            "case {case:?}: expected errno {errno:?}, got {reported}"
        );
    }
}

/// The probe this suite runs: its module body imports the socket interface and the TLS layer,
/// exactly as a recorded in-VPC probe's does, and its handler opens the connections the event
/// names and reports what each came to.
const PROBE_SOURCE: &str = r#"
import base64
import socket
import ssl

_PREFIX = "ANSWER "


def _read_exactly(reader, size):
    held = b""
    while len(held) < size:
        block = reader.recv(size - len(held))
        if not block:
            break
        held += block
    return held


def _plain(case):
    opened = socket.create_connection((case["host"], case["port"]), 10)
    try:
        payload = case["send"].encode("utf-8")
        opened.sendall(payload)
        answer = _read_exactly(opened, len(_PREFIX) + len(payload))
        return {
            "answer": answer.decode("utf-8", "replace"),
            "peer": list(opened.getpeername()),
            "timeout": opened.gettimeout(),
        }
    finally:
        opened.close()


def _tls(case):
    context = ssl.create_default_context()
    opened = socket.create_connection((case["host"], case["port"]), 10)
    try:
        wrapped = context.wrap_socket(opened, server_hostname=case["host"])
        payload = case["send"].encode("utf-8")
        wrapped.sendall(payload)
        answer = _read_exactly(wrapped, len(_PREFIX) + len(payload))
        negotiated = wrapped.cipher()
        version = wrapped.version()
        certificate = wrapped.getpeercert(True) or b""
        released = wrapped.unwrap()
        return {
            "answer": answer.decode("utf-8", "replace"),
            "version": version,
            "cipher": negotiated[0] if negotiated else None,
            "bits": negotiated[2] if negotiated else None,
            "certificate": base64.b64encode(certificate).decode("ascii"),
            "unwrapped": isinstance(released, socket.socket),
        }
    finally:
        opened.close()


def _makefile(case):
    opened = socket.create_connection((case["host"], case["port"]), 10)
    try:
        held = opened.makefile("rwb")
        held.write(case["send"].encode("utf-8"))
        held.flush()
        return {"answer": held.readline().decode("utf-8", "replace")}
    finally:
        opened.close()


def _sequential(case):
    answers = []
    for at in range(int(case["count"])):
        opened = socket.create_connection((case["host"], case["port"]), 10)
        try:
            payload = ("%s-%d\n" % (case["send"].strip(), at)).encode("utf-8")
            opened.sendall(payload)
            answers.append(
                _read_exactly(opened, len(_PREFIX) + len(payload)).decode("utf-8", "replace")
            )
        finally:
            opened.close()
    return {"answers": answers}


_KINDS = {
    "plain": _plain,
    "tls": _tls,
    "makefile": _makefile,
    "sequential": _sequential,
}


def _run(case):
    try:
        answered = _KINDS[case["kind"]](case)
        answered["ok"] = True
        return answered
    except BaseException as failed:
        return {
            "ok": False,
            "error_type": type(failed).__name__,
            "error": str(failed),
            "errno": getattr(failed, "errno", None),
        }


def lambda_handler(event, context):
    return dict((case["name"], _run(case)) for case in event["cases"])
"#;
