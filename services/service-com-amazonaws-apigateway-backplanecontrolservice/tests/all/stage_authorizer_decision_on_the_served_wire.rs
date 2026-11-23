// SPDX-License-Identifier: CC0-1.0

//! What a client READING THE WIRE is answered by a `CUSTOM`-authorized method of a deployed
//! stage: the authorizer's decision, as this emulator's OWN entry point serves it on a socket.
//!
//! THE RULE UNDER TEST, one level above the recorded case:
//!
//! > A request to a deployed method whose `authorizationType` is `CUSTOM` is answered
//! > SYNCHRONOUSLY, on the connection it arrived on, with the decision the `Authorizer`'s
//! > function returned for the value the request carried at the authorizer's `identitySource`:
//! > a policy allowing `execute-api:Invoke` on the invoked method is the integration's own
//! > answer, a policy that explicitly denies it is `403` with API Gateway's explicit-deny
//! > sentence, and a request carrying nothing at the identity source is `401`
//! > `{"message":"Unauthorized"}`. Each answer is a COMPLETE HTTP response: its status line,
//! > its own `content-length` and exactly that many bytes of body, so a client that keeps the
//! > connection open reads the next answer on the same connection rather than waiting for
//! > bytes that never come.
//!
//! Why this suite exists beside [`crate::stage_authorizer_deny_blocks_the_backend`], which
//! pins the same decisions through the router IN PROCESS: a client of a published invoke URL
//! is a stock HTTP client, and what it gets is the response as the SERVED socket wrote it. An
//! answer whose framing disagrees with its body — a `content-length` naming other than the
//! bytes that follow, a connection closed before the body — is the same object in process and
//! a client that blocks on the wire, so the only level at which "the caller is answered" is
//! observable at all is a real socket. The router is served the way this emulator's own
//! entrypoint serves it (`emulator_server::serve_on_ephemeral_port`), with request
//! authentication ON, as a started emulator verifies signatures: an invoke of a deployed stage
//! is served unsigned, and the value at the identity source is a bearer token rather than a
//! credential.
//!
//! And what the stage RECORDED about each of those answers: the access-log record names the
//! request id the answer reported on the wire, the status it carried and the STAGE-QUALIFIED
//! path of the api — `$context.path` is "the request path", which carries nothing of the
//! address the client happened to reach this emulator by, so one request has one path whichever
//! way it was addressed and whether the api served it or refused it.
//!
//! The rows differ along what the rule generalises over: WHAT the authorizer's function
//! returned for the presented token (an `Allow` naming the invoked method, an explicit `Deny`,
//! an `Allow` naming another method), whether the identity source is carried at all, and HOW
//! the request addresses the stage (the api's own `execute-api` host, and that endpoint carried
//! in the path behind the reserved `/_aws` namespace on the emulator's own address). Every row
//! travels on ONE connection, in order, so a row is read only if the row before it was framed.
//!
//! The wiring — both functions, the api, its authorizer, the `CUSTOM` method, the grants and
//! the deployment — is the sibling suite's ([`deployed`]), so the two suites cannot disagree
//! about the api under test.

use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use crate::stage_authorizer_deny_blocks_the_backend::{
    awaited_message, delivered, deployed, emulator, ACCESS_GROUP, EXPLICIT_DENY, IDENTITY_HEADER,
    NOT_AUTHORIZED, REGION, RESOURCE_PATH, STAGE, UNAUTHORIZED_MESSAGE,
};

/// The field the answer reports the id of the request it processed in, which the access-log
/// record's `$context.requestId` has to equal.
const REQUEST_ID_FIELD: &str = "x-amzn-requestid";

/// How long one answer is waited for on the socket. Generous, and on the ANSWER rather than on
/// a span: nothing here is graded on how fast the host runs, and the wait exists only so a
/// stalled read fails the suite instead of hanging the whole test binary.
const ANSWER_WAIT: Duration = Duration::from_secs(60);

/// How the request addresses the deployed stage.
#[derive(Debug, Clone, Copy)]
enum Addressing {
    /// The api's own `{api-id}.execute-api.{region}.amazonaws.com` host, which is the address
    /// API Gateway publishes for a deployed stage.
    OwnEndpointHost,
    /// The emulator's own address, with that endpoint named in the path behind the reserved
    /// `/_aws` namespace — the address a client that resolves AWS's own hostnames nowhere
    /// reaches the very same stage by.
    EndpointInThePath,
}

