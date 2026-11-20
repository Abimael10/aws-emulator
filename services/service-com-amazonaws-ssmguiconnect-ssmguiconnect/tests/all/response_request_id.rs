// SPDX-License-Identifier: CC0-1.0

//! Every response this service sends names the request it answered.
//!
//! The rule under test: a response of this service carries the header its protocol reports
//! the service request id in (`x-amzn-RequestId`, matched case-insensitively — the header
//! the model's own CORS configuration exposes, `smithy_query(query="service_info")`),
//! freshly minted per call, on EVERY answer — each operation's success, each modelled
//! error, and the framework-level failures no operation produces — and a FAILURE states
//! that same id a second time, in the top-level `requestId` member of its JSON error body
//! beside the `message`. It is a property of the RESPONSE rather than of any modelled
//! shape, so no output or error shape of this model declares it, no handler can set it, and
//! it is what an SDK surfaces as `ResponseMetadata.RequestId` /
//! `ServiceError::request_id()`.
//!
//! What the rule does NOT change, and this suite pins too: the status, the error code and
//! the `message` of every one of those answers stay exactly what they were, and the body of
//! a SUCCESS is still exactly the modelled output — the id joins an error body only.
//!
//! The rows span what the rule generalises over: which operation answered (all three the
//! model binds), whether the answer is a success or a failure, the failure's status and
//! code (404 `ResourceNotFoundException`, 400 `ValidationException`, 403
//! `AccessDeniedException` from the account gate and from the IAM gate alike), whether a
//! body was carried at all, and which LAYER answered — a handler, the modelled constraint
//! walk ahead of it, and the router's own routing failure for a request that names no
//! modelled operation.

use serde_json::{json, Value};

use crate::identity_authorization::{
    error_code, preferences, request, update_body, Emulator, ACCOUNT_KEY, DELETE, GET, KEY_ARN,
    REGION, REQUEST_ID_HEADER, UPDATE,
};

/// Whether `id` has the 8-4-4-4-12 lowercase-hex form an AWS request id carries. Written
/// out rather than read from the crate, so the assertion does not compare the
/// implementation with itself.
fn names_a_request_id(id: &str) -> bool {
    let mut groups = id.split('-');
    for width in [8usize, 4, 4, 4, 12] {
        let Some(group) = groups.next() else {
            return false;
        };
        if group.len() != width
            || !group
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return false;
        }
    }
    groups.next().is_none()
}

/// RULE: every answer of every operation names the request it answered, whatever its
/// status, code and layer — in the header, and in the error body of a failure — and two
/// answers name two requests.
#[tokio::test]
async fn every_answer_names_the_request_it_answered() {
    let long_token = "t".repeat(65);

    // (case, operation, body, expected status, expected error code)
    let rows: [(&str, &str, Value, u16, &str); 7] = [
        (
            "the delete's success, with nothing configured",
            DELETE,
            json!({ "ClientToken": "id-delete" }),
            200,
            "",
        ),
        (
            "the read's 404, with nothing configured",
            GET,
            json!({}),
            404,
            "ResourceNotFoundException",
        ),
        (
            "the write's 403 from the account gate",
            UPDATE,
            update_body("id-update"),
            403,
            "AccessDeniedException",
        ),
        (
            "a constraint refusal the walk raised, on the write",
            UPDATE,
            json!({
                "ConnectionRecordingPreferences":
                    preferences("123456789012", "rdp-recordings", KEY_ARN),
                "ClientToken": long_token.clone(),
            }),
            400,
            "ValidationException",
        ),
        (
            "a constraint refusal the walk raised, on the delete",
            DELETE,
            json!({ "ClientToken": long_token }),
            400,
            "ValidationException",
        ),
        (
            "a required member absent from the write",
            UPDATE,
            json!({ "ClientToken": "id-absent" }),
            400,
            "ValidationException",
        ),
        (
            "a @pattern refusal the handler raised, on the write",
            UPDATE,
            json!({
                "ConnectionRecordingPreferences":
                    preferences("123456789012", "192.168.1.1", KEY_ARN),
                "ClientToken": "id-pattern",
            }),
            400,
            "ValidationException",
        ),
    ];

    let mut seen: Vec<String> = Vec::new();
    for (case, operation, body, status, code) in rows {
        let emulator = Emulator::new();
        let reply = emulator.call(ACCOUNT_KEY, operation, &body).await;
        assert_eq!(
            reply.status, status,
            "case {case:?}: expected {status}, got {reply:?}"
        );
        assert_eq!(
            reply.code, code,
            "case {case:?}: expected code {code:?}, got {reply:?}"
        );
        let request_id = reply
            .request_id
            .clone()
            .unwrap_or_else(|| panic!("case {case:?}: expected a request id, got {reply:?}"));
        assert!(
            names_a_request_id(&request_id),
            "case {case:?}: expected a well-formed request id, got {request_id:?}"
        );
        // A FAILURE states that one id in its error body too, beside the `message`; a
        // success's body is the modelled output, which gains nothing.
        let in_body = reply
            .body
            .get("requestId")
            .and_then(Value::as_str)
            .map(str::to_string);
        if status >= 400 {
            assert_eq!(
                in_body.as_deref(),
                Some(request_id.as_str()),
                "case {case:?}: expected the error body to name the same request as the \
                 header, got {:?}",
                reply.body
            );
        } else {
            assert_eq!(
                in_body, None,
                "case {case:?}: expected a success body to stay the modelled output, got {:?}",
                reply.body
            );
        }
        assert!(
            !seen.contains(&request_id),
            "case {case:?}: expected an id of its own, but {request_id:?} already named another \
             answer"
        );
        seen.push(request_id);
    }
}

