// SPDX-License-Identifier: CC0-1.0

//! The TEMPLATES three refusals of this service are worded from, driven end to end
//! over the wire (`awsJson1_1`, `POST /` with `X-Amz-Target: AWSEvents.<Operation>`)
//! through the real `MultiServiceRouter`.
//!
//! The rules asserted here, each over a table spanning its class:
//!
//! 1. A constraint clause echoes the rejected value VERBATIM and in full — the echo
//!    is the only place a refusal reports the offending input back, so the message
//!    carries the caller's own bytes, whatever their length, and never a prefix of
//!    them annotated with a byte count. The rows differ along the member, its
//!    declared maximum (256, 512, 1600, 4096), how far past it the value reaches,
//!    and whether its characters are one byte or two, with the value AT the maximum
//!    as the accepted control.
//! 2. An event pattern past the service's pattern quota is refused with the sentence
//!    naming the PARAMETER, the RULE it was given for and the QUOTA — and the
//!    measurement is of the text as sent, whitespace counted, so a pattern whose
//!    compact form would fit is refused too. The rows differ along the rule, along
//!    how the quota is crossed (compact text, space padding, newline padding, far
//!    past) and along a pattern past the MODELLED maximum, which is a different
//!    refusal; the quota exactly is the accepted control, and the rule keeps the
//!    pattern it had.
//! 3. A tag key in the namespace reserved for Amazon Web Services' own tags is
//!    refused with one FIXED sentence, on every call of this service that writes or
//!    removes tags, and the resource's tags are left as they were. The rows differ
//!    along the call (a tag write, a tag removal, a bus create, a rule write), along
//!    the spelling of the prefix and along where the reserved key sits in the
//!    request, with keys that merely resemble the prefix as the accepted controls.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_eventbridge_awsevents::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region every request below is signed for.
const REGION: &str = "eu-west-2";

/// `EventBusName`'s modelled `@length` maximum, from
/// `smithy_query(query="shape", shape="EventBusName")`. Written out here rather
/// than imported, so a wrong bound in the implementation cannot make a row pass.
const EVENT_BUS_NAME_MAX: usize = 256;

/// `EventBusNameOrArn`'s modelled `@length` maximum (`PutRule$EventBusName`), from
/// `smithy_query(query="shape", shape="EventBusNameOrArn")`.
const EVENT_BUS_NAME_OR_ARN_MAX: usize = 1600;

/// `RuleDescription`'s modelled `@length` maximum, from
/// `smithy_query(query="shape", shape="RuleDescription")`.
const RULE_DESCRIPTION_MAX: usize = 512;

/// `EventPattern`'s modelled `@length` maximum, from
/// `smithy_query(query="shape", shape="EventPattern")`. It is NOT the service's
/// pattern quota ([`PATTERN_QUOTA`]), which is lower and charged where a rule is
/// written.
const EVENT_PATTERN_MAX: usize = 4096;

/// The service's event-pattern quota for a RULE, in characters, as EventBridge
/// publishes it.
const PATTERN_QUOTA: usize = 2048;

/// The one sentence a request naming a tag key in the reserved namespace is refused
/// with.
const SYSTEM_TAGS: &str = "Add/Remove system tags is not allowed.";

/// The emulator: one router carrying only this service over one shared engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(engine));
    router
}

/// One `awsJson1_1` call to EventBridge over the router.
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

/// The wire error code (`__type`), without its namespace; the empty string on a
/// success.
fn error_code(body: &Value) -> &str {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .rsplit('#')
        .next()
        .unwrap_or_default()
}

/// The `message` an error body carries.
fn message(body: &Value) -> &str {
    body.get("message")
        .or_else(|| body.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
}

/// This emulator's account id — the account segment of every ARN it mints.
fn account() -> &'static str {
    engine::emulator_account_id()
}

/// `arn:aws:events:<region>:<account>:event-bus/<name>`, built here rather than
/// read from the implementation.
fn bus_arn(name: &str) -> String {
    format!("arn:aws:events:{REGION}:{}:event-bus/{name}", account())
}

/// The whole `ValidationException` message for ONE violated constraint.
fn one_clause(value: &str, path: &str, requirement: &str) -> String {
    format!(
        "1 validation error detected: Value '{value}' at '{path}' failed to satisfy constraint: \
         {requirement}"
    )
}

/// The requirement a value past a `@length` maximum failed.
fn at_most(max: usize) -> String {
    format!("Member must have length less than or equal to {max}")
}

/// An event pattern of exactly `characters` characters: a `source` filter padded out
/// with `pad` inside its own JSON, so the padding counts toward the length as sent
/// while the pattern stays one the compiler accepts and whose compact form is short.
fn padded_pattern(characters: usize, pad: char) -> String {
    let skeleton = json!({ "source": ["com.demo.widgets"] }).to_string();
    let padding = characters.saturating_sub(skeleton.len());
    format!(
        "{}{}{}",
        &skeleton[..skeleton.len() - 1],
        pad.to_string().repeat(padding),
        "}"
    )
}