impl Addressing {
    /// The request target and the `host` field one row sends, given the api, the marker the
    /// request carries and the authority the served socket answers on.
    fn target(self, api: &str, marker: &str, served: &str) -> (String, String) {
        let path = format!("/{STAGE}/{RESOURCE_PATH}?marker={marker}");
        match self {
            Self::OwnEndpointHost => (path, format!("{api}.execute-api.{REGION}.amazonaws.com")),
            Self::EndpointInThePath => (
                format!("/_aws/execute-api/{api}{path}"),
                served.to_string(),
            ),
        }
    }
}

/// One row: the token presented, how the request is addressed, and what the stage owes it.
struct Row {
    case: &'static str,
    /// The value at the authorizer's identity source, or `None` for a request carrying none.
    token: Option<&'static str>,
    addressing: Addressing,
    /// The status the client reads off the wire.
    status: u16,
    /// A fragment the body carries — the refusal's own sentence, or the marker the integration
    /// answered with, which is the request's own marker and is substituted per row.
    body_holds: &'static str,
}

/// The table the rule is asserted over, in the order it travels the one connection.
const ROWS: &[Row] = &[
    Row {
        case: "a token whose policy allows the invoked method, on the api's own endpoint host",
        token: Some("allow-me"),
        addressing: Addressing::OwnEndpointHost,
        status: 200,
        body_holds: "{marker}",
    },
    Row {
        case: "a token whose policy explicitly denies the invoked method",
        token: Some("deny-me"),
        addressing: Addressing::OwnEndpointHost,
        status: 403,
        body_holds: EXPLICIT_DENY,
    },
    Row {
        case: "a token whose policy allows another method of the same api",
        token: Some("other-method"),
        addressing: Addressing::OwnEndpointHost,
        status: 403,
        body_holds: NOT_AUTHORIZED,
    },
    Row {
        case: "a request carrying nothing at the identity source",
        token: None,
        addressing: Addressing::OwnEndpointHost,
        status: 401,
        body_holds: UNAUTHORIZED_MESSAGE,
    },
    Row {
        case: "the same deny, reached with the endpoint carried in the path",
        token: Some("deny-me"),
        addressing: Addressing::EndpointInThePath,
        status: 403,
        body_holds: EXPLICIT_DENY,
    },
    Row {
        case: "a token whose policy allows the invoked method, with the endpoint in the path",
        token: Some("allow-me"),
        addressing: Addressing::EndpointInThePath,
        status: 200,
        body_holds: "{marker}",
    },
];

/// One answer as it was READ off the wire: the status of its status line, the field lines it
/// carried and the body its own `content-length` named.
#[derive(Debug)]
struct OnTheWire {
    status: u16,
    /// The id the answer reported the request under, which its access-log record names.
    request_id: Option<String>,
    head: String,
    body: String,
}

/// Read one HTTP/1.1 response from `socket`: the head up to the empty line, then exactly the
/// `content-length` the head declared.
///
/// A response whose head never ends, or whose body is shorter than the length it declared,
/// leaves this `Err(reason)` — which is what a client that blocks on such an answer sees, and
/// what this suite exists to catch. An answer carrying no `content-length` at all is such a
/// reason too: every answer this plane sends declares its own length, and a chunked one would
/// be a different framing than the rule under test states.
async fn read_answer(socket: &mut tokio::net::TcpStream) -> Result<OnTheWire, String> {
    let mut raw: Vec<u8> = Vec::new();
    let mut byte = [0_u8; 1];
    // The head, byte by byte: the body may not be read past, because the next row's answer
    // follows it on this same connection.
    while !raw.ends_with(b"\r\n\r\n") {
        match socket.read(&mut byte).await {
            Ok(0) => {
                return Err(format!(
                    "the connection closed after {} byte(s) of the head: {:?}",
                    raw.len(),
                    String::from_utf8_lossy(&raw)
                ))
            }
            Ok(_) => raw.push(byte[0]),
            Err(error) => return Err(format!("the head could not be read: {error}")),
        }
    }
    let head = String::from_utf8_lossy(&raw).to_string();
    let status: u16 = head
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| format!("no status in the status line of {head:?}"))?;
    let request_id = field_of(&head, REQUEST_ID_FIELD);
    let length: usize = field_of(&head, "content-length")
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| format!("no content-length field in {head:?}"))?;
    let mut body = vec![0_u8; length];
    socket
        .read_exact(&mut body)
        .await
        .map_err(|error| format!("the {length}-byte body the head declared was not read: {error}"))?;
    Ok(OnTheWire {
        status,
        request_id,
        head,
        body: String::from_utf8_lossy(&body).to_string(),
    })
}

