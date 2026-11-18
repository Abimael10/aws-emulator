// SPDX-License-Identifier: CC0-1.0

//! What a match type handed an argument of the wrong TYPE is refused with, end
//! to end over the wire (`awsJson1_1`, `POST /` with `X-Amz-Target:
//! AWSEvents.<Operation>`) through the real `MultiServiceRouter`.
//!
//! The RULE asserted here: when the event-pattern reader refuses a match
//! expression because the value given to a match type is not of the type that
//! match type takes, the reason is that match type's OWN sentence — the match
//! type's name, then what its pattern must be — and the reason names neither
//! the member the expression sits under nor the field path leading to it, and
//! enumerates no alternative form; the location clause that follows the reason
//! (`\n at [Source: (String)"<the pattern as submitted>"; line: <n>, column:
//! <c>]`) is what points at the argument. The two affix match types negated by
//! `anything-but` are read there by one reader, which names the PAIR of forms
//! it accepts (`prefix/suffix`). The refusal itself is
//! `InvalidEventPatternException` (HTTP 400, `TestEventPattern` /
//! `PutRule` — `smithy_query(query="operation", operation="TestEventPattern")`
//! models it as a client error), and one reader words it for every operation
//! handed a pattern.
//!
//! The model says nothing about the match types or about which patterns match
//! (`TestEventPattern` inputs are `EventPattern @required @length(0,4096)` and
//! `Event @required`, its output a single `Result` boolean), so the match types
//! exercised below are the ones the pattern language defines — `prefix`,
//! `suffix`, `equals-ignore-case`, `wildcard`, `cidr`, `anything-but` and
//! `numeric` — and the sentence shape is the one the service answers for a
//! `prefix` matcher given a number. `exists` states its own sentence in the
//! same shape, which `pattern_refusal_wording` asserts.
//!
//! The table spans the class rather than one case: the rows differ along which
//! match type is at fault, which wrong JSON type its argument has (number,
//! boolean, null, string where a list belongs, list, object), how deep the
//! expression sits (a top-level field, one object in, two objects in), whether
//! the match type is nested under another one (`anything-but`, and the
//! case-folding form of `prefix`/`suffix`), and how the pattern is spelled
//! (one line, or several).

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_eventbridge_awsevents::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region every request below is signed for.
const REGION: &str = "us-west-2";

/// The emulator: one router carrying only this service over one shared engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(engine));
    router
}

