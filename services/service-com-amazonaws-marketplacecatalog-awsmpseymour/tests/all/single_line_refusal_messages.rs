// SPDX-License-Identifier: CC0-1.0

//! Every sentence this service builds around a SUBMITTED value is one line, end to end
//! over HTTP.
//!
//! One rule is under test: the members that carry such a sentence —
//! `ValidationException$Message` and `ErrorDetail$ErrorMessage` (both
//! `ExceptionMessageContent`) and `BatchDescribeErrorDetail$ErrorMessage`
//! (`BatchDescribeErrorMessageContent`) — all declare `@pattern("^(.)+$")` beside
//! `@length(1, 2048)` (`smithy_query(query="shape",
//! shape="ExceptionMessageContent,BatchDescribeErrorMessageContent")`), and `.` matches no
//! line terminator. So a submitted value carrying one is quoted back with that terminator
//! ESCAPED, and the sentence stays one line inside those 2048 characters — whichever
//! member carried the value and whichever channel the refusal took.
//!
//! The rows span the class the rule generalises over: each line terminator the single-line
//! rule excludes, the operation and member that carried it (query-bound and body-bound,
//! top level and nested), the length of the value, and the four channels a quoted value
//! reaches a caller through — the top-level `ValidationException` of a handler, the
//! request-boundary refusal of a member held to its modelled `@pattern`, the per-entry
//! `Errors` of a batch read, and the `ErrorDetailList` of a change that failed.
//!
//! Every assertion goes through the router, so each case exercises routing,
//! deserialization, the handler and serialization.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_marketplacecatalog_awsmpseymour::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The catalog every operation's `Catalog` member documents as its fixed value.
const CATALOG: &str = "AWSMarketplace";

/// A SigV4 credential scope naming this service's signing name (`aws-marketplace`), which
/// is what routes a request here.
const SIGNED_FOR_EU_WEST_1: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/eu-west-1/aws-marketplace/aws4_request, \
     SignedHeaders=host, Signature=abc";

/// How long a message may be: the `@length(max)` of both message shapes
/// (`smithy_query(query="shape",
/// shape="ExceptionMessageContent,BatchDescribeErrorMessageContent")`), written out here
/// rather than read from the crate so a wrong implementation cannot make this suite pass
/// by quoting its own bound.
const MESSAGE_MAX: usize = 2048;

/// The line terminators a single-line value admits none of — the characters a regular
/// expression's `.` does not match, which is what makes `^(.)+$` a single line: line feed,
/// carriage return, next line, line separator and paragraph separator.
///
/// `(the name of the case, the character, the escape a message shows it as)`.
const LINE_TERMINATORS: &[(&str, char, &str)] = &[
    ("a line feed (U+000A)", '\n', r"\n"),
    ("a carriage return (U+000D)", '\r', r"\r"),
    ("a next line (U+0085)", '\u{85}', r"\u0085"),
    ("a line separator (U+2028)", '\u{2028}', r"\u2028"),
    ("a paragraph separator (U+2029)", '\u{2029}', r"\u2029"),
];

/// How long this suite waits for a change set to end its run before it gives up: far more
/// than the spans the implementation publishes, so a loaded machine is not a failure.
const RUN_DEADLINE: Duration = Duration::from_secs(60);

/// How long this suite waits between two reads of a change set it is waiting on.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// One emulator: a router carrying only this service, over one shared engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(engine));
    router
}

/// What the emulator answered: the HTTP status and the JSON body.
struct Answer {
    status: u16,
    body: Value,
}

impl Answer {
    /// The wire error code (`__type`), or the empty string on a success.
    fn error_code(&self) -> &str {
        let raw = self.body["__type"].as_str().unwrap_or_default();
        raw.rsplit('#').next().unwrap_or(raw)
    }

    /// The string at `pointer`, or the empty string.
    fn text(&self, pointer: &str) -> String {
        self.body
            .pointer(pointer)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    /// The fault this answer carries: a handler-stated refusal spells it `Message`, and a
    /// request-boundary refusal (which takes the protocol's envelope) spells it `message`.
    fn message(&self) -> String {
        let reported = self.text("/Message");
        if reported.is_empty() {
            return self.text("/message");
        }
        reported
    }
}

async fn call(router: &MultiServiceRouter, method: &str, uri: &str, body: Option<Value>) -> Answer {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let mut builder = ::http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "catalog.marketplace.eu-west-1.amazonaws.com")
        .header("authorization", SIGNED_FOR_EU_WEST_1);
    if !payload.is_empty() {
        builder = builder.header("content-type", "application/json");
    }
    let request = builder
        .body(Bytes::from(payload))
        .expect("the test request is well formed");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|error| {
            panic!(
                "{method} {uri} answered a body that is not JSON ({error}): {:?}",
                String::from_utf8_lossy(&bytes)
            )
        })
    };
    Answer { status, body }
}

