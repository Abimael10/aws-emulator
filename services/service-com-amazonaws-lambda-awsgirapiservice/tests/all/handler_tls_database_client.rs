// SPDX-License-Identifier: CC0-1.0

//! End to end: the whole shape a TLS-ONLY DATABASE CLIENT takes inside a `python3.x`
//! function — `CreateFunction` then `Invoke` over the router, against a data-plane listener
//! this emulator's own core opened in this process.
//!
//! The rule pinned here: **a managed `python3.x` runtime presents the standard library of the
//! Python it advertises, so a handler module whose body imports `ssl` beside `socket`,
//! `hashlib`, `hmac`, `struct` and `base64` imports cleanly and the client it is written for
//! RUNS — the key derivation and keyed digests its authentication needs compute the published
//! values, `struct` frames the wire messages, and the TLS session it negotiates over a
//! connection to a plane this emulator serves carries those frames to the plane and back.**
//! A client of a database that only accepts TLS necessarily imports `ssl` in its module body,
//! so an import that fails is the runtime missing part of its own standard library, which is
//! never the caller's deployment package's fault.
//!
//! The rows differ along what the rule generalises over: the digest family the derivation is
//! keyed with, the iteration count, the password and salt bytes (including an embedded NUL and
//! inputs longer than one block), the derived-key length, the message the signature is over,
//! whether the frames travel under a negotiated session or in the clear, how many frames one
//! session carries and how large each frame's body is.
//!
//! What the expectations are held against is never this runtime's own answer: a derivation's
//! value is the published vector for it, or — at one iteration, where the construction IS one
//! application of its pseudorandom function — the value the `hmac` crate computes here, and
//! every value derived from it is computed in this test from maintained crates.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use emulator_server::{ListenerConn, NativeHandler, ResourceListener};
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::ACTIVATION_DELAY;
use service_com_amazonaws_lambda_awsgirapiservice::EmulatedService;
use service_common::data_plane_addressing::listener_authority;
use service_common::dialable_data_planes::{
    served_in_process, DialablePlanes, OpenedPlanes, SharedHandler,
};
use sha1::Sha1;
use sha2::{Digest as _, Sha256};
use smithy_server_runtime::MultiServiceRouter;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

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
/// scheduling, not for the handler's work.
const FUNCTION_TIMEOUT_SECONDS: i32 = 25;

/// How many times the invocation is re-driven while the answer reports one of this test
/// binary's own host bounds rather than a run.
const HOST_BOUND_ATTEMPTS: usize = 6;

/// What this suite names as the service answering at the addresses it opens: these stand for
/// a database's own endpoint.
const SERVED_BY: &str = "lambda-database-client-probe";

/// The length prefix of one frame of the plane's protocol, in bytes: a little-endian 32-bit
/// count of the whole frame, the prefix included — the framing a document database's wire
/// protocol puts in front of every message.
const HEADER: usize = 4;

/// What the key derivation is keyed with for the authentication exchange, and the fixed
/// strings its two derived keys are named by.
///
/// Source: RFC 5802 (SCRAM), whose client proof is
/// `ClientKey XOR HMAC(H(ClientKey), AuthMessage)` with `ClientKey = HMAC(SaltedPassword,
/// "Client Key")` — the computation a document database's SCRAM-SHA-1 and SCRAM-SHA-256
/// mechanisms run in the client.
const CLIENT_KEY_LABEL: &[u8] = b"Client Key";

/// One row of the derivation table: what a client computes before it has sent a byte.
struct Derivation {
    /// What the case is called, in the event and in every assertion.
    case: &'static str,
    /// The digest family the derivation and the signatures are keyed with.
    algorithm: &'static str,
    /// The secret the key is derived from, and the salt the server would have sent.
    password: &'static [u8],
    salt: &'static [u8],
    /// How many iterations the derivation runs, and how many bytes it answers.
    iterations: u32,
    dklen: usize,
    /// The message the client's signature is over.
    auth_message: &'static str,
    /// The PUBLISHED value of the derivation, where one is published for these inputs.
    ///
    /// Source: RFC 6070, the test vectors for PBKDF2 with HMAC-SHA-1. A row with none is
    /// checked against the construction's own definition instead (see [`derived_key`]).
    published: Option<&'static str>,
}

