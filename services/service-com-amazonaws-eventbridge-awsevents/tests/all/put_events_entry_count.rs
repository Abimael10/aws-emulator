// SPDX-License-Identifier: CC0-1.0

//! How a `PutEvents` request INSIDE the modelled entry bound is judged: entry by
//! entry — end to end over the wire (`awsJson1_1`, `POST /` with
//! `X-Amz-Target: AWSEvents.<Operation>`) through the real `MultiServiceRouter`,
//! with Amazon SQS mounted beside EventBridge on one engine so a delivery is
//! asserted where a client observes one: on the queue a rule targets.
//!
//! The RULE asserted here: `Detail`, `DetailType` and `Source` "are required for
//! EventBridge to successfully send an event to an event bus. If you include event
//! entries in a request that do not include each of those properties, EventBridge
//! fails that entry" (`PutEventsRequestEntry`), so a request the boundary admits —
//! one carrying at most the modelled ten entries — is answered 200 with one result
//! row per submitted entry, in the request's order: an entry missing one of those
//! properties carries an error code and is published nowhere, and every other entry
//! of the same request is published and delivered all the same. That is the
//! OPPOSITE half of the whole-request refusal a list past its modelled `@length`
//! earns (`tests/all/put_events_entry_count_limit.rs`): one is the batch judged per
//! row, the other the batch refused as a unit.
//!
//! The rows span the class rather than one case: they differ along WHICH required
//! property the one bad entry leaves out and WHERE in the batch it sits (first, in
//! the middle, last).
//!
//! The harness (the router with EventBridge, Amazon SQS and the services a target
//! lives in on one engine) is `tests/all/target_dispatch.rs`'s.

use serde_json::{json, Value};

use crate::put_events_entry_count_limit::{
    delivered_indexes, given_bus_rule_and_queue, received, BUS,
};
use crate::target_dispatch::{emulator, events, DETAIL_TYPE, SOURCE};

/// The error code an entry EventBridge will not publish carries on its own row.
const INVALID_ARGUMENT: &str = "InvalidArgument";

/// One entry carrying every property an event needs, with `index` in its detail so
/// a delivery can be told from its siblings.
fn whole_entry(index: usize) -> Value {
    json!({
        "Source": SOURCE,
        "DetailType": DETAIL_TYPE,
        "Detail": json!({ "index": index }).to_string(),
        "EventBusName": BUS,
    })
}

/// [`whole_entry`] with `member` left out.
fn entry_without(index: usize, member: &str) -> Value {
    let mut entry = whole_entry(index);
    entry
        .as_object_mut()
        .expect("an entry is an object")
        .remove(member);
    entry
}

/// A batch of `count` entries where the one at `bad_at` leaves `member` out.
fn batch(count: usize, bad_at: usize, member: &str) -> Vec<Value> {
    (0..count)
        .map(|index| {
            if index == bad_at {
                entry_without(index, member)
            } else {
                whole_entry(index)
            }
        })
        .collect()
}

/// An entry the service will not publish fails on its own row, and the rest of its
/// batch is delivered.
#[tokio::test]
async fn an_unpublishable_entry_fails_alone_and_its_batch_is_delivered() {
    // (case, how many entries, which one is bad, the property it leaves out)
    let rows = [
        ("the first entry carries no DetailType", 10usize, 0usize, "DetailType"),
        ("an entry in the middle carries no Detail", 10, 4, "Detail"),
        ("the last entry carries no Source", 10, 9, "Source"),
    ];

    for (case, count, bad_at, member) in rows {
        let emulator = emulator();
        let url = given_bus_rule_and_queue(&emulator, &format!("per-entry-{member}")).await;
        let submitted = batch(count, bad_at, member);
        let (status, answer) = events(
            &emulator,
            "PutEvents",
            &json!({ "Entries": &submitted }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: a batch inside the bound is answered, got {status} {answer}"
        );
        assert_eq!(
            answer["FailedEntryCount"],
            json!(1),
            "case {case:?}: exactly the one bad entry fails, got {answer}"
        );
        let rows_answered = answer["Entries"].as_array().cloned().unwrap_or_default();
        assert_eq!(
            rows_answered.len(),
            count,
            "case {case:?}: one result row per submitted entry, got {answer}"
        );
        for (index, row) in rows_answered.iter().enumerate() {
            if index == bad_at {
                assert_eq!(
                    row["ErrorCode"].as_str(),
                    Some(INVALID_ARGUMENT),
                    "case {case:?}: row {index} is the refused entry, got {row}"
                );
                assert!(
                    row["EventId"].is_null(),
                    "case {case:?}: a refused entry is given no event id, got {row}"
                );
            } else {
                assert!(
                    row["ErrorCode"].is_null() && row["EventId"].as_str().is_some(),
                    "case {case:?}: row {index} is a published entry, got {row}"
                );
            }
        }

        // Every entry but the refused one reached the rule's target.
        let expected: Vec<i64> = (0..count)
            .filter(|index| *index != bad_at)
            .map(|index| index as i64)
            .collect();
        let messages = received(&emulator, &url, expected.len()).await;
        let indexes = delivered_indexes(&messages);
        assert_eq!(
            indexes, expected,
            "case {case:?}: expected {expected:?} on the queue, got {indexes:?} from \
             {messages:?}"
        );
    }
}
