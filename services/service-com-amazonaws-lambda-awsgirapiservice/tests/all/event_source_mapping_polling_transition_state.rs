// SPDX-License-Identifier: CC0-1.0

//! WHICH transitional `State` an accepted `UpdateEventSourceMapping` reports, and that the
//! delivery gate follows it.
//!
//! The RULE pinned here: **an event source mapping reports the transitional state of the
//! change it has accepted and is still applying. `UpdateEventSourceMapping` is the call that
//! can "pause invocation and resume later", so an accepted update that turns polling OFF is
//! the pause and reports `Disabling` until it is in force, one that turns polling back ON is
//! the resume and reports `Enabling`, and one that leaves polling exactly as it found it
//! changed the mapping's configuration and reports `Updating` — the three transitional states
//! `EventSourceMappingConfiguration$State` enumerates for an update ("Creating, Enabling,
//! Enabled, Disabling, Disabled, Updating, or Deleting"). Each of them then settles into the
//! state the mapping's own `Enabled` setting asks for.**
//!
//! Why it matters to a client: pausing a consumer is a control used during maintenance, and
//! the client that uses it waits for the pause to take hold. A pause that reported the
//! configuration-update state would be indistinguishable from an unrelated re-batching, so a
//! client watching the mapping converge could not tell which change it was watching.
//!
//! The rows differ along what the rule generalises over: WHAT the update does to polling
//! (turns it off, turns it on, states the setting the mapping is already at, states no
//! `Enabled` at all, states nothing at all) and WHICH event source the mapping polls (a
//! queue and each of the two shard sources), so the state is driven by the change and not by
//! the source. Every row reads the mapping back through `GetEventSourceMapping` as well, so a
//! state that was answered but never stored fails here.
//!
//! The second test is the ROUND TRIP through a real delivery: the same pause and resume on a
//! queue mapping whose function runs, so the gate is still the mapping's state — a message
//! sent while it is paused stays on the source queue and is handed over once the resume has
//! settled. `tests/all/event_source_mapping_pause_and_resume_delivery.rs` owns that gate for
//! the two ways a mapping comes to be paused; what is added here is that it holds across the
//! states the pause and the resume now report.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::execution::{
    HOST_WAIT_CEILING, PERMIT_WAIT,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mappings::{
    await_mapping_state, call, create_function, create_mapping, create_queue, create_request,
    create_running_function, get_mapping, given_enabled_mapping, queue_message_counts,
    router_on, router_with_queues, send_message, taken_bodies, Answer, DYNAMODB_SOURCE,
    FORWARDING_HANDLER, KINESIS_SOURCE, LOOK_AGAIN, MAPPINGS_URI, QUEUE_SOURCE,
    RESULTS_QUEUE_URL, STATE_DISABLED, STATE_ENABLED,
};

/// The three transitional states an accepted update reports, from
/// `EventSourceMappingConfiguration$State`: "It can be one of the following: Creating,
/// Enabling, Enabled, Disabling, Disabled, Updating, or Deleting."
const STATE_ENABLING: &str = "Enabling";
/// See [`STATE_ENABLING`].
const STATE_DISABLING: &str = "Disabling";
/// See [`STATE_ENABLING`].
const STATE_UPDATING: &str = "Updating";

/// How long an observation of work the emulator owes — a batch polled, a handler run, a
/// message forwarded — waits before it is a failure. Generous, and taken from what the
/// emulator allows ONE attempt rather than picked here, so a row fails only when the work did
/// not happen at all.
const BUDGET: Duration = PERMIT_WAIT
    .saturating_add(HOST_WAIT_CEILING)
    .saturating_add(Duration::from_secs(60));

/// The source queue's `VisibilityTimeout` in seconds, and the function's own `Timeout`: a
/// queue maps onto a function only while it outlasts that function's timeout.
const VISIBILITY_TIMEOUT: i64 = 30;
/// See [`VISIBILITY_TIMEOUT`].
const FUNCTION_TIMEOUT: i64 = 10;

/// The body delivered while the mapping polls, and the one sent while it is paused.
const WARM_UP: &str = "delivered-before-the-pause";
/// See [`WARM_UP`].
const WITHHELD: &str = "sent-while-pausing";

/// `UpdateEventSourceMapping` on the mapping `uuid` names.
async fn update_mapping(router: &MultiServiceRouter, uuid: &str, body: &Value) -> Answer {
    call(router, "PUT", &format!("{MAPPINGS_URI}/{uuid}"), body).await
}

/// One row: the mapping the update arrives at, what the update states, the transitional state
/// it must be accepted as, and the state it must settle into.
struct Row {
    case: &'static str,
    /// The function the mapping is created on, which also names the row's resources.
    function: &'static str,
    event_source: &'static str,
    /// What the mapping is created with, so the source's own required members are stated and
    /// the row starts from a polling or a paused mapping as it needs to.
    create_extras: Value,
    /// Whether that create leaves the mapping polling, which is what the row's update then
    /// changes or leaves alone. Stated here from the REQUEST rather than read back from the
    /// store, so the row says what it drives.
    created_polling: bool,
    update: Value,
    accepted_as: &'static str,
    settles_into: &'static str,
}

