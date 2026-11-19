// SPDX-License-Identifier: CC0-1.0

//! What a `PutTargets` REQUEST carrying an `InputTransformer` whose template
//! names a placeholder nothing fills does to the rule it was sent for — driven
//! end to end over the wire through the real `MultiServiceRouter`.
//!
//! The RULE asserted here: an `InputTemplate` is the text "where you specify
//! placeholders that will be filled with the values of the keys from
//! `InputPathsMap`" (`InputTransformer$InputTemplate`), so a target whose
//! template names a placeholder neither its own `InputPathsMap` declares nor
//! EventBridge fills from the delivery itself is refused when it is CONFIGURED —
//! HTTP 400, the service-common `ValidationException`, the message naming the
//! member, the offending target by the `Id` the REQUEST gave it and the
//! placeholder as the template wrote it — and the refusal is of the WHOLE
//! request: not one target of it is installed, and a target the rule already
//! carries keeps the configuration it already had.
//!
//! `tests/all/input_template_placeholders.rs` spans WHICH templates are refused,
//! one target at a time. This suite spans what the refusal does to the REQUEST
//! and to the rule: its rows differ along how many targets the request carries,
//! WHERE in the request the offending target stands (its only entry, the first,
//! the middle, the last), whether that target is a new `Id` or an upsert of one
//! the rule already holds, whether the request carries other targets that are
//! perfectly valid, how many offending targets and placeholders there are, and
//! what the undeclared placeholder is named.
//!
//! The harness (the router with EventBridge mounted on one engine, and the
//! requests signed for it) is `tests/all/target_dispatch.rs`'s.

use serde_json::{json, Value};

use crate::target_dispatch::{emulator, events, given_rule, Emulator, REGION};

/// The id of the target every rule below already carries, and whose stored
/// configuration a refused request must leave exactly as it was.
const KEPT_TARGET_ID: &str = "kept";

/// The ARN of an Amazon SNS topic of this account and Region: a target kind
/// EventBridge invokes under a resource-based policy, so it names no role and
/// every row's targets are accepted but for the template under test.
fn topic_arn(name: &str) -> String {
    format!(
        "arn:aws:sns:{REGION}:{}:{name}",
        engine::emulator_account_id()
    )
}

/// The paths map every template below fills its declared placeholders from.
fn paths() -> Value {
    json!({ "who": "$.detail.who" })
}

/// The transformer the target the rule already carries is configured with: a
/// template whose every placeholder the map declares.
fn kept_transformer() -> Value {
    json!({ "InputPathsMap": paths(), "InputTemplate": "{\"who\": <who>}" })
}

/// A target entry with `id`, the one topic ARN, and `template` rendered through
/// [`paths`].
fn target(id: &str, template: &str) -> Value {
    json!({
        "Id": id,
        "Arn": topic_arn("orders"),
        "InputTransformer": { "InputPathsMap": paths(), "InputTemplate": template },
    })
}

/// A template whose every placeholder [`paths`] declares.
const FILLED: &str = "{\"who\": <who>}";
/// A template naming one placeholder [`paths`] does not declare.
const UNFILLED: &str = "{\"who\": <who>, \"nope\": <missing>}";