/// One row of the session table: the frames a client sends on one connection.
struct Session {
    /// What the case is called, in the event and in every assertion.
    case: &'static str,
    /// Whether the client negotiates a TLS session before it sends a frame.
    tls: bool,
    /// The plane's address, as a control plane publishes one.
    host: String,
    port: u16,
    /// The derivation row whose client proof is sent as the session's FIRST frame body, so
    /// the bytes that cross the session are the authentication the client computed.
    proof_from: Option<&'static str>,
    /// The bodies of the frames that follow it.
    bodies: Vec<Vec<u8>>,
}

/// A data plane of this emulator, as a resource's own listener.
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

/// The protocol the planes of this suite serve: read one length-prefixed frame, answer one
/// length-prefixed frame whose body is the SHA-256 of the body that arrived.
///
/// Framed on the prefix rather than on one read, so a body larger than one write is answered
/// once — and an answer that depends on the WHOLE body is what makes "the bytes that crossed
/// are the bytes the client computed" assertable from outside the run.
struct FrameDigest;

impl NativeHandler for FrameDigest {
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
                loop {
                    let body = match next_frame(&mut held) {
                        Framed::Body(body) => body,
                        Framed::Incomplete => break,
                        // A length no frame can have is not something more bytes will
                        // complete, so the connection ends here instead of waiting out the
                        // client's own timeout.
                        Framed::Impossible => return,
                    };
                    let digest = Sha256::digest(&body);
                    let length = (HEADER + digest.len()) as u32;
                    let mut answer = length.to_le_bytes().to_vec();
                    answer.extend_from_slice(&digest);
                    if conn.write_all(&answer).await.is_err() || conn.flush().await.is_err() {
                        return;
                    }
                }
            }
        })
    }
}

/// What the bytes held so far amount to.
enum Framed {
    /// The body of one whole frame, taken out of what has arrived.
    Body(Vec<u8>),
    /// The frame is still arriving.
    Incomplete,
    /// The prefix names a length no frame of this protocol can have, which no further byte
    /// makes well-formed.
    Impossible,
}

/// The next whole frame held, taken out of what has arrived.
fn next_frame(held: &mut Vec<u8>) -> Framed {
    if held.len() < HEADER {
        return Framed::Incomplete;
    }
    let prefix: [u8; HEADER] = held[..HEADER].try_into().expect("the prefix is four bytes");
    let length = u32::from_le_bytes(prefix) as usize;
    if length < HEADER {
        return Framed::Impossible;
    }
    if held.len() < length {
        return Framed::Incomplete;
    }
    let frame: Vec<u8> = held.drain(..length).collect();
    Framed::Body(frame[HEADER..].to_vec())
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

/// One AWS Lambda call over its `restJson1` wire. Nothing in the credential scope is
/// verified: the harness turns request authentication off.
async fn lambda(router: &MultiServiceRouter, method: &str, uri: &str, body: &[u8]) -> Answer {
    let host = format!("lambda.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("http://{host}{uri}"))
        .header("host", host)
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=emulator-test-key/20260101/{REGION}/lambda/\
                 aws4_request, SignedHeaders=host, Signature=not-verified"
            ),
        )
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
///
/// The import is the rule's first half, so the absence of `X-Amz-Function-Error` is asserted
/// here: a module body that imports `ssl` has to import, and a runtime missing part of its own
/// standard library is exactly what answers an invocation with one.
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
        "the handler's module body imports the TLS layer of its own standard library, so the \
         module imports and the invocation carries no function error, got {failure:?} with {}",
        answer.text()
    );
    answer.json()
}

