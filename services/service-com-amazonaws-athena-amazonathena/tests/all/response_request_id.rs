// SPDX-License-Identifier: CC0-1.0

//! The request an Athena response names itself with, end to end over the wire.
//!
//! Every row drives a real `awsJson1_1` request through the router the emulator mounts and
//! reads the RESPONSE HEADER an SDK surfaces as the call's request id — so what is asserted
//! is what an `athena` client (and any layer relaying its answers) can observe, not an
//! internal call.
//!
//! The rule under test, asserted over a TABLE of answers rather than one recorded request:
//! every response this service sends names the request it answered in the
//! `x-amzn-requestid` header, whichever layer rendered it, and no two calls are named by
//! one id.
//!
//! The rows vary what the rule generalises over: whether the answer is a success or a
//! refusal, the HTTP status, and the LAYER that produced it — a handler's output, a
//! handler-raised modelled refusal, a constraint refusal the boundary validator raised
//! while an operation was being served, the scaffold's 501, and a routing failure no
//! operation produced at all.
//!
//! What the rule does not change, and this suite pins beside it: the status each call
//! answers with, and the body of a refusal — its `__type` and its `Message` — stay exactly
//! what the layer that rendered them wrote.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{call_reported, error_code, message, router, ACCOUNT_ACCESS_KEY_ID};

/// The header AWS reports a response's own request id in, and the name an SDK reads its
/// response metadata's request id off. Written out as the literal a client sees rather
/// than taken from a constant of the service's, so changing that constant fails this suite
/// instead of moving with it.
const REQUEST_ID_HEADER: &str = "x-amzn-requestid";

/// The id a client reads as "no id at all": the all-zeros UUID. No response may report it.
const PLACEHOLDER_ID: &str = "00000000-0000-0000-0000-000000000000";

/// A query execution id no `StartQueryExecution` of this suite ever minted, which is what
/// the not-found rows read.
const UNMINTED_EXECUTION: &str = "8e58232c-89af-4f1d-89b5-0ebc79aa8b03";

/// One answer of this service: the call that produces it, and the status it comes back
/// with.
struct Answer {
    /// What this row varies; named in every assertion.
    case: &'static str,
    /// The `X-Amz-Target` operation, which for the routing-failure row names no modelled
    /// operation at all.
    operation: &'static str,
    body: Value,
    /// The status that call answers with, so a row that stopped reaching the layer it is
    /// about fails instead of silently asserting the id of some other answer. `None` for
    /// the row whose status is the framework's own rendering of a target naming no
    /// modelled operation, which this suite does not pin: what it is about is that such an
    /// answer names its request too.
    status: Option<u16>,
}

/// Every layer of this service that can answer, with the workgroup the rows that read one
/// back created first.
async fn answers(router: &MultiServiceRouter) -> Vec<Answer> {
    let (status, _headers, created) = call_reported(
        router,
        ACCOUNT_ACCESS_KEY_ID,
        "CreateWorkGroup",
        &json!({ "Name": "reported_group" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the suite's own workgroup must be created, got {status} with {created}"
    );

    vec![
        Answer {
            case: "a mutating success",
            operation: "CreateWorkGroup",
            body: json!({ "Name": "reported_group_second" }),
            status: Some(200),
        },
        Answer {
            case: "a reporting success",
            operation: "GetWorkGroup",
            body: json!({ "WorkGroup": "reported_group" }),
            status: Some(200),
        },
        Answer {
            case: "a listing success, whose answer carries a collection",
            operation: "ListWorkGroups",
            body: json!({}),
            status: Some(200),
        },
        Answer {
            // A handler-raised modelled refusal: no execution answers to that id.
            case: "a handler-raised modelled refusal",
            operation: "GetQueryExecution",
            body: json!({ "QueryExecutionId": UNMINTED_EXECUTION }),
            status: Some(400),
        },
        Answer {
            // A second operation of the same class, so the id is not a property of one
            // handler.
            case: "a handler-raised modelled refusal of a mutating call",
            operation: "StopQueryExecution",
            body: json!({ "QueryExecutionId": UNMINTED_EXECUTION }),
            status: Some(400),
        },
        Answer {
            // A collision this service refuses, on the same status through another path.
            case: "a re-create of a stored name",
            operation: "CreateWorkGroup",
            body: json!({ "Name": "reported_group" }),
            status: Some(400),
        },
        Answer {
            // A constraint refusal the boundary validator raised while an operation was
            // being served: `QueryExecutionId` is `@required`.
            case: "a boundary constraint refusal",
            operation: "GetQueryExecution",
            body: json!({}),
            status: Some(400),
        },
        Answer {
            // The scaffold's 501: an answer a still-stubbed operation produced.
            case: "a scaffolded operation's 501",
            operation: "ListApplicationDPUSizes",
            body: json!({}),
            status: Some(501),
        },
        Answer {
            // An answer NO operation produced: a target this service's model names no
            // operation for, rendered by the framework's routing-failure renderer. WHICH
            // failure status that renderer answers is not this suite's subject, so the row
            // requires a failure and nothing narrower.
            case: "a target naming no modelled operation",
            operation: "ThereIsNoSuchOperation",
            body: json!({}),
            status: None,
        },
    ]
}

/// Make one call and answer `(status, the request id it reported, body)`.
///
/// The request is built by this crate's own harness, so the suite states no request shape
/// or credential scope of its own. The request id is `None` for a response that named no
/// request, which is the divergence this suite is about.
async fn call(
    router: &MultiServiceRouter,
    operation: &str,
    body: &Value,
) -> (u16, Option<String>, Value) {
    let (status, headers, parsed) =
        call_reported(router, ACCOUNT_ACCESS_KEY_ID, operation, body).await;
    let reported = headers
        .get(REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    (status, reported, parsed)
}

/// Whether `id` has the form almost every AWS service names a request by, Athena among
/// them: the hyphenated 8-4-4-4-12 groups of LOWERCASE hex digits of a version-4 UUID.
///
/// Stated here rather than read off the service, so the FORM is pinned by this suite.
fn is_uuid_shaped(id: &str) -> bool {
    let mut groups = id.split('-');
    for width in [8usize, 4, 4, 4, 12] {
        match groups.next() {
            Some(group) => {
                let hex = group.len() == width
                    && group
                        .bytes()
                        .all(|digit| digit.is_ascii_hexdigit() && !digit.is_ascii_uppercase());
                if !hex {
                    return false;
                }
            }
            None => return false,
        }
    }
    groups.next().is_none()
}

/// RULE: every response this service sends names the request it answered in the
/// `x-amzn-requestid` header, whichever layer rendered it.
#[tokio::test]
async fn every_response_names_the_request_it_answered() {
    let router = router();

    for answer in answers(&router).await {
        let case = answer.case;
        let (status, reported, body) = call(&router, answer.operation, &answer.body).await;
        match answer.status {
            Some(expected) => assert_eq!(
                status, expected,
                "case {case:?}: expected HTTP {expected}, got {status} with {body}"
            ),
            None => assert!(
                status >= 400,
                "case {case:?}: expected a failure status, got {status} with {body}"
            ),
        }
        let reported = reported.unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected the response to name its request in \
                 {REQUEST_ID_HEADER:?}, got none and the body {body}"
            )
        });
        assert!(
            !reported.is_empty() && reported != PLACEHOLDER_ID && is_uuid_shaped(&reported),
            "case {case:?}: expected a UUID-shaped request id, got {reported:?}"
        );
    }
}

