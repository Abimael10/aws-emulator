// SPDX-License-Identifier: CC0-1.0

//! The HTTP SERVER a managed node runtime presents to a handler, end to end over the wire:
//! a `nodejs*` function whose code creates a server of its own, listens on a loopback
//! address and then sends its own requests to that address.
//!
//! RULE: a managed node runtime supplies the SERVER half of the `http` and `https` core
//! modules — a server a handler creates holds the port it bound for as long as it listens,
//! and a request the same execution environment addresses at a loopback address and that
//! port is answered by that server's own listener: the status, the reason phrase, the
//! headers it wrote and the ones a server writes for every answer beside them (when it
//! answered, and how the body is framed), over a request whose method, target, authority
//! and body are the ones the sender stated. A port no server holds answers nothing, and a
//! port one server holds is not bound a second time.
//!
//! That is the environment a handler builds when it runs a local callback receiver, a
//! sidecar endpoint or a client of its own against an address it serves itself — on a
//! managed runtime such an address is of the execution environment alone, which no caller
//! outside it reaches.
//!
//! The rows differ along what the rule generalises over: the METHOD sent and whether it
//! carries a body, the STATUS answered and whether the response states its reason phrase
//! itself, HOW the response is written (one call, or a head and several writes), WHICH form
//! the `listen` call takes and whether it names the port or asks for any free one, WHICH
//! loopback name the sender addresses, and the LIFECYCLE of the port — one released by
//! `close`, and one a second server asks for while it is held.
//!
//! The suite RUNS handlers, so it takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use std::sync::Arc;

use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

use crate::invocation_log_delivery::{function_memory_size_mb, function_timeout_seconds};

/// Where `CreateFunction` is bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// A SigV4 credential scope naming this service's signing name. Nothing in it is verified:
/// the harness turns request authentication off.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=emulator-test-key/20260101/us-east-1/lambda/aws4_request, \
     SignedHeaders=host, Signature=not-verified";

/// A well-formed execution role ARN, satisfying the model's `RoleArn` pattern.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// The `Runtime` the function carries: a managed node runtime, which is what supplies the
/// two modules under test.
const RUNTIME: &str = "nodejs22.x";

/// The function every row invokes.
const FUNCTION: &str = "node-http-server-probe";

/// The header the sender puts on its request, which the server reports back having read.
const PROBE_HEADER: &str = "sent by the handler";

/// The header the server puts on its answer, which the sender reports back having read.
const ANSWERED_BY: &str = "the handler's own server";

/// The handler: it starts a server that answers as the event asks, sends ONE request of its
/// own to the address that server bound, and reports what each side saw.
///
/// Nothing here synthesises either half: the request the server reports is the one the
/// client sent, and the answer the client reports is the one the server wrote.
const HANDLER_SOURCE: &str = r#"
'use strict';

const http = require('http');

// The server one row asks for: it reads the whole request, then answers the status, the
// phrase and the headers the row states, with what it read as the document.
function started(event) {
    return new Promise((resolve, reject) => {
        const server = http.createServer((request, response) => {
            let body = '';
            request.setEncoding('utf8');
            request.on('data', (chunk) => { body += chunk; });
            request.on('end', () => {
                const document = JSON.stringify({
                    seen: {
                        method: request.method,
                        url: request.url,
                        host: request.headers.host,
                        probe: request.headers['x-probe'] || '',
                        version: request.httpVersion,
                        body: body,
                    },
                });
                if (event.write === 'in-parts') {
                    response.statusCode = event.status;
                    response.setHeader('content-type', 'application/json');
                    response.setHeader('x-answered-by', event.answeredBy);
                    response.write(document.slice(0, 7));
                    response.end(document.slice(7));
                    return;
                }
                const headers = {
                    'content-type': 'application/json',
                    'x-answered-by': event.answeredBy,
                };
                if (event.phrase) {
                    response.writeHead(event.status, event.phrase, headers);
                } else {
                    response.writeHead(event.status, headers);
                }
                response.end(document);
            });
        });
        server.on('error', reject);
        if (event.listen === 'port-and-host') {
            server.listen(event.port, '127.0.0.1', () => resolve(server));
        } else if (event.listen === 'options') {
            server.listen({ port: event.port, host: '127.0.0.1' }, () => resolve(server));
        } else {
            server.listen(event.port, () => resolve(server));
        }
    });
}

