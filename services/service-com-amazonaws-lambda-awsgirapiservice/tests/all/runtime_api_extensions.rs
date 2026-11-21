// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the EXTENSIONS and TELEMETRY API this emulator serves a running
//! handler at the address `AWS_LAMBDA_RUNTIME_API` names.
//!
//! RULE: a request a running handler or extension makes to that address reaches the
//! interface's own endpoints. `POST /2020-01-01/extension/register` registers an extension
//! against the invocation's own execution environment: it is answered `200`, naming the
//! identifier it minted in `Lambda-Extension-Identifier` and the function the environment is
//! running, and every later request of that extension is read against that registration. The
//! Telemetry API's `PUT /2022-07-01/telemetry` READS the subscribe body into the members it
//! publishes — so a body that states one of them wrongly is refused
//! `Telemetry.DeserializationError`, naming the member, what it holds and what belongs there —
//! and then judges the rules those members carry beyond their types, refusing a body that
//! breaks them `Telemetry.ValidationError` with each failure in turn; a body that satisfies
//! them all is the registration's subscription, answered `OK`.
//!
//! The rows differ along what the rule generalises over: the member of the subscribe body, how
//! it fails to state a subscription (absent, of another type, a value the member does not
//! publish, outside a published range, at each edge of one), every value the published lists
//! carry, and whether the caller is an extension this environment registered.

use std::io::Write as _;
use std::sync::Arc;

use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

use crate::function_lifecycle::make_activation_due;

/// Where `CreateFunction` is bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// A SigV4 credential scope naming this service's signing name. Nothing in it is verified:
/// the harness turns request authentication off.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=emulator-test-key/20260101/us-east-1/lambda/aws4_request, \
     SignedHeaders=host, Signature=not-verified";

/// A well-formed execution role ARN, satisfying the model's `RoleArn` pattern.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// The handler the probing function is configured with.
const HANDLER: &str = "lambda_function.lambda_handler";

/// Where an extension registers.
const REGISTER_PATH: &str = "/2020-01-01/extension/register";

/// Where a registered extension subscribes to the telemetry streams.
const SUBSCRIBE_PATH: &str = "/2022-07-01/telemetry";

/// The class a subscribe body the API could not READ is refused under.
const DESERIALIZATION_ERROR: &str = "Telemetry.DeserializationError";

/// The class a subscribe body the API read but will not accept is refused under.
const VALIDATION_ERROR: &str = "Telemetry.ValidationError";

/// The handler every row runs: it makes the calls the event names against the runtime API, in
/// order, and reports what each was answered — so what is asserted is what that interface
/// answered the guest. A call marked `as_registered` carries the identifier the register call
/// before it minted, which is how an extension's later requests name themselves.
const HANDLER_SOURCE: &str = r#"
import json
import os
import urllib.error
import urllib.request


def _sent(address, call, identifier):
    url = "http://" + address + call["path"]
    headers = dict(call.get("headers") or {})
    if call.get("as_registered"):
        headers["Lambda-Extension-Identifier"] = call.get("identifier") or identifier or ""
    body = call.get("body")
    sent = urllib.request.Request(
        url,
        data=body.encode("utf-8") if body is not None else None,
        method=call.get("method", "GET"),
    )
    for name, value in headers.items():
        sent.add_header(name, value)
    try:
        with urllib.request.urlopen(sent) as answer:
            return {
                "status": answer.status,
                "body": answer.read().decode("utf-8"),
                "minted": answer.headers.get("Lambda-Extension-Identifier"),
            }
    except urllib.error.HTTPError as error:
        return {
            "status": error.code,
            "body": error.read().decode("utf-8"),
            "minted": error.headers.get("Lambda-Extension-Identifier"),
        }


def lambda_handler(event, context):
    address = os.environ["AWS_LAMBDA_RUNTIME_API"]
    identifier = None
    observed = []
    for call in event["calls"]:
        answer = _sent(address, call, identifier)
        if answer["minted"]:
            identifier = answer["minted"]
        observed.append(answer)
    return {"observed": observed, "identifier": identifier}