/// RULE (the id is the CALL's): two answers never name the same request, whether they are
/// successes or refusals, and whichever layer rendered them.
#[tokio::test]
async fn two_responses_never_name_the_same_request() {
    let router = router();
    let mut reported: Vec<String> = Vec::new();

    for answer in answers(&router).await {
        // Twice per row, so the id is shown to be per CALL rather than per answer kind.
        for attempt in 1..=2 {
            let case = format!("{} (attempt {attempt})", answer.case);
            let (_, named, body) = call(&router, answer.operation, &answer.body).await;
            let named = named.unwrap_or_else(|| {
                panic!("case {case:?}: expected a reported request id, got the body {body}")
            });
            reported.push(named);
        }
    }

    let mut distinct = reported.clone();
    distinct.sort();
    distinct.dedup();
    assert_eq!(
        distinct.len(),
        reported.len(),
        "case every answered call: expected one request id per call, got {reported:?}"
    );
}

/// RULE (naming the request changes nothing else): a refusal still carries the class and
/// the sentence its own renderer wrote, beside the id its response reports.
#[tokio::test]
async fn naming_the_request_leaves_the_refusal_body_alone() {
    let router = router();

    // (case, the operation, its request, the sentence the refusal states)
    let rows: Vec<(&str, &str, Value, String)> = vec![
        (
            "a read of an execution no start minted",
            "GetQueryExecution",
            json!({ "QueryExecutionId": UNMINTED_EXECUTION }),
            format!("QueryExecution {UNMINTED_EXECUTION} was not found"),
        ),
        (
            "a stop of an execution no start minted",
            "StopQueryExecution",
            json!({ "QueryExecutionId": UNMINTED_EXECUTION }),
            format!("QueryExecution {UNMINTED_EXECUTION} was not found"),
        ),
        (
            "a read of the results of an execution no start minted",
            "GetQueryResults",
            json!({ "QueryExecutionId": UNMINTED_EXECUTION }),
            format!("QueryExecution {UNMINTED_EXECUTION} was not found"),
        ),
    ];

    for (case, operation, body, sentence) in rows {
        let (status, reported, answered) = call(&router, operation, &body).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {answered}"
        );
        assert_eq!(
            error_code(&answered),
            "InvalidRequestException",
            "case {case:?}: expected this service's own refusal shape, got {answered}"
        );
        assert_eq!(
            message(&answered),
            sentence,
            "case {case:?}: expected the sentence {sentence:?}, got {answered}"
        );
        let reported = reported.unwrap_or_else(|| {
            panic!("case {case:?}: expected the refusal to name its request, got {answered}")
        });
        assert!(
            is_uuid_shaped(&reported),
            "case {case:?}: expected a UUID-shaped request id, got {reported:?}"
        );
    }
}