/// RULE: two answers to the same request name two requests — the id identifies the CALL,
/// not the state or the input, so a retried call is still a new answer with a new id.
#[tokio::test]
async fn each_call_is_named_by_an_id_of_its_own() {
    let emulator = Emulator::new();
    let mut seen: Vec<String> = Vec::new();
    // The same body, repeated: an idempotent replay answers the first outcome, and still
    // names the request that asked for it.
    for attempt in 1..=4 {
        let reply = emulator
            .call(ACCOUNT_KEY, DELETE, &json!({ "ClientToken": "repeated" }))
            .await;
        assert_eq!(
            reply.status, 200,
            "case \"attempt {attempt}\": expected the delete to succeed, got {reply:?}"
        );
        let request_id = reply.request_id.clone().unwrap_or_else(|| {
            panic!("case \"attempt {attempt}\": expected a request id, got {reply:?}")
        });
        assert!(
            !seen.contains(&request_id),
            "case \"attempt {attempt}\": expected a fresh id, got the earlier {request_id:?}"
        );
        seen.push(request_id);
    }
}

/// RULE: the id is on the answers NO operation produced too — a request that names no
/// modelled operation of this service is a failure the router words, and it names the
/// request it answered like every other response.
#[tokio::test]
async fn an_answer_no_operation_produced_names_its_request_too() {
    let emulator = Emulator::new();
    // (case, the operation name the path carries)
    let rows: [(&str, &str); 3] = [
        ("an operation this service does not bind", "ListConnectionRecordings"),
        ("an operation of another service", "DescribeInstances"),
        ("no operation at all", ""),
    ];
    for (case, operation) in rows {
        let (status, request_id, body) = emulator
            .dispatch(request(REGION, ACCOUNT_KEY, operation, &json!({})))
            .await;
        assert!(
            status >= 400,
            "case {case:?}: expected the request to fail, got {status} with {body}"
        );
        let request_id = request_id
            .unwrap_or_else(|| panic!("case {case:?}: expected a request id, got {status}: {body}"));
        assert!(
            names_a_request_id(&request_id),
            "case {case:?}: expected a well-formed request id, got {request_id:?}"
        );
        // The router words that failure in this service's own restJson1 envelope, so it is a
        // JSON object and it names the same request the header does.
        let document: Value = serde_json::from_str(&body)
            .unwrap_or_else(|_| panic!("case {case:?}: expected a JSON error body, got {body}"));
        let members = document.as_object().unwrap_or_else(|| {
            panic!("case {case:?}: expected a JSON error OBJECT, got {body}")
        });
        assert_eq!(
            members.get("requestId").and_then(Value::as_str),
            Some(request_id.as_str()),
            "case {case:?}: expected the error body to name the same request as the header, \
             got {body}"
        );
    }
}

