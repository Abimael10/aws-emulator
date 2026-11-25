// SPDX-License-Identifier: CC0-1.0

//! What an EMPTY operand of a match type that compares TEXT admits, observed where a
//! client observes it: on the Amazon SQS queue each rule of a custom bus targets, end
//! to end over the wire (`awsJson1_1`, `POST /` with `X-Amz-Target:
//! AWSEvents.<Operation>`) through the real `MultiServiceRouter`.
//!
//! The RULE asserted here: an operand is compared as the TEXT it is, so an empty
//! operand is the empty text and not a wildcard. `prefix` and `suffix` compare one END
//! of the leaf against it, and every string starts and ends with the empty text, so
//! they hold for every STRING leaf — the empty string included; `wildcard` and
//! `equals-ignore-case` compare the WHOLE leaf against it, so they hold only for the
//! empty string. No match type that compares text holds for a leaf that is not text (a
//! number, a boolean, `null`), and none holds for a field the event does not carry at
//! all — `exists` is the only match type about presence. A field holding a LIST offers
//! the values its members carry, so a list carrying the empty string satisfies both
//! readings. Each rule's target therefore receives exactly the events its own operand
//! admits and nothing else. Recorded behaviour: the model constrains `EventPattern` by
//! `@length` alone (`smithy_query(query="operation", operation="PutRule")`), so the
//! comparison the operands state comes from the recording and from the matcher's own
//! documentation.
//!
//! The rows span the class rather than one case: the OPERAND rows differ along which
//! end of the leaf the match type compares (one end, the whole of it), whether the
//! comparison folds case, whether the operand is written out or nested under the
//! case-folding match type, and whether it is a match expression at all (a literal term
//! is the same comparison written without one); the VALUE rows along the kind of the
//! leaf (string, number, boolean, null, absent, list) and, for the strings, along their
//! length, their case and whether they are blank. The same table is then read back
//! through `TestEventPattern`, so what the reporting surface answers and what the live
//! rule delivers are one verdict.
//!
//! The last test is the other half of a refused UPDATE: a `PutRule` the validator
//! refuses mints nothing and changes nothing, so the rule that was already there keeps
//! delivering exactly what its own operand matches. The refusal's own wording is pinned
//! by `tests/all/pattern_numeric_and_wildcard_operands.rs`; what is pinned here is that
//! the live rule survives it.
//!
//! The harness (the router with EventBridge, Amazon SQS and the services a target lives
//! in on one engine) is `tests/all/target_dispatch.rs`'s.

use serde_json::{json, Value};
use service_com_amazonaws_eventbridge_awsevents::state::SERVICE_PRINCIPAL;

use crate::target_dispatch::{
    drain, emulator, events, given_queue_with_policy, sqs, Emulator, DETAIL_TYPE, REGION, SOURCE,
};

/// The custom bus every rule below lives on, and the field its patterns are about.
const BUS: &str = "empty-operand-bus";
const FIELD: &str = "s";

/// The member each published detail carries so a drained message says which row it is.
const MARKER: &str = "marker";

/// One operand row: the case, the whole TERM the rule's pattern carries for [`FIELD`],
/// and the labels of the value rows that term admits.
struct OperandRow {
    case: &'static str,
    term: fn() -> Value,
    admits: &'static [&'static str],
}

/// One value row: its label, and the leaf the event carries at [`FIELD`] — `None` for
/// the row whose event does not carry that field at all.
struct ValueRow {
    label: &'static str,
    value: Option<fn() -> Value>,
}

/// The labels of the value rows whose leaf is a STRING, which is what a comparison
/// against one END of the leaf admits.
const EVERY_STRING: &[&str] = &[
    "a non-empty string",
    "the empty string",
    "a blank string",
    "an upper-case string",
    "a list carrying the empty string",
];

/// The labels of the value rows whose leaf is the EMPTY string, which is what a
/// comparison against the WHOLE leaf admits.
const ONLY_THE_EMPTY_STRING: &[&str] = &["the empty string", "a list carrying the empty string"];

