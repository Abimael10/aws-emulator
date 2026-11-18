// SPDX-License-Identifier: CC0-1.0

//! What the event-pattern language admits as a match type's OPERAND, and what
//! those operands match — end to end over the wire (`awsJson1_1`, `POST /` with
//! `X-Amz-Target: AWSEvents.<Operation>`) through the real
//! `MultiServiceRouter`.
//!
//! The RULES asserted here:
//!
//! * a `numeric` comparison compares IEEE-754 binary64 doubles, so a numeral no
//!   double carries exactly names a number the comparison cannot answer about: as
//!   a pattern's bound it is refused (`InvalidEventPatternException`, HTTP 400,
//!   `Event pattern is not valid. Reason: Cannot compare number : <the bound as
//!   written>` and the source location just past the argument list the reader
//!   had read), and as an EVENT's value it is a value the comparison does not
//!   consider, so no `numeric` term holds for it;
//! * one validator reads a pattern for every operation that is handed one, so
//!   the same bound is refused with the same message by `TestEventPattern` and by
//!   `PutRule` — and a refused `PutRule` mints no rule, which a `DescribeRule`
//!   under that name then reports as the missing resource it is;
//! * a word JSON has no spelling for (`Infinity`, `-Infinity`, `NaN`) is refused
//!   as the non-standard token class it is, at the position just PAST the token,
//!   while an ordinary unquoted word keeps its own sentence and the position it
//!   STARTS at;
//! * inside a `wildcard` operand a backslash makes the next character literal,
//!   and the two characters the operand's grammar defines — the wildcard and the
//!   backslash — are the two it may escape: an escaped wildcard is literal text,
//!   so it neither continues a run of wildcards (the operand is admitted) nor
//!   matches anything but the character itself, and an escape of any other
//!   character is refused;
//! * a refusal about ONE member of a list a match type was handed is stated by
//!   two readers — the one that found the fault and the one that handed the list
//!   over — so its location clause is carried twice;
//! * a `cidr` operand names the BLOCK its prefix length cuts out, so an address
//!   carrying bits past that prefix stands for the block containing it; an
//!   operand that is not text at all is refused by the plain string-operand
//!   requirement, and a rule of the notation itself is the whole message, with
//!   no source location;
//! * a pattern the language admits is STORED as submitted: `PutRule` answers the
//!   rule's ARN and `DescribeRule` reads the pattern back byte for byte.
//!
//! Each table spans its class rather than one case: the numeric rows differ along
//! how the numeral is written (integer, fraction, exponent), its sign, its
//! magnitude and whether a double carries it; the wildcard rows along where the
//! escape sits in the operand, which character it escapes, how many wildcards
//! surround it, and whether the event carries the literal character or the text a
//! wildcard reading of it would admit; the cidr rows along which address of the
//! block was written, the prefix length and the address family.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_eventbridge_awsevents::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region every request below is signed for.
const REGION: &str = "us-west-2";

/// The reason a refusal of a bound outside the comparison's numeric domain
/// states, before the bound as the request wrote it.
const CANNOT_COMPARE: &str = "Cannot compare number : ";

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

fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(engine));
    router
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

/// A valid event envelope carrying `detail`: every field
/// `TestEventPattern$Event` makes mandatory ("the following fields are mandatory:
/// id account source time region resources detail-type").
fn envelope(detail: Value) -> String {
    json!({
        "version": "0",
        "id": "1f7c9c2e-6b1a-4f3d-8a2b-5c6d7e8f9a0b",
        "account": engine::emulator_account_id(),
        "source": "com.demo.orders",
        "time": "2026-03-04T10:00:00Z",
        "region": REGION,
        "resources": [],
        "detail-type": "Order Placed",
        "detail": detail,
    })
    .to_string()
}

/// The whole message a refused pattern must answer with: the opening, the reason,
/// and the source location of the position just past `read` — the text the
/// validator had to read to find the fault.
///
/// The line and column are COMPUTED from that text rather than written down, so
/// each row states the rule (where the validator got to) instead of a number.
fn expected_refusal(pattern: &str, reason: &str, read: &str) -> String {
    let line = 1 + read.matches('\n').count();
    let last = read.rsplit_once('\n').map_or(read, |(_, after)| after);
    let column = 1 + last.chars().count();
    format!(
        "Event pattern is not valid. Reason: {reason}\n at [Source: (String)\"{pattern}\"; \
         line: {line}, column: {column}]"
    )
}

/// A pattern whose only term is a `numeric` comparison written out as `terms`,
/// with the prefix of it the VALIDATOR has consumed once it has read that
/// argument list — which is where every refusal read out of the list is
/// reported.
fn numeric_pattern(terms: &str) -> (String, String) {
    let read = format!("{{\"detail\": {{\"n\": [{{\"numeric\": [{terms}]");
    (format!("{read}}}]}}}}"), read)
}

