// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHICH REQUEST the envelope of a CloudWatch Logs refusal names.
//!
//! RULE: a refusal message this service DECIDES is the sentence for its class followed by the
//! SDK error envelope naming the request THIS RESPONSE answers — ` (Service: AWSLogs; Status
//! Code: <the status answered>; Error Code: <the code answered>; Request ID: <this response's
//! own id>; Proxy: null)`. A refusal it RELAYS is not one of those: the text of a query that
//! does not compile is the query compiler's own, and it is the WHOLE message — the clause an
//! AWS SDK shows after an error is assembled CLIENT-SIDE out of the status line, the error
//! code and the request-id header, so none of it is serialized in that message. Either way the
//! response reports its own request in the header this protocol names one in
//! (`x-amzn-RequestId`), which is where a client reads it.
//!
//! The table spans what the rule generalises over: whether the refusal is relayed from the
//! compiler or decided by the service, which fault the compiler reported (a symbol it stopped
//! on, the end of the query string, and the snippet syntax a query that has to select its own
//! log groups is read as), and which class a decided refusal is answered under — including a
//! `MalformedQueryException` the SERVICE decides over the time range rather than over the query
//! text, so the distinction under test is the SOURCE of the message and not its error code.
//!
//! Every case drives the service the way a client does — `POST /` with
//! `X-Amz-Target: Logs_20140328.StartQuery` through the router that mounts it — so a message no
//! handler actually wrote cannot pass.

use serde_json::{json, Value};

use crate::harness::{now_ms, Logs};

/// The operation under test.
const START: &str = "StartQuery"; 

/// The status every refusal below is answered with.
const REFUSAL_STATUS: u16 = 400;

/// The error classes the rows are refused under, each declared by the operation that answers it
/// (`smithy_query(query="operation",operation="StartQuery")`).
const MALFORMED_QUERY: &str = "MalformedQueryException";
/// See [`MALFORMED_QUERY`].
const INVALID_PARAMETER: &str = "InvalidParameterException";
/// See [`MALFORMED_QUERY`].
const NOT_FOUND: &str = "ResourceNotFoundException";

/// What the envelope of a message the service DECIDES states, and the framing a RELAYED message
/// carries none of.
const ENVELOPE_FRAMING: [&str; 5] = [
    "(Service:",
    "Status Code:",
    "Error Code:",
    "Request ID:",
    "Proxy:",
];

/// The header this protocol names the request a response answers in.
const REQUEST_ID_HEADER: &str = "x-amzn-requestid";

/// Which form a refusal's message has to take.
#[derive(Clone, Copy, Debug)]
enum Names {
    /// The sentence for its class, then the envelope naming the request this very response
    /// answers, as the service words a refusal it decides.
    ThisResponse,
    /// The compiler's own text and nothing else, as this service relays it.
    NoEnvelope,
}

/// A window in the unit `StartQuery` documents its range in ("epoch time, the number of seconds
/// since January 1, 1970"), an hour wide and ending now.
fn window() -> (i64, i64) {
    let now_secs = now_ms() / 1_000 + 1;
    (now_secs - 60 * 60, now_secs)
}

/// The envelope the rule requires: the status and the code the response answers with, and the
/// request it names.
fn expected_clause(code: &str, request_id: &str) -> String {
    format!(
        " (Service: AWSLogs; Status Code: {REFUSAL_STATUS}; Error Code: {code}; Request ID: \
         {request_id}; Proxy: null)"
    )
}

