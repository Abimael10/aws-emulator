// SPDX-License-Identifier: CC0-1.0

//! What an event source mapping ANSWERS once its deletion has been accepted.
//!
//! `DeleteEventSourceMapping` "deletes an event source mapping… When you delete an event
//! source mapping, it enters a `Deleting` state and might not be completely deleted for
//! several seconds", so the delete is ACCEPTED and the removal it owes is the clock's.
//!
//! RULE: from the instant a delete is accepted, the mapping is no longer one a further call
//! may ACT on — every call that would change it (a second delete, an update of any member)
//! answers the modelled `ResourceNotFoundException` (HTTP 404) with the service's fixed
//! not-found sentence and changes nothing — while a READ of it still reports the `Deleting`
//! state the teardown is in, until the removal the clock owes falls due and the read is
//! that same not-found. The windows the model's `ResourceInUseException` names ("you tried
//! to update an event source mapping in the CREATING state, or you tried to delete an event
//! source mapping currently UPDATING") are the ones a mapping is still THERE for; a mapping
//! being deleted is not one of them.
//!
//! The rows differ along what the rule generalises over: WHICH event source the mapping
//! polls (a queue, a Kinesis stream, a DynamoDB stream), WHICH call arrives after the delete
//! (a second delete, and an update of a different member per row), and whether the delete
//! was accepted inside the mapping's own `Creating` window or after it had settled. Every
//! row also carries a POSITIVE CONTROL — a twin mapping on the same source, held by a second
//! function, that nobody deleted — so a blanket refusal fails here instead of passing as the
//! rule.
//!
//! Nothing sleeps on the emulator's behalf and nothing waits a span of its own: each wait is
//! for the state the mapping itself REPORTS — the settle out of `Creating`, and the removal the
//! delete owes.

use std::sync::Arc;
use std::time::Instant;

use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::RESOURCE_NOT_FOUND_MESSAGE;
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mappings::{
    await_mapping_state, call, create_function, create_mapping, create_request, get_mapping,
    router_on, Answer, DYNAMODB_SOURCE, KINESIS_SOURCE, LOOK_AGAIN, MAPPINGS_URI, QUEUE_SOURCE,
    STATE_ENABLED,
};

/// The `State` a mapping a delete was accepted for reports, from
/// `EventSourceMappingConfiguration$State`.
const STATE_DELETING: &str = "Deleting";

/// The modelled error every call on a mapping this account no longer holds — or no longer
/// holds for changing — answers with, at HTTP 404.
const RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";

/// How long a row waits for the removal the delete owes before it is a failure: GENEROUS, so a
/// row fails only when the removal never happened at all.
const REMOVAL_BUDGET: std::time::Duration = std::time::Duration::from_secs(60);

/// `UpdateEventSourceMapping` on the mapping `uuid` names.
async fn update_mapping(router: &MultiServiceRouter, uuid: &str, body: &Value) -> Answer {
    call(router, "PUT", &format!("{MAPPINGS_URI}/{uuid}"), body).await
}

/// `DeleteEventSourceMapping` on the mapping `uuid` names.
async fn delete_mapping(router: &MultiServiceRouter, uuid: &str) -> Answer {
    call(
        router,
        "DELETE",
        &format!("{MAPPINGS_URI}/{uuid}"),
        &Value::Null,
    )
    .await
}

/// The `message` of a wire error, however the protocol spells the member.
fn message(answer: &Answer) -> String {
    answer
        .string("message")
        .or_else(|| answer.string("Message"))
        .unwrap_or_default()
}

/// The `UUID` an answer that created a mapping carries.
fn uuid_of(case: &str, answer: &Answer) -> String {
    answer
        .string("UUID")
        .unwrap_or_else(|| panic!("case {case:?}: no UUID in {}", answer.text()))
}

/// Assert `answered` is the modelled not-found, carrying the service's fixed sentence.
fn assert_not_found(case: &str, what: &str, answered: &Answer) {
    assert_eq!(
        (
            answered.status,
            answered.error_code().contains(RESOURCE_NOT_FOUND)
        ),
        (404, true),
        "case {case:?}: {what} on a mapping whose delete was accepted must be 404 \
         {RESOURCE_NOT_FOUND}, got {} {}",
        answered.status,
        answered.text()
    );
    let reported = message(answered);
    assert_eq!(
        reported, RESOURCE_NOT_FOUND_MESSAGE,
        "case {case:?}: {what} must carry the fixed not-found sentence, expected \
         {RESOURCE_NOT_FOUND_MESSAGE:?}, got {reported:?}"
    );
}

/// One row: a mapping on one event source, the update that arrives after its delete, and
/// whether the delete was accepted before or after the create's own settle.
struct Row {
    case: &'static str,
    /// The function the mapping under test is held by, and the stem of its twin's name.
    function: &'static str,
    event_source: &'static str,
    /// What the mapping is created with, so the source's own required members are stated.
    create_extras: Value,
    /// The update that arrives once the delete has been accepted — and which the twin takes
    /// at that same instant.
    update: Value,
    /// Whether the row waits out the create's settle before deleting.
    settled_first: bool,
}

