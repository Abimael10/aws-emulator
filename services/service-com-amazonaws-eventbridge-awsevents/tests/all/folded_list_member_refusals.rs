// SPDX-License-Identifier: CC0-1.0

//! The refusal of a non-string member of the list the case-folding match type is
//! handed under `anything-but`, end to end over the wire (`awsJson1_1`, `POST /`
//! with `X-Amz-Target: AWSEvents.<Operation>`) through the real
//! `MultiServiceRouter`.
//!
//! The RULE asserted here: that list holds the strings the comparison folds, so a
//! member of any other kind is not one of them and the pattern is refused
//! (`InvalidEventPatternException`, HTTP 400) with a reason naming THE LIST the
//! member sits in and the kinds of token that are no string — and the validators
//! this service runs a pattern through do not spell the match type that owns the
//! list alike: the one that compiles a RULE's pattern (`PutRule`) names it by the
//! short name of the comparison it folds, while the one that compiles an ARCHIVE's
//! (`CreateArchive`, `UpdateArchive`) names it in full. Both state the source
//! location of the position the reader had reached TWICE — the reader that found
//! the fault states it, and so does the reader that handed the whole list over —
//! and a refused write mints nothing, which a read under that name then reports as
//! the missing resource it is.
//!
//! Both operations validate the pattern they are handed: `PutRule.EventPattern`
//! and `CreateArchive.EventPattern` are each modelled `@length 0..4096` and each
//! operation models `InvalidEventPatternException` (client, httpError 400) —
//! `smithy_query(query="operation", operation="PutRule,CreateArchive")`.
//!
//! The rows span the class rather than one case: they differ along which kind of
//! token the offending member is (an integer, a fraction, `null`, `true`, `false`)
//! and where in the list it sits (first, last, after two strings).

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_eventbridge_awsevents::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region every request below is signed for.
const REGION: &str = "us-west-2";

/// The error this service answers a pattern its language does not admit with.
const INVALID_EVENT_PATTERN: &str = "InvalidEventPatternException";

/// The reason each validator states for a member of that list that is no string.
/// Written out here rather than imported, so a wrong constant in the
/// implementation cannot make a row pass.
const REASON_FOR_A_RULE: &str =
    "Inside anything-but/ignore-case list, number|start|null|boolean is not supported.";
const REASON_FOR_AN_ARCHIVE: &str =
    "Inside anything-but/equals-ignore-case list, number|start|null|boolean is not supported.";

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

/// The wire error code of a failure body (`__type` on awsJson), without its
/// namespace.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .map(|name| name.rsplit('#').next().unwrap_or(name).to_string())
        .unwrap_or_default()
}