/// RULE: a relayed compile failure carries the compiler's text alone, a decided refusal carries
/// its sentence plus the envelope naming this response's own request.
#[tokio::test]
async fn an_envelope_stands_only_where_the_service_words_the_refusal() {
    let logs = Logs::new();
    const GROUP: &str = "/relayed/one";
    logs.ok("CreateLogGroup", &json!({"logGroupName": GROUP}))
        .await;
    let (start, end) = window();

    // (case, the request, the class it is refused under, which request its envelope names)
    let cases: Vec<(&str, Value, &str, Names)> = vec![
        (
            "a query whose second stage is no command of the language",
            json!({
                "logGroupName": GROUP,
                "startTime": start,
                "endTime": end,
                "queryString": "fields @message | abc",
            }),
            MALFORMED_QUERY,
            Names::NoEnvelope,
        ),
        (
            "a query that ends where an operand was owed",
            json!({
                "logGroupName": GROUP,
                "startTime": start,
                "endTime": end,
                "queryString": "fields @message | sort",
            }),
            MALFORMED_QUERY,
            Names::NoEnvelope,
        ),
        (
            // No member names the log groups, so the query string is read as the snippet that
            // has to select them, and the compiler stops on its opening symbol.
            "a query that selects its log groups nowhere",
            json!({
                "startTime": start,
                "endTime": end,
                "queryString": "fields @message",
            }),
            MALFORMED_QUERY,
            Names::NoEnvelope,
        ),
        (
            // The same error CLASS, decided by the service over the time range rather than
            // relayed from the compiler: its envelope names this response's request.
            "a query window that closes before the log group existed",
            json!({
                "logGroupName": GROUP,
                "startTime": start - 10 * 24 * 60 * 60,
                "endTime": start - 9 * 24 * 60 * 60,
                "queryString": "fields @message",
            }),
            MALFORMED_QUERY,
            Names::ThisResponse,
        ),
        (
            "a query window that runs backwards",
            json!({
                "logGroupName": GROUP,
                "startTime": end,
                "endTime": start,
                "queryString": "fields @message",
            }),
            INVALID_PARAMETER,
            Names::ThisResponse,
        ),
        (
            "a query over a log group this account does not hold",
            json!({
                "logGroupName": "/relayed/absent",
                "startTime": start,
                "endTime": end,
                "queryString": "fields @message",
            }),
            NOT_FOUND,
            Names::ThisResponse,
        ),
    ];

    for (case, request, code, names) in cases {
        let (status, headers, payload) = logs.call_raw(START, &request).await;
        let text = String::from_utf8(payload.to_vec())
            .unwrap_or_else(|_| panic!("case {case}: the error body is text, got {payload:?}"));
        let body: Value = serde_json::from_str(&text)
            .unwrap_or_else(|_| panic!("case {case}: the error body is JSON, got {text:?}"));
        assert_eq!(
            status, REFUSAL_STATUS,
            "case {case}: expected the refusal status, got {status} with {text}"
        );
        assert_eq!(
            crate::harness::error_code(&body),
            code,
            "case {case}: expected {code}, got {text}"
        );
        // Every response names the request it answers, whichever refusal it carries.
        let reported = headers
            .get(REQUEST_ID_HEADER)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_else(|| panic!("case {case}: the response names the request it answers"))
            .to_string();
        let said = body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case}: a refusal carries a message, got {text}"))
            .to_string();
        match names {
            Names::ThisResponse => {
                let clause = expected_clause(code, &reported);
                assert!(
                    said.ends_with(&clause),
                    "case {case}: expected the envelope {clause:?}, got {said:?}"
                );
                // The sentence the envelope follows is not empty: a refusal says what is wrong
                // before it states the facts of the response, and states that envelope ONCE.
                let sentence = said.strip_suffix(&clause).unwrap_or_default();
                assert!(
                    !sentence.trim().is_empty() && !sentence.contains("(Service:"),
                    "case {case}: expected one sentence and one envelope, got {said:?}"
                );
            }
            Names::NoEnvelope => {
                // The compiler's text and nothing else: no SDK exception framing, and so no
                // second request id beside the one the header reports.
                assert!(
                    !said.trim().is_empty(),
                    "case {case}: a refusal states what is wrong, got {said:?}"
                );
                for framing in ENVELOPE_FRAMING {
                    assert!(
                        !said.contains(framing),
                        "case {case}: expected no {framing:?} framing in a relayed message, got \
                         {said:?}"
                    );
                }
                assert!(
                    !said.contains(&reported),
                    "case {case}: a relayed message names no request, got {said:?} beside the \
                     reported {reported:?}"
                );
            }
        }
    }
}

/// RULE (the other dimension): two identical calls refused by the COMPILER carry the same
/// message, while the responses still name two different requests — so a client that keyed a
/// cache or a log line on a relayed message sees one text, and the per-call id stays in the
/// header where this protocol puts it.
#[tokio::test]
async fn a_relayed_message_is_the_same_text_for_two_calls_that_name_two_requests() {
    let logs = Logs::new();
    const GROUP: &str = "/relayed/per-call";
    logs.ok("CreateLogGroup", &json!({"logGroupName": GROUP}))
        .await;
    let (start, end) = window();
    let request = json!({
        "logGroupName": GROUP,
        "startTime": start,
        "endTime": end,
        "queryString": "fields @message | frobnicate x",
    });

    let mut messages: Vec<String> = Vec::new();
    let mut reported: Vec<String> = Vec::new();
    for attempt in 0..3 {
        let case = format!("call {attempt}");
        let (status, headers, payload) = logs.call_raw(START, &request).await;
        let body: Value = serde_json::from_slice(&payload)
            .unwrap_or_else(|_| panic!("case {case}: the error body is JSON"));
        assert_eq!(
            status, REFUSAL_STATUS,
            "case {case}: expected the refusal status, got {status} with {body}"
        );
        messages.push(
            body.get("message")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("case {case}: a refusal carries a message, got {body}"))
                .to_string(),
        );
        reported.push(
            headers
                .get(REQUEST_ID_HEADER)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_else(|| panic!("case {case}: the response names its request"))
                .to_string(),
        );
    }
    let first = messages
        .first()
        .expect("the loop ran at least once")
        .clone();
    assert!(
        messages.iter().all(|said| *said == first),
        "expected one text for the same relayed refusal, got {messages:?}"
    );
    let mut distinct = reported.clone();
    distinct.sort();
    distinct.dedup();
    assert_eq!(
        distinct.len(),
        reported.len(),
        "expected one request id per response, got {reported:?}"
    );
}