// ONE request to `port`, as the row states it, reported as what came back — or as the
// failure the request reported.
function asked(port, event) {
    return new Promise((resolve) => {
        const sending = http.request({
            hostname: event.host,
            port: port,
            path: event.path,
            method: event.method,
            headers: { 'x-probe': event.probe },
        }, (answer) => {
            let text = '';
            answer.setEncoding('utf8');
            answer.on('data', (chunk) => { text += chunk; });
            answer.on('end', () => resolve({
                status: answer.statusCode,
                reason: answer.statusMessage,
                type: answer.headers['content-type'],
                answeredBy: answer.headers['x-answered-by'],
                length: answer.headers['content-length'],
                encoding: answer.headers['transfer-encoding'],
                dated: typeof answer.headers.date === 'string'
                    && !isNaN(Date.parse(answer.headers.date)),
                complete: answer.complete,
                text: text,
            }));
        });
        sending.on('error', (error) => resolve({ failed: error.message, code: error.code }));
        if (event.body) {
            sending.end(event.body);
        } else {
            sending.end();
        }
    });
}

// A second server asking for the port the first one holds.
function alsoTaking(port) {
    return new Promise((resolve) => {
        const second = http.createServer(() => undefined);
        second.on('error', (error) => resolve({ code: error.code, message: error.message }));
        second.on('listening', () => {
            second.close();
            resolve({ listening: true });
        });
        second.listen(port, '127.0.0.1');
    });
}

const closed = (server) => new Promise((resolve) => server.close(() => resolve(true)));

exports.handler = async (event) => {
    const server = await started(event);
    const bound = server.address();
    const answered = await asked(bound.port, event);
    const report = { bound: bound, answered: answered };
    if (event.then === 'close') {
        report.closed = await closed(server);
        report.listening = server.listening;
        report.afterClose = await asked(bound.port, event);
        return report;
    }
    if (event.then === 'take-the-port') {
        report.alsoTaken = await alsoTaking(bound.port);
    }
    await closed(server);
    return report;
};
"#;

/// What a dispatched request answered.
struct Answer {
    status: u16,
    headers: http::HeaderMap,
    body: bytes::Bytes,
}

impl Answer {
    fn json(&self) -> Value {
        if self.body.is_empty() {
            return Value::Null;
        }
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    fn header(&self, name: &str) -> Option<String> {
        self.headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    }
}

/// One Lambda emulator on its own engine, reached the way the production router reaches it.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(&engine)));
    router
}

/// Dispatch one request to the Lambda emulator.
async fn call(router: &MultiServiceRouter, method: &str, uri: &str, body: &[u8]) -> Answer {
    let request = http::Request::builder()
        .method(method)
        .uri(format!("http://lambda.us-east-1.amazonaws.com{uri}"))
        .header("host", "lambda.us-east-1.amazonaws.com")
        .header("authorization", AUTHORIZATION)
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

/// Create the probe function and wait until it reports `Active`.
async fn given_function(router: &MultiServiceRouter) {
    let created = call(
        router,
        "POST",
        FUNCTIONS_URI,
        &serde_json::to_vec(&json!({
            "FunctionName": FUNCTION,
            "Role": ROLE,
            "Runtime": RUNTIME,
            "Handler": "index.handler",
            "Timeout": function_timeout_seconds(),
            "MemorySize": function_memory_size_mb(),
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(crate::node_invoke_run::package(&[("index.js", HANDLER_SOURCE)])),
            },
        }))
        .expect("the create body serializes"),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "expected CreateFunction to answer 201, got {} {}",
        created.status,
        created.text()
    );
    await_active(router).await;
}

