// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the two readings every `EventSourceMappingConfiguration` answer
//! carries about a mapping's own history: WHEN it last changed, and what its poller last
//! came to.
//!
//! Two rules, over the one shape `CreateEventSourceMapping`, `GetEventSourceMapping`,
//! `UpdateEventSourceMapping`, `DeleteEventSourceMapping` and `ListEventSourceMappings` all
//! answer:
//!
//! 1. `LastModified` ("The date that the event source mapping was last updated or that its
//!    state changed") is reported in the form this `restJson1` service's protocol gives a
//!    timestamp the model declares no format on — the epoch-seconds NUMBER — at the
//!    WHOLE-SECOND resolution the service reports that date at, rather than as a sub-second
//!    reading of the emulator's own clock. It is the same reading whichever operation answers
//!    the mapping, and it never goes backwards.
//! 2. `LastProcessingResult` ("The result of the event source mapping's last processing
//!    attempt") is a reading of the POLLER making those attempts: a mapping whose polling is
//!    paused ("When false, Lambda pauses polling and invocation") has no poller and so no
//!    attempt to report, and the member is absent from the update that pauses it, from every
//!    read of it while it is paused, and from the delete of it — until a resumed poller
//!    attempts a batch of its own and reports what that attempt came to. The first suite
//!    carries the paused reading of every source it spans: a mapping whose poller attempted
//!    nothing reports what that source reports before a first attempt, whether its polling
//!    was paused or never started.
//!
//! Both are driven over the ONE mapping harness this test binary has
//! ([`crate::event_source_mappings`]), through real HTTP requests to the router, and every
//! reading is taken back out of the store through a read operation, so a value that was
//! answered but never persisted fails here. The first suite's rows differ along what rule 1
//! generalises over: WHICH event source the mapping polls (a queue, each of the two shard
//! sources and an MSK cluster), whether it was created polling or paused, and which
//! operation answered the mapping (the create, the read, the listing, the update and the
//! delete). The second suite drives a mapping that really polls, so the result it reports is
//! one its poller recorded; its passes are the suite's own
//! ([`crate::event_source_mappings::DrivenPolling`]), so nothing here waits on how fast the
//! host runs.

use serde_json::{json, Value};

use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mappings::{
    await_mapping_state, call, create_function, create_mapping, create_queue, create_request,
    create_running_function, driven_router_with_queues, get_mapping, given_enabled_mapping,
    list_mappings, queue_held, router_on, send_message, shard_source_readings, Answer,
    DYNAMODB_SOURCE, FORWARDING_HANDLER, KINESIS_SOURCE, MAPPINGS_URI, MSK_SOURCE, QUEUE_SOURCE,
    RESULTS_QUEUE_URL, STATE_DISABLED, STATE_ENABLED,
};

/// The transitional state an update that PAUSES polling answers, from
/// `EventSourceMappingConfiguration$State`: "It can be one of the following: Creating,
/// Enabling, Enabled, Disabling, Disabled, Updating, or Deleting."
const STATE_DISABLING: &str = "Disabling";

/// `LastProcessingResult` after an attempt that delivered its batch to the function, as
/// `crate::event_source_mappings` states it for every suite over a delivery.
const RESULT_OK: &str = "OK";

/// How long the handler of the polling suite has to run in, and how many messages one of its
/// batches may carry: one message per pass, so each pass is one attempt whose result is the
/// one the row reads.
const HANDLER_TIMEOUT_SECONDS: i64 = 5;

/// One row of the instant-form suite: the mapping to create, the event source it polls and
/// the state it is in once its create has settled.
struct Row {
    case: &'static str,
    function: &'static str,
    event_source: &'static str,
    /// What the create states beside the function and the event source.
    extras: Value,
    /// The `State` the mapping reports once every transition its create owes is due, which
    /// is also the state it returns to after a configuration change settles.
    settles_into: &'static str,
    /// Whether the create owes a transition at all: a mapping asked for with polling PAUSED
    /// has no poller to stand up and is created in its terminal state, so nothing moves its
    /// `LastModified` after the create answered.
    owes_transition: bool,
}