"#;

/// One dispatched request's answer.
struct Answer {
    status: u16,
    body: bytes::Bytes,
}

impl Answer {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or_else(|error| {
            panic!(
                "expected a JSON body, got {error}: {}",
                String::from_utf8_lossy(&self.body)
            )
        })
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }
}

/// One service mounted on its own engine, reached the way the production router reaches it.
fn router_on(engine: &Arc<engine::InMemoryEngine>) -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(engine)));
    router
}

/// Dispatch one control-plane request.
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
        body,
    }
}

/// A .zip deployment package holding the handler above.
fn package() -> Vec<u8> {
    let mut archive = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    archive
        .start_file::<_, ()>("lambda_function.py", zip::write::SimpleFileOptions::default())
        .expect("the entry must open");
    archive
        .write_all(HANDLER_SOURCE.as_bytes())
        .expect("the entry must be written");
    archive
        .finish()
        .expect("the archive must finish")
        .into_inner()
}

/// Create the probing function, bring it to `Active` — the transition is owed by the CLOCK,
/// so the due instant is brought forward rather than waited out — and report its ARN.
async fn given_function(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    name: &str,
) -> String {
    let created = call(
        router,
        "POST",
        FUNCTIONS_URI,
        &serde_json::to_vec(&json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": HANDLER,
            "Timeout": 30,
            "MemorySize": 512,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD.encode(package()),
            },
        }))
        .expect("the request body must serialize"),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "CreateFunction {name:?} must succeed, got {} {}",
        created.status,
        created.text()
    );
    make_activation_due(engine);
    created.json()["FunctionArn"]        .as_str()
        .expect("the created function must report its ARN")
        .to_string()
}

/// Invoke the probing function with `calls` and report what each call was answered.
async fn observed(router: &MultiServiceRouter, name: &str, calls: Value) -> Vec<Value> {
    let answered = call(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{name}/invocations"),
        &serde_json::to_vec(&json!({ "calls": calls })).expect("the event must serialize"),
    )
    .await;
    assert_eq!(
        answered.status,
        200,
        "the probing invocation must succeed, got {} {}",
        answered.status,
        answered.text()
    );
    let reported = answered.json();
    assert!(
        reported.get("observed").is_some(),
        "the probe itself must not fail, got {}",
        answered.text()
    );
    reported["observed"]
        .as_array()
        .expect("the probe must report a list")
        .clone()
}

/// One register call, as an extension makes it.
fn registering(name: &str, events: Value) -> Value {
    json!({
        "method": "POST",
        "path": REGISTER_PATH,
        "headers": { "Lambda-Extension-Name": name },
        "body": serde_json::to_string(&json!({ "events": events }))
            .expect("the body must serialize"),
    })
}

/// One subscribe call carrying `body`, made as the extension the register before it minted an
/// identifier for.
fn subscribing(body: &str) -> Value {
    json!({
        "method": "PUT",
        "path": SUBSCRIBE_PATH,
        "as_registered": true,
        "body": body,
    })
}

/// A subscribe body with every member stated, with `member` changed to `value` — or removed
/// where `value` is null.
fn subscribe_body(member: &str, value: Value) -> String {
    let mut document = json!({
        "schemaVersion": "2022-12-13",
        "types": ["platform", "function", "extension"],
        "destination": { "protocol": "HTTP", "URI": "http://sandbox.localdomain:8080/x" },
    });
    if value.is_null() {
        document
            .as_object_mut()
            .expect("an object")
            .remove(member);
    } else {
        document[member] = value;
    }
    serde_json::to_string(&document).expect("the body must serialize")
}