/// Wait until the function reports `Active`, which is when an invocation runs its code.
async fn await_active(router: &MultiServiceRouter) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    loop {
        let read = call(
            router,
            "GET",
            &format!("{FUNCTIONS_URI}/{FUNCTION}/configuration"),
            &[],
        )
        .await;
        let state = read
            .json()
            .get("State")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if state == "Active" {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "expected the function to become Active, it reports {state:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// One row: what the handler is asked to serve and to send, and what each side must report.
struct Row {
    case: &'static str,
    /// The event the handler is invoked with.
    event: Value,
    /// The port the server must report having bound, or `None` for a row that asked for any
    /// free one.
    bound: Option<u64>,
    /// The status the sender must read back.
    status: u64,
    /// The reason phrase it must read back.
    reason: &'static str,
    /// The request target the server must report having read.
    url: &'static str,
    /// The body it must report having read.
    body: &'static str,
}

/// The port the rows that name one ask for. It is in the range IANA registers for ephemeral
/// use, which is where a port a program picks for itself comes from.
const NAMED_PORT: u64 = 51515;

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a GET answered by the handler's own server",
            event: json!({
                "method": "GET", "path": "/probe?first=1", "status": 200,
                "host": "127.0.0.1", "port": 0, "probe": PROBE_HEADER,
                "answeredBy": ANSWERED_BY,
            }),
            bound: None,
            status: 200,
            reason: "OK",
            url: "/probe?first=1",
            body: "",
        },
        Row {
            case: "a POST carrying a body, answered with a created status",
            event: json!({
                "method": "POST", "path": "/receipts", "status": 201,
                "host": "127.0.0.1", "port": 0, "probe": PROBE_HEADER,
                "answeredBy": ANSWERED_BY, "body": "{\"callback\":\"done\"}",
            }),
            bound: None,
            status: 201,
            reason: "Created",
            url: "/receipts",
            body: "{\"callback\":\"done\"}",
        },
        Row {
            case: "a status whose reason phrase the server stated itself",
            event: json!({
                "method": "PUT", "path": "/brew", "status": 418,
                "phrase": "a phrase of this server's own",
                "host": "127.0.0.1", "port": 0, "probe": PROBE_HEADER,
                "answeredBy": ANSWERED_BY, "body": "tea",
            }),
            bound: None,
            status: 418,
            reason: "a phrase of this server's own",
            url: "/brew",
            body: "tea",
        },
        Row {
            case: "a status whose phrase the server left to the protocol",
            event: json!({
                "method": "DELETE", "path": "/absent", "status": 404,
                "host": "127.0.0.1", "port": 0, "probe": PROBE_HEADER,
                "answeredBy": ANSWERED_BY,
            }),
            bound: None,
            status: 404,
            reason: "Not Found",
            url: "/absent",
            body: "",
        },
        Row {
            case: "an answer written as a head and several writes",
            event: json!({
                "method": "POST", "path": "/in-parts", "status": 200,
                "write": "in-parts",
                "host": "127.0.0.1", "port": 0, "probe": PROBE_HEADER,
                "answeredBy": ANSWERED_BY, "body": "written in parts",
            }),
            bound: None,
            status: 200,
            reason: "OK",
            url: "/in-parts",
            body: "written in parts",
        },
        Row {
            case: "a port the listen call named itself, with its host",
            event: json!({
                "method": "GET", "path": "/named-port", "status": 200,
                "listen": "port-and-host",
                "host": "127.0.0.1", "port": NAMED_PORT, "probe": PROBE_HEADER,
                "answeredBy": ANSWERED_BY,
            }),
            bound: Some(NAMED_PORT),
            status: 200,
            reason: "OK",
            url: "/named-port",
            body: "",
        },
        Row {
            case: "a listen stating its port and host as options",
            event: json!({
                "method": "GET", "path": "/as-options", "status": 200,
                "listen": "options",
                "host": "127.0.0.1", "port": NAMED_PORT, "probe": PROBE_HEADER,
                "answeredBy": ANSWERED_BY,
            }),
            bound: Some(NAMED_PORT),
            status: 200,
            reason: "OK",
            url: "/as-options",
            body: "",
        },
        Row {
            case: "a sender addressing the loopback host by name",
            event: json!({
                "method": "GET", "path": "/by-name", "status": 200,
                "host": "localhost", "port": 0, "probe": PROBE_HEADER,
                "answeredBy": ANSWERED_BY,
            }),
            bound: None,
            status: 200,
            reason: "OK",
            url: "/by-name",
            body: "",
        },
        Row {
            case: "the port released by close",
            event: json!({
                "method": "GET", "path": "/released", "status": 200,
                "then": "close",
                "host": "127.0.0.1", "port": 0, "probe": PROBE_HEADER,
                "answeredBy": ANSWERED_BY,
            }),
            bound: None,
            status: 200,
            reason: "OK",
            url: "/released",
            body: "",
        },
        Row {
            case: "a second server asking for a port that is held",
            event: json!({
                "method": "GET", "path": "/held", "status": 200,
                "then": "take-the-port",
                "host": "127.0.0.1", "port": 0, "probe": PROBE_HEADER,
                "answeredBy": ANSWERED_BY,
            }),
            bound: None,
            status: 200,
            reason: "OK",
            url: "/held",
            body: "",
        },
    ]
}