/// The rows: every kind of event source whose mapping this harness can create without the
/// source's own service, created both polling and paused.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a queue mapping created polling",
            function: "esm-instant-queue",
            event_source: QUEUE_SOURCE,
            extras: json!({}),
            settles_into: STATE_ENABLED,
            owes_transition: true,
        },
        Row {
            case: "a queue mapping created paused",
            function: "esm-instant-queue-paused",
            event_source: QUEUE_SOURCE,
            extras: json!({ "Enabled": false }),
            settles_into: STATE_DISABLED,
            owes_transition: false,
        },
        Row {
            case: "a kinesis mapping created polling",
            function: "esm-instant-kinesis",
            event_source: KINESIS_SOURCE,
            extras: json!({ "StartingPosition": "LATEST" }),
            settles_into: STATE_ENABLED,
            owes_transition: true,
        },
        Row {
            case: "a dynamodb stream mapping created paused",
            function: "esm-instant-dynamodb",
            event_source: DYNAMODB_SOURCE,
            extras: json!({ "Enabled": false, "StartingPosition": "TRIM_HORIZON" }),
            settles_into: STATE_DISABLED,
            owes_transition: false,
        },
        Row {
            case: "an msk mapping created polling",
            function: "esm-instant-msk",
            event_source: MSK_SOURCE,
            extras: json!({ "StartingPosition": "LATEST", "Topics": ["orders"] }),
            settles_into: STATE_ENABLED,
            owes_transition: true,
        },
    ]
}

/// The whole number of seconds since the epoch `value` names, or `None` for a value that is
/// not that at all — a string, or a number carrying a fractional part.
///
/// `Value::as_i64` is what pins BOTH halves of the form: it answers `Some` only for a JSON
/// NUMBER, and only for one whose text carries no fraction, which is the whole-second
/// resolution the date is reported at.
fn epoch_seconds(value: Option<&Value>) -> Option<i64> {
    value?.as_i64()
}

/// Assert the `LastModified` of `answered` is a whole-second epoch instant at or after
/// `floor`, and answer the instant it names.
fn asserted_instant(case: &str, label: &str, answered: &Value, floor: Option<i64>) -> i64 {
    let reported = answered.get("LastModified");
    let seconds = epoch_seconds(reported).unwrap_or_else(|| {
        panic!(
            "case {case:?}: {label} must report LastModified as a whole-second epoch number, \
             got {reported:?} in {answered}"
        )
    });
    assert!(
        seconds > 0,
        "case {case:?}: {label} must report an instant past the epoch, got {seconds} in \
         {answered}"
    );
    if let Some(floor) = floor {
        assert!(
            seconds >= floor,
            "case {case:?}: {label} must report LastModified at or after {floor}, got \
             {seconds} in {answered}"
        );
    }
    seconds
}