/// [`numeric_pattern`], with the prefix of it a PARSE that could not read the
/// list's last token has consumed: a parse stops just past the token it could
/// not read, before the list closes.
fn numeric_pattern_stopping_at_the_bound(terms: &str) -> (String, String) {
    let (pattern, read) = numeric_pattern(terms);
    let stopped = read
        .strip_suffix(']')
        .expect("the validator's prefix ends at the argument list's close")
        .to_string();
    (pattern, stopped)
}

/// A pattern whose only term is a `wildcard` operand written out as `operand`
/// (the text as the pattern spells it, escapes included), with the prefix of it
/// the reader has consumed once it has read that operand.
fn wildcard_pattern(operand: &str) -> (String, String) {
    let read = format!("{{\"detail\": {{\"s\": [{{\"wildcard\": \"{operand}\"");
    (format!("{read}}}]}}}}"), read)
}

/// `TestEventPattern` with this pattern and this event.
async fn test_pattern(
    router: &MultiServiceRouter,
    pattern: &str,
    event: &str,
) -> (u16, Value) {
    events(
        router,
        "TestEventPattern",
        &json!({ "EventPattern": pattern, "Event": event }),
    )
    .await
}

/// One numeric-bound row: the case, the bound as the pattern writes it, and
/// whether a double carries that numeral exactly.
struct BoundRow {
    case: &'static str,
    written: &'static str,
    comparable: bool,
}

/// The bounds a `numeric` comparison admits are exactly the numerals a binary64
/// double carries.
///
/// The rows span the class: an integer at the boundary of exact integer
/// representation and the next one past it (2^53 and 2^53 + 1, either sign), an
/// integer far past it, a fraction no finite binary expansion states exactly, an
/// exponent form, the largest finite double, and zero.
#[tokio::test]
async fn a_numeric_bound_outside_the_comparable_numbers_is_refused() {
    let router = emulator();
    let event = envelope(json!({ "n": 1 }));
    let rows: [BoundRow; 12] = [
        BoundRow {
            case: "the largest integer every double carries",
            written: "9007199254740992",
            comparable: true,
        },
        BoundRow {
            case: "the next integer up, which rounds to its neighbour",
            written: "9007199254740993",
            comparable: false,
        },
        BoundRow {
            case: "that integer negated",
            written: "-9007199254740993",
            comparable: false,
        },
        BoundRow {
            case: "the largest negative integer every double carries",
            written: "-9007199254740992",
            comparable: true,
        },
        BoundRow {
            case: "an integer of eighteen digits",
            written: "123456789012345678",
            comparable: false,
        },
        BoundRow {
            case: "the largest unsigned 64-bit integer",
            written: "18446744073709551615",
            comparable: false,
        },
        BoundRow {
            case: "a fraction no finite binary expansion states",
            written: "0.1",
            comparable: true,
        },
        BoundRow {
            case: "a fraction of fifteen significant digits",
            written: "-2.00000000000001",
            comparable: true,
        },
        BoundRow {
            case: "a fraction of eighteen significant digits",
            written: "2.00000000000000001",
            comparable: false,
        },
        BoundRow {
            case: "a numeral written with an exponent",
            written: "1e30",
            comparable: true,
        },
        BoundRow {
            case: "the largest finite double",
            written: "1.7976931348623157e308",
            comparable: true,
        },
        BoundRow {
            case: "zero",
            written: "0",
            comparable: true,
        },
    ];

    for row in rows {
        let BoundRow {
            case,
            written,
            comparable,
        } = row;
        let (pattern, read) = numeric_pattern(&format!("\"=\", {written}"));
        let (status, body) = test_pattern(&router, &pattern, &event).await;
        if comparable {
            assert_eq!(
                status, 200,
                "case {case:?}: a bound a double carries must be admitted, got {status} {body}"
            );
            continue;
        }
        assert_eq!(
            status, 400,
            "case {case:?}: a bound no double carries must be refused, got {status} {body}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidEventPatternException",
            "case {case:?}: expected InvalidEventPatternException, got {body}"
        );
        let expected = expected_refusal(&pattern, &format!("{CANNOT_COMPARE}{written}"), &read);
        let actual = error_message(&body);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {expected:?}, got {actual:?}"
        );
    }
}