/// The rows: every way an update can leave polling, on three of the sources
/// `EventSourceArn` enumerates.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a pause of a polling queue mapping",
            function: "esm-transition-pause",
            event_source: QUEUE_SOURCE,
            create_extras: json!({ "BatchSize": 5 }),
            created_polling: true,
            update: json!({ "Enabled": false }),
            accepted_as: STATE_DISABLING,
            settles_into: STATE_DISABLED,
        },
        Row {
            case: "a resume of a paused queue mapping",
            function: "esm-transition-resume",
            event_source: QUEUE_SOURCE,
            create_extras: json!({ "Enabled": false, "BatchSize": 5 }),
            created_polling: false,
            update: json!({ "Enabled": true }),
            accepted_as: STATE_ENABLING,
            settles_into: STATE_ENABLED,
        },
        Row {
            case: "a pause of a polling kinesis mapping, stated beside a configuration member",
            function: "esm-transition-pause-stream",
            event_source: KINESIS_SOURCE,
            create_extras: json!({ "StartingPosition": "LATEST", "BatchSize": 50 }),
            created_polling: true,
            update: json!({ "Enabled": false, "BatchSize": 60 }),
            accepted_as: STATE_DISABLING,
            settles_into: STATE_DISABLED,
        },
        Row {
            case: "a resume of a paused dynamodb mapping, stated beside a configuration member",
            function: "esm-transition-resume-stream",
            event_source: DYNAMODB_SOURCE,
            create_extras: json!({ "StartingPosition": "TRIM_HORIZON", "Enabled": false }),
            created_polling: false,
            update: json!({ "Enabled": true, "MaximumRetryAttempts": 4 }),
            accepted_as: STATE_ENABLING,
            settles_into: STATE_ENABLED,
        },
        Row {
            case: "polling stated as it already is on a polling mapping",
            function: "esm-transition-already-polling",
            event_source: QUEUE_SOURCE,
            create_extras: json!({ "BatchSize": 5 }),
            created_polling: true,
            update: json!({ "Enabled": true }),
            accepted_as: STATE_UPDATING,
            settles_into: STATE_ENABLED,
        },
        Row {
            case: "polling stated as it already is on a paused mapping",
            function: "esm-transition-already-paused",
            event_source: QUEUE_SOURCE,
            create_extras: json!({ "Enabled": false, "BatchSize": 5 }),
            created_polling: false,
            update: json!({ "Enabled": false }),
            accepted_as: STATE_UPDATING,
            settles_into: STATE_DISABLED,
        },
        Row {
            case: "a configuration member alone on a polling mapping",
            function: "esm-transition-configuration",
            event_source: QUEUE_SOURCE,
            create_extras: json!({ "BatchSize": 5 }),
            created_polling: true,
            update: json!({ "BatchSize": 9 }),
            accepted_as: STATE_UPDATING,
            settles_into: STATE_ENABLED,
        },
        Row {
            case: "a configuration member alone on a paused mapping",
            function: "esm-transition-configuration-paused",
            event_source: QUEUE_SOURCE,
            create_extras: json!({ "Enabled": false, "BatchSize": 5 }),
            created_polling: false,
            update: json!({ "BatchSize": 9 }),
            accepted_as: STATE_UPDATING,
            settles_into: STATE_DISABLED,
        },
        Row {
            case: "no member at all, which only re-accepts the mapping",
            function: "esm-transition-nothing",
            event_source: QUEUE_SOURCE,
            create_extras: json!({ "BatchSize": 5 }),
            created_polling: true,
            update: json!({}),
            accepted_as: STATE_UPDATING,
            settles_into: STATE_ENABLED,
        },
    ]
}

/// RULE: the transitional state an accepted update reports names the change it is applying —
/// the pause's, the resume's, or the configuration one — and the mapping then settles into
/// the state its `Enabled` setting asks for.
#[tokio::test]
async fn an_accepted_update_reports_the_transitional_state_of_the_change_it_applies() {
    for row in rows() {
        let case = row.case;
        let engine = std::sync::Arc::new(engine::InMemoryEngine::new());
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
        let uuid = created
            .string("UUID")
            .unwrap_or_else(|| panic!("case {case:?}: the create answers a UUID"));

        // The update is sent to a mapping that has left its own create window, so the state
        // it answers is the UPDATE's and not the create's.
        let started_at = if row.created_polling {
            STATE_ENABLED
        } else {
            STATE_DISABLED
        };
        await_mapping_state(&router, case, &uuid, started_at).await;

        let updated = update_mapping(&router, &uuid, &row.update).await;
        assert_eq!(
            updated.status,
            202,
            "case {case:?}: expected the modelled 202 from UpdateEventSourceMapping, got {} {}",
            updated.status,
            updated.text()
        );
        assert_eq!(
            updated.string("State").as_deref(),
            Some(row.accepted_as),
            "case {case:?}: the accepted update must report {}, got {}",
            row.accepted_as,
            updated.text()
        );

        // Read back: the state the update answered is the STORED one, not a value the answer
        // made up, and it is still that state until the clock makes the move out of it due.
        let reading = get_mapping(&router, &uuid).await;
        assert_eq!(
            (reading.status, reading.string("State")),
            (200, Some(row.accepted_as.to_string())),
            "case {case:?}: GetEventSourceMapping must report the stored {} before the settle \
             is due, got {} {}",
            row.accepted_as,
            reading.status,
            reading.text()
        );

        // And the clock then settles it into the state its `Enabled` setting asks for.
        await_mapping_state(&router, case, &uuid, row.settles_into).await;
    }
}

