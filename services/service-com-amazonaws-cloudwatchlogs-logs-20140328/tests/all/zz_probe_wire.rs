// SPDX-License-Identifier: CC0-1.0

//! Probes of `StartQuery` OVER THE WIRE for the one relation between a query's time range and
//! the log group it reads that a whole-second range can get wrong: the range's end is stated in
//! whole seconds and is INCLUSIVE, while a log group's `creationTime` is an epoch MILLISECOND.
//!
//! The rule under test: the second a range's `endTime` names is part of the range ("the range is
//! inclusive, so the specified end time is included in the query", `StartQuery$endTime`, in
//! "epoch time, the number of seconds since January 1, 1970"), so a range ending in the second a
//! log group was created IN reaches that group's creation instant and the query is served —
//! however far into that second the group was created. Only a range that closes before that
//! second began covers no instant of the group's lifetime.
//!
//! The table spans what the rule generalises over: where inside its second the group's creation
//! instant falls (the store's own instant, and an instant placed at the start, the middle and the
//! last millisecond of a second), and the ends one second either side of it.
//!
//! Every case drives the service the way a client does — `POST /` with
//! `X-Amz-Target: Logs_20140328.<Operation>` through the router that mounts it — so an answer a
//! handler does not compute from the shared store cannot pass.

use serde_json::{json, Value};

use service_com_amazonaws_cloudwatchlogs_logs_20140328::state::ComAmazonawsCloudwatchlogsLogs20140328State as State;

use crate::harness::Logs;

/// The operation under test.
const START: &str = "StartQuery";

/// A query string that compiles, so no case is refused for its text.
const VALID_QUERY: &str = "fields @timestamp, @message";

/// Place the stored log group `group`'s creation instant at `creation_ms`.
///
/// No request sets a log group's `creationTime` — the create stamps it from the clock — so a case
/// that is about WHERE INSIDE ITS SECOND that instant falls writes it into the shared store the
/// create wrote it to, and asserts the rest over the wire.
fn place_creation(logs: &Logs, group: &str, creation_ms: i64) {
    let state = logs.engine().state::<State>();
    let mut store = state.lock();
    let held = store
        .log_groups
        .get_mut(group)
        .unwrap_or_else(|| panic!("the log group {group} is stored"));
    held.creation_time_ms = creation_ms;
}

/// RULE: a range ending in the second the group was created in is served, and one ending before
/// that second is refused — wherever inside its second the creation instant falls.
#[tokio::test]
async fn the_second_a_group_was_created_in_is_part_of_the_range() {
    // (case, the millisecond inside its second the creation instant falls at)
    let cases: [(&str, i64); 4] = [
        ("the first millisecond of the second", 0),
        ("the middle of the second", 500),
        ("the last millisecond of the second", 999),
        ("just past the start of the second", 1),
    ];
    for (case, millis) in cases {
        let logs = Logs::new();
        let group = "/window-second/group";
        logs.ok("CreateLogGroup", &json!({"logGroupName": group}))
            .await;
        // A whole second well inside the ingestion clock's past, plus the offset the row places
        // the creation instant at.
        let created_second = crate::harness::now_ms() / 1_000 - 600;
        place_creation(&logs, group, created_second * 1_000 + millis);

        // (what the end names, the end, whether the query is served)
        let ends: [(&str, i64, bool); 3] = [
            ("the second the group was created in", created_second, true),
            ("the second after it", created_second + 1, true),
            ("the second before it", created_second - 1, false),
        ];
        for (named, end_time, served) in ends {
            let (status, body) = logs
                .call(
                    START,
                    &json!({
                        "logGroupName": group,
                        "startTime": 0,
                        "endTime": end_time,
                        "queryString": VALID_QUERY,
                    }),
                )
                .await;
            if served {
                assert_eq!(
                    status, 200,
                    "case {case:?} with the end at {named}: expected the query to be served, \
                     got {status} with {body}"
                );
                assert!(
                    body["queryId"].as_str().is_some_and(|id| !id.is_empty()),
                    "case {case:?} with the end at {named}: a served start answers a queryId, \
                     got {body}"
                );
            } else {
                assert_eq!(
                    status, 400,
                    "case {case:?} with the end at {named}: expected the query to be refused, \
                     got {status} with {body}"
                );
                assert_eq!(
                    crate::harness::error_code(&body),
                    "MalformedQueryException",
                    "case {case:?} with the end at {named}: expected the malformed-query code, \
                     got {body}"
                );
            }
        }
    }
}

/// RULE: a range whose end names the current second reaches a group created in that same second,
/// which is what a client that creates a group and queries it straight away asks for.
#[tokio::test]
async fn a_group_created_in_the_second_the_range_ends_in_is_queryable() {
    let logs = Logs::new();
    let case = "a group created in the second the range ends in";
    let group = "/window-second/fresh";
    logs.ok("CreateLogGroup", &json!({"logGroupName": group}))
        .await;
    // The group was created at THIS instant, so the second its creation instant falls in is the
    // one the store reports it in; the range ends in that very second.
    let reported = logs
        .describe_group(group)
        .await
        .unwrap_or_else(|| panic!("case {case:?}: the created group is reported"));
    let created_ms = reported
        .get("creationTime")
        .and_then(Value::as_i64)
        .unwrap_or_else(|| panic!("case {case:?}: a log group carries creationTime, got {reported}"));
    let answer = logs
        .ok(
            START,
            &json!({
                "logGroupName": group,
                "startTime": created_ms / 1_000 - 3_600,
                "endTime": created_ms / 1_000,
                "queryString": VALID_QUERY,
            }),
        )
        .await;
    assert!(
        answer["queryId"].as_str().is_some_and(|id| !id.is_empty()),
        "case {case:?}: the query is served and minted an id, got {answer}"
    );
}