/// A bound no double carries is refused wherever it sits in the argument list,
/// and the position reported is the one just past the list the reader had read.
#[tokio::test]
async fn a_numeric_bound_outside_the_comparable_numbers_is_refused_in_any_pair() {
    let router = emulator();
    let event = envelope(json!({ "n": 1 }));
    // (case, the argument list as written, the bound the refusal names)
    let rows: [(&str, &str, &str); 3] = [
        (
            "the bound of the only pair",
            "\">\", 9007199254740993",
            "9007199254740993",
        ),
        (
            "the bound of the second pair",
            "\">\", 1, \"<\", 18446744073709551615",
            "18446744073709551615",
        ),
        (
            "the bound of the first of two pairs",
            "\">\", 123456789012345678, \"<\", 5",
            "123456789012345678",
        ),
    ];

    for (case, terms, bound) in rows {
        let (pattern, read) = numeric_pattern(terms);
        let (status, body) = test_pattern(&router, &pattern, &event).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the pattern to be refused, got {status} {body}"
        );
        let expected = expected_refusal(&pattern, &format!("{CANNOT_COMPARE}{bound}"), &read);
        let actual = error_message(&body);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {expected:?}, got {actual:?}"
        );
    }
}

/// One validator reads every pattern this service is handed, so `PutRule` refuses
/// the bound `TestEventPattern` refuses, with the same message — and the refusal
/// mints no rule, which is what a later `DescribeRule` under that name reports.
#[tokio::test]
async fn a_rule_is_never_minted_for_a_pattern_the_checker_refuses() {
    let router = emulator();
    let event = envelope(json!({ "n": 1 }));
    let (pattern, read) = numeric_pattern("\"=\", 9007199254740993");
    let expected = expected_refusal(
        &pattern,
        &format!("{CANNOT_COMPARE}9007199254740993"),
        &read,
    );

    let (status, body) = test_pattern(&router, &pattern, &event).await;
    assert_eq!(
        status, 400,
        "TestEventPattern must refuse the bound, got {status} {body}"
    );
    let actual = error_message(&body);
    assert_eq!(
        actual, expected,
        "TestEventPattern: expected {expected:?}, got {actual:?}"
    );

    let name = "unrepresentable-bound";
    let (status, body) = events(
        &router,
        "PutRule",
        &json!({ "Name": name, "EventPattern": pattern }),
    )
    .await;
    assert_eq!(
        status, 400,
        "PutRule must refuse the same bound, got {status} {body}"
    );
    assert_eq!(
        error_code(&body),
        "InvalidEventPatternException",
        "PutRule: expected InvalidEventPatternException, got {body}"
    );
    let actual = error_message(&body);
    assert_eq!(
        actual, expected,
        "PutRule: expected {expected:?}, got {actual:?}"
    );
    assert!(
        body.get("RuleArn").is_none(),
        "PutRule must mint no rule for a refused pattern, got {body}"
    );

    let (status, body) = events(&router, "DescribeRule", &json!({ "Name": name })).await;
    assert_eq!(
        status, 400,
        "DescribeRule must report the rule as missing, got {status} {body}"
    );
    assert_eq!(
        error_code(&body),
        "ResourceNotFoundException",
        "DescribeRule: expected ResourceNotFoundException, got {body}"
    );
    let actual = error_message(&body);
    let expected = format!("Rule {name} does not exist on EventBus default.");
    assert_eq!(
        actual, expected,
        "DescribeRule: expected {expected:?}, got {actual:?}"
    );
}

/// The words JSON cannot spell are refused as the class of their own they are, at
/// the position just PAST the token, while an ordinary unquoted word keeps its
/// own sentence and the position it starts at.
///
/// The rows differ along which word was written, whether it carries a sign, where
/// in the pattern it sits, and whether it is one of these words at all.
#[tokio::test]
async fn a_word_json_cannot_spell_is_refused_as_a_non_standard_token() {
    let router = emulator();
    let event = envelope(json!({ "n": 1 }));
    // (case, the pattern as submitted, the reason, the text read before it)
    let feature = "JsonReadFeature.ALLOW_NON_NUMERIC_NUMBERS";
    let rows: [(&str, String, String, String); 6] = [
        (
            "an infinity as a numeric bound",
            numeric_pattern_stopping_at_the_bound("\"=\", Infinity").0,
            format!("Non-standard token 'Infinity': enable `{feature}` to allow"),
            numeric_pattern_stopping_at_the_bound("\"=\", Infinity").1,
        ),
        (
            "a negative infinity as a numeric bound",
            numeric_pattern_stopping_at_the_bound("\"=\", -Infinity").0,
            format!("Non-standard token '-Infinity': enable `{feature}` to allow"),
            numeric_pattern_stopping_at_the_bound("\"=\", -Infinity").1,
        ),
        (
            "a not-a-number as a numeric bound",
            numeric_pattern_stopping_at_the_bound("\"<\", NaN").0,
            format!("Non-standard token 'NaN': enable `{feature}` to allow"),
            numeric_pattern_stopping_at_the_bound("\"<\", NaN").1,
        ),
        (
            "an infinity as a literal term",
            r#"{"detail": {"n": [Infinity]}}"#.to_string(),
            format!("Non-standard token 'Infinity': enable `{feature}` to allow"),
            r#"{"detail": {"n": [Infinity"#.to_string(),
        ),
        (
            "the short spelling of an infinity",
            numeric_pattern_stopping_at_the_bound("\"=\", INF").0,
            format!("Non-standard token 'INF': enable `{feature}` to allow"),
            numeric_pattern_stopping_at_the_bound("\"=\", INF").1,
        ),
        // An unquoted word that is not one of them is the parser's own
        // unrecognized token, named WHOLE at the position it starts at.
        (
            "an unquoted word that is not one of them",
            r#"{"detail": {"n": [nul]}}"#.to_string(),
            "Unrecognized token 'nul': was expecting (JSON String, Number, Array, Object or \
             token 'null', 'true' or 'false')"
                .to_string(),
            r#"{"detail": {"n": ["#.to_string(),
        ),
    ];

    for (case, pattern, reason, read) in rows {
        assert!(
            pattern.starts_with(&read),
            "case {case:?}: the row's read prefix {read:?} must be a prefix of {pattern:?}"
        );
        let (status, body) = test_pattern(&router, &pattern, &event).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the pattern to be refused, got {status} {body}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidEventPatternException",
            "case {case:?}: expected InvalidEventPatternException, got {body}"
        );
        let expected = expected_refusal(&pattern, &reason, &read);
        let actual = error_message(&body);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {expected:?}, got {actual:?}"
        );
    }
}