/// The operand rows: each is one match type handed the empty string.
fn operand_rows() -> [OperandRow; 6] {
    [
        OperandRow {
            case: "an empty prefix operand",
            term: || json!({ "prefix": "" }),
            admits: EVERY_STRING,
        },
        OperandRow {
            case: "an empty suffix operand",
            term: || json!({ "suffix": "" }),
            admits: EVERY_STRING,
        },
        OperandRow {
            case: "an empty prefix operand nested under the case-folding match type",
            term: || json!({ "prefix": { "equals-ignore-case": "" } }),
            admits: EVERY_STRING,
        },
        OperandRow {
            case: "an empty wildcard operand",
            term: || json!({ "wildcard": "" }),
            admits: ONLY_THE_EMPTY_STRING,
        },
        OperandRow {
            case: "an empty equals-ignore-case operand",
            term: || json!({ "equals-ignore-case": "" }),
            admits: ONLY_THE_EMPTY_STRING,
        },
        OperandRow {
            case: "the empty string as a literal term",
            term: || json!(""),
            admits: ONLY_THE_EMPTY_STRING,
        },
    ]
}

/// The value rows: one published event each, differing along the kind of the leaf.
fn value_rows() -> [ValueRow; 10] {
    [
        ValueRow {
            label: "a non-empty string",
            value: Some(|| json!("text")),
        },
        ValueRow {
            label: "the empty string",
            value: Some(|| json!("")),
        },
        ValueRow {
            label: "a blank string",
            value: Some(|| json!(" ")),
        },
        ValueRow {
            label: "an upper-case string",
            value: Some(|| json!("TEXT")),
        },
        ValueRow {
            label: "an integer",
            value: Some(|| json!(0)),
        },
        ValueRow {
            label: "a fraction",
            value: Some(|| json!(7.5)),
        },
        ValueRow {
            label: "a boolean",
            value: Some(|| json!(true)),
        },
        ValueRow {
            label: "null",
            value: Some(|| json!(null)),
        },
        ValueRow {
            label: "a list carrying the empty string",
            value: Some(|| json!(["x", ""])),
        },
        ValueRow {
            label: "no such field",
            value: None,
        },
    ]
}

/// The `detail` one value row publishes: the marker that names the row, and the leaf
/// itself where the row carries one.
fn detail(row: &ValueRow) -> Value {
    let mut detail = serde_json::Map::new();
    detail.insert(MARKER.to_string(), json!(row.label));
    if let Some(value) = row.value {
        detail.insert(FIELD.to_string(), value());
    }
    Value::Object(detail)
}

/// The whole pattern one operand row's rule carries: the publication's own source and
/// detail type, and that row's term for [`FIELD`].
fn pattern(row: &OperandRow) -> String {
    json!({
        "source": [SOURCE],
        "detail-type": [DETAIL_TYPE],
        "detail": { FIELD: [(row.term)()] },
    })
    .to_string()
}

/// An event envelope carrying `detail`, in the shape `TestEventPattern` is handed one.
fn envelope(detail: &Value) -> String {
    json!({
        "id": "00000000-0000-0000-0000-000000000000",
        "version": "0",
        "detail-type": DETAIL_TYPE,
        "source": SOURCE,
        "account": engine::emulator_account_id(),
        "time": "2026-01-01T00:00:00Z",
        "region": REGION,
        "resources": [],
        "detail": detail,
    })
    .to_string()
}

/// The queue policy a rule's SQS target needs, scoped to THAT rule: the service
/// principal allowed `sqs:SendMessage` on that queue for deliveries whose source is the
/// rule's own ARN, which is how a client admits one rule's deliveries and no others.
fn granting_the_rule(queue_arn: &str, rule_arn: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "AllowThisRule",
            "Effect": "Allow",
            "Principal": { "Service": SERVICE_PRINCIPAL },
            "Action": "sqs:SendMessage",
            "Resource": queue_arn,
            "Condition": { "ArnEquals": { "aws:SourceArn": rule_arn } },
        }],
    })
    .to_string()
}

/// The custom bus the rows publish to.
async fn given_bus(emulator: &Emulator) {
    let (status, body) = events(emulator, "CreateEventBus", &json!({ "Name": BUS })).await;
    assert_eq!(status, 200, "CreateEventBus answered {status} {body}");
}

