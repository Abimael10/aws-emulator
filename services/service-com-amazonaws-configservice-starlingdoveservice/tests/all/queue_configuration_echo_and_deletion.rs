// SPDX-License-Identifier: CC0-1.0

//! What a RUNNING recorder leaves in the history of a queue that was never changed before
//! it went away: the create-then-delete sequence, and nothing else.
//!
//! The rule pinned here: when a resource the recorder has discovered goes away in its own
//! service, Config appends ONE further configuration item carrying
//! `configurationItemStatus` of `ResourceDeleted`, which keeps the state the resource was
//! last recorded in and the id, name and ARN that identify it — so the resource's history
//! holds its whole sequence, oldest to newest, and outlives the resource.
//!
//! This is the row class the sibling suite does not cover: every queue of
//! `recorded_queue_across_every_read_surface` is CHANGED before it is deleted, so the
//! sequence it reads back has an updated state in the middle, while a resource created and
//! then deleted with nothing in between must hold exactly its discovery and its deletion.
//! Its helpers, its table and its status constants are reused from there rather than
//! restated, and the `configuration` document's agreement with the live Amazon SQS read is
//! pinned by `scratch_resume`, so neither is repeated here.
//!
//! Every configuration item asserted on is one the recorder OBSERVED: nothing below calls
//! `PutResourceConfig`, and every queue is created, read and deleted through Amazon SQS'
//! own API.

use serde_json::json;

use crate::config_recorded_resource_compliance::Emulator;
use crate::recorded_queue_across_every_read_surface::{
    await_history, captured_at, configuration, create_queue, created_with, history_oldest_first,
    live_attribute, statuses, QUEUES, RESOURCE_DELETED, RESOURCE_DISCOVERED,
};

/// The attribute Amazon SQS reports a queue's own ARN under
/// (`com.amazonaws.sqs#QueueAttributeName`), which is what a configuration item of the queue
/// carries as its `arn`.
const QUEUE_ARN: &str = "QueueArn";

#[tokio::test]
async fn a_queue_deleted_unchanged_holds_exactly_its_discovery_and_its_deletion() {
    let emulator = Emulator::new();
    emulator.recording().await;

    // The rows are the sibling suite's table — a queue created with no attributes, with one,
    // with several, and a FIFO queue — read here with nothing happening to the queue between
    // its discovery and its deletion.
    for (case, name, _, _) in QUEUES {
        let queue_url = create_queue(case, &emulator, name, &created_with(name)).await;

        // The recorder discovers the queue with no further Config call.
        let discovered = await_history(
            case,
            &emulator,
            &queue_url,
            "a discovery item",
            |items| !items.is_empty(),
        )
        .await;
        assert_eq!(
            statuses(&discovered),
            vec![RESOURCE_DISCOVERED.to_string()],
            "case {case:?}: the only item of a newly discovered resource is its discovery, \
             got {discovered:?}"
        );
        let discovery = discovered[0].clone();
        let queue_arn = live_attribute(case, &emulator, &queue_url, QUEUE_ARN).await;

        // The queue going away appends ONE item, so the history holds exactly the two states
        // the resource went through — and the newest-first order this read answers by default
        // is the reverse of the sequence `Forward` reports.
        emulator
            .sqs("DeleteQueue", &json!({ "QueueUrl": queue_url }))
            .await;
        let newest_first = await_history(
            case,
            &emulator,
            &queue_url,
            "the deletion item",
            |items| statuses(items).first().map(String::as_str) == Some(RESOURCE_DELETED),
        )
        .await;
        assert_eq!(
            statuses(&newest_first),
            vec![RESOURCE_DELETED.to_string(), RESOURCE_DISCOVERED.to_string()],
            "case {case:?}: a queue nothing changed holds its deletion and its discovery, \
             got {newest_first:?}"
        );
        let oldest_first = history_oldest_first(case, &emulator, &queue_url).await;
        assert_eq!(
            statuses(&oldest_first),
            vec![RESOURCE_DISCOVERED.to_string(), RESOURCE_DELETED.to_string()],
            "case {case:?}: read the other way round the sequence is the discovery then the \
             deletion, got {oldest_first:?}"
        );
        let captured = captured_at(case, &oldest_first);
        assert!(
            captured.windows(2).all(|pair| pair[0] <= pair[1]),
            "case {case:?}: the items come in the order they were captured, got {captured:?}"
        );

        // The deletion item keeps the state the queue was last recorded in, and the three
        // members that identify the resource it is about.
        let deletion = &oldest_first[1];
        assert_eq!(
            configuration(case, deletion)[QUEUE_ARN],
            queue_arn,
            "case {case:?}: the deletion item's configuration carries the queue's own ARN, \
             got {deletion:?}"
        );
        assert_eq!(
            configuration(case, deletion),
            configuration(case, &discovery),
            "case {case:?}: the deletion records no configuration the queue never had, got \
             {deletion:?}"
        );
        assert_eq!(
            deletion["resourceId"],
            json!(queue_url),
            "case {case:?}: the deletion item names the queue's own url, got {deletion:?}"
        );
        assert_eq!(
            deletion["resourceName"],
            json!(name),
            "case {case:?}: the deletion item names the queue's custom name, got {deletion:?}"
        );
        assert_eq!(
            deletion["arn"], queue_arn,
            "case {case:?}: the deletion item keeps the ARN Amazon SQS reported for the \
             queue, got {deletion:?}"
        );
    }
}