/// The error document one answer carries: its class and its detail.
fn errored(answer: &Value) -> (String, String) {
    let document: Value = serde_json::from_str(answer["body"].as_str().unwrap_or_default())
        .unwrap_or_else(|error| panic!("expected a JSON error body, got {error}: {answer}"));
    (
        document["errorType"].as_str().unwrap_or_default().to_string(),
        document["errorMessage"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
    )
}

/// RULE: the register call registers the extension against the invocation's own execution
/// environment — answering with the identifier it minted and the function that environment is
/// running — and every later request of that extension is read against that registration.
#[tokio::test]
async fn an_extension_registers_and_its_later_requests_are_read_against_it() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let arn = given_function(&router, &engine, "extensions-api-probe").await;
    let account = arn
        .split(':')
        .nth(4)
        .expect("the ARN must name an account")
        .to_string();

    let answers = observed(
        &router,
        "extensions-api-probe",
        json!([
            registering("telemetry-probe", json!(["INVOKE", "SHUTDOWN"])),
            // A telemetry-only extension registers for no lifecycle event at all, and is
            // registered all the same.
            registering("logs-probe", json!([])),
            // The subscription is held against the registration the identifier names.
            subscribing(&subscribe_body("schemaVersion", json!("2022-12-13"))),
            // An identifier this environment minted nothing for is from no extension
            // registered here — read before the body is, so the refusal does not depend on
            // what the body states.
            json!({
                "method": "PUT",
                "path": SUBSCRIBE_PATH,
                "as_registered": true,
                "identifier": "7f1d4a5e-0000-4000-8000-000000000000",
                "body": subscribe_body("schemaVersion", json!("2022-12-13")),
            }),
            json!({
                "method": "PUT",
                "path": SUBSCRIBE_PATH,
                "as_registered": true,
                "identifier": "7f1d4a5e-0000-4000-8000-000000000000",
                "body": "not a document",
            }),
            // An extension's own error report is accepted.
            json!({
                "method": "POST",
                "path": "/2020-01-01/extension/init/error",
                "as_registered": true,
                "headers": { "Lambda-Extension-Function-Error-Type": "Extension.TestError" },
                "body": serde_json::to_string(&json!({ "errorMessage": "probe" }))
                    .expect("the body must serialize"),
            }),
            // A register call that names no extension registers none.
            json!({ "method": "POST", "path": REGISTER_PATH, "body": "{\"events\":[]}" }),
        ]),
    )
    .await;

    // (case, the answer, the status it must carry)
    let statuses: &[(&str, &Value, u16)] = &[
        ("the register call", &answers[0], 200),
        ("a register call asking for no event", &answers[1], 200),
        ("the subscribe as the registered extension", &answers[2], 200),
        ("the subscribe as an unregistered extension", &answers[3], 403),
        (
            "the subscribe as an unregistered extension, with a body that states no \
             subscription",
            &answers[4],
            403,
        ),
        ("the extension's own error report", &answers[5], 202),
        ("a register call naming no extension", &answers[6], 403),
    ];
    for (case, answer, expected) in statuses {
        assert_eq!(
            answer["status"].as_u64(),
            Some(u64::from(*expected)),
            "case {case:?}: expected HTTP {expected}, got {answer}"
        );
    }
    let minted: Vec<&str> = answers[..2]
        .iter()
        .map(|answer| answer["minted"].as_str().unwrap_or_default())
        .collect();
    assert!(
        !minted[0].is_empty() && !minted[1].is_empty() && minted[0] != minted[1],
        "case \"two registrations\": expected an identifier for each, got {minted:?}"
    );
    let reported: Value = serde_json::from_str(answers[0]["body"].as_str().unwrap_or_default())
        .expect("the register answer must carry a JSON document");
    assert_eq!(
        reported,
        json!({
            "functionName": "extensions-api-probe",
            "functionVersion": "$LATEST",
            "handler": HANDLER,
            "accountId": account,
        }),
        "case \"the register call\": expected the function this environment runs, got \
         {reported}"
    );
    assert_eq!(
        answers[2]["body"].as_str(),
        Some("\"OK\""),
        "case \"the subscribe as the registered extension\": expected the word the API \
         answers, got {}",
        answers[2]
    );
}