/// `value` as a query-string parameter carries it: every byte outside the unreserved set
/// percent-encoded, so a submitted line terminator reaches the handler as sent.
fn encoded(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(*byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

/// The first line terminator `text` carries, or `None` when it is a single line.
fn line_terminator(text: &str) -> Option<char> {
    text.chars()
        .find(|c| LINE_TERMINATORS.iter().any(|(_, excluded, _)| excluded == c))
}

/// One request built around a submitted value, and what the answer has to be.
struct Case {
    /// The member and channel this row is about.
    case: String,
    method: &'static str,
    uri: String,
    body: Option<Value>,
    /// The status the refusal carries: the modelled `ValidationException`'s 422 for a
    /// member a handler judges, and the protocol's 400 for one the request boundary
    /// refuses against its modelled `@pattern`.
    status: u16,
    /// What the sentence opens with, before the quoted value.
    prefix: &'static str,
}

/// Every refusal that quotes `value`, one per member and channel.
fn cases(value: &str) -> Vec<Case> {
    vec![
        Case {
            case: "DescribeEntity: a query-bound entityId".to_string(),
            method: "GET",
            uri: format!(
                "/DescribeEntity?catalog={CATALOG}&entityId={}",
                encoded(value)
            ),
            body: None,
            status: 422,
            prefix: "[Requested entity id '",
        },
        Case {
            case: "CancelChangeSet: a query-bound catalog".to_string(),
            method: "PATCH",
            uri: format!(
                "/CancelChangeSet?catalog={}&changeSetId=cs-abc123",
                encoded(value)
            ),
            body: None,
            status: 422,
            prefix: "[Requested catalog '",
        },
        Case {
            case: "GetResourcePolicy: a query-bound resourceArn".to_string(),
            method: "GET",
            uri: format!("/GetResourcePolicy?resourceArn={}", encoded(value)),
            body: None,
            status: 422,
            prefix: "[Requested resource arn '",
        },
        Case {
            case: "TagResource: a body-bound ResourceArn".to_string(),
            method: "POST",
            uri: "/TagResource".to_string(),
            body: Some(json!({
                "ResourceArn": value,
                "Tags": [{"Key": "owner", "Value": "catalog"}],
            })),
            status: 422,
            prefix: "[Requested resource arn '",
        },
        Case {
            case: "StartChangeSet: a body-bound ClientRequestToken".to_string(),
            method: "POST",
            uri: "/StartChangeSet".to_string(),
            body: Some(json!({
                "Catalog": CATALOG,
                "ClientRequestToken": value,
                "ChangeSet": [{
                    "ChangeType": "CreateProduct",
                    "Entity": {"Type": "AmiProduct@1.0"},
                    "DetailsDocument": {"ProductTitle": "a single line"},
                }],
            })),
            status: 422,
            prefix: "[Requested client request token '",
        },
        Case {
            case: "ListEntities: a nested Sort.SortBy".to_string(),
            method: "POST",
            uri: "/ListEntities".to_string(),
            body: Some(json!({
                "Catalog": CATALOG,
                "EntityType": "AmiProduct",
                "Sort": {"SortBy": value},
            })),
            status: 422,
            prefix: "[Requested sort by '",
        },
        Case {
            // A member held to its own modelled `@pattern`
            // (`smithy_query(query="shape", shape="FilterName")` — `^[a-zA-Z]+$`, which
            // admits no line terminator), so the request boundary refuses it before any
            // handler runs: the other channel a submitted value is quoted back through.
            case: "ListEntities: a boundary-refused filter name".to_string(),
            method: "POST",
            uri: "/ListEntities".to_string(),
            body: Some(json!({
                "Catalog": CATALOG,
                "EntityType": "AmiProduct",
                "FilterList": [{"Name": value, "ValueList": ["prod-abc123"]}],
            })),
            status: 400,
            prefix: "[Requested name '",
        },
    ]
}

/// Assert `message` is a sentence a message member may carry: one line, inside the length
/// both message shapes declare, and not empty.
fn assert_single_line(case: &str, message: &str) {
    assert!(
        !message.is_empty(),
        "case {case:?}: the refusal must state a message, got {message:?}"
    );
    assert!(
        line_terminator(message).is_none(),
        "case {case:?}: a message member admits no line terminator, got {:?} in {message:?}",
        line_terminator(message)
    );
    assert!(
        message.chars().count() <= MESSAGE_MAX,
        "case {case:?}: a message is at most {MESSAGE_MAX} characters, got {} in {message:?}",
        message.chars().count()
    );
}

/// RULE: a refusal quoting a submitted value that carries a line terminator shows that
/// terminator escaped, and the sentence stays one line inside the message length.
#[tokio::test]
async fn a_refusal_quoting_a_submitted_line_break_stays_one_line() {
    let router = emulator();

    for (terminator, character, escape) in LINE_TERMINATORS {
        let value = format!("a{character}b");
        for row in cases(&value) {
            let case = format!("{}: {terminator}", row.case);
            let answer = call(&router, row.method, &row.uri, row.body.clone()).await;
            assert_eq!(
                (answer.status, answer.error_code()),
                (row.status, "ValidationException"),
                "case {case:?}: expected {} ValidationException, got {} {:?} with {:?}",
                row.status,
                answer.status,
                answer.error_code(),
                answer.body
            );
            let message = answer.message();
            assert_single_line(&case, &message);
            assert!(
                message.starts_with(row.prefix),
                "case {case:?}: the sentence must open with {:?}, got {message:?}",
                row.prefix
            );
            assert!(
                message.contains(&format!("a{escape}b")),
                "case {case:?}: the submitted value must be quoted with the terminator \
                 escaped as {:?}, got {message:?}",
                format!("a{escape}b")
            );
        }
    }
}

/// The same rule's CONTROL: a submitted value carrying no line terminator is quoted
/// verbatim, so the escaping above is about the terminator and not about the value.
///
/// The rows span the ways a value can miss the published syntax without a line break: a
/// space, a character the syntax excludes, a backslash, and a value past the length a
/// message can quote whole.
#[tokio::test]
async fn a_submitted_value_without_a_line_break_is_quoted_verbatim() {
    let router = emulator();

    for (case, value) in [
        ("a space", "a b".to_string()),
        ("a character outside the syntax", "bad#id".to_string()),
        ("a backslash", r"a\nb".to_string()),
        ("a value longer than a message", "z".repeat(4000)),
    ] {
        let answer = call(
            &router,
            "GET",
            &format!(
                "/DescribeEntity?catalog={CATALOG}&entityId={}",
                encoded(&value)
            ),
            None,
        )
        .await;
        assert_eq!(
            (answer.status, answer.error_code()),
            (422, "ValidationException"),
            "case {case:?}: expected 422 ValidationException, got {} {:?} with {:?}",
            answer.status,
            answer.error_code(),
            answer.body
        );
        let message = answer.message();
        assert_single_line(case, &message);
        let quoted = value.chars().take(64).collect::<String>();
        assert!(
            message.contains(&quoted),
            "case {case:?}: the value must be quoted as sent, got {message:?}"
        );
    }
}

/// RULE: the same holds when the quoted value is cut — a value far longer than a message
/// is quoted as far as it fits, and the cut sentence is still one line inside the bound.
#[tokio::test]
async fn a_cut_quote_of_a_value_carrying_a_line_break_stays_one_line() {
    let router = emulator();

    for (terminator, character, _) in LINE_TERMINATORS {
        let value = format!("{}{character}{}", "y".repeat(3000), "z".repeat(3000));
        let answer = call(
            &router,
            "GET",
            &format!(
                "/DescribeEntity?catalog={CATALOG}&entityId={}",
                encoded(&value)
            ),
            None,
        )
        .await;
        let case = format!("a 6001-character entity id carrying {terminator}");
        assert_eq!(
            (answer.status, answer.error_code()),
            (422, "ValidationException"),
            "case {case:?}: expected 422 ValidationException, got {} {:?} with {:?}",
            answer.status,
            answer.error_code(),
            answer.body
        );
        let message = answer.message();
        assert_single_line(&case, &message);
        assert!(
            message.contains("..."),
            "case {case:?}: a quote that was cut must say so, got {message:?}"
        );
    }
}

/// RULE, on the per-entry channel: a batch read reports a malformed entry in `Errors`, and
/// that entry's `ErrorMessage` is held to the single line its own
/// `BatchDescribeErrorMessageContent` declares.
#[tokio::test]
async fn a_batch_entry_error_message_stays_one_line() {
    let router = emulator();

    for (terminator, character, escape) in LINE_TERMINATORS {
        let value = format!("a{character}b");
        let case = format!("BatchDescribeEntities: an entry whose EntityId carries {terminator}");
        let answer = call(
            &router,
            "POST",
            "/BatchDescribeEntities",
            Some(json!({
                "EntityRequestList": [{"Catalog": CATALOG, "EntityId": value}],
            })),
        )
        .await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: a batch read answers 200 whatever its entries resolve to, got \
             {} with {:?}",
            answer.status, answer.body
        );
        let errors = answer
            .body
            .pointer("/Errors")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            errors.len(),
            1,
            "case {case:?}: the one malformed entry must be reported once, got {:?}",
            answer.body
        );
        let detail = errors
            .values()
            .next()
            .expect("the one reported entry is present");
        assert_eq!(
            detail["ErrorCode"].as_str().unwrap_or_default(),
            "ValidationException",
            "case {case:?}: a malformed id is a validation failure, got {detail:?}"
        );
        let message = detail["ErrorMessage"].as_str().unwrap_or_default();
        assert_single_line(&case, message);
        assert!(
            message.starts_with("Requested entity id '"),
            "case {case:?}: the entry must state why it failed, got {message:?}"
        );
        assert!(
            message.contains(&format!("a{escape}b")),
            "case {case:?}: the submitted id must be quoted with the terminator escaped \
             as {:?}, got {message:?}",
            format!("a{escape}b")
        );
    }
}