/// Open a data plane of this emulator: a listener the core binds, recorded as the plane an
/// in-process client reaches at the address it published — which is what a service owning a
/// resource does when it publishes that resource's endpoint.
async fn open_plane(engine: &Arc<engine::InMemoryEngine>, secured: bool) -> Plane {
    let listeners = engine.listeners();
    let serving = Arc::new(FrameDigest);
    let listener = match secured {
        true => listeners.tcp(SharedHandler(Arc::clone(&serving))).await,
        false => {
            listeners
                .tcp_plaintext(SharedHandler(Arc::clone(&serving)))
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
            &served_in_process(serving, upgrader),
        );
    Plane {
        listener,
        _dialable: dialable,
    }
}

/// The keyed digest of `message` under `key`, from the `hmac` crate: this test's own oracle,
/// so what the run computed is held against a maintained implementation and not against
/// itself.
fn keyed(algorithm: &str, key: &[u8], message: &[u8]) -> Vec<u8> {
    match algorithm {
        "sha1" => {
            let mut mac = Hmac::<Sha1>::new_from_slice(key).expect("HMAC takes a key of any size");
            mac.update(message);
            mac.finalize().into_bytes().to_vec()
        }
        "sha256" => {
            let mut mac =
                Hmac::<Sha256>::new_from_slice(key).expect("HMAC takes a key of any size");
            mac.update(message);
            mac.finalize().into_bytes().to_vec()
        }
        other => panic!("this suite keys with sha1 and sha256, not {other:?}"),
    }
}

/// The plain digest of `data`, from the same crates.
fn digest(algorithm: &str, data: &[u8]) -> Vec<u8> {
    match algorithm {
        "sha1" => Sha1::digest(data).to_vec(),
        "sha256" => Sha256::digest(data).to_vec(),
        other => panic!("this suite digests with sha1 and sha256, not {other:?}"),
    }
}

/// What the derivation of one row must answer.
///
/// Either the published vector for those inputs, or — at ONE iteration, where the
/// construction is defined as a single application of its pseudorandom function over the salt
/// followed by the block index (RFC 8018, section 5.2) — the value the `hmac` crate computes
/// for that one application.
fn derived_key(row: &Derivation) -> Vec<u8> {
    if let Some(published) = row.published {
        let expected = hex::decode(published).expect("the published vector is hexadecimal");
        assert_eq!(
            expected.len(),
            row.dklen,
            "case {:?}: the published vector is {} bytes, not the {} the row asks for",
            row.case,
            expected.len(),
            row.dklen
        );
        return expected;
    }
    assert_eq!(
        row.iterations, 1,
        "case {:?}: a row with no published vector is checked against one application of the \
         pseudorandom function, which is the construction only at one iteration",
        row.case
    );
    let mut over = row.salt.to_vec();
    over.extend_from_slice(&1_u32.to_be_bytes());
    let block = keyed(row.algorithm, row.password, &over);
    assert_eq!(
        block.len(),
        row.dklen,
        "case {:?}: one application of the function answers {} bytes, not the {} the row asks \
         for",
        row.case,
        block.len(),
        row.dklen
    );
    block
}

/// The authentication a client computes from a derived key: its two keys, its signature and
/// the proof it sends, all from this test's own oracle.
struct Authentication {
    client_key: Vec<u8>,
    stored_key: Vec<u8>,
    signature: Vec<u8>,
    proof: Vec<u8>,
}

fn authentication(row: &Derivation) -> Authentication {
    let salted = derived_key(row);
    let client_key = keyed(row.algorithm, &salted, CLIENT_KEY_LABEL);
    let stored_key = digest(row.algorithm, &client_key);
    let signature = keyed(row.algorithm, &stored_key, row.auth_message.as_bytes());
    let proof = client_key
        .iter()
        .zip(signature.iter())
        .map(|(left, right)| left ^ right)
        .collect();
    Authentication {
        client_key,
        stored_key,
        signature,
        proof,
    }
}

/// The derivation table: the key material a TLS-only document database's client computes
/// before it sends its first frame.
///
/// The first five rows are the published vectors; the last two carry inputs no published
/// vector covers, so they are held against the construction's own definition. Together the
/// rows differ along the digest family, the iteration count, the password and salt bytes
/// (an embedded NUL, and inputs longer than one digest block), the derived length and the
/// message the signature is over.
fn derivations() -> Vec<Derivation> {
    vec![
        Derivation {
            case: "one iteration of the published vectors",
            algorithm: "sha1",
            password: b"password",
            salt: b"salt",
            iterations: 1,
            dklen: 20,
            auth_message: "n=dbadmin,r=clientnonce,s=c2FsdA==,i=1,c=biws,r=clientnonce",
            published: Some("0c60c80f961f0e71f3a9b524af6012062fe037a6"),
        },
        Derivation {
            case: "two iterations of the same inputs",
            algorithm: "sha1",
            password: b"password",
            salt: b"salt",
            iterations: 2,
            dklen: 20,
            auth_message: "n=dbadmin,r=clientnonce,s=c2FsdA==,i=2,c=biws,r=clientnonce",
            published: Some("ea6c014dc72d6f8ccd1ed92ace1d41f0d8de8957"),
        },
        Derivation {
            case: "the iteration count a document database asks for",
            algorithm: "sha1",
            password: b"password",
            salt: b"salt",
            iterations: 4096,
            dklen: 20,
            auth_message: "n=dbadmin,r=clientnonce,s=c2FsdA==,i=4096,c=biws,r=clientnonce",
            published: Some("4b007901b765489abead49d926f721d065a429c1"),
        },
        Derivation {
            case: "a password and salt longer than one digest block",
            algorithm: "sha1",
            password: b"passwordPASSWORDpassword",
            salt: b"saltSALTsaltSALTsaltSALTsaltSALTsalt",
            iterations: 4096,
            dklen: 25,
            auth_message: "n=dbadmin,r=longnonce,i=4096,c=biws,r=longnonce",
            published: Some("3d2eec4fe41c849b80c8d83662c0e44a8b291a964cf2f07038"),
        },
        Derivation {
            case: "a password and salt carrying a NUL byte",
            algorithm: "sha1",
            password: b"pass\0word",
            salt: b"sa\0lt",
            iterations: 4096,
            dklen: 16,
            auth_message: "n=dbadmin,r=nulnonce,i=4096,c=biws,r=nulnonce",
            published: Some("56fa6aa75548099dcc37d7f03425e0c3"),
        },
        Derivation {
            case: "the other digest family the mechanism is defined over",
            algorithm: "sha256",
            password: b"password",
            salt: b"salt",
            iterations: 1,
            dklen: 32,
            auth_message: "n=dbadmin,r=clientnonce,s=c2FsdA==,i=1,c=biws,r=clientnonce",
            published: None,
        },
        Derivation {
            case: "a secret and salt of a cluster's own making",
            algorithm: "sha256",
            password: b"sPZ-7Qn4rT2xK9mv",
            salt: b"\x00\x11\x22\x33\x44\x55\x66\x77\x88\x99\xaa\xbb\xcc\xdd\xee\xff",
            iterations: 1,
            dklen: 32,
            auth_message: "n=dbadmin,r=anothernonce,i=1,c=biws,r=anothernonce",
            published: None,
        },
    ]
}

/// The session table: what the client then sends, and over what.
fn sessions(secured: &Plane, plain: &Plane) -> Vec<Session> {
    vec![
        Session {
            case: "the authentication proof under a negotiated session",
            tls: true,
            host: secured.host(),
            port: secured.port(),
            proof_from: Some("the iteration count a document database asks for"),
            bodies: vec![b"insert".to_vec()],
        },
        Session {
            case: "frame after frame on one session",
            tls: true,
            host: secured.host(),
            port: secured.port(),
            proof_from: Some("the other digest family the mechanism is defined over"),
            bodies: vec![
                b"insert".to_vec(),
                b"find".to_vec(),
                // A body larger than one write, which is what makes the plane's framing, and
                // not one read, decide where the frame ends.
                vec![b'd'; 48 * 1024],
                Vec::new(),
            ],
        },
        Session {
            case: "the same frames in the clear",
            tls: false,
            host: plain.host(),
            port: plain.port(),
            proof_from: Some("a secret and salt of a cluster's own making"),
            bodies: vec![b"insert".to_vec(), b"find".to_vec()],
        },
    ]
}

/// The whole rule, driven END TO END: one real `CreateFunction`, one real `Invoke`, the
/// handler's own standard library, and real listeners of this emulator's core.
///
/// ONE invocation for the whole table, the way the sibling run-based suites do it: the memory
/// bound a run is stopped on is read from the WHOLE process and this binary runs its suites in
/// parallel, so one interpreter for the table is a cost this suite does not put on the rest of
/// the target.
#[tokio::test]
async fn a_python_handler_runs_the_client_of_a_tls_only_database() {
    let _stream = one_invocation_stream_at_a_time().await;
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = emulator(&engine);
    let secured = open_plane(&engine, true).await;
    let plain = open_plane(&engine, false).await;
    let function = "tls-database-client-probe";
    create_function(&router, function).await;

    let derivations = derivations();
    let sessions = sessions(&secured, &plain);
    let event = json!({
        "crypto": derivations
            .iter()
            .map(|row| json!({
                "name": row.case,
                "algorithm": row.algorithm,
                "password": hex::encode(row.password),
                "salt": hex::encode(row.salt),
                "iterations": row.iterations,
                "dklen": row.dklen,
                "auth_message": row.auth_message,
            }))
            .collect::<Vec<Value>>(),
        "sessions": sessions
            .iter()
            .map(|row| json!({
                "name": row.case,
                "tls": row.tls,
                "host": row.host,
                "port": row.port,
                "proof_from": row.proof_from,
                "bodies": row.bodies.iter().map(hex::encode).collect::<Vec<String>>(),
            }))
            .collect::<Vec<Value>>(),
    });
    let document = answered(&router, function, &event).await;

    // The derivations: every value the client computed is the published one, or the one the
    // crates here compute for the same inputs.
    for row in &derivations {
        let case = row.case;
        let reported = document
            .get("crypto")
            .and_then(|held| held.get(case))
            .cloned()
            .unwrap_or(Value::Null);
        assert_eq!(
            reported.get("ok").and_then(Value::as_bool),
            Some(true),
            "case {case:?}: expected the client's key derivation to run, got {reported}"
        );
        let expected = authentication(row);
        let salted = derived_key(row);
        for (member, bytes) in [
            ("salted", salted),
            ("client_key", expected.client_key),
            ("stored_key", expected.stored_key),
            ("signature", expected.signature),
        ] {
            let actual = reported
                .get(member)
                .and_then(Value::as_str)
                .unwrap_or("<not reported>");
            assert_eq!(
                actual,
                hex::encode(&bytes),
                "case {case:?}: expected {member} {:?}, got {actual:?} in {reported}",
                hex::encode(&bytes)
            );
        }
        let proof = reported
            .get("proof")
            .and_then(Value::as_str)
            .unwrap_or("<not reported>");
        assert_eq!(
            proof,
            base64::engine::general_purpose::STANDARD.encode(&expected.proof),
            "case {case:?}: expected the client proof the mechanism defines, got {proof:?} in \
             {reported}"
        );
    }

    // The sessions: the frames the client framed reached the plane, and the plane's answer to
    // each is the digest of the body that crossed — so the authentication bytes that arrived
    // are the ones the oracle above computed.
    for row in &sessions {
        let case = row.case;
        let reported = document
            .get("sessions")
            .and_then(|held| held.get(case))
            .cloned()
            .unwrap_or(Value::Null);
        assert_eq!(
            reported.get("ok").and_then(Value::as_bool),
            Some(true),
            "case {case:?}: expected the client's session to be opened and answered, got \
             {reported}"
        );
        let mut expected_bodies: Vec<Vec<u8>> = Vec::new();
        if let Some(named) = row.proof_from {
            let from = derivations
                .iter()
                .find(|candidate| candidate.case == named)
                .expect("a session names a derivation row of this suite");
            expected_bodies.push(authentication(from).proof);
        }
        expected_bodies.extend(row.bodies.iter().cloned());
        let frames = reported
            .get("frames")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            frames.len(),
            expected_bodies.len(),
            "case {case:?}: expected {} frames to be carried, got {reported}",
            expected_bodies.len()
        );
        for (at, body) in expected_bodies.iter().enumerate() {
            let frame = &frames[at];
            let sent = frame.get("sent").and_then(Value::as_str).unwrap_or("");
            assert_eq!(
                sent,
                hex::encode(body),
                "case {case:?} frame {at}: expected the body the client holds, got {sent:?}"
            );
            let prefix = frame.get("header").and_then(Value::as_str).unwrap_or("");
            let framed = (HEADER + body.len()) as u32;
            assert_eq!(
                prefix,
                hex::encode(framed.to_le_bytes()),
                "case {case:?} frame {at}: expected the frame's own length prefix, got \
                 {prefix:?}"
            );
            let answer = frame.get("answer").and_then(Value::as_str).unwrap_or("");
            assert_eq!(
                answer,
                hex::encode(Sha256::digest(body)),
                "case {case:?} frame {at}: expected the plane's answer to the body that \
                 crossed, got {answer:?}"
            );
            let answered_prefix = frame
                .get("answered_header")
                .and_then(Value::as_str)
                .unwrap_or("");
            assert_eq!(
                answered_prefix,
                hex::encode(((HEADER + Sha256::output_size()) as u32).to_le_bytes()),
                "case {case:?} frame {at}: expected the plane's own length prefix, got \
                 {answered_prefix:?}"
            );
        }
        if row.tls {
            let version = reported.get("version").and_then(Value::as_str).unwrap_or("");
            assert!(
                version.starts_with("TLSv1."),
                "case {case:?}: expected the protocol version the handshake negotiated, got \
                 {version:?}"
            );
            let cipher = reported.get("cipher").and_then(Value::as_str).unwrap_or("");
            assert!(
                !cipher.is_empty(),
                "case {case:?}: expected the cipher suite the handshake negotiated, got \
                 {cipher:?}"
            );
        }
    }
}