/// RULE 1: every operation that answers a mapping reports `LastModified` as the whole-second
/// epoch instant of the mapping's last change, and it never goes backwards over the
/// mapping's life.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_mapping_reports_its_last_change_date_in_whole_epoch_seconds() {
    let engine = std::sync::Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    for row in rows() {
        let case = row.case;
        create_function(&router, row.function).await;
        let created = create_mapping(
            &router,
            &create_request(row.function, Some(row.event_source), &row.extras),
        )
        .await;
        assert_eq!(
            created.status,
            202,
            "case {case:?}: CreateEventSourceMapping must answer 202, got {} {}",
            created.status,
            created.text()
        );
        let uuid = created
            .string("UUID")
            .unwrap_or_else(|| panic!("case {case:?}: the create answers a UUID, got {}", created.text()));
        let at_create = asserted_instant(case, "the create", &created.json(), None);

        // The read and the listing answer the stored instant, in the same form — and the
        // same reading, for a mapping whose create owes no transition to move it.
        let fetched = get_mapping(&router, &uuid).await;
        let at_read = asserted_instant(case, "the read-back", &fetched.json(), Some(at_create));
        if !row.owes_transition {
            assert_eq!(
                fetched.json().get("LastModified"),
                created.json().get("LastModified"),
                "case {case:?}: nothing has changed the mapping, so the read-back reports the \
                 instant the create stored, got {} after {}",
                fetched.text(),
                created.text()
            );
        }
        let listed = list_mappings(&router, &format!("EventSourceArn={}", row.event_source)).await;
        let reported = listed
            .mappings()
            .into_iter()
            .find(|mapping| mapping.get("UUID").and_then(Value::as_str) == Some(uuid.as_str()))
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: the listing must report this mapping, got {}",
                    listed.text()
                )
            });
        let at_listing = asserted_instant(case, "the listing", &reported, Some(at_read));

        // A change re-stamps it, in the same form, and the delete's own answer carries it
        // too. Each call waits for the state the one before it owes, because a mapping is
        // available to be updated only once its create has settled and to be deleted only
        // once its update has.
        await_mapping_state(&router, case, &uuid, row.settles_into).await;
        let settled = get_mapping(&router, &uuid).await;
        let at_settled = asserted_instant(case, "the settled read", &settled.json(), Some(at_listing));
        let updated = call(
            &router,
            "PUT",
            &format!("{MAPPINGS_URI}/{uuid}"),
            &json!({ "BatchSize": 7 }),
        )
        .await;
        assert_eq!(
            updated.status,
            202,
            "case {case:?}: UpdateEventSourceMapping must answer 202, got {} {}",
            updated.status,
            updated.text()
        );
        let at_update = asserted_instant(case, "the update", &updated.json(), Some(at_settled));
        await_mapping_state(&router, case, &uuid, row.settles_into).await;

        // And the pause, which ends the attempt history a POLLING mapping reports (rule 2):
        // a mapping whose poller has attempted nothing still reports what its own source
        // reports before a first attempt — nothing at all for a queue, an MSK cluster or an
        // Amazon MQ broker, and the no-records reading for a shard source, whose poller
        // reports the absence of an attempt as a reading of its own
        // ([`crate::event_source_mappings::shard_source_readings`]).
        let paused = call(
            &router,
            "PUT",
            &format!("{MAPPINGS_URI}/{uuid}"),
            &json!({ "Enabled": false }),
        )
        .await;
        let unattempted = shard_source_readings(Some(row.event_source)).1;
        assert_eq!(
            paused.string("LastProcessingResult").as_deref(),
            unattempted,
            "case {case:?}: a paused mapping whose poller attempted nothing reports \
             {unattempted:?}, got {}",
            paused.text()
        );
        await_mapping_state(&router, case, &uuid, STATE_DISABLED).await;
        let deleted = delete_mapping(&router, &uuid).await;
        assert_eq!(
            deleted.status,
            202,
            "case {case:?}: DeleteEventSourceMapping must answer 202, got {} {}",
            deleted.status,
            deleted.text()
        );
        asserted_instant(case, "the delete", &deleted.json(), Some(at_update));
    }
}