/// RULE: the subscribe body is READ into the members the API publishes, and a body that states
/// none of them is refused under the class of the reading that failed, with the detail of what
/// it states.
#[tokio::test]
async fn a_subscribe_body_that_states_no_subscription_is_refused_as_it_reads() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    given_function(&router, &engine, "telemetry-refusal-probe").await;

    // (case, the subscribe body, the class it is refused under, what the detail must name)
    let rows: Vec<(&str, String, &str, &str)> = vec![
        (
            "a schema version the member does not publish",
            subscribe_body("schemaVersion", json!("9999-99-99")),
            DESERIALIZATION_ERROR,
            "unknown variant `9999-99-99`, expected one of `2020-08-15`, `2021-03-18`, \
             `2022-07-01`, `2022-12-13`, `2025-01-29`",
        ),
        (
            "no schema version at all",
            subscribe_body("schemaVersion", Value::Null),
            DESERIALIZATION_ERROR,
            "missing field `schemaVersion`",
        ),
        (
            "a stream the member does not publish",
            subscribe_body("types", json!(["platform", "audit"])),
            DESERIALIZATION_ERROR,
            "unknown variant `audit`, expected one of `platform`, `function`, `extension`",
        ),
        (
            "streams that are not a list",
            subscribe_body("types", json!("platform")),
            DESERIALIZATION_ERROR,
            "invalid type: string \"platform\"",
        ),
        (
            "no destination at all",
            subscribe_body("destination", Value::Null),
            DESERIALIZATION_ERROR,
            "missing field `destination`",
        ),
        (
            "a destination protocol the member does not publish",
            subscribe_body(
                "destination",
                json!({ "protocol": "TCP", "URI": "http://sandbox.localdomain:8080/x" }),
            ),
            DESERIALIZATION_ERROR,
            "unknown variant `TCP`, expected `HTTP`",
        ),
        (
            "a destination naming no URI",
            subscribe_body("destination", json!({ "protocol": "HTTP" })),
            DESERIALIZATION_ERROR,
            "missing field `URI`",
        ),
        (
            "a body that is no document at all",
            "not a document".to_string(),
            DESERIALIZATION_ERROR,
            "expected ident",
        ),
        (
            "no stream asked for",
            subscribe_body("types", json!([])),
            VALIDATION_ERROR,
            "types should not be empty",
        ),
        (
            "a destination naming no port",
            subscribe_body(
                "destination",
                json!({ "protocol": "HTTP", "URI": "http://sandbox.localdomain/x" }),
            ),
            VALIDATION_ERROR,
            "URI port is not provided",
        ),
        (
            "every rule of the body broken at once",
            {
                let mut document: Value = serde_json::from_str(&subscribe_body(
                    "types",
                    json!([]),
                ))
                .expect("a document");
                document["destination"] =
                    json!({ "protocol": "HTTP", "URI": "http://sandbox.localdomain" });
                document["buffering"] = json!({ "maxItems": 1 });
                serde_json::to_string(&document).expect("the body must serialize")
            },
            VALIDATION_ERROR,
            "types should not be empty; URI port is not provided; maxItems should be between \
             1000 and 10000",
        ),
        // Each bound of `buffering`, one past each of its published edges.
        (
            "the most events one delivery may carry, one past its low edge",
            subscribe_body("buffering", json!({ "maxItems": 999 })),
            VALIDATION_ERROR,
            "maxItems should be between 1000 and 10000",
        ),
        (
            "the most events one delivery may carry, one past its high edge",
            subscribe_body("buffering", json!({ "maxItems": 10_001 })),
            VALIDATION_ERROR,
            "maxItems should be between 1000 and 10000",
        ),
        (
            "the most bytes one delivery may carry, one past its low edge",
            subscribe_body("buffering", json!({ "maxBytes": 262_143 })),
            VALIDATION_ERROR,
            "maxBytes should be between 262144 and 1048576",
        ),
        (
            "the most bytes one delivery may carry, one past its high edge",
            subscribe_body("buffering", json!({ "maxBytes": 1_048_577 })),
            VALIDATION_ERROR,
            "maxBytes should be between 262144 and 1048576",
        ),
        (
            "how long events are held, one past its low edge",
            subscribe_body("buffering", json!({ "timeoutMs": 24 })),
            VALIDATION_ERROR,
            "timeoutMs should be between 25 and 30000",
        ),
        (
            "how long events are held, one past its high edge",
            subscribe_body("buffering", json!({ "timeoutMs": 30_001 })),
            VALIDATION_ERROR,
            "timeoutMs should be between 25 and 30000",
        ),
    ];

    let mut calls = vec![registering("telemetry-probe", json!([]))];
    calls.extend(rows.iter().map(|(_case, body, _class, _detail)| subscribing(body)));
    let answers = observed(&router, "telemetry-refusal-probe", json!(calls)).await;

    for (at, (case, _body, class, detail)) in rows.iter().enumerate() {
        let answer = &answers[at + 1];
        assert_eq!(
            answer["status"].as_u64(),
            Some(400),
            "case {case:?}: expected HTTP 400, got {answer}"
        );
        let (refused, message) = errored(answer);
        assert_eq!(
            refused, *class,
            "case {case:?}: expected {class:?}, got {refused:?}"
        );
        assert!(
            message.contains(detail),
            "case {case:?}: expected a detail naming {detail:?}, got {message:?}"
        );
    }
}