/// Read the results queue until `body` has been forwarded, answering every body seen.
async fn awaited_forward(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    body: &str,
) -> Vec<String> {
    let deadline = Instant::now() + BUDGET;
    let mut seen: Vec<String> = Vec::new();
    loop {
        seen.extend(taken_bodies(router, case, url).await);
        if seen.iter().any(|forwarded| forwarded == body) {
            return seen;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the handler to be handed {body:?}, it forwarded {seen:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// RULE (the delivery half): the gate is the state the mapping reports, so the pause the
/// `Disabling` window announces really stops the polling and the resume the `Enabling` window
/// announces really resumes it — the message sent meanwhile stays on the source queue and is
/// handed to the function once the resume has settled.
#[tokio::test]
async fn the_announced_pause_withholds_the_queue_and_the_announced_resume_delivers_it() {
    let case = "a queue mapping paused and resumed through the announced windows";
    let router = router_with_queues();
    let (results_url, _results_arn) =
        create_queue(&router, case, "transition-results", &json!({})).await;
    let (source_url, source_arn) = create_queue(
        &router,
        case,
        "transition-source",
        &json!({ "VisibilityTimeout": VISIBILITY_TIMEOUT.to_string() }),
    )
    .await;
    let function = "transition-gate-fn";
    create_running_function(
        &router,
        case,
        function,
        FORWARDING_HANDLER,
        FUNCTION_TIMEOUT,
        &json!({ RESULTS_QUEUE_URL: results_url }),
    )
    .await;

    let uuid = given_enabled_mapping(
        &router,
        case,
        &json!({
            "FunctionName": function,
            "EventSourceArn": source_arn,
            "BatchSize": 1,
        }),
    )
    .await;

    // The positive control: while it polls, a message sent to the source queue reaches the
    // handler, which forwards it.
    send_message(&router, case, &source_url, WARM_UP, None).await;
    let forwarded = awaited_forward(&router, case, &results_url, WARM_UP).await;
    assert_eq!(
        forwarded,
        vec![WARM_UP.to_string()],
        "case {case:?}: the polling mapping must hand the handler exactly the message it was \
         sent, it forwarded {forwarded:?}"
    );

    // The pause, announced as its own window and then in force.
    let paused = update_mapping(&router, &uuid, &json!({ "Enabled": false })).await;
    assert_eq!(
        (paused.status, paused.string("State")),
        (202, Some(STATE_DISABLING.to_string())),
        "case {case:?}: the accepted pause must report {STATE_DISABLING}, got {} {}",
        paused.status,
        paused.text()
    );
    await_mapping_state(&router, case, &uuid, STATE_DISABLED).await;

    // Paused: the message sent now is still WAITING on the source queue — no consumer took
    // it, so it is neither in flight nor gone — and the handler was handed nothing.
    send_message(&router, case, &source_url, WITHHELD, None).await;
    let counts = queue_message_counts(&router, case, &source_url).await;
    assert_eq!(
        counts,
        (1, 0, 0),
        "case {case:?}: a paused mapping polls nothing, so the message must be waiting on the \
         source queue, got {counts:?} as (visible, in flight, delayed)"
    );
    let quiet = taken_bodies(&router, case, &results_url).await;
    assert!(
        quiet.is_empty(),
        "case {case:?}: a paused mapping invokes nothing, so the handler can forward nothing, \
         but it forwarded {quiet:?}"
    );

    // The resume, announced as its own window and then in force: the same mapping polls the
    // message it was withholding and hands it over.
    let resumed = update_mapping(&router, &uuid, &json!({ "Enabled": true })).await;
    assert_eq!(
        (resumed.status, resumed.string("State")),
        (202, Some(STATE_ENABLING.to_string())),
        "case {case:?}: the accepted resume must report {STATE_ENABLING}, got {} {}",
        resumed.status,
        resumed.text()
    );
    await_mapping_state(&router, case, &uuid, STATE_ENABLED).await;
    let delivered = awaited_forward(&router, case, &results_url, WITHHELD).await;
    assert_eq!(
        delivered,
        vec![WITHHELD.to_string()],
        "case {case:?}: the mapping resumed must hand the handler the withheld message and \
         nothing else, it forwarded {delivered:?}"
    );
}