/// RULE, on the change-error channel: an `ErrorDetail$ErrorMessage` quoting a submitted
/// details value is held to the same single line, which is what makes the rule about the
/// members that carry a sentence rather than about one operation.
#[tokio::test]
async fn a_change_error_message_quoting_a_details_value_stays_one_line() {
    let router = emulator();

    for (terminator, character, escape) in LINE_TERMINATORS {
        let case = format!("StartChangeSet: a ProductTitle carrying {terminator}");
        let started = call(
            &router,
            "POST",
            "/StartChangeSet",
            Some(json!({
                "Catalog": CATALOG,
                "ChangeSet": [{
                    "ChangeType": "CreateProduct",
                    "Entity": {"Type": "DataProduct@1.0"},
                    "DetailsDocument": {"ProductTitle": format!("two{character}lines")},
                }],
            })),
        )
        .await;
        assert_eq!(
            started.status, 200,
            "case {case:?}: content only applying can judge must be accepted, got {} with \
             {:?}",
            started.status, started.body
        );
        let id = started.text("/ChangeSetId");
        let described = ended_change_set(&router, &case, &id).await;
        assert_eq!(
            described.text("/Status"),
            "FAILED",
            "case {case:?}: a details value the catalog cannot read must fail the change \
             set, got {:?}",
            described.body
        );
        let message = described
            .body
            .pointer("/ChangeSet/0/ErrorDetailList/0/ErrorMessage")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        assert_single_line(&case, &message);
        assert!(
            message.contains(&format!("two{escape}lines")),
            "case {case:?}: the submitted value must be quoted with the terminator escaped \
             as {:?}, got {message:?}",
            format!("two{escape}lines")
        );
    }
}

/// Change set `id` once its run has ended: read again until its status is no longer one of
/// the two the clock owes a transition from, or [`RUN_DEADLINE`] passes.
async fn ended_change_set(router: &MultiServiceRouter, case: &str, id: &str) -> Answer {
    let deadline = Instant::now() + RUN_DEADLINE;
    loop {
        let described = call(
            router,
            "GET",
            &format!("/DescribeChangeSet?catalog={CATALOG}&changeSetId={id}"),
            None,
        )
        .await;
        let status = described.text("/Status");
        if !matches!(status.as_str(), "PREPARING" | "APPLYING") {
            return described;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: change set {id} never ended its run, last status {status:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}