/// The probe this suite runs: its module body imports the TLS layer of the standard library
/// beside the digest, keyed-digest, framing and encoding modules a database client needs —
/// exactly as a recorded in-VPC probe's does — and its handler computes the authentication the
/// event's rows name and carries it to the planes the event addresses.
const PROBE_SOURCE: &str = r#"
import base64
import hashlib
import hmac
import socket
import ssl
import struct

# The whole frame's length, the prefix included, is the first little-endian 32-bit integer of
# every message of the plane's protocol.
_PREFIX = 4


def _read_exactly(conn, size):
    held = b""
    while len(held) < size:
        block = conn.recv(size - len(held))
        if not block:
            break
        held += block
    return held


def _derive(case):
    algorithm = case["algorithm"]
    salted = hashlib.pbkdf2_hmac(
        algorithm,
        bytes.fromhex(case["password"]),
        bytes.fromhex(case["salt"]),
        int(case["iterations"]),
        int(case["dklen"]),
    )
    client_key = hmac.new(salted, b"Client Key", algorithm).digest()
    stored_key = hashlib.new(algorithm, client_key).digest()
    signature = hmac.new(
        stored_key, case["auth_message"].encode("utf-8"), algorithm
    ).digest()
    proof = bytes(left ^ right for left, right in zip(client_key, signature))
    reported = {
        "salted": salted.hex(),
        "client_key": client_key.hex(),
        "stored_key": stored_key.hex(),
        "signature": signature.hex(),
        "proof": base64.b64encode(proof).decode("ascii"),
    }
    return proof, reported