/// One wildcard-operand row: the case, the operand as the pattern spells it
/// (escapes included), and the reason the language refuses it for — `None` for an
/// operand it admits.
struct OperandRow {
    case: &'static str,
    operand: &'static str,
    reason: Option<&'static str>,
}

/// A `wildcard` operand may escape the two characters its grammar defines and no
/// others, and an escaped wildcard is literal text rather than one of a run.
///
/// The rows span the class: an escaped wildcard between two wildcards, beside
/// one, on its own and at either end of the operand; an escaped backslash; an
/// escape of a character the grammar does not define, at the end of the operand
/// as well as inside it; and two wildcards in a row, which no escape makes legal.
#[tokio::test]
async fn a_wildcard_operand_admits_the_escapes_its_grammar_defines_and_no_others() {
    let router = emulator();
    let event = envelope(json!({ "s": "a*b" }));
    let rows: [OperandRow; 12] = [
        OperandRow {
            case: "an escaped wildcard between two wildcards",
            operand: r"*\\**",
            reason: None,
        },
        OperandRow {
            case: "an escaped wildcard on its own",
            operand: r"\\*",
            reason: None,
        },
        OperandRow {
            case: "an escaped wildcard after a literal run",
            operand: r"a\\*b",
            reason: None,
        },
        OperandRow {
            case: "two escaped wildcards in a row",
            operand: r"\\*\\*",
            reason: None,
        },
        OperandRow {
            case: "an escaped backslash between two wildcards",
            operand: r"*\\\\*",
            reason: None,
        },
        OperandRow {
            case: "an escaped backslash before a wildcard",
            operand: r"\\\\*",
            reason: None,
        },
        OperandRow {
            case: "a wildcard with no escape at all",
            operand: "a*b",
            reason: None,
        },
        OperandRow {
            case: "an escape of a letter",
            operand: r"a\\b",
            reason: Some("Invalid escape character at pos 2"),
        },
        OperandRow {
            case: "an escape at the end of the operand",
            operand: r"ab\\",
            reason: Some("Invalid escape character at pos 3"),
        },
        OperandRow {
            case: "an escape of a letter after a wildcard",
            operand: r"*\\n",
            reason: Some("Invalid escape character at pos 2"),
        },
        OperandRow {
            case: "two wildcards in a row",
            operand: "a**b",
            reason: Some("Consecutive wildcard characters at pos 2"),
        },
        OperandRow {
            case: "two wildcards in a row after an escaped one",
            operand: r"\\***",
            reason: Some("Consecutive wildcard characters at pos 3"),
        },
    ];

    for row in rows {
        let OperandRow {
            case,
            operand,
            reason,
        } = row;
        let (pattern, read) = wildcard_pattern(operand);
        let (status, body) = test_pattern(&router, &pattern, &event).await;
        match reason {
            None => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: the operand must be admitted, got {status} {body}"
                );
                assert!(
                    body.get("Result").is_some(),
                    "case {case:?}: an admitted operand must answer a Result, got {body}"
                );
            }
            Some(reason) => {
                assert_eq!(
                    status, 400,
                    "case {case:?}: the operand must be refused, got {status} {body}"
                );
                assert_eq!(
                    error_code(&body),
                    "InvalidEventPatternException",
                    "case {case:?}: expected InvalidEventPatternException, got {body}"
                );
                let expected = expected_refusal(&pattern, reason, &read);
                let actual = error_message(&body);
                assert_eq!(
                    actual, expected,
                    "case {case:?}: expected {expected:?}, got {actual:?}"
                );
            }
        }
    }
}

