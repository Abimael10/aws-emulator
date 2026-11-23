// SPDX-License-Identifier: CC0-1.0

//! An end-to-end test for WHAT a `CreateCrawler` refusal about a store the crawl reads a
//! target through is allowed to say.
//!
//! The rule under test: when the policies in force on the crawler's `Role` do not admit the
//! calls a crawl makes on the store a target names, Glue refuses the create with its OWN
//! sentence for that store — fixed text whose variable part is the target the request carried,
//! exactly as sent — and never with the authorization decision it reached internally. That
//! decision is a reading of the role's identity policies, so its answer (the acting principal,
//! the call it asked about, the policy class the grant was withheld by) is exactly what a Glue
//! control-plane refusal may not relay: a client, a log rule or a deployment tool keyed on
//! Glue's wording matches the sentence and not that answer.
//!
//! The rule is asserted here on the role shape such a crawler carries in practice — the
//! crawl's own reads granted on the data behind the target and nothing granted on the store
//! that target is reached through — for BOTH stores a target is read through: the Amazon SQS
//! queue an S3 target is crawled from, and the Amazon DynamoDB table a `DynamoDBTarget` names.
//! The tables that span the other ways that reading can come out against the role (no document
//! at all, a grant pointing at another queue, another account or another service, an explicit
//! `Deny`, every call the queue's service publishes granted one at a time) live in
//! [`crate::crawler_event_queue`], which asserts the same property on every one of them
//! through the same [`crate::crawler_event_queue::assert_discloses_no_decision`] — so this
//! suite adds the rule's statement for the recorded request's own shape and copies none of
//! those rows.

use std::sync::Arc;

use serde_json::json;
use service_com_amazonaws_glue_awsglue::state::EVENT_QUEUE_ACTIONS;

use crate::crawler_event_queue::{
    assert_discloses_no_decision, data_read_grant, dynamodb_crawler, event_mode_crawler,
    expected_inaccessible, expected_table_grant_missing, queue_arn, seed_role, without_request_id,
    TARGET_BUCKET,
};
use crate::wire::{call, emulator_on, error_code, error_message};

/// Glue's refusal of a crawler whose role cannot read the store one of its targets names
/// states Glue's own sentence about that store and nothing of the authorization decision it
/// made to get there — on either store a target is read through.
#[tokio::test]
async fn a_target_store_refusal_states_glues_own_sentence_only() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_on(engine.clone());
    let queue = queue_arn("own-sentence-events");
    let table = "own-sentence-items";
    // The role both creates below run as: it may read the data behind the target, and its
    // documents say nothing whatever about the store that target is reached through.
    let role_name = "own-sentence-role";
    let role = seed_role(&engine, role_name, &[data_read_grant(TARGET_BUCKET)]);

    // The S3 target crawled from a queue the role cannot consume.
    let case = "an event queue the role is shown nothing about";
    let (status, body) = call(
        &router,
        "CreateCrawler",
        &event_mode_crawler("own-sentence-queue-crawler", &role, &queue),
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected 400, got {status} with {body}"
    );
    assert!(
        error_code(&body).contains("InvalidInputException"),
        "case {case:?}: expected InvalidInputException, got {}",
        error_code(&body)
    );
    let message = error_message(&body);
    assert_discloses_no_decision(case, &message, &role, role_name);
    // This refusal asks for no grant either: it names no queue call, because there is none to
    // ask for on a queue whose existence this role cannot establish.
    for action in EVENT_QUEUE_ACTIONS {
        assert!(
            !message.contains(action),
            "case {case:?}: this refusal must ask for no grant, but it names {action:?}: got \
             {message:?}"
        );
    }
    assert_eq!(
        message,
        expected_inaccessible(&queue),
        "case {case:?}: expected Glue's own sentence naming the queue the target carried, got \
         {message:?}"
    );

    // And the DynamoDB target whose table the same role cannot read.
    let case = "a DynamoDB table the role may not read";
    let (status, body) = call(
        &router,
        "CreateCrawler",
        &dynamodb_crawler("own-sentence-table-crawler", &role, table),
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected 400, got {status} with {body}"
    );
    assert!(
        error_code(&body).contains("InvalidInputException"),
        "case {case:?}: expected InvalidInputException, got {}",
        error_code(&body)
    );
    let message = error_message(&body);
    assert_discloses_no_decision(case, &message, &role, role_name);
    assert_eq!(
        without_request_id(case, &message),
        expected_table_grant_missing(table),
        "case {case:?}: expected Glue's own sentence naming the table the target carried, got \
         {message:?}"
    );

    // Neither refused create left a crawler behind.
    for crawler in ["own-sentence-queue-crawler", "own-sentence-table-crawler"] {
        let (status, body) = call(&router, "GetCrawler", &json!({ "Name": crawler })).await;
        assert_eq!(
            status, 400,
            "case {crawler:?}: a refused create must store no crawler, got {status} with {body}"
        );
        assert!(
            error_code(&body).contains("EntityNotFoundException"),
            "case {crawler:?}: expected EntityNotFoundException, got {}",
            error_code(&body)
        );
    }
}