/// Create the bus `name`, which the rows below tag and retag.
async fn given_bus(router: &MultiServiceRouter, name: &str) {
    let (status, body) = events(router, "CreateEventBus", &json!({ "Name": name })).await;
    assert_eq!(
        status, 200,
        "CreateEventBus {name} answered {status} {body}"
    );
}

/// Store the rule `name` on the default bus with the pattern `pattern`.
async fn given_rule(router: &MultiServiceRouter, name: &str, pattern: &str) {
    let (status, body) = events(
        router,
        "PutRule",
        &json!({ "Name": name, "EventPattern": pattern }),
    )
    .await;
    assert_eq!(status, 200, "PutRule {name} answered {status} {body}");
}

/// The `EventPattern` `DescribeRule` reports for the rule `name`.
async fn described_pattern(router: &MultiServiceRouter, name: &str) -> String {
    let (status, body) = events(router, "DescribeRule", &json!({ "Name": name })).await;
    assert_eq!(status, 200, "DescribeRule {name} answered {status} {body}");
    body["EventPattern"].as_str().unwrap_or_default().to_string()
}

/// The tag keys `ListTagsForResource` reports for `arn`, sorted.
async fn listed_tag_keys(router: &MultiServiceRouter, arn: &str) -> Vec<String> {
    let (status, body) = events(router, "ListTagsForResource", &json!({ "ResourceARN": arn })).await;
    assert_eq!(
        status, 200,
        "ListTagsForResource {arn} answered {status} {body}"
    );
    let mut keys: Vec<String> = body["Tags"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|tag| tag["Key"].as_str().unwrap_or_default().to_string())
        .collect();
    keys.sort();
    keys
}

/// RULE: a constraint clause states the rejected value as the caller sent it — every
/// byte of it, with no prefix cut and no byte-count annotation in its place.
///
/// The rows span the class along the member and its declared maximum (a bus name's
/// 256, a rule description's 512, a bus-name-or-ARN's 1600, an event pattern's
/// 4096), along how far past that maximum the value reaches (one character, one
/// character of two bytes, hundreds, a thousand) and along the value AT the maximum,
/// which is accepted.
#[tokio::test]
async fn a_constraint_clause_echoes_the_submitted_value_in_full() {
    let router = emulator();
    let long_bus_name = "a".repeat(EVENT_BUS_NAME_MAX + 1);
    let longer_bus_name = "b".repeat(EVENT_BUS_NAME_MAX + 1024);
    let long_target_bus = "c".repeat(EVENT_BUS_NAME_OR_ARN_MAX + 1);
    let long_description = "d".repeat(RULE_DESCRIPTION_MAX + 1);
    // Two bytes per character, so a clause that echoed a byte-bounded prefix would
    // also cut a character in half.
    let wide_description = "é".repeat(RULE_DESCRIPTION_MAX + 1);
    let long_pattern = padded_pattern(EVENT_PATTERN_MAX + 1, ' ');
    // (case, operation, request, the whole message it is refused with — `None` for a
    //  value this service takes)
    let rows: Vec<(&str, &str, Value, Option<String>)> = vec![
        (
            "a bus name one character past its maximum",
            "CreateEventBus",
            json!({ "Name": long_bus_name }),
            Some(one_clause(
                &long_bus_name,
                "name",
                &at_most(EVENT_BUS_NAME_MAX),
            )),
        ),
        (
            "a bus name a thousand characters past its maximum",
            "CreateEventBus",
            json!({ "Name": longer_bus_name }),
            Some(one_clause(
                &longer_bus_name,
                "name",
                &at_most(EVENT_BUS_NAME_MAX),
            )),
        ),
        (
            "a rule's bus name past EventBusNameOrArn's maximum",
            "PutRule",
            json!({ "Name": "echo-rule", "EventBusName": long_target_bus }),
            Some(one_clause(
                &long_target_bus,
                "eventBusName",
                &at_most(EVENT_BUS_NAME_OR_ARN_MAX),
            )),
        ),
        (
            "a rule description one character past its maximum",
            "PutRule",
            json!({ "Name": "echo-rule", "Description": long_description }),
            Some(one_clause(
                &long_description,
                "description",
                &at_most(RULE_DESCRIPTION_MAX),
            )),
        ),
        (
            "a rule description of two-byte characters past its maximum",
            "PutRule",
            json!({ "Name": "echo-rule", "Description": wide_description }),
            Some(one_clause(
                &wide_description,
                "description",
                &at_most(RULE_DESCRIPTION_MAX),
            )),
        ),
        (
            "an event pattern past EventPattern's modelled maximum",
            "PutRule",
            json!({ "Name": "echo-rule", "EventPattern": long_pattern }),
            Some(one_clause(
                &long_pattern,
                "eventPattern",
                &at_most(EVENT_PATTERN_MAX),
            )),
        ),
        (
            "a bus name AT its maximum",
            "CreateEventBus",
            json!({ "Name": "e".repeat(EVENT_BUS_NAME_MAX) }),
            None,
        ),
        (
            "a rule description AT its maximum",
            "PutRule",
            json!({
                "Name": "echo-rule",
                "Description": "f".repeat(RULE_DESCRIPTION_MAX),
                "EventPattern": padded_pattern(64, ' '),
            }),
            None,
        ),
    ];

    for (case, operation, request, expected) in &rows {
        let (status, body) = events(&router, operation, request).await;
        let Some(expected) = expected else {
            assert_eq!(
                status, 200,
                "case {case:?}: expected {operation} to accept the value, got {status} {body}"
            );
            continue;
        };
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} {body}"
        );
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {body}"
        );
        let actual = message(&body);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {expected:?}, got {actual:?}"
        );
        assert!(
            !actual.contains(" bytes]"),
            "case {case:?}: the clause carries no byte-count annotation, got {actual:?}"
        );
    }
}