/// One match row: the case, the `wildcard` operand as the pattern spells it, the
/// string the event carries, and whether the operand matches it.
struct MatchRow {
    case: &'static str,
    operand: &'static str,
    value: &'static str,
    matched: bool,
}

/// An escaped metacharacter compares LITERALLY: it matches that character and
/// nothing else, so the text a wildcard reading of it would admit is withheld.
///
/// The rows span the class: the literal asterisk at the start, the middle and the
/// end of the value; a literal backslash; an escape beside unescaped wildcards;
/// and an operand with no escape, whose wildcards still absorb any run.
#[tokio::test]
async fn an_escaped_wildcard_matches_only_the_character_it_escapes() {
    let router = emulator();
    let rows: [MatchRow; 14] = [
        MatchRow {
            case: "an escaped wildcard between wildcards against a literal asterisk",
            operand: r"*\\**",
            value: "a*b",
            matched: true,
        },
        MatchRow {
            case: "an escaped wildcard between wildcards against text without one",
            operand: r"*\\**",
            value: "ab",
            matched: false,
        },
        MatchRow {
            case: "an escaped wildcard on its own against the asterisk alone",
            operand: r"\\*",
            value: "*",
            matched: true,
        },
        MatchRow {
            case: "an escaped wildcard on its own against other text",
            operand: r"\\*",
            value: "x",
            matched: false,
        },
        MatchRow {
            case: "an escaped wildcard on its own against text that merely ends with one",
            operand: r"\\*",
            value: "a*",
            matched: false,
        },
        MatchRow {
            case: "an escaped wildcard inside a literal run against the literal",
            operand: r"a\\*b",
            value: "a*b",
            matched: true,
        },
        MatchRow {
            case: "an escaped wildcard inside a literal run against what a wildcard admits",
            operand: r"a\\*b",
            value: "axb",
            matched: false,
        },
        MatchRow {
            case: "an escaped backslash between wildcards against a literal backslash",
            operand: r"*\\\\*",
            value: r"x\y",
            matched: true,
        },
        MatchRow {
            case: "an escaped backslash between wildcards against text without one",
            operand: r"*\\\\*",
            value: "xy",
            matched: false,
        },
        MatchRow {
            case: "an escaped wildcard at the end against text ending with one",
            operand: r"a*\\*",
            value: "abc*",
            matched: true,
        },
        MatchRow {
            case: "an escaped wildcard at the end against text ending otherwise",
            operand: r"a*\\*",
            value: "abcd",
            matched: false,
        },
        // An operand with no escape keeps the wildcard's own meaning.
        MatchRow {
            case: "an unescaped wildcard against a run it absorbs",
            operand: "a*b",
            value: "axxb",
            matched: true,
        },
        MatchRow {
            case: "an unescaped wildcard against the empty run",
            operand: "a*b",
            value: "ab",
            matched: true,
        },
        MatchRow {
            case: "an unescaped wildcard against text the literals do not frame",
            operand: "a*b",
            value: "axxc",
            matched: false,
        },
    ];

    for row in rows {
        let MatchRow {
            case,
            operand,
            value,
            matched,
        } = row;
        let (pattern, _) = wildcard_pattern(operand);
        let event = envelope(json!({ "s": value }));
        let (status, body) = test_pattern(&router, &pattern, &event).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the pattern to be admitted, got {status} {body}"
        );
        let actual = body.get("Result").and_then(Value::as_bool);
        assert_eq!(
            actual,
            Some(matched),
            "case {case:?}: operand {operand:?} against {value:?}: expected Result \
             {matched}, got {actual:?}"
        );
    }
}

/// One event-number row: the case, the comparison the pattern writes, the number
/// the event carries as its own text, and whether the comparison holds.
struct EventNumberRow {
    case: &'static str,
    terms: &'static str,
    carried: &'static str,
    matched: bool,
}