/// The value of the field line `name` of a response head, or `None` when it carries none.
fn field_of(head: &str, name: &str) -> Option<String> {
    head.lines().find_map(|line| {
        let (field, value) = line.split_once(':')?;
        field
            .eq_ignore_ascii_case(name)
            .then(|| value.trim().to_string())
    })
}

/// RULE: the authorizer's decision reaches a client on the connection it asked on, as a
/// complete HTTP response — whichever way the policy decided and however the stage was
/// addressed.
#[tokio::test]
async fn the_authorizer_decision_reaches_a_client_reading_the_wire() {
    let mut router = emulator();
    let api = deployed(&router).await;
    // Signature verification belongs to the started emulator, and this is the level at which
    // it holds or does not: the invoke below carries a bearer token at the identity source and
    // no credential at all, as a client of a published invoke URL sends none.
    router.set_request_authentication(true);

    // The SAME router, served by the framework that serves this emulator's own data port. The
    // handle it answers with is that router, which the records below are read back through.
    let (endpoint, mut served) = ::emulator_server::serve_on_ephemeral_port(router).await;
    let port: u16 = endpoint
        .rsplit_once(':')
        .and_then(|(_before, port)| port.parse().ok())
        .unwrap_or_else(|| panic!("no port in the served endpoint {endpoint:?}"));
    let served_authority = format!("127.0.0.1:{port}");
    let mut socket = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("the served port accepts a connection");
    // Each row's case, the status it was answered with and the id the answer reported it under,
    // in the order the connection carried them.
    let mut answers: Vec<(&str, u16, String)> = Vec::new();

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let marker = format!("wire-{index}");
        let (target, host) = row.addressing.target(&api, &marker, &served_authority);
        let identity = match row.token {
            Some(token) => format!("{IDENTITY_HEADER}: {token}\r\n"),
            None => String::new(),
        };
        // The field lines a stock client sends beside the ones this row is about: the answer is
        // read on a kept-open connection, which is what every HTTP/1.1 client does.
        let request = format!(
            "GET {target} HTTP/1.1\r\nhost: {host}\r\n{identity}user-agent: emulator-suite\r\n\
             accept: */*\r\naccept-encoding: gzip, deflate\r\nconnection: keep-alive\r\n\r\n"
        );
        socket
            .write_all(request.as_bytes())
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: the request is written: {error}"));

        let answered = match ::tokio::time::timeout(ANSWER_WAIT, read_answer(&mut socket)).await {
            Ok(Ok(answered)) => answered,
            Ok(Err(reason)) => panic!("case {case:?}: no complete answer on the wire: {reason}"),
            Err(_elapsed) => panic!(
                "case {case:?}: the stage sent no answer a client could read on the connection \
                 it asked on"
            ),
        };
        assert_eq!(
            answered.status, row.status,
            "case {case:?}: expected HTTP {} on the wire, got {} with {:?}",
            row.status, answered.status, answered.body
        );
        let expected = row.body_holds.replace("{marker}", &marker);
        assert!(
            answered.body.contains(&expected),
            "case {case:?}: expected the answer's body to carry {expected:?}, got {:?} under \
             {:?}",
            answered.body,
            answered.head
        );
        let request_id = answered.request_id.clone().unwrap_or_else(|| {
            panic!(
                "case {case:?}: the answer reported no {REQUEST_ID_FIELD} field under {:?}",
                answered.head
            )
        });
        answers.push((case, row.status, request_id));
    }

    // What the stage RECORDED about each of those answers, read back through CloudWatch Logs'
    // own reader on the same router — with verification off for this suite's own reads, which
    // carry no real signature.
    served.set_request_authentication(false);
    for (case, status, request_id) in answers {
        let awaited = awaited_message(&served, ACCESS_GROUP, |message| {
            message.contains(&request_id)
        })
        .await;
        let record = match awaited {
            Some(record) => record,
            // Only the failure needs the whole group, so it is read only then.
            None => panic!(
                "case {case:?}: expected an access-log record for {request_id:?} in \
                 {ACCESS_GROUP:?}, got {:?}",
                delivered(&served, ACCESS_GROUP).await
            ),
        };
        let logged: Value = ::serde_json::from_str(&record).unwrap_or(Value::Null);
        assert_eq!(
            (logged.get("path").cloned(), logged.get("status").cloned()),
            (
                Some(json!(format!("/{STAGE}/{RESOURCE_PATH}"))),
                Some(json!(status.to_string()))
            ),
            "case {case:?}: expected the stage's record of this request to name the \
             stage-qualified path and the status it was answered with, got {record:?}"
        );
    }
}