/// One row of the pattern-quota table: the case, the rule the pattern is given
/// for, the pattern text, and the error code and whole message it is refused with
/// — `None` for a pattern this service stores.
type QuotaRow = (
    &'static str,
    &'static str,
    String,
    Option<(&'static str, String)>,
);

/// RULE: an event pattern longer than the service's pattern quota is refused with the
/// sentence naming the parameter, the rule it was given for and the quota, counted
/// over the text as sent — and the rule keeps the pattern it had.
///
/// The rows span the class along the rule the pattern was given for, along how the
/// quota is crossed (the compact text itself, padding with blanks, padding with
/// newlines, far past it) and along a pattern past the MODELLED maximum, which is
/// the boundary's own constraint refusal rather than this quota; the pattern at the
/// quota exactly is accepted.
#[tokio::test]
async fn an_event_pattern_past_the_quota_names_the_parameter_the_rule_and_the_limit() {
    let router = emulator();
    let kept = json!({ "source": ["com.demo.kept"] }).to_string();
    for rule in ["quota-rule", "other.rule-2"] {
        given_rule(&router, rule, &kept).await;
    }
    // The refusal a rule's over-quota pattern answers: fixed text around the rule the
    // request named and the quota, written out here rather than imported.
    fn quota_refusal(rule: &str) -> String {
        format!("Parameter EventPattern for rule {rule} exceeds limit of {PATTERN_QUOTA}.")
    }
    // (case, the rule, the pattern, the code and message it is refused with — `None`
    //  for a pattern this service stores)
    let rows: Vec<QuotaRow> = vec![
        (
            "one character past the quota",
            "quota-rule",
            padded_pattern(PATTERN_QUOTA + 1, ' '),
            Some((
                "InvalidEventPatternException",
                quota_refusal("quota-rule"),
            )),
        ),
        (
            "newline padding takes it past the quota",
            "quota-rule",
            padded_pattern(PATTERN_QUOTA + 7, '\n'),
            Some((
                "InvalidEventPatternException",
                quota_refusal("quota-rule"),
            )),
        ),
        (
            "far past the quota, on another rule",
            "other.rule-2",
            padded_pattern(PATTERN_QUOTA + 900, ' '),
            Some((
                "InvalidEventPatternException",
                quota_refusal("other.rule-2"),
            )),
        ),
        (
            // Past the member's own declared maximum: refused at the boundary, as the
            // constraint violation it is, and never with the quota's sentence.
            "past EventPattern's modelled maximum",
            "quota-rule",
            padded_pattern(EVENT_PATTERN_MAX + 1, ' '),
            Some((
                "ValidationException",
                one_clause(
                    &padded_pattern(EVENT_PATTERN_MAX + 1, ' '),
                    "eventPattern",
                    &at_most(EVENT_PATTERN_MAX),
                ),
            )),
        ),
        (
            "the quota exactly",
            "quota-rule",
            padded_pattern(PATTERN_QUOTA, ' '),
            None,
        ),
    ];

    for (case, rule, pattern, refusal) in &rows {
        let (status, body) = events(
            &router,
            "PutRule",
            &json!({ "Name": rule, "EventPattern": pattern }),
        )
        .await;
        let Some((code, expected)) = refusal else {
            assert_eq!(
                status, 200,
                "case {case:?}: expected a pattern of {} characters to be stored, got {status} \
                 {body}",
                pattern.chars().count()
            );
            continue;
        };
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} {body}"
        );
        assert_eq!(
            error_code(&body),
            *code,
            "case {case:?}: expected {code}, got {body}"
        );
        let actual = message(&body);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {expected:?}, got {actual:?}"
        );
    }

    // A refused PutRule stored nothing: each rule still carries the pattern it was
    // created with, except the one row the quota admitted.
    let stored = described_pattern(&router, "quota-rule").await;
    assert_eq!(
        stored,
        padded_pattern(PATTERN_QUOTA, ' '),
        "the accepted row is the pattern quota-rule carries, got {stored:?}"
    );
    let untouched = described_pattern(&router, "other.rule-2").await;
    assert_eq!(
        untouched, kept,
        "a refused PutRule leaves the rule's stored pattern alone, got {untouched:?}"
    );
}