/// A `numeric` comparison considers only the numbers a double carries, so an
/// event whose value is a numeral no double carries matches no `numeric` term —
/// even one the exact integer would satisfy.
///
/// The rows span the class: the integer boundary of exact representation and the
/// next one past it, either sign, each comparison kind, and a value that is not a
/// number at all.
#[tokio::test]
async fn a_numeric_term_does_not_consider_an_event_number_no_double_carries() {
    let router = emulator();
    let rows: [EventNumberRow; 9] = [
        EventNumberRow {
            case: "equality against the largest integer every double carries",
            terms: "\"=\", 9007199254740992",
            carried: "9007199254740992",
            matched: true,
        },
        EventNumberRow {
            case: "equality against the integer one past it",
            terms: "\"=\", 9007199254740992",
            carried: "9007199254740993",
            matched: false,
        },
        EventNumberRow {
            case: "a lower bound the exact integer satisfies",
            terms: "\">\", 9007199254740992",
            carried: "9007199254740993",
            matched: false,
        },
        EventNumberRow {
            case: "an upper bound the exact negative integer satisfies",
            terms: "\"<\", -9007199254740992",
            carried: "-9007199254740993",
            matched: false,
        },
        EventNumberRow {
            case: "an upper bound against an integer of eighteen digits",
            terms: "\"<\", 200000000000000000",
            carried: "123456789012345678",
            matched: false,
        },
        EventNumberRow {
            case: "an upper bound against a number a double carries",
            terms: "\"<\", 200000000000000000",
            carried: "123456789012345",
            matched: true,
        },
        EventNumberRow {
            case: "a range a fraction falls inside",
            terms: "\">=\", 1, \"<\", 3",
            carried: "2.5",
            matched: true,
        },
        EventNumberRow {
            case: "a range a fraction falls outside",
            terms: "\">=\", 1, \"<\", 3",
            carried: "3.5",
            matched: false,
        },
        EventNumberRow {
            case: "a value that is the text of a number",
            terms: "\"=\", 1",
            carried: "\"1\"",
            matched: false,
        },
    ];

    for row in rows {
        let EventNumberRow {
            case,
            terms,
            carried,
            matched,
        } = row;
        let (pattern, _) = numeric_pattern(terms);
        // The event's number is written as its own TEXT, so the digits the event
        // carries are the row's own and not a rendering of them.
        let event = envelope(
            serde_json::from_str::<Value>(&format!("{{\"n\": {carried}}}"))
                .expect("the row's event detail is JSON"),
        );
        let (status, body) = test_pattern(&router, &pattern, &event).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the pattern to be admitted, got {status} {body}"
        );
        let actual = body.get("Result").and_then(Value::as_bool);
        assert_eq!(
            actual,
            Some(matched),
            "case {case:?}: {terms} against {carried}: expected Result {matched}, got \
             {actual:?}"
        );
    }
}

/// One malformed-operand row: the case, the pattern as submitted, the reason
/// the refusal states, and the prefix of the pattern the reader had consumed
/// when it refused.
struct ListMemberRow {
    case: &'static str,
    pattern: &'static str,
    reason: &'static str,
    read: &'static str,
}

/// A refusal about ONE member of a list a match type was handed states its
/// location TWICE: the reader that found the fault states it, and so does the
/// reader that handed the whole list over — the same position, since neither
/// reader moved.
///
/// The rows differ along which nested match type was handed the list, which
/// kind of value the offending member is, and where in the list it sits.
#[tokio::test]
async fn a_refusal_about_one_member_of_a_list_states_its_location_for_each_reader() {
    let router = emulator();
    let event = envelope(json!({ "s": "retail", "n": 1 }));
    let rows: [ListMemberRow; 5] = [
        ListMemberRow {
            case: "a number in the list a nested prefix was handed",
            pattern: r#"{"detail": {"s": [{"anything-but": {"prefix": ["a", 1]}}]}}"#,
            reason: "prefix/suffix match pattern must be a string",
            read: r#"{"detail": {"s": [{"anything-but": {"prefix": ["a", 1"#,
        },
        ListMemberRow {
            case: "a boolean in the list a nested suffix was handed",
            pattern: r#"{"detail": {"s": [{"anything-but": {"suffix": [true, "a"]}}]}}"#,
            reason: "prefix/suffix match pattern must be a string",
            read: r#"{"detail": {"s": [{"anything-but": {"suffix": [true"#,
        },
        ListMemberRow {
            case: "a number in the list a nested wildcard was handed",
            pattern: r#"{"detail": {"s": [{"anything-but": {"wildcard": ["a*", 5]}}]}}"#,
            reason: "wildcard match pattern must be a string",
            read: r#"{"detail": {"s": [{"anything-but": {"wildcard": ["a*", 5"#,
        },
        ListMemberRow {
            case: "null in the list a nested case-folding match type was handed",
            pattern: r#"{"detail": {"s": [{"anything-but": {"equals-ignore-case": [null]}}]}}"#,
            reason: "Inside anything-but/equals-ignore-case list, number|start|null|boolean is \
                     not supported.",
            read: r#"{"detail": {"s": [{"anything-but": {"equals-ignore-case": [null"#,
        },
        ListMemberRow {
            case: "a number in the list a nested case-folding match type was handed",
            pattern: r#"{"detail": {"s": [{"anything-but": {"equals-ignore-case": ["a", 1]}}]}}"#,
            reason: "Inside anything-but/equals-ignore-case list, number|start|null|boolean is \
                     not supported.",
            read: r#"{"detail": {"s": [{"anything-but": {"equals-ignore-case": ["a", 1"#,
        },
    ];

    for row in rows {
        let ListMemberRow {
            case,
            pattern,
            reason,
            read,
        } = row;
        assert!(
            pattern.starts_with(read),
            "case {case:?}: the row's read prefix {read:?} must be a prefix of {pattern:?}"
        );
        let (status, body) = test_pattern(&router, pattern, &event).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the pattern to be refused, got {status} {body}"
        );
        let once = expected_refusal(pattern, reason, read);
        let location = once
            .split_once("\n at [Source:")
            .map(|(_, location)| format!("\n at [Source:{location}"))
            .unwrap_or_default();
        let expected = format!("{once}{location}");
        let actual = error_message(&body);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {expected:?}, got {actual:?}"
        );
    }
}