/// RULE 2: the result a mapping reports is its POLLER's — an attempt it made while polling,
/// absent for as long as polling is paused, and reported again once a resumed poller has
/// attempted a batch.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_paused_mapping_reports_no_processing_attempt() {
    let case = "a queue mapping paused after a delivery and resumed";
    let (router, mut driven) = driven_router_with_queues();
    let (source_url, source_arn) = create_queue(&router, case, "esm-attempt-source", &json!({})).await;
    let (results_url, _) = create_queue(&router, case, "esm-attempt-results", &json!({})).await;
    create_running_function(
        &router,
        case,
        "esm-attempt-consumer",
        FORWARDING_HANDLER,
        HANDLER_TIMEOUT_SECONDS,
        &json!({ RESULTS_QUEUE_URL: results_url }),
    )
    .await;
    let uuid = given_enabled_mapping(
        &router,
        case,
        &json!({
            "FunctionName": "esm-attempt-consumer",
            "EventSourceArn": source_arn,
            "BatchSize": 1,
        }),
    )
    .await;

    // A delivery the poller made: the mapping reports what that attempt came to.
    send_message(&router, case, &source_url, "before the pause", None).await;
    driven.poll_once().await;
    let polled = get_mapping(&router, &uuid).await;
    assert_eq!(
        polled.string("LastProcessingResult").as_deref(),
        Some(RESULT_OK),
        "case {case:?}: the mapping must report the attempt its poller made, got {}",
        polled.text()
    );

    // The pause: the update that stops the poller reports the disable's own state and no
    // attempt at all, and neither does any read of the paused mapping.
    let paused = call(
        &router,
        "PUT",
        &format!("{MAPPINGS_URI}/{uuid}"),
        &json!({ "Enabled": false }),
    )
    .await;
    assert_eq!(
        (paused.status, paused.string("State").as_deref()),
        (202, Some(STATE_DISABLING)),
        "case {case:?}: the pause must answer 202 and the disable's own state, got {} {}",
        paused.status,
        paused.text()
    );
    assert_eq!(
        paused.json().get("LastProcessingResult"),
        None,
        "case {case:?}: a mapping nothing is polling reports no processing attempt, got {}",
        paused.text()
    );
    await_mapping_state(&router, case, &uuid, STATE_DISABLED).await;
    let while_paused = get_mapping(&router, &uuid).await;
    assert_eq!(
        while_paused.json().get("LastProcessingResult"),
        None,
        "case {case:?}: the read-back of a paused mapping reports no processing attempt, got {}",
        while_paused.text()
    );
    let listed = list_mappings(&router, &format!("EventSourceArn={source_arn}")).await;
    let reported = listed
        .mappings()
        .into_iter()
        .find(|mapping| mapping.get("UUID").and_then(Value::as_str) == Some(uuid.as_str()))
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: the listing must report this mapping, got {}",
                listed.text()
            )
        });
    assert_eq!(
        reported.get("LastProcessingResult"),
        None,
        "case {case:?}: the listing of a paused mapping reports no processing attempt, got \
         {reported}"
    );

    // And the resume: a pass of the poller that is running again reports its own attempt.
    resumed(&router, case, &uuid).await;
    send_message(&router, case, &source_url, "after the resume", None).await;
    driven.poll_once().await;
    let polled_again = get_mapping(&router, &uuid).await;
    assert_eq!(
        polled_again.string("LastProcessingResult").as_deref(),
        Some(RESULT_OK),
        "case {case:?}: the resumed poller's own attempt is what the mapping reports, got {}",
        polled_again.text()
    );
    let held = queue_held(&router, case, &source_url).await;
    assert_eq!(
        held, 0,
        "case {case:?}: both messages were processed, so the source must be drained, got \
         {held} held"
    );

    // The delete of a mapping that was paused again carries no attempt either: its last
    // description is the one a client logs.
    let repaused = call(
        &router,
        "PUT",
        &format!("{MAPPINGS_URI}/{uuid}"),
        &json!({ "Enabled": false }),
    )
    .await;
    assert_eq!(
        repaused.status,
        202,
        "case {case:?}: the second pause must answer 202, got {} {}",
        repaused.status,
        repaused.text()
    );
    await_mapping_state(&router, case, &uuid, STATE_DISABLED).await;
    let deleted = delete_mapping(&router, &uuid).await;
    assert_eq!(
        deleted.json().get("LastProcessingResult"),
        None,
        "case {case:?}: the delete of a paused mapping reports no processing attempt, got {}",
        deleted.text()
    );
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

/// Resume polling on the mapping `uuid` names and wait until the resume is in force, so the
/// pass made after it is a pass of a poller that is running.
async fn resumed(router: &MultiServiceRouter, case: &str, uuid: &str) {
    let resumed = call(
        router,
        "PUT",
        &format!("{MAPPINGS_URI}/{uuid}"),
        &json!({ "Enabled": true }),
    )
    .await;
    assert_eq!(
        resumed.status,
        202,
        "case {case:?}: the resume must answer 202, got {} {}",
        resumed.status,
        resumed.text()
    );
    await_mapping_state(router, case, uuid, STATE_ENABLED).await;
}