/// RULE: a request naming a tag key in the namespace reserved for Amazon Web
/// Services' own tags is refused with one fixed sentence, whichever call carried it,
/// and the resource keeps exactly the tags it had.
///
/// The rows span the class along the CALL (a tag write, a tag removal, a bus create,
/// a rule write), along the spelling of the prefix (lower, upper, mixed case, the
/// prefix alone) and along where the reserved key sits (alone, after a legal key,
/// before one), with keys that merely resemble the prefix as the accepted controls.
#[tokio::test]
async fn a_reserved_tag_key_is_refused_with_one_fixed_sentence() {
    let router = emulator();
    given_bus(&router, "tagged-bus").await;
    let arn = bus_arn("tagged-bus");
    let (status, body) = events(
        &router,
        "TagResource",
        &json!({ "ResourceARN": arn, "Tags": [{ "Key": "owner", "Value": "ada" }] }),
    )
    .await;
    assert_eq!(status, 200, "TagResource answered {status} {body}");

    // (case, the keys the request names, whether this service takes them)
    let rows = [
        ("the reserved prefix", vec!["aws:cost-centre"], false),
        ("the reserved prefix in upper case", vec!["AWS:Service"], false),
        (
            "the reserved prefix in mixed case",
            vec!["Aws:cloudformation:stack-name"],
            false,
        ),
        ("the reserved prefix alone", vec!["aws:"], false),
        (
            "a legal key and then a reserved one",
            vec!["team", "aws:created-by"],
            false,
        ),
        (
            "a reserved key and then a legal one",
            vec!["aws:created-by", "team"],
            false,
        ),
        ("the prefix without its colon", vec!["aws-owner"], true),
        ("a key merely starting with aws", vec!["awsome"], true),
        ("the prefix inside the key", vec!["mine:aws:owner"], true),
    ];

    for (index, (case, keys, accepted)) in rows.iter().enumerate() {
        let tags: Vec<Value> = keys
            .iter()
            .map(|key| json!({ "Key": key, "Value": "v" }))
            .collect();
        let expected_status = if *accepted { 200 } else { 400 };
        // (the call, the request it carries these keys in). The REMOVAL comes first,
        // so a control row's key is not added and then taken away again: a removal of
        // a key the bus does not carry removes nothing.
        let calls: [(&str, Value); 4] = [
            (
                "UntagResource",
                json!({ "ResourceARN": arn, "TagKeys": keys }),
            ),
            (
                "TagResource",
                json!({ "ResourceARN": arn, "Tags": tags }),
            ),
            (
                "CreateEventBus",
                json!({ "Name": format!("tagged-bus-{index}"), "Tags": tags }),
            ),
            (
                "PutRule",
                json!({
                    "Name": format!("tagged-rule-{index}"),
                    "EventPattern": padded_pattern(64, ' '),
                    "Tags": tags,
                }),
            ),
        ];
        for (operation, request) in &calls {
            let (status, body) = events(&router, operation, request).await;
            assert_eq!(
                status, expected_status,
                "case {case:?} through {operation}: expected HTTP {expected_status}, got {status} \
                 {body}"
            );
            if *accepted {
                continue;
            }
            assert_eq!(
                error_code(&body),
                "ValidationException",
                "case {case:?} through {operation}: expected ValidationException, got {body}"
            );
            let actual = message(&body);
            assert_eq!(
                actual, SYSTEM_TAGS,
                "case {case:?} through {operation}: expected {SYSTEM_TAGS:?}, got {actual:?}"
            );
        }
    }

    // Every refusal above was atomic: the bus carries the key it was tagged with and
    // the three the controls added, and nothing a refused request named — neither the
    // reserved keys nor the legal key that travelled with one, and the removals took
    // nothing away.
    let keys = listed_tag_keys(&router, &arn).await;
    assert_eq!(
        keys,
        vec![
            "aws-owner".to_string(),
            "awsome".to_string(),
            "mine:aws:owner".to_string(),
            "owner".to_string(),
        ],
        "a refused tag call leaves the resource's tags alone, got {keys:?}"
    );
}