/// The range a port nobody named comes from, as IANA registers it for ephemeral use.
const FIRST_EPHEMERAL: u64 = 49152;
const LAST_EPHEMERAL: u64 = 65535;

/// A handler's own server answers the requests its own execution environment addresses at
/// the port it bound, and holds that port only while it is listening.
#[tokio::test]
async fn a_node_handler_answers_its_own_requests_with_the_runtimes_http_server() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let router = emulator();
    given_function(&router).await;

    for row in rows() {
        let case = row.case;
        let answered = call(
            &router,
            "POST",
            &format!("{FUNCTIONS_URI}/{FUNCTION}/invocations"),
            &serde_json::to_vec(&row.event).expect("the event serializes"),
        )
        .await;
        assert_eq!(
            answered.status,
            200,
            "case {case:?}: expected HTTP 200 from Invoke, got {} {}",
            answered.status,
            answered.text()
        );
        assert_eq!(
            answered.header("x-amz-function-error"),
            None,
            "case {case:?}: the handler must not fault, got {}",
            answered.text()
        );
        let report = answered.json();

        // The port the server bound: the one the call named, or one of the range a port
        // nobody named comes from.
        let bound = report
            .pointer("/bound/port")
            .and_then(Value::as_u64)
            .unwrap_or_else(|| panic!("case {case:?}: expected a bound port, got {report}"));
        match row.bound {
            Some(port) => assert_eq!(
                bound, port,
                "case {case:?}: expected the server to bind the port it named, got {report}"
            ),
            None => assert!(
                (FIRST_EPHEMERAL..=LAST_EPHEMERAL).contains(&bound),
                "case {case:?}: expected a port of the ephemeral range, got {bound}"
            ),
        }

        // What the sender read back is what the server wrote.
        let got = &report["answered"];
        assert_eq!(
            got.get("status").and_then(Value::as_u64),
            Some(row.status),
            "case {case:?}: expected the server's own status, got {got}"
        );
        assert_eq!(
            got.get("reason").and_then(Value::as_str),
            Some(row.reason),
            "case {case:?}: expected the reason phrase {expected:?}, got {got}",
            expected = row.reason
        );
        assert_eq!(
            got.get("type").and_then(Value::as_str),
            Some("application/json"),
            "case {case:?}: expected the header the server set, got {got}"
        );
        assert_eq!(
            got.get("answeredBy").and_then(Value::as_str),
            Some(ANSWERED_BY),
            "case {case:?}: expected the header the server set, got {got}"
        );
        assert_eq!(
            got.get("complete").and_then(Value::as_bool),
            Some(true),
            "case {case:?}: expected the whole answer to have been delivered, got {got}"
        );
        // The headers a server writes for every answer beside the ones the handler set: when
        // it answered, and how the body is framed — by its length, or as chunks for a body
        // the server had already begun writing.
        assert_eq!(
            got.get("dated").and_then(Value::as_bool),
            Some(true),
            "case {case:?}: expected the instant the server answered, got {got}"
        );
        let in_parts = row.event["write"] == json!("in-parts");
        assert_eq!(
            got.get("encoding").and_then(Value::as_str),
            in_parts.then_some("chunked"),
            "case {case:?}: expected a body written in parts to be framed as chunks, got {got}"
        );
        assert_eq!(
            got.get("length").and_then(Value::as_str).map(str::to_string),
            (!in_parts).then(|| {
                got.get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .len()
                    .to_string()
            }),
            "case {case:?}: expected the body's own length, got {got}"
        );

        // What the server read is what the sender stated.
        let seen: Value = serde_json::from_str(
            got.get("text")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("case {case:?}: expected a document, got {got}")),
        )
        .unwrap_or_else(|error| panic!("case {case:?}: expected JSON, got {error}: {got}"));
        let seen = &seen["seen"];
        let method = row.event["method"].as_str().unwrap_or_default();
        assert_eq!(
            seen.get("method").and_then(Value::as_str),
            Some(method),
            "case {case:?}: expected the server to read the method sent, got {seen}"
        );
        assert_eq!(
            seen.get("url").and_then(Value::as_str),
            Some(row.url),
            "case {case:?}: expected the server to read the target sent, got {seen}"
        );
        assert_eq!(
            seen.get("body").and_then(Value::as_str),
            Some(row.body),
            "case {case:?}: expected the server to read the body sent, got {seen}"
        );
        assert_eq!(
            seen.get("probe").and_then(Value::as_str),
            Some(PROBE_HEADER),
            "case {case:?}: expected the server to read the header sent, got {seen}"
        );
        assert_eq!(
            seen.get("version").and_then(Value::as_str),
            Some("1.1"),
            "case {case:?}: expected the version the request was carried under, got {seen}"
        );
        let authority = format!("{}:{bound}", row.event["host"].as_str().unwrap_or_default());
        assert_eq!(
            seen.get("host").and_then(Value::as_str),
            Some(authority.as_str()),
            "case {case:?}: expected the server to read the authority addressed, got {seen}"
        );

        // The lifecycle of the port, for the rows that drive it.
        if row.event["then"] == json!("close") {
            assert_eq!(
                report.get("listening").and_then(Value::as_bool),
                Some(false),
                "case {case:?}: a closed server listens on nothing, got {report}"
            );
            let after = &report["afterClose"];
            assert_eq!(
                after.get("code").and_then(Value::as_str),
                Some("ECONNREFUSED"),
                "case {case:?}: a released port answers nothing, got {after}"
            );
            assert_eq!(
                after.get("status"),
                None,
                "case {case:?}: a request nothing answered carries no status, got {after}"
            );
        }
        if row.event["then"] == json!("take-the-port") {
            let also = &report["alsoTaken"];
            assert_eq!(
                also.get("code").and_then(Value::as_str),
                Some("EADDRINUSE"),
                "case {case:?}: a held port is not bound a second time, got {also}"
            );
            assert_eq!(
                also.get("listening"),
                None,
                "case {case:?}: the second server listens on nothing, got {also}"
            );
        }
    }
}