/// One row: the target list one `PutTargets` request carries, the refusal it must
/// answer (the offending target's `Id` and the placeholder nothing fills, or
/// `None` for a request that is accepted), the ids the rule holds afterwards in
/// their stored order, and the transformer the kept target then carries.
struct Row {
    case: &'static str,
    targets: Vec<Value>,
    refused: Option<(&'static str, &'static str)>,
    stored: Vec<&'static str>,
    kept: Value,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the request's only target names an undeclared placeholder",
            targets: vec![target("t1", UNFILLED)],
            refused: Some(("t1", "missing")),
            stored: vec![KEPT_TARGET_ID],
            kept: kept_transformer(),
        },
        Row {
            case: "the FIRST of three targets names an undeclared placeholder",
            targets: vec![
                target("t1", UNFILLED),
                target("t2", FILLED),
                target("t3", FILLED),
            ],
            refused: Some(("t1", "missing")),
            stored: vec![KEPT_TARGET_ID],
            kept: kept_transformer(),
        },
        Row {
            case: "the MIDDLE of three targets names an undeclared placeholder",
            targets: vec![
                target("t1", FILLED),
                target("t2", UNFILLED),
                target("t3", FILLED),
            ],
            refused: Some(("t2", "missing")),
            stored: vec![KEPT_TARGET_ID],
            kept: kept_transformer(),
        },
        Row {
            case: "the LAST of three targets names an undeclared placeholder",
            targets: vec![
                target("t1", FILLED),
                target("t2", FILLED),
                target("t3", UNFILLED),
            ],
            refused: Some(("t3", "missing")),
            stored: vec![KEPT_TARGET_ID],
            kept: kept_transformer(),
        },
        Row {
            case: "an UPSERT of the target the rule already carries",
            targets: vec![target(KEPT_TARGET_ID, UNFILLED)],
            refused: Some((KEPT_TARGET_ID, "missing")),
            stored: vec![KEPT_TARGET_ID],
            kept: kept_transformer(),
        },
        Row {
            case: "a valid new target beside an invalid upsert of the kept one",
            targets: vec![target("t1", FILLED), target(KEPT_TARGET_ID, UNFILLED)],
            refused: Some((KEPT_TARGET_ID, "missing")),
            stored: vec![KEPT_TARGET_ID],
            kept: kept_transformer(),
        },
        Row {
            case: "two offending targets: the earlier one in the request is named",
            targets: vec![
                target("t1", "{\"a\": <first>}"),
                target("t2", "{\"b\": <second>}"),
            ],
            refused: Some(("t1", "first")),
            stored: vec![KEPT_TARGET_ID],
            kept: kept_transformer(),
        },
        Row {
            case: "two undeclared placeholders in one template: the earlier one is named",
            targets: vec![target("t1", "{\"a\": <alpha>, \"b\": <beta>}")],
            refused: Some(("t1", "alpha")),
            stored: vec![KEPT_TARGET_ID],
            kept: kept_transformer(),
        },
        Row {
            case: "an undeclared placeholder named in the reserved namespace",
            targets: vec![target("t1", "{\"r\": <aws.events.nope>}")],
            refused: Some(("t1", "aws.events.nope")),
            stored: vec![KEPT_TARGET_ID],
            kept: kept_transformer(),
        },
        Row {
            case: "three targets whose every placeholder is filled",
            targets: vec![
                target("t1", FILLED),
                target("t2", "{\"who\": \"<who>\"}"),
                // `aws.events.rule-name` is one of the variables EventBridge
                // fills from the delivery, so no map has to declare it (the
                // predefined variables of EventBridge's target input
                // transformation, each of which
                // `tests/all/input_template_placeholders.rs` tables).
                target("t3", "{\"rule\": <aws.events.rule-name>}"),
            ],
            refused: None,
            stored: vec![KEPT_TARGET_ID, "t1", "t2", "t3"],
            kept: kept_transformer(),
        },
        Row {
            case: "an accepted upsert replaces the kept target's own transformer",
            targets: vec![target(KEPT_TARGET_ID, "{\"only\": <who>}")],
            refused: None,
            stored: vec![KEPT_TARGET_ID],
            kept: json!({ "InputPathsMap": paths(), "InputTemplate": "{\"only\": <who>}" }),
        },
    ]
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

/// The targets `rule` holds, in their stored order, as `ListTargetsByRule`
/// reports them.
async fn stored_targets(emulator: &Emulator, case: &str, rule: &str) -> Vec<Value> {
    let (status, listed) = events(emulator, "ListTargetsByRule", &json!({ "Rule": rule })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: ListTargetsByRule answered {status} {listed}"
    );
    listed["Targets"].as_array().cloned().unwrap_or_default()
}

#[tokio::test]
async fn a_request_naming_an_unfilled_placeholder_installs_none_of_its_targets() {
    let emulator = emulator();

    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let rule = format!("transform-request-{index}");
        given_rule(
            &emulator,
            &rule,
            json!([{
                "Id": KEPT_TARGET_ID,
                "Arn": topic_arn("orders"),
                "InputTransformer": kept_transformer(),
            }]),
        )
        .await;

        let (status, body) = events(
            &emulator,
            "PutTargets",
            &json!({ "Rule": rule, "Targets": row.targets }),
        )
        .await;

        match row.refused {
            Some((target_id, placeholder)) => {
                assert_eq!(
                    status, 400,
                    "case {case:?}: expected the request to be refused, got {status} {body}"
                );
                let code = error_code(&body);
                assert_eq!(
                    code, "ValidationException",
                    "case {case:?}: expected ValidationException, got {code:?} in {body}"
                );
                let message = body["message"].as_str().unwrap_or_default();
                let expected = format!(
                    "InputTemplate for target {target_id} contains invalid placeholder \
                     {placeholder}."
                );
                assert_eq!(
                    message, expected,
                    "case {case:?}: expected {expected:?}, got {message:?}"
                );
            }
            None => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected the request to be accepted, got {status} {body}"
                );
                assert_eq!(
                    body["FailedEntryCount"],
                    json!(0),
                    "case {case:?}: PutTargets reported a failed entry: {body}"
                );
            }
        }

        let stored = stored_targets(&emulator, case, &rule).await;
        let ids: Vec<&str> = stored
            .iter()
            .map(|target| target["Id"].as_str().unwrap_or_default())
            .collect();
        assert_eq!(
            ids, row.stored,
            "case {case:?}: expected the rule to hold {:?}, got {ids:?}",
            row.stored
        );
        let kept = stored
            .iter()
            .find(|target| target["Id"] == json!(KEPT_TARGET_ID))
            .unwrap_or_else(|| panic!("case {case:?}: the kept target is gone: {stored:?}"));
        assert_eq!(
            kept["InputTransformer"], row.kept,
            "case {case:?}: expected the kept target to carry {:?}, got {:?}",
            row.kept, kept["InputTransformer"]
        );
    }
}
