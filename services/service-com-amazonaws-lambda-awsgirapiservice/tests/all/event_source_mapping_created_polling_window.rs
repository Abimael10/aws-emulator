// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the window `CreateEventSourceMapping` opens — the one a mapping is
//! transitional in, and the one it is "not available" to be changed in.
//!
//! The rule under test: what a create has to STAND UP is the POLLING it was asked for.
//! `Enabled` is "when true, the event source mapping is active. When false, Lambda pauses
//! polling and invocation. Default: True", so a create that asks for an ACTIVE mapping is
//! accepted rather than applied (202) and answers the transitional `Creating` until the
//! clock settles it into `Enabled`, while a create that asks for polling to be PAUSED asks
//! for no poller at all and answers the terminal `Disabled` it asked for — on the create,
//! on the read-back and in the listing, with nothing left for the clock to bring. The same
//! rule decides the availability window `ResourceInUseException` names ("you tried to
//! update an event source mapping in the CREATING state"): a mapping that was never
//! `Creating` takes a change straight away.
//!
//! The rows span what the rule generalises over: HOW the polling setting is stated
//! (omitted, `true`, `false`) crossed with WHICH event source the mapping polls (a queue,
//! both shard sources and an MSK cluster) — the mapping's own source decides its defaults
//! and its poller, and may not decide the state its create answers.
//!
//! Every row drives real HTTP requests through the router, over the ONE mapping harness
//! this test binary has ([`crate::event_source_mappings`]), and reads each mapping back
//! through `GetEventSourceMapping` and `ListEventSourceMappings`, so a state that was
//! answered but never stored fails here.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mappings::{
    await_mapping_state, call, create_function, create_mapping, create_request, created_state,
    get_mapping, list_mappings, router_on, Answer, DYNAMODB_SOURCE, KINESIS_SOURCE, MAPPINGS_URI,
    MSK_SOURCE, QUEUE_SOURCE, STATE_CREATING, STATE_DISABLED, STATE_ENABLED,
};

/// One row: the mapping to create and the terminal state its polling setting asks for.
struct Row {
    name: &'static str,
    function: &'static str,
    event_source: &'static str,
    /// What the create states beside the function and the event source.
    extras: Value,
    /// The `State` the mapping is in once every transition its create owed is in force.
    settles_into: &'static str,
}

/// The rows: the three ways the polling setting is stated, over the event sources whose
/// pollers differ.
fn rows() -> Vec<Row> {
    vec![
        Row {
            name: "a queue mapping stating no polling setting",
            function: "esm-window-queue-default",
            event_source: QUEUE_SOURCE,
            extras: json!({}),
            settles_into: STATE_ENABLED,
        },
        Row {
            name: "a queue mapping asked to be active",
            function: "esm-window-queue-active",
            event_source: QUEUE_SOURCE,
            extras: json!({ "Enabled": true }),
            settles_into: STATE_ENABLED,
        },
        Row {
            name: "a queue mapping asked for with polling paused",
            function: "esm-window-queue-paused",
            event_source: QUEUE_SOURCE,
            extras: json!({ "Enabled": false }),
            settles_into: STATE_DISABLED,
        },
        Row {
            name: "a kinesis mapping asked for with polling paused",
            function: "esm-window-kinesis-paused",
            event_source: KINESIS_SOURCE,
            extras: json!({ "Enabled": false, "StartingPosition": "LATEST" }),
            settles_into: STATE_DISABLED,
        },
        Row {
            name: "a dynamodb stream mapping asked for with polling paused",
            function: "esm-window-dynamodb-paused",
            event_source: DYNAMODB_SOURCE,
            extras: json!({ "Enabled": false, "StartingPosition": "TRIM_HORIZON" }),
            settles_into: STATE_DISABLED,
        },
        Row {
            name: "an msk mapping asked for with polling paused",
            function: "esm-window-msk-paused",
            event_source: MSK_SOURCE,
            extras: json!({ "Enabled": false, "Topics": ["orders"] }),
            settles_into: STATE_DISABLED,
        },
        Row {
            name: "an msk mapping asked to be active",
            function: "esm-window-msk-active",
            event_source: MSK_SOURCE,
            extras: json!({ "Enabled": true, "Topics": ["orders"] }),
            settles_into: STATE_ENABLED,
        },
    ]
}

/// `UpdateEventSourceMapping` on the mapping `uuid` names, asking for nothing but the
/// batch size — a change every source admits, so what a row asserts about it is the
/// mapping's availability and not the member the request stated.
async fn update_batch_size(router: &MultiServiceRouter, uuid: &str) -> Answer {
    call(
        router,
        "PUT",
        &format!("{MAPPINGS_URI}/{uuid}"),
        &json!({ "BatchSize": 4 }),
    )
    .await
}

