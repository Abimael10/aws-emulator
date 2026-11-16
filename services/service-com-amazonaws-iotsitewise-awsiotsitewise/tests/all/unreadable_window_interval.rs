// SPDX-License-Identifier: CC0-1.0

//! What an asset-model write answers for a tumbling-window `interval` it cannot
//! READ a span from, driven over HTTP through the same `MultiServiceRouter` the
//! emulator boots with.
//!
//! The rule: reading the value comes before judging the span it names, so a
//! window value is refused in one of TWO ways. Text no span can be read from is
//! refused as the text it is — the value quoted as the input that was read, then
//! stated plainly, with nothing of the formula in it, because the reading never
//! looked at the formula. Text a span IS read from is then held against the
//! published limits (`smithy_query(query="doc", shape="TumblingWindow",
//! member="interval")`: "The interval time must be between 1 minute and 1 week")
//! and, outside them, refused naming the metric's own expression and the interval
//! as the request spelled it.
//!
//! The rows differ along WHY no span can be read — a spelled-out unit, a word, a
//! unit letter outside the documented catalogue, a fractional or signed count, an
//! ISO 8601 prefix with no span after it, a count and unit separated — and the
//! two refusal classes and the accepted span sit in one table, so the suite
//! cannot pass by refusing everything or by answering one sentence to everything.
//! Every row stays inside `Interval`'s own `@length` 2..23
//! (`smithy_query(query="shape", shape="Interval")`), so what refuses it is the
//! reading and not the boundary's length check.
//!
//! The documented unit catalogue is `m`, `h`, `d`, `w` (and `s` for an offset),
//! so a letter outside it — tested here on `y`, which the issue did not cite —
//! names no span whatever count precedes it.
//!
//! The wire harness lives in `super::asset_lifecycle`.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::asset_lifecycle::{call, error_code, router};
// The `ListAssetModels` reading that answers "does the account hold a model by
// this name" lives in the sibling suite these rows sit beside; it is reached here
// rather than written a second time.
use crate::asset_model_definition_validation::holds_name;

/// The error class IoT SiteWise refuses a definition it will not store with.
const INVALID_REQUEST: &str = "InvalidRequestException";

/// The metric expression every row writes.
const EXPRESSION: &str = "avg(power)";

/// What the write answers for one row's interval.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    /// A span inside the published limits: the write is accepted.
    Accepted,
    /// A span read and found outside them.
    OutsideTheLimits,
    /// No span at all: the value could not be read.
    Unreadable,
}

/// The sentence a value no span could be read from is refused with, written out
/// by hand from the recorded class.
fn unreadable_message(interval: &str) -> String {
    format!("`{interval}`: Unable to parse window time interval {interval}")
}

/// The sentence a span outside the published limits is refused with.
fn outside_the_limits_message(interval: &str) -> String {
    format!("`{EXPRESSION}`: Window time interval '{interval}' is not within limits")
}

/// `POST /asset-models` for a model named `name` whose metric aggregates over the
/// tumbling window `interval`.
async fn create(router: &MultiServiceRouter, name: &str, interval: &str) -> (u16, Value) {
    call(
        router,
        "POST",
        "/asset-models",
        Some(json!({
            "assetModelName": name,
            "assetModelProperties": [
                {"name": "Power", "dataType": "DOUBLE", "type": {"measurement": {}}},
                {
                    "name": "Average",
                    "dataType": "DOUBLE",
                    "type": {"metric": {
                        "expression": EXPRESSION,
                        "variables": [{"name": "power", "value": {"propertyId": "Power"}}],
                        "window": {"tumbling": {"interval": interval}},
                    }},
                },
            ],
        })),
    )
    .await
}

/// The rows the table below is driven over: every reason a value names no span,
/// beside a span outside the limits and a span inside them.
fn rows() -> Vec<(&'static str, &'static str, Expect)> {
    vec![
        ("a count and a spelled-out unit", "5 minutes", Expect::Unreadable),
        ("a spelled-out unit alone", "minutes", Expect::Unreadable),
        ("a word naming no span", "later", Expect::Unreadable),
        (
            "a unit letter outside the documented catalogue",
            "5y",
            Expect::Unreadable,
        ),
        ("a fractional count", "1.5m", Expect::Unreadable),
        ("a signed count", "-5m", Expect::Unreadable),
        ("a count and unit separated", "5 m", Expect::Unreadable),
        ("an ISO 8601 prefix with no span", "PT", Expect::Unreadable),
        ("a unit with no count", "+m", Expect::Unreadable),
        ("one second below the floor", "59s", Expect::OutsideTheLimits),
        ("one day past the ceiling", "8d", Expect::OutsideTheLimits),
        (
            "below the floor in ISO 8601",
            "PT30S",
            Expect::OutsideTheLimits,
        ),
        ("a span inside the limits", "5m", Expect::Accepted),
        ("the floor in ISO 8601", "PT1M", Expect::Accepted),
    ]
}