/// RULE: the id joins an error body as ONE top-level member of the envelope the renderer
/// wrote, beside the `__type` and the `message` — which it leaves exactly as they were.
///
/// The rows span the error classes the model binds to these operations and the operations
/// that answer them: a 404 from the read, a 403 from the write's account gate, and a 400 the
/// constraint walk raised ahead of the write.
#[tokio::test]
async fn an_error_body_carries_the_id_beside_the_envelope_it_already_had() {
    // (case, operation, body, expected status, expected error code)
    let rows: [(&str, &str, Value, u16, &str); 3] = [
        (
            "the read's 404",
            GET,
            json!({}),
            404,
            "ResourceNotFoundException",
        ),
        (
            "the write's 403",
            UPDATE,
            update_body("id-403"),
            403,
            "AccessDeniedException",
        ),
        (
            "the write's 400",
            UPDATE,
            json!({
                "ConnectionRecordingPreferences":
                    preferences("123456789012", "rdp-recordings", KEY_ARN),
                "ClientToken": "t".repeat(65),
            }),
            400,
            "ValidationException",
        ),
    ];

    for (case, operation, body, status, code) in rows {
        let emulator = Emulator::new();
        let reply = emulator.call(ACCOUNT_KEY, operation, &body).await;
        assert_eq!(
            reply.status, status,
            "case {case:?}: expected {status}, got {reply:?}"
        );
        assert_eq!(
            reply.code, code,
            "case {case:?}: expected code {code:?}, got {reply:?}"
        );
        let members = reply
            .body
            .as_object()
            .unwrap_or_else(|| panic!("case {case:?}: expected a JSON error body, got {reply:?}"));
        let mut named: Vec<&str> = members.keys().map(String::as_str).collect();
        named.sort_unstable();
        assert_eq!(
            named,
            ["__type", "message", "requestId"],
            "case {case:?}: expected the id beside the envelope the renderer wrote, got {:?}",
            reply.body
        );
        assert!(
            !reply.message.is_empty(),
            "case {case:?}: expected the modelled message to survive, got {reply:?}"
        );
        assert_eq!(
            members.get("requestId").and_then(Value::as_str),
            reply.request_id.as_deref(),
            "case {case:?}: expected the body to name the same request as the header, got \
             {reply:?}"
        );
    }
}

/// RULE: the id names the response and nothing else about the answer changes — the body a
/// success carries is still exactly the modelled output, and the body a refusal carries is
/// still exactly the modelled error.
#[tokio::test]
async fn naming_the_request_changes_nothing_else_about_the_answer() {
    let emulator = Emulator::new();
    emulator.enroll(true);

    let written = emulator
        .call(ACCOUNT_KEY, UPDATE, &update_body("unchanged"))
        .await;
    assert_eq!(
        written.status, 200,
        "case \"the write\": expected it to succeed, got {written:?}"
    );
    assert_eq!(
        written.body,
        json!({
            "ClientToken": "unchanged",
            "ConnectionRecordingPreferences":
                preferences("123456789012", "rdp-recordings", KEY_ARN),
        }),
        "case \"the write\": expected exactly the modelled output, got {:?}",
        written.body
    );

    let read = emulator.call(ACCOUNT_KEY, GET, &json!({})).await;
    assert_eq!(
        read.status, 200,
        "case \"the read\": expected the stored record, got {read:?}"
    );
    assert_eq!(
        read.body, written.body,
        "case \"the read\": expected the record the write reported, got {:?}",
        read.body
    );

    // And the two answers are still named by two different requests.
    assert_ne!(
        written.request_id, read.request_id,
        "expected two answers to name two requests, got {:?} twice",
        read.request_id
    );
}

/// RULE: the id names the response ONCE — the layer that reports it is idempotent, so no
/// answer carries two ids however many layers it passed through, on an answer an operation
/// produced and on an answer none did.
#[tokio::test]
async fn the_id_is_reported_once_per_response() {
    let emulator = Emulator::new();
    // (case, the operation the path names)
    let rows: [(&str, &str); 3] = [
        ("an answer the read produced", GET),
        ("an answer the delete produced", DELETE),
        (
            "an answer no operation produced",
            "ListConnectionRecordings",
        ),
    ];
    for (case, operation) in rows {
        let response = emulator
            .respond(request(REGION, ACCOUNT_KEY, operation, &json!({})))
            .await;
        let status = response.status().as_u16();
        let reported: Vec<String> = response
            .headers()
            .get_all(REQUEST_ID_HEADER)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .map(str::to_string)
            .collect();
        assert_eq!(
            reported.len(),
            1,
            "case {case:?}: expected exactly one request-id header on the {status}, got \
             {reported:?}"
        );
        assert!(
            names_a_request_id(&reported[0]),
            "case {case:?}: expected a well-formed request id, got {:?}",
            reported[0]
        );
        // And exactly once in the body of a FAILURE, however many layers answered: a
        // success's body is the modelled output, which gains none.
        let body = String::from_utf8_lossy(response.body()).to_string();
        let expected = usize::from(status >= 400);
        assert_eq!(
            body.matches("\"requestId\"").count(),
            expected,
            "case {case:?}: expected {expected} request id(s) in the body of the {status}, got \
             {body}"
        );
    }

    // The modelled error body of the read is untouched by the layer that named it.
    let read = emulator.call(ACCOUNT_KEY, GET, &json!({})).await;
    assert_eq!(
        error_code(&read.body),
        "ResourceNotFoundException",
        "case \"the read's error body\": expected the modelled error, got {read:?}"
    );
}