#[tokio::test]
async fn a_create_answers_the_transitional_state_only_where_it_stands_a_poller_up() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);

    for row in rows() {
        let case = row.name;
        create_function(&router, row.function).await;
        let created = create_mapping(
            &router,
            &create_request(row.function, Some(row.event_source), &row.extras),
        )
        .await;
        assert_eq!(
            created.status,
            202,
            "case {case:?}: the create must answer 202 Accepted, got {} {}",
            created.status,
            created.text()
        );
        // The expectation is read off the REQUEST, through the one statement of the rule
        // these suites share, rather than off the answer being checked.
        let expected = created_state(&row.extras);
        assert_eq!(
            created.string("State"),
            Some(expected.to_string()),
            "case {case:?}: expected State {expected}, got {}",
            created.text()
        );
        let uuid = created.string("UUID").unwrap_or_else(|| {
            panic!(
                "case {case:?}: the create answers a UUID, got {}",
                created.text()
            )
        });

        // A mapping created in its terminal state has nothing left for the clock to bring,
        // so every reader reports that state at once; one created transitional may already
        // have settled by the time the next read reaches the store, which is the other
        // state its own row admits.
        let admissible = |state: Option<&str>| {
            state.is_some_and(|reported| reported == expected || reported == row.settles_into)
        };
        let fetched = get_mapping(&router, &uuid).await;
        assert!(
            (fetched.status, admissible(fetched.string("State").as_deref())) == (200, true),
            "case {case:?}: the read-back must report {expected} or {}, got {} {}",
            row.settles_into,
            fetched.status,
            fetched.text()
        );

        // The availability window `ResourceInUseException` names is the create's own
        // stand-up: a mapping that was never `Creating` takes a change straight away.
        if expected == STATE_DISABLED {
            let updated = update_batch_size(&router, &uuid).await;
            assert_eq!(
                updated.status,
                202,
                "case {case:?}: a mapping created in its terminal state is available to be \
                 updated at once, got {} {}",
                updated.status,
                updated.text()
            );
        }

        // Whatever the row asked for, the mapping ends up in the state its polling setting
        // asks for: waited for by LOOKING for it, never by sleeping a span.
        await_mapping_state(&router, case, &uuid, row.settles_into).await;
        let settled = get_mapping(&router, &uuid).await;
        assert_eq!(
            (settled.status, settled.string("State")),
            (200, Some(row.settles_into.to_string())),
            "case {case:?}: expected the terminal {} once every transition was in force, \
             got {} {}",
            row.settles_into,
            settled.status,
            settled.text()
        );
    }
}

#[tokio::test]
async fn a_mapping_created_paused_is_never_reported_transitional_by_any_reader() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let function = "esm-window-readers";
    create_function(&router, function).await;

    // One mapping per source, all paused, so the three readers are compared over a set of
    // mappings rather than over one.
    let mut created = Vec::new();
    for (index, (source, extras)) in [
        (QUEUE_SOURCE, json!({ "Enabled": false })),
        (
            KINESIS_SOURCE,
            json!({ "Enabled": false, "StartingPosition": "LATEST" }),
        ),
        (
            DYNAMODB_SOURCE,
            json!({ "Enabled": false, "StartingPosition": "TRIM_HORIZON" }),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let case = format!("paused mapping {index} on {source}");
        let answered =
            create_mapping(&router, &create_request(function, Some(source), &extras)).await;
        assert_eq!(
            (answered.status, answered.string("State")),
            (202, Some(STATE_DISABLED.to_string())),
            "case {case:?}: a create that pauses polling answers {STATE_DISABLED}, got {} {}",
            answered.status,
            answered.text()
        );
        created.push((case, answered.string("UUID").expect("the create answers a UUID")));
    }

    let listed = list_mappings(&router, &format!("FunctionName={function}")).await;
    for (case, uuid) in &created {
        let fetched = get_mapping(&router, uuid).await;
        assert_eq!(
            (fetched.status, fetched.string("State")),
            (200, Some(STATE_DISABLED.to_string())),
            "case {case:?}: the read-back of a paused mapping reports {STATE_DISABLED}, got \
             {} {}",
            fetched.status,
            fetched.text()
        );
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
            reported.get("State").and_then(Value::as_str),
            Some(STATE_DISABLED),
            "case {case:?}: the listing reports {STATE_DISABLED} too, got {reported}"
        );
        assert_ne!(
            reported.get("State").and_then(Value::as_str),
            Some(STATE_CREATING),
            "case {case:?}: no reader may report a state nothing is being stood up for, got \
             {reported}"
        );
    }
}