/// One `awsJson1_1` call to this service.
async fn events(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let credential = format!(
        "AKIDTESTTESTTEST/20260101/{REGION}/events/aws4_request, SignedHeaders=host, \
         Signature=0000"
    );
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("events.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("AWSEvents.{operation}"))
        .header(
            "authorization",
            format!("AWS4-HMAC-SHA256 Credential={credential}"),
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

/// The wire error code (`__type`), without its namespace.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .map(|name| name.rsplit('#').next().unwrap_or(name).to_string())
        .unwrap_or_default()
}

/// The `message` an error body carries.
fn error_message(body: &Value) -> String {
    body.get("message")
        .or_else(|| body.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// A valid event envelope: every field `TestEventPattern$Event` makes mandatory
/// ("the following fields are mandatory: id account source time region resources
/// detail-type"), carrying a `detail` the rows' field paths reach into. The
/// pattern is read before the event is, so every row reaches the reader
/// whatever this envelope holds.
fn envelope() -> String {
    json!({
        "version": "0",
        "id": "9c2f6f6a-6f0e-4a3e-9f1b-7a1d2c3b4e5f",
        "account": engine::emulator_account_id(),
        "source": "com.demo.orders",
        "time": "2026-03-04T10:00:00Z",
        "region": REGION,
        "resources": [],
        "detail-type": "Order Placed",
        "detail": { "group": "retail", "inner": { "leaf": "value" } },
    })
    .to_string()
}

/// One row: the case, the pattern as submitted, the match type the reason must
/// name, what that match type's pattern must be, and the PREFIX of the
/// submitted pattern the reader had consumed when it refused — its argument's
/// opening token, which is as much of it as deciding the argument's type takes.
struct Row {
    case: &'static str,
    pattern: &'static str,
    match_type: &'static str,
    must_be: &'static str,
    read: &'static str,
}

/// What a match type that compares TEXT requires of its argument.
const A_STRING: &str = "a string";

/// Every row the rule is asserted on.
fn rows() -> Vec<Row> {
    vec![
        // `prefix` given each JSON type that is not a string, at the depth a
        // detail member sits at.
        Row {
            case: "prefix given a number",
            pattern: r#"{"detail": {"group": [{"prefix": 5}]}}"#,
            match_type: "prefix",
            must_be: A_STRING,
            read: r#"{"detail": {"group": [{"prefix": 5"#,
        },
        Row {
            case: "prefix given a boolean",
            pattern: r#"{"detail": {"group": [{"prefix": true}]}}"#,
            match_type: "prefix",
            must_be: A_STRING,
            read: r#"{"detail": {"group": [{"prefix": true"#,
        },
        Row {
            case: "prefix given null",
            pattern: r#"{"detail": {"group": [{"prefix": null}]}}"#,
            match_type: "prefix",
            must_be: A_STRING,
            read: r#"{"detail": {"group": [{"prefix": null"#,
        },
        Row {
            case: "prefix given a list of strings",
            pattern: r#"{"detail": {"group": [{"prefix": ["re"]}]}}"#,
            match_type: "prefix",
            must_be: A_STRING,
            read: r#"{"detail": {"group": [{"prefix": ["#,
        },
        Row {
            case: "prefix given an object naming no form it takes",
            pattern: r#"{"detail": {"group": [{"prefix": {"wildcard": "re*"}}]}}"#,
            match_type: "prefix",
            must_be: A_STRING,
            read: r#"{"detail": {"group": [{"prefix": {"#,
        },
        // The one nested form `prefix` and `suffix` take is the case-folding
        // match type, whose own argument is a string too — so the reason names
        // THAT match type when its argument is not one.
        Row {
            case: "prefix's nested case-folding form given a number",
            pattern: r#"{"detail": {"group": [{"prefix": {"equals-ignore-case": 5}}]}}"#,
            match_type: "equals-ignore-case",
            must_be: A_STRING,
            read: r#"{"detail": {"group": [{"prefix": {"equals-ignore-case": 5"#,
        },
        // A top-level field of the envelope, so a reason that named the field
        // path would still name something.
        Row {
            case: "prefix given a number on a top-level field",
            pattern: r#"{"source": [{"prefix": 5}]}"#,
            match_type: "prefix",
            must_be: A_STRING,
            read: r#"{"source": [{"prefix": 5"#,
        },
        // Two objects in.
        Row {
            case: "suffix given a number two objects in",
            pattern: r#"{"detail": {"inner": {"leaf": [{"suffix": 7}]}}}"#,
            match_type: "suffix",
            must_be: A_STRING,
            read: r#"{"detail": {"inner": {"leaf": [{"suffix": 7"#,
        },
        Row {
            case: "suffix given a list",
            pattern: r#"{"detail": {"group": [{"suffix": ["ail"]}]}}"#,
            match_type: "suffix",
            must_be: A_STRING,
            read: r#"{"detail": {"group": [{"suffix": ["#,
        },
        Row {
            case: "equals-ignore-case given a number",
            pattern: r#"{"detail": {"group": [{"equals-ignore-case": 5}]}}"#,
            match_type: "equals-ignore-case",
            must_be: A_STRING,
            read: r#"{"detail": {"group": [{"equals-ignore-case": 5"#,
        },
        Row {
            case: "equals-ignore-case given a list",
            pattern: r#"{"source": [{"equals-ignore-case": ["COM.DEMO.ORDERS"]}]}"#,
            match_type: "equals-ignore-case",
            must_be: A_STRING,
            read: r#"{"source": [{"equals-ignore-case": ["#,
        },
        Row {
            case: "wildcard given a number",
            pattern: r#"{"detail": {"group": [{"wildcard": 5}]}}"#,
            match_type: "wildcard",
            must_be: A_STRING,
            read: r#"{"detail": {"group": [{"wildcard": 5"#,
        },
        Row {
            case: "wildcard given a boolean",
            pattern: r#"{"detail": {"group": [{"wildcard": false}]}}"#,
            match_type: "wildcard",
            must_be: A_STRING,
            read: r#"{"detail": {"group": [{"wildcard": false"#,
        },
        // The same rule under `anything-but`, which negates a nested string
        // match type. The two affix forms are read there by one reader, so its
        // reason names the PAIR it accepts; every other negated match type
        // states its own sentence.
        Row {
            case: "a wildcard nested under anything-but given a number",
            pattern: r#"{"detail": {"group": [{"anything-but": {"wildcard": 5}}]}}"#,
            match_type: "wildcard",
            must_be: A_STRING,
            read: r#"{"detail": {"group": [{"anything-but": {"wildcard": 5"#,
        },
        Row {
            case: "a prefix nested under anything-but given a number",
            pattern: r#"{"detail": {"group": [{"anything-but": {"prefix": 5}}]}}"#,
            match_type: "prefix/suffix",
            must_be: A_STRING,
            read: r#"{"detail": {"group": [{"anything-but": {"prefix": 5"#,
        },
        Row {
            case: "a suffix nested under anything-but given a boolean",
            pattern: r#"{"detail": {"group": [{"anything-but": {"suffix": true}}]}}"#,
            match_type: "prefix/suffix",
            must_be: A_STRING,
            read: r#"{"detail": {"group": [{"anything-but": {"suffix": true"#,
        },
        // A non-string MEMBER of a list such a match type was handed is
        // refused with the same sentence, but its location is stated by two
        // readers rather than one, so the message carries the location clause
        // twice: that form is asserted where the rule about it is
        // (`pattern_numeric_and_wildcard_operands`).
        // Match types whose argument is not text: the sentence is the same
        // shape, stating what each one's pattern must be.
        Row {
            // A value that is not TEXT is refused before any block is read out
            // of it: the plain string-operand requirement, which the service
            // states for the first match type that compares text.
            case: "cidr given a number",
            pattern: r#"{"detail": {"group": [{"cidr": 10}]}}"#,
            match_type: "prefix",
            must_be: A_STRING,
            read: r#"{"detail": {"group": [{"cidr": 10"#,
        },
        Row {
            case: "numeric given a bare number instead of operator/value pairs",
            pattern: r#"{"detail": {"group": [{"numeric": 5}]}}"#,
            match_type: "numeric",
            must_be: "an array of operator/value pairs",
            read: r#"{"detail": {"group": [{"numeric": 5"#,
        },
        Row {
            case: "anything-but given a boolean",
            pattern: r#"{"detail": {"group": [{"anything-but": true}]}}"#,
            match_type: "anything-but",
            must_be: "a value, a list of values, or one nested string operator",
            read: r#"{"detail": {"group": [{"anything-but": true"#,
        },
        Row {
            case: "anything-but given a list carrying a boolean",
            pattern: r#"{"detail": {"group": [{"anything-but": ["retail", false]}]}}"#,
            match_type: "anything-but",
            must_be: "a list of strings or a list of numbers",
            read: r#"{"detail": {"group": [{"anything-but": ["#,
        },
        // The position is a fact about the TEXT, so a pattern spelled over
        // several lines reports the line the argument falls on, and the reason
        // is the same sentence.
        Row {
            case: "prefix given a number in a pattern spelled over several lines",
            pattern: "{\n  \"detail\": {\n    \"group\": [{\"prefix\": 5}]\n  }\n}",
            match_type: "prefix",
            must_be: A_STRING,
            read: "{\n  \"detail\": {\n    \"group\": [{\"prefix\": 5",
        },
    ]
}

/// The whole message a refused pattern must answer with: the opening, the
/// match type's own sentence, and the source location of the position just past
/// `read` — the text the reader had to consume to find the fault. The line and
/// column are COMPUTED from that text rather than written down, and the column
/// counts characters from the start of the line the position falls on.
fn expected_message(row: &Row) -> String {
    let line = 1 + row.read.matches('\n').count();
    let last = row
        .read
        .rsplit_once('\n')
        .map_or(row.read, |(_, after)| after);
    let column = 1 + last.chars().count();
    format!(
        "Event pattern is not valid. Reason: {} match pattern must be {}\n at [Source: \
         (String)\"{}\"; line: {line}, column: {column}]",
        row.match_type, row.must_be, row.pattern
    )
}

/// The reason part of a message: what stands between the opening and the
/// location clause.
fn reason_of(message: &str) -> String {
    message
        .strip_prefix("Event pattern is not valid. Reason: ")
        .unwrap_or(message)
        .split("\n at [Source:")
        .next()
        .unwrap_or_default()
        .to_string()
}

#[tokio::test]
async fn an_argument_of_the_wrong_type_is_refused_with_its_match_types_own_sentence() {
    let router = emulator();
    let event = envelope();

    for row in rows() {
        // The row describes its own pattern, so a row that does not fails
        // rather than passing vacuously.
        assert!(
            row.pattern.starts_with(row.read),
            "case {:?}: the row's prefix {:?} must be a prefix of {:?}",
            row.case,
            row.read,
            row.pattern
        );
        let expected = expected_message(&row);
        // One reader words the refusal for every operation handed a pattern,
        // so the pre-flight check and the write answer the same message.
        for operation in ["TestEventPattern", "PutRule"] {
            let request = if operation == "TestEventPattern" {
                json!({ "EventPattern": row.pattern, "Event": event })
            } else {
                json!({ "Name": "refused-pattern", "EventPattern": row.pattern })
            };
            let (status, body) = events(&router, operation, &request).await;
            assert_eq!(
                status, 400,
                "case {:?} on {operation}: expected HTTP 400, got {status} {body}",
                row.case
            );
            assert_eq!(
                error_code(&body),
                "InvalidEventPatternException",
                "case {:?} on {operation}: expected InvalidEventPatternException, got {body}",
                row.case
            );
            let message = error_message(&body);
            assert_eq!(
                message, expected,
                "case {:?} on {operation}: expected {expected:?}, got {message:?}",
                row.case
            );
            // The reason names the match type and what its pattern must be,
            // and nothing else: no member of the pattern, and no field path.
            let reason = reason_of(&message);
            for named in [
                "detail",
                "group",
                "inner",
                "leaf",
                "source",
                "detail.group",
                "detail.inner.leaf",
            ] {
                assert!(
                    !reason.contains(named),
                    "case {:?} on {operation}: the reason {reason:?} must not name the member \
                     {named:?}",
                    row.case
                );
            }
        }
    }
}

/// A pattern the language DOES admit is still accepted, so the refusals above
/// reach no well-formed matcher: each match type above is exercised in the form
/// it takes, and `TestEventPattern` answers the verdict rather than a refusal.
#[tokio::test]
async fn a_well_formed_argument_is_accepted_for_every_match_type_above() {
    let router = emulator();
    let event = envelope();

    // (case, the pattern, whether it matches the envelope above)
    let accepted: [(&str, Value, bool); 11] = [
        (
            "prefix given a string",
            json!({ "detail": { "group": [{ "prefix": "re" }] } }),
            true,
        ),
        (
            "prefix given its nested case-folding form",
            json!({ "detail": { "group": [{ "prefix": { "equals-ignore-case": "RE" } }] } }),
            true,
        ),
        (
            "suffix given a string",
            json!({ "detail": { "inner": { "leaf": [{ "suffix": "ue" }] } } }),
            true,
        ),
        (
            "equals-ignore-case given a string",
            json!({ "source": [{ "equals-ignore-case": "COM.DEMO.ORDERS" }] }),
            true,
        ),
        (
            "wildcard given a string",
            json!({ "detail": { "group": [{ "wildcard": "re*il" }] } }),
            true,
        ),
        (
            "anything-but given a list of strings",
            json!({ "detail": { "group": [{ "anything-but": ["wholesale"] }] } }),
            true,
        ),
        (
            "a wildcard nested under anything-but",
            json!({ "detail": { "group": [{ "anything-but": { "wildcard": "whole*" } }] } }),
            true,
        ),
        // The case-folding form the affix match types take nested under
        // themselves is admitted under `anything-but` too, and evaluated there:
        // the negation holds only when the folded prefix does not.
        (
            "a prefix's case-folding form nested under anything-but, folding to the leaf",
            json!({
                "detail": {
                    "group": [{ "anything-but": { "prefix": { "equals-ignore-case": "RE" } } }]
                }
            }),
            false,
        ),
        (
            "a prefix's case-folding form nested under anything-but, folding to something else",
            json!({
                "detail": {
                    "group": [{ "anything-but": { "prefix": { "equals-ignore-case": "WHOLE" } } }]
                }
            }),
            true,
        ),
        (
            "numeric given operator/value pairs",
            json!({ "detail": { "group": [{ "numeric": [">", 0] }] } }),
            false,
        ),
        (
            "cidr given a block",
            json!({ "detail": { "group": [{ "cidr": "10.0.0.0/24" }] } }),
            false,
        ),
    ];

    for (case, pattern, matches) in accepted {
        let pattern = pattern.to_string();
        let (status, body) = events(
            &router,
            "TestEventPattern",
            &json!({ "EventPattern": pattern, "Event": event }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the pattern to be admitted (HTTP 200), got {status} {body}"
        );
        let result = body.get("Result").and_then(Value::as_bool);
        assert_eq!(
            result,
            Some(matches),
            "case {case:?}: expected Result {matches}, got {body}"
        );
    }
}