/// One `cidr` row: the case, the operand as the pattern spells it, the address
/// the event carries, and whether the block the operand names contains it.
struct CidrRow {
    case: &'static str,
    operand: &'static str,
    address: &'static str,
    inside: bool,
}

/// A `cidr` operand names the BLOCK its prefix length cuts out, so an address
/// carrying bits past that prefix names a host inside that block and the
/// operand stands for the block: it admits every address in it and no other,
/// exactly as the same block written from its network address does.
///
/// The rows differ along which address of the block was written (its network
/// address, a host inside it), the prefix length, the address family, and
/// whether the event's address falls inside the block or just outside it.
#[tokio::test]
async fn a_cidr_operand_stands_for_the_block_its_prefix_cuts_out() {
    let router = emulator();
    let rows: [CidrRow; 10] = [
        CidrRow {
            case: "a host address inside the block it was written from",
            operand: "198.51.100.130/25",
            address: "198.51.100.130",
            inside: true,
        },
        CidrRow {
            case: "another address of that same block",
            operand: "198.51.100.130/25",
            address: "198.51.100.129",
            inside: true,
        },
        CidrRow {
            case: "the network address of that same block",
            operand: "198.51.100.130/25",
            address: "198.51.100.128",
            inside: true,
        },
        CidrRow {
            case: "an address one below that block",
            operand: "198.51.100.130/25",
            address: "198.51.100.127",
            inside: false,
        },
        CidrRow {
            case: "the same block written from its network address",
            operand: "198.51.100.128/25",
            address: "198.51.100.129",
            inside: true,
        },
        CidrRow {
            case: "a host address with a shorter prefix",
            operand: "10.0.1.7/24",
            address: "10.0.1.9",
            inside: true,
        },
        CidrRow {
            case: "an address outside that shorter prefix",
            operand: "10.0.1.7/24",
            address: "10.0.2.9",
            inside: false,
        },
        CidrRow {
            case: "a block written canonically",
            operand: "10.0.0.0/8",
            address: "10.1.2.3",
            inside: true,
        },
        CidrRow {
            case: "an IPv6 host address inside its block",
            operand: "2001:db8::5/64",
            address: "2001:db8::9",
            inside: true,
        },
        CidrRow {
            case: "an IPv6 address outside that block",
            operand: "2001:db8::5/64",
            address: "2001:db9::9",
            inside: false,
        },
    ];

    for row in rows {
        let CidrRow {
            case,
            operand,
            address,
            inside,
        } = row;
        let pattern = format!("{{\"detail\": {{\"ip\": [{{\"cidr\": \"{operand}\"}}]}}}}");
        let event = envelope(json!({ "ip": address }));
        let (status, body) = test_pattern(&router, &pattern, &event).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the operand must be admitted, got {status} {body}"
        );
        let actual = body.get("Result").and_then(Value::as_bool);
        assert_eq!(
            actual,
            Some(inside),
            "case {case:?}: {operand} against {address}: expected Result {inside}, got \
             {actual:?}"
        );
    }
}

/// An operand that is not TEXT at all is refused by the plain string-operand
/// requirement, which the service states for the first match type that
/// compares text — no block, and no other reading of the operand, is attempted
/// on a value that is not a string.
#[tokio::test]
async fn a_cidr_operand_that_is_not_text_is_refused_as_a_string_operand() {
    let router = emulator();
    let event = envelope(json!({ "ip": "10.0.1.7" }));
    // (case, the operand as the pattern writes it)
    let rows: [(&str, &str); 3] = [
        ("a number", "5"),
        ("a boolean", "true"),
        ("a list", r#"["10.0.0.0/8"]"#),
    ];

    for (case, operand) in rows {
        let read = format!("{{\"detail\": {{\"ip\": [{{\"cidr\": {operand}");
        // A value read by its opening token alone stops at that token: a list
        // is entered, and every scalar is one whole token.
        let read = match operand.starts_with('[') {
            true => "{\"detail\": {\"ip\": [{\"cidr\": [".to_string(),
            false => read,
        };
        let pattern = format!("{{\"detail\": {{\"ip\": [{{\"cidr\": {operand}}}]}}}}");
        let (status, body) = test_pattern(&router, &pattern, &event).await;
        assert_eq!(
            status, 400,
            "case {case:?}: the operand must be refused, got {status} {body}"
        );
        let expected = expected_refusal(&pattern, "prefix match pattern must be a string", &read);
        let actual = error_message(&body);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {expected:?}, got {actual:?}"
        );
    }
}