/// The `message` a failure body carries.
fn error_message(body: &Value) -> String {
    body.get("message")
        .or_else(|| body.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The whole message a refused pattern must answer with: the opening, the reason,
/// and the source location of the position just past `read` — the text the
/// validator had to read to find the fault — stated once for each of the two
/// readers of a member of a list.
///
/// The line and column are COMPUTED from that text rather than written down, so
/// each row states the rule (where the validator got to) instead of a number.
fn expected_refusal(pattern: &str, reason: &str, read: &str) -> String {
    let line = 1 + read.matches('\n').count();
    let last = read.rsplit_once('\n').map_or(read, |(_, after)| after);
    let column = 1 + last.chars().count();
    let location =
        format!("\n at [Source: (String)\"{pattern}\"; line: {line}, column: {column}]");
    format!("Event pattern is not valid. Reason: {reason}{location}{location}")
}

/// The ARN of the account's default bus, which is the event source an archive is
/// created over.
fn default_bus_arn() -> String {
    format!(
        "arn:aws:events:{REGION}:{}:event-bus/default",
        engine::emulator_account_id()
    )
}

/// One row: the case, the pattern as submitted, and the prefix of it the reader
/// had consumed when it refused.
struct Row {
    case: &'static str,
    pattern: &'static str,
    read: &'static str,
}

/// Both validators refuse the member, each naming the list in its own words, and
/// neither mints the resource the refused request asked for.
#[tokio::test]
async fn a_non_string_in_the_folded_list_is_refused_in_each_validators_words() {
    let rows: [Row; 6] = [
        Row {
            case: "an integer after a string",
            pattern: r#"{"detail": {"k": [{"anything-but": {"equals-ignore-case": ["a", 1]}}]}}"#,
            read: r#"{"detail": {"k": [{"anything-but": {"equals-ignore-case": ["a", 1"#,
        },
        Row {
            case: "an integer before a string",
            pattern: r#"{"detail": {"k": [{"anything-but": {"equals-ignore-case": [7, "a"]}}]}}"#,
            read: r#"{"detail": {"k": [{"anything-but": {"equals-ignore-case": [7"#,
        },
        Row {
            case: "a fraction after two strings",
            pattern: r#"{"detail": {"k": [{"anything-but": {"equals-ignore-case": ["a", "b", 2.5]}}]}}"#,
            read: r#"{"detail": {"k": [{"anything-but": {"equals-ignore-case": ["a", "b", 2.5"#,
        },
        Row {
            case: "null as the only member",
            pattern: r#"{"detail": {"k": [{"anything-but": {"equals-ignore-case": [null]}}]}}"#,
            read: r#"{"detail": {"k": [{"anything-but": {"equals-ignore-case": [null"#,
        },
        Row {
            case: "true as the only member",
            pattern: r#"{"detail": {"k": [{"anything-but": {"equals-ignore-case": [true]}}]}}"#,
            read: r#"{"detail": {"k": [{"anything-but": {"equals-ignore-case": [true"#,
        },
        Row {
            case: "false after a string",
            pattern: r#"{"detail": {"k": [{"anything-but": {"equals-ignore-case": ["x", false]}}]}}"#,
            read: r#"{"detail": {"k": [{"anything-but": {"equals-ignore-case": ["x", false"#,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let Row {
            case,
            pattern,
            read,
        } = row;
        assert!(
            pattern.starts_with(read),
            "case {case:?}: the row's read prefix {read:?} must be a prefix of {pattern:?}"
        );
        let router = emulator();

        // The RULE compiler: its own sentence, and no rule under that name.
        let rule = format!("folded-list-{index}");
        let (status, refused) = events(
            &router,
            "PutRule",
            &json!({ "Name": &rule, "EventPattern": pattern }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: PutRule must refuse the pattern, got {status} {refused}"
        );
        assert_eq!(
            error_code(&refused),
            INVALID_EVENT_PATTERN,
            "case {case:?}: PutRule answered {refused}"
        );
        let expected = expected_refusal(pattern, REASON_FOR_A_RULE, read);
        let actual = error_message(&refused);
        assert_eq!(
            actual, expected,
            "case {case:?}: PutRule: expected {expected:?}, got {actual:?}"
        );
        let (status, described) =
            events(&router, "DescribeRule", &json!({ "Name": &rule })).await;
        assert_eq!(
            status, 400,
            "case {case:?}: a refused PutRule mints no rule, so DescribeRule must report \
             none; got {status} {described}"
        );
        assert_eq!(
            error_code(&described),
            "ResourceNotFoundException",
            "case {case:?}: DescribeRule answered {described}"
        );

        // The ARCHIVE compiler: the same fault, the match type named in full, and
        // no archive under that name.
        let archive = format!("folded-list-archive-{index}");
        let (status, refused) = events(
            &router,
            "CreateArchive",
            &json!({
                "ArchiveName": &archive,
                "EventSourceArn": default_bus_arn(),
                "EventPattern": pattern,
            }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: CreateArchive must refuse the pattern, got {status} {refused}"
        );
        assert_eq!(
            error_code(&refused),
            INVALID_EVENT_PATTERN,
            "case {case:?}: CreateArchive answered {refused}"
        );
        let expected = expected_refusal(pattern, REASON_FOR_AN_ARCHIVE, read);
        let actual = error_message(&refused);
        assert_eq!(
            actual, expected,
            "case {case:?}: CreateArchive: expected {expected:?}, got {actual:?}"
        );
        let (status, described) = events(
            &router,
            "DescribeArchive",
            &json!({ "ArchiveName": &archive }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: a refused CreateArchive mints no archive, so DescribeArchive \
             must report none; got {status} {described}"
        );
        assert_eq!(
            error_code(&described),
            "ResourceNotFoundException",
            "case {case:?}: DescribeArchive answered {described}"
        );
    }
}