def _session(case, proofs):
    bodies = []
    named = case.get("proof_from")
    if named:
        bodies.append(proofs[named])
    for held in case.get("bodies") or []:
        bodies.append(bytes.fromhex(held))
    opened = socket.create_connection((case["host"], case["port"]), 10)
    try:
        conn = opened
        if case["tls"]:
            conn = ssl.create_default_context().wrap_socket(
                opened, server_hostname=case["host"]
            )
        frames = []
        for body in bodies:
            prefix = struct.pack("<i", _PREFIX + len(body))
            conn.sendall(prefix + body)
            answered = _read_exactly(conn, _PREFIX)
            length = struct.unpack("<i", answered)[0]
            answer = _read_exactly(conn, length - _PREFIX)
            frames.append(
                {
                    "sent": body.hex(),
                    "header": prefix.hex(),
                    "answered_header": answered.hex(),
                    "answer": answer.hex(),
                }
            )
        reported = {"frames": frames}
        if case["tls"]:
            negotiated = conn.cipher()
            reported["version"] = conn.version()
            reported["cipher"] = negotiated[0] if negotiated else None
        return reported
    finally:
        opened.close()


def _failed(error):
    return {
        "ok": False,
        "error_type": type(error).__name__,
        "error": str(error),
    }


def lambda_handler(event, context):
    derived = {}
    proofs = {}
    for case in event["crypto"]:
        try:
            proof, reported = _derive(case)
            proofs[case["name"]] = proof
            reported["ok"] = True
            derived[case["name"]] = reported
        except BaseException as failed:
            derived[case["name"]] = _failed(failed)
    sessions = {}
    for case in event["sessions"]:
        try:
            reported = _session(case, proofs)
            reported["ok"] = True
            sessions[case["name"]] = reported
        except BaseException as failed:
            sessions[case["name"]] = _failed(failed)
    return {"crypto": derived, "sessions": sessions}
"#;