/// A rule of the CIDR NOTATION is read by the address parser out of the
/// block's own text, so the whole message is that parser's reason and carries
/// no source location at all.
///
/// The rows differ along which rule of the notation the text breaks: the one
/// mask separator it needs, and the mask leaving a range of addresses to
/// compare against, in each address family.
#[tokio::test]
async fn a_cidr_operand_the_notation_refuses_states_the_reason_alone() {
    let router = emulator();
    let event = envelope(json!({ "ip": "10.0.1.7" }));
    // (case, the operand, the reason the notation states)
    let rows: [(&str, &str, &str); 5] = [
        ("text carrying no mask separator", "abc", "Malformed CIDR, one '/' required"),
        (
            "an address with no mask at all",
            "10.0.0.0",
            "Malformed CIDR, one '/' required",
        ),
        (
            "text carrying two mask separators",
            "10.0.0.0/8/16",
            "Malformed CIDR, one '/' required",
        ),
        (
            "a mask as wide as the IPv4 family",
            "10.0.1.7/32",
            "IPv4 mask bits must be < 32",
        ),
        (
            "a mask as wide as the IPv6 family",
            "2001:db8::1/128",
            "IPv6 mask bits must be < 128",
        ),
    ];

    for (case, operand, reason) in rows {
        let pattern = format!("{{\"detail\": {{\"ip\": [{{\"cidr\": \"{operand}\"}}]}}}}");
        let (status, body) = test_pattern(&router, &pattern, &event).await;
        assert_eq!(
            status, 400,
            "case {case:?}: the operand must be refused, got {status} {body}"
        );
        let expected = format!("Event pattern is not valid. Reason: {reason}");
        let actual = error_message(&body);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {expected:?}, got {actual:?}"
        );
    }
}

/// A pattern the language admits is STORED as submitted: the rule is created and
/// read back with the pattern byte for byte, on the bus the request named by
/// omission.
#[tokio::test]
async fn a_rule_carrying_an_escaped_wildcard_is_created_and_read_back() {
    let router = emulator();
    let (pattern, _) = wildcard_pattern(r"*\\**");
    let name = "escaped-star-rule";

    let (status, body) = events(
        &router,
        "PutRule",
        &json!({ "Name": name, "EventPattern": pattern }),
    )
    .await;
    assert_eq!(
        status, 200,
        "PutRule must store the pattern, got {status} {body}"
    );
    let account = engine::emulator_account_id();
    let expected_arn = format!("arn:aws:events:{REGION}:{account}:rule/{name}");
    let actual = body.get("RuleArn").and_then(Value::as_str);
    assert_eq!(
        actual,
        Some(expected_arn.as_str()),
        "PutRule: expected RuleArn {expected_arn:?}, got {actual:?}"
    );

    let (status, body) = events(&router, "DescribeRule", &json!({ "Name": name })).await;
    assert_eq!(
        status, 200,
        "DescribeRule must report the rule, got {status} {body}"
    );
    for (member, expected) in [
        ("Name", name),
        ("Arn", expected_arn.as_str()),
        ("EventPattern", pattern.as_str()),
        ("EventBusName", "default"),
        ("State", "ENABLED"),
        ("CreatedBy", account),
    ] {
        let actual = body.get(member).and_then(Value::as_str);
        assert_eq!(
            actual,
            Some(expected),
            "DescribeRule: expected {member} {expected:?}, got {actual:?}"
        );
    }

    // The stored pattern is the one that was submitted, so it still matches
    // exactly what the checker said it would.
    for (value, matched) in [("a*b", true), ("ab", false)] {
        let event = envelope(json!({ "s": value }));
        let (status, body) = test_pattern(&router, &pattern, &event).await;
        assert_eq!(
            status, 200,
            "the stored pattern must still be testable, got {status} {body}"
        );
        let actual = body.get("Result").and_then(Value::as_bool);
        assert_eq!(
            actual,
            Some(matched),
            "the stored pattern against {value:?}: expected Result {matched}, got {actual:?}"
        );
    }
}