/// RULE: a body that satisfies every rule of its members IS the subscription, whichever
/// published value each member takes.
#[tokio::test]
async fn a_subscribe_body_the_api_accepts_is_the_registrations_subscription() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    given_function(&router, &engine, "telemetry-subscribe-probe").await;

    let mut cases: Vec<(String, String)> = Vec::new();
    // Every schema version the member publishes, which is the list its own refusal names.
    for version in [
        "2020-08-15",
        "2021-03-18",
        "2022-07-01",
        "2022-12-13",
        "2025-01-29",
    ] {
        cases.push((
            format!("the schema version {version}"),
            subscribe_body("schemaVersion", json!(version)),
        ));
    }
    // Every stream it publishes, each on its own.
    for stream in ["platform", "function", "extension"] {
        cases.push((
            format!("the stream {stream}"),
            subscribe_body("types", json!([stream])),
        ));
    }
    // Each bound of `buffering`, at both of its published edges.
    for (edge, items, bytes, timeout) in [
        ("the low edge", 1_000, 262_144, 25),
        ("the high edge", 10_000, 1_048_576, 30_000),
    ] {
        cases.push((
            format!("buffering at {edge} of every bound"),
            subscribe_body(
                "buffering",
                json!({ "maxItems": items, "maxBytes": bytes, "timeoutMs": timeout }),
            ),
        ));
    }
    // A destination written without a scheme still names the port it is reached on.
    cases.push((
        "a destination written as an address".to_string(),
        subscribe_body(
            "destination",
            json!({ "protocol": "HTTP", "URI": "sandbox.localdomain:8080/x" }),
        ),
    ));

    let mut calls = vec![registering("telemetry-probe", json!([]))];
    calls.extend(cases.iter().map(|(_case, body)| subscribing(body)));
    let answers = observed(&router, "telemetry-subscribe-probe", json!(calls)).await;

    for (at, (case, _body)) in cases.iter().enumerate() {
        let answer = &answers[at + 1];
        assert_eq!(
            answer["status"].as_u64(),
            Some(200),
            "case {case:?}: expected HTTP 200, got {answer}"
        );
        assert_eq!(
            answer["body"].as_str(),
            Some("\"OK\""),
            "case {case:?}: expected the word the API answers, got {answer}"
        );
    }
}