/// The rows: one per event source and per member an update states, across both sides of the
/// create's own settle.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a settled queue mapping whose batch size an update states",
            function: "esm-gone-batch",
            event_source: QUEUE_SOURCE,
            create_extras: json!({ "BatchSize": 5 }),
            update: json!({ "BatchSize": 2 }),
            settled_first: true,
        },
        Row {
            case: "a queue mapping deleted inside its own creating window, then paused",
            function: "esm-gone-pause",
            event_source: QUEUE_SOURCE,
            create_extras: json!({ "BatchSize": 5 }),
            update: json!({ "Enabled": false }),
            settled_first: false,
        },
        Row {
            case: "a kinesis mapping whose filter criteria an update states",
            function: "esm-gone-filter",
            event_source: KINESIS_SOURCE,
            create_extras: json!({ "StartingPosition": "LATEST", "BatchSize": 100 }),
            update: json!({
                "FilterCriteria": { "Filters": [{ "Pattern": "{\"eventName\":[\"INSERT\"]}" }] },
            }),
            settled_first: true,
        },
        Row {
            case: "a dynamodb mapping whose retry settings an update states",
            function: "esm-gone-stream",
            event_source: DYNAMODB_SOURCE,
            create_extras: json!({ "StartingPosition": "TRIM_HORIZON" }),
            update: json!({ "MaximumRetryAttempts": 3 }),
            settled_first: false,
        },
    ]
}

/// RULE: a mapping whose delete has been accepted is no longer a mapping to change — a
/// further delete and an update of it are the modelled not-found and change nothing — while
/// a read still reports it as `Deleting` until the removal the clock owes is due.
#[tokio::test]
async fn a_mapping_whose_delete_was_accepted_refuses_every_further_change() {
    for row in rows() {
        let case = row.case;
        let engine = Arc::new(engine::InMemoryEngine::new());
        let router = router_on(&engine);
        create_function(&router, row.function).await;

        let created = create_mapping(
            &router,
            &create_request(row.function, Some(row.event_source), &row.create_extras),
        )
        .await;
        assert_eq!(
            created.status,
            202,
            "case {case:?}: the mapping under test must be created, got {} {}",
            created.status,
            created.text()
        );
        let uuid = uuid_of(case, &created);

        // The positive control: a twin on the SAME event source, held by a second function,
        // that no delete touches. (One function polls one source once, so the twin needs its
        // own function.)
        let twin_function = format!("{}-twin", row.function);
        create_function(&router, &twin_function).await;
        let twin = create_mapping(
            &router,
            &create_request(&twin_function, Some(row.event_source), &row.create_extras),
        )
        .await;
        assert_eq!(
            twin.status,
            202,
            "case {case:?}: the twin mapping must be created, got {} {}",
            twin.status,
            twin.text()
        );
        let twin_uuid = uuid_of(case, &twin);

        if row.settled_first {
            await_mapping_state(&router, case, &uuid, STATE_ENABLED).await;
        }

        let deleted = delete_mapping(&router, &uuid).await;
        assert_eq!(
            (deleted.status, deleted.string("State")),
            (202, Some(STATE_DELETING.to_string())),
            "case {case:?}: the delete must be accepted and report {STATE_DELETING}, got {} {}",
            deleted.status,
            deleted.text()
        );

        // A second delete of it addresses no mapping a call may act on.
        let again = delete_mapping(&router, &uuid).await;
        assert_not_found(case, "a second DeleteEventSourceMapping", &again);

        // Nor does an update: there is nothing left to change.
        let updated = update_mapping(&router, &uuid, &row.update).await;
        assert_not_found(case, "UpdateEventSourceMapping", &updated);

        // And neither refused call changed anything: the mapping still reads as being
        // deleted, carrying the settings the create stored.
        let reading = get_mapping(&router, &uuid).await;
        assert_eq!(
            (reading.status, reading.string("State")),
            (200, Some(STATE_DELETING.to_string())),
            "case {case:?}: a read inside the teardown must report {STATE_DELETING}, got {} {}",
            reading.status,
            reading.text()
        );
        for (member, want) in row
            .create_extras
            .as_object()
            .expect("the create's extras are an object")
        {
            // `StartingPosition` is a shard source's own create-time member, reported back
            // through its own suite; what is read here is what an update would have changed.
            if member == "StartingPosition" {
                continue;
            }
            assert_eq!(
                reading.json().get(member),
                Some(want),
                "case {case:?}: the refused update must have changed nothing, so {member} must \
                 still be {want}, got {}",
                reading.text()
            );
        }

        // The twin, which nobody deleted, takes that same update at this same instant: the
        // refusal is scoped to the mapping whose delete was accepted. A mapping still settling
        // out of its create is not available to take one, so the twin's own settle — which the
        // twin reports — is waited for first.
        await_mapping_state(&router, case, &twin_uuid, STATE_ENABLED).await;
        let twin_updated = update_mapping(&router, &twin_uuid, &row.update).await;
        assert_eq!(
            twin_updated.status,
            202,
            "case {case:?}: the twin nobody deleted must take the same update, got {} {}",
            twin_updated.status,
            twin_updated.text()
        );

        // Once the removal the clock owed is due, the mapping is held no more: the read is
        // the same not-found every call on it already answered. The removal is waited FOR —
        // looked at again until the read is that not-found — rather than slept through.
        let deadline = Instant::now() + REMOVAL_BUDGET;
        loop {
            let read = get_mapping(&router, &uuid).await;
            if read.status == 404 {
                assert_not_found(case, "GetEventSourceMapping", &read);
                break;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: the mapping {uuid} was never removed, last read {} {}",
                read.status,
                read.text()
            );
            tokio::time::sleep(LOOK_AGAIN).await;
        }
        let gone_delete = delete_mapping(&router, &uuid).await;
        assert_not_found(case, "DeleteEventSourceMapping", &gone_delete);
        let gone_update = update_mapping(&router, &uuid, &row.update).await;
        assert_not_found(case, "UpdateEventSourceMapping", &gone_update);
    }
}