/// RULE: a window value no span can be read from is refused as the text it is,
/// and only a value a span WAS read from is held against the published limits.
#[tokio::test]
async fn an_unreadable_window_interval_is_refused_as_the_text_it_is() {
    let router = router();

    for (at, (case, interval, expected)) in rows().into_iter().enumerate() {
        let name = format!("WindowReading-{at}");
        let (status, body) = create(&router, &name, interval).await;
        let stated = body["message"].as_str().unwrap_or_default().to_string();
        match expected {
            Expect::Accepted => {
                assert_eq!(
                    status, 202,
                    "case {case:?}: the interval {interval:?} names a span inside the \
                     limits, expected 202, got {status} {body}"
                );
            }
            Expect::Unreadable | Expect::OutsideTheLimits => {
                assert_eq!(
                    status, 400,
                    "case {case:?}: expected 400 for the interval {interval:?}, got \
                     {status} {body}"
                );
                assert_eq!(
                    error_code(&body),
                    INVALID_REQUEST,
                    "case {case:?}: expected {INVALID_REQUEST}, got {body}"
                );
                let message = if expected == Expect::Unreadable {
                    unreadable_message(interval)
                } else {
                    outside_the_limits_message(interval)
                };
                assert_eq!(
                    stated, message,
                    "case {case:?}: expected {message:?}, got {stated:?}"
                );
                assert!(
                    !holds_name(&router, &name).await,
                    "case {case:?}: a refused create must leave no asset model behind"
                );
            }
        }
    }
}

/// RULE (the same one, on the other write of a definition): `UpdateAssetModel`
/// reads a window value exactly as `CreateAssetModel` does, so the same interval
/// is refused with the same sentence there, and the stored definition is left
/// alone.
#[tokio::test]
async fn an_update_reads_a_window_interval_the_same_way() {
    let router = router();

    let (status, created) = create(&router, "ReadingOnUpdate", "5m").await;
    assert_eq!(status, 202, "the control create: got {status} {created}");
    let model_id = created["assetModelId"]
        .as_str()
        .unwrap_or_else(|| panic!("no assetModelId in {created}"))
        .to_string();

    // An update states the WHOLE property set, and each property it keeps carries
    // the id it was minted with, so the rows below change nothing but the window.
    let (status, before) = call(&router, "GET", &format!("/asset-models/{model_id}"), None).await;
    assert_eq!(status, 200, "DescribeAssetModel: got {status} {before}");
    let stored = |name: &str| -> String {
        before["assetModelProperties"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .find(|property| property["name"] == name)
            .and_then(|property| property["id"].as_str().map(str::to_string))
            .unwrap_or_else(|| panic!("no property {name:?} in {before}"))
    };
    let power = stored("Power");
    let average = stored("Average");

    for (case, interval, expected) in rows() {
        let (status, body) = call(
            &router,
            "PUT",
            &format!("/asset-models/{model_id}"),
            Some(json!({
                "assetModelName": "ReadingOnUpdate",
                "assetModelProperties": [
                    {
                        "id": &power,
                        "name": "Power",
                        "dataType": "DOUBLE",
                        "type": {"measurement": {}},
                    },
                    {
                        "id": &average,
                        "name": "Average",
                        "dataType": "DOUBLE",
                        "type": {"metric": {
                            "expression": EXPRESSION,
                            "variables": [
                                {"name": "power", "value": {"propertyId": &power}},
                            ],
                            "window": {"tumbling": {"interval": interval}},
                        }},
                    },
                ],
            })),
        )
        .await;
        let stated = body["message"].as_str().unwrap_or_default().to_string();
        match expected {
            Expect::Accepted => assert_eq!(
                status, 202,
                "case {case:?}: the interval {interval:?} names a span inside the limits, \
                 expected 202, got {status} {body}"
            ),
            Expect::Unreadable | Expect::OutsideTheLimits => {
                let message = if expected == Expect::Unreadable {
                    unreadable_message(interval)
                } else {
                    outside_the_limits_message(interval)
                };
                assert_eq!(
                    status, 400,
                    "case {case:?}: expected 400 for the interval {interval:?}, got \
                     {status} {body}"
                );
                assert_eq!(
                    stated, message,
                    "case {case:?}: expected {message:?}, got {stated:?}"
                );
            }
        }
    }
}