/// One row's rule on [`BUS`] carrying `pattern`, with a queue of its own as its only
/// target and a policy admitting that rule's deliveries; answers the queue's url.
async fn given_row(emulator: &Emulator, case: &str, name: &str, pattern: &str) -> String {
    let (url, queue_arn) = given_queue_with_policy(emulator, name, None).await;
    let (status, body) = events(
        emulator,
        "PutRule",
        &json!({
            "Name": name,
            "EventBusName": BUS,
            "EventPattern": pattern,
            "State": "ENABLED",
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutRule answered {status} {body}"
    );
    let rule_arn = body["RuleArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no RuleArn in {body}"))
        .to_string();
    let (status, body) = sqs(
        emulator,
        "SetQueueAttributes",
        &json!({
            "QueueUrl": url,
            "Attributes": { "Policy": granting_the_rule(&queue_arn, &rule_arn) },
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: SetQueueAttributes answered {status} {body}"
    );
    let (status, body) = events(
        emulator,
        "PutTargets",
        &json!({
            "Rule": name,
            "EventBusName": BUS,
            "Targets": [{ "Id": "queue", "Arn": queue_arn }],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutTargets answered {status} {body}"
    );
    assert_eq!(
        body["FailedEntryCount"],
        json!(0),
        "case {case:?}: PutTargets reported a failed entry: {body}"
    );
    url
}

/// Publish `details` onto [`BUS`], in requests of at most the modelled `Entries`
/// maximum so the publication itself is one AWS accepts.
async fn publish(emulator: &Emulator, details: &[Value]) {
    for batch in details.chunks(5) {
        let entries: Vec<Value> = batch
            .iter()
            .map(|detail| {
                json!({
                    "Source": SOURCE,
                    "DetailType": DETAIL_TYPE,
                    "EventBusName": BUS,
                    "Detail": detail.to_string(),
                })
            })
            .collect();
        let (status, body) = events(emulator, "PutEvents", &json!({ "Entries": entries })).await;
        assert_eq!(status, 200, "PutEvents answered {status} {body}");
        assert_eq!(
            body["FailedEntryCount"],
            json!(0),
            "PutEvents reported a failed entry: {body}"
        );
    }
}

/// The markers of the events waiting on the queue at `url`, sorted so a set is compared
/// as a set.
async fn markers(emulator: &Emulator, case: &str, url: &str) -> Vec<String> {
    let mut found: Vec<String> = drain(emulator, url)
        .await
        .iter()
        .map(|message| {
            let body = message["Body"].as_str().unwrap_or_default();
            let delivered: Value = serde_json::from_str(body).unwrap_or_else(|error| {
                panic!("case {case:?}: the delivered body is not JSON ({error}): {body:?}")
            });
            delivered["detail"][MARKER]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: the delivered envelope names no row: {delivered}")
                })
                .to_string()
        })
        .collect();
    found.sort();
    found
}

/// Each rule's SQS target receives exactly the events its own empty operand admits: a
/// comparison against one END of the leaf admits every string, one against the WHOLE
/// leaf only the empty string, and neither admits a leaf that is not text or a field
/// the event does not carry.
#[tokio::test]
async fn an_empty_text_operand_delivers_exactly_the_leaves_it_compares_equal() {
    let emulator = emulator();
    given_bus(&emulator).await;

    let operands = operand_rows();
    let values = value_rows();
    let mut queues = Vec::with_capacity(operands.len());
    for (index, row) in operands.iter().enumerate() {
        let name = format!("empty-operand-row-{index}");
        let url = given_row(&emulator, row.case, &name, &pattern(row)).await;
        queues.push(url);
    }

    let details: Vec<Value> = values.iter().map(detail).collect();
    publish(&emulator, &details).await;

    for (row, url) in operands.iter().zip(&queues) {
        let case = row.case;
        let mut expected: Vec<String> = row.admits.iter().map(|&label| label.to_string()).collect();
        expected.sort();
        let found = markers(&emulator, case, url).await;
        assert_eq!(
            found, expected,
            "case {case:?}: expected the target to receive exactly {expected:?}, got {found:?}"
        );
    }
}

/// The verdict the reporting surface answers is the one the live rule delivers by: for
/// every operand row and every value row, `TestEventPattern` answers `Result` exactly
/// where that row's target receives the event.
#[tokio::test]
async fn test_event_pattern_answers_the_verdict_the_delivery_matrix_shows() {
    let emulator = emulator();
    for row in &operand_rows() {
        let case = row.case;
        let pattern = pattern(row);
        for value in &value_rows() {
            let label = value.label;
            let expected = row.admits.contains(&label);
            let (status, body) = events(
                &emulator,
                "TestEventPattern",
                &json!({
                    "EventPattern": pattern,
                    "Event": envelope(&detail(value)),
                }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?} against {label:?}: TestEventPattern answered {status} {body}"
            );
            assert_eq!(
                body["Result"],
                json!(expected),
                "case {case:?} against {label:?}: expected Result {expected}, got {body}"
            );
        }
    }
}

/// A `PutRule` the pattern validator refuses changes nothing: the rule that was already
/// there keeps its stored `EventPattern` and keeps delivering exactly what that operand
/// matches, so a client whose update was refused still has the rule it deployed.
#[tokio::test]
async fn a_refused_update_leaves_the_live_rule_delivering_its_own_operand() {
    let emulator = emulator();
    given_bus(&emulator).await;
    let case = "a wildcard operand carrying an escape the grammar does not define";
    let live = json!({
        "source": [SOURCE],
        "detail-type": [DETAIL_TYPE],
        "detail": { FIELD: [{ "wildcard": "a*b" }] },
    })
    .to_string();
    let url = given_row(&emulator, case, "empty-operand-live-rule", &live).await;

    // What the rule matched before the refused update.
    publish(&emulator, &[detail(&ValueRow {
        label: "the live operand's text",
        value: Some(|| json!("axxb")),
    })])
    .await;
    let before = markers(&emulator, case, &url).await;
    assert_eq!(
        before,
        vec!["the live operand's text".to_string()],
        "case {case:?}: expected the live rule to deliver its own match, got {before:?}"
    );

    // The update: a `wildcard` operand escaping a character its grammar gives no
    // meaning to, which the validator refuses — naming the position just past the
    // escape marker, and the position in the submitted text the reader had reached.
    let refused = json!({
        "source": [SOURCE],
        "detail-type": [DETAIL_TYPE],
        "detail": { FIELD: [{ "wildcard": "a\\b" }] },
    })
    .to_string();
    let (status, body) = events(
        &emulator,
        "PutRule",
        &json!({
            "Name": "empty-operand-live-rule",
            "EventBusName": BUS,
            "EventPattern": refused,
            "State": "ENABLED",
        }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected the update to be refused, got {status} {body}"
    );
    assert_eq!(
        body["__type"].as_str().unwrap_or_default(),
        "InvalidEventPatternException",
        "case {case:?}: expected InvalidEventPatternException, got {body}"
    );
    let expected = format!(
        "Event pattern is not valid. Reason: Invalid escape character at pos 2\n at [Source: \
         (String)\"{refused}\"; line: 1, column: {}]",
        1 + refused
            .split_once("\\\\b\"")
            .map(|(read, _)| read.chars().count() + "\\\\b\"".chars().count())
            .unwrap_or_default()
    );
    assert_eq!(
        body["message"].as_str().unwrap_or_default(),
        expected,
        "case {case:?}: expected {expected:?}, got {body}"
    );

    // The rule the refusal left: its stored pattern, and what it still delivers.
    let (status, body) = events(
        &emulator,
        "DescribeRule",
        &json!({ "Name": "empty-operand-live-rule", "EventBusName": BUS }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeRule answered {status} {body}"
    );
    assert_eq!(
        body["EventPattern"].as_str().unwrap_or_default(),
        live,
        "case {case:?}: expected the stored pattern to survive the refusal, got {body}"
    );
    publish(&emulator, &[
        detail(&ValueRow {
            label: "the live operand's text, after the refusal",
            value: Some(|| json!("ayyb")),
        }),
        detail(&ValueRow {
            label: "a text the live operand does not match",
            value: Some(|| json!("zzz")),
        }),
    ])
    .await;
    let after = markers(&emulator, case, &url).await;
    assert_eq!(
        after,
        vec!["the live operand's text, after the refusal".to_string()],
        "case {case:?}: expected the live rule to keep matching its own operand and nothing \
         else, got {after:?}"
    );
}
