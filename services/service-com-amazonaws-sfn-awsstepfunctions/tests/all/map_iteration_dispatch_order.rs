// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the order an INLINE `Map` state's iteration events are recorded
//! in, driven over this service's own wire protocol (`awsJson1_0`, `POST /` with
//! `X-Amz-Target: AWSStepFunctions.<Operation>`) through the production
//! `MultiServiceRouter`.
//!
//! The rule under test: a `Map` state DISPATCHES the iterations its concurrency admits
//! together, so every one of them is STARTED — and recorded as started — before any
//! iteration body runs. A state that declares no `MaxConcurrency` admits every item at
//! once, so its history opens the state with one started event per item; one that
//! declares a figure admits that many at a time, so the started events come in blocks of
//! that size, each block ahead of the bodies of the iterations it dispatched.
//!
//! This is what a client reconstructs the fan-out from: how many iterations were
//! dispatched, and when. A serialized walk — one iteration started, run and finished
//! before the next is started — reads as a `Map` that ran with a concurrency of one
//! whatever the definition declared.
//!
//! The rows differ along what the rule generalises over: the item count, the declared
//! `MaxConcurrency` (none, one, fewer than the items, more than the items), and whether
//! the items divide evenly into blocks of it.
//!
//! The harness (`Emulator`, `create_machine`, `start_execution`, `describe_execution`,
//! `string`) is the one `state_machine_lifecycle` builds and the history reader is
//! `execution_history`'s; `tests/all/main.rs` picks this file up, so each is reached
//! through `crate::<suite>`.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::execution_history::history;
use crate::state_machine_lifecycle::{
    create_machine, describe_execution, start_execution, string, Emulator,
};

/// How long a row waits for an execution to reach a terminal status: generous, because
/// what it waits for is the emulator's own settle delay and not the host's speed.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);

/// The events the iterations of an inline `Map` are bracketed by.
const ITERATION_STARTED: &str = "MapIterationStarted";
const ITERATION_SUCCEEDED: &str = "MapIterationSucceeded";
const STATE_STARTED: &str = "MapStateStarted";

/// Poll `execution` until it reports a status other than `RUNNING`, and answer that
/// record. Panics naming `case` when [`SETTLE_DEADLINE`] passes first.
async fn settled(emulator: &Emulator, case: &str, execution: &str) -> Value {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let described = describe_execution(emulator, execution).await;
        if string(&described, "status") != "RUNNING" {
            return described;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the execution to reach a terminal status, got {described:?}"
        );
        ::tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// A definition whose inline `Map` iterates `$.items` with one `Pass` per item, carrying
/// `MaxConcurrency` only when `concurrency` names one.
fn definition(concurrency: Option<i64>) -> String {
    let mut state = json!({
        "Type": "Map",
        "ItemsPath": "$.items",
        "ItemProcessor": {
            "ProcessorConfig": { "Mode": "INLINE" },
            "StartAt": "Each",
            "States": { "Each": { "Type": "Pass", "End": true } },
        },
        "End": true,
    });
    if let Some(concurrency) = concurrency {
        state["MaxConcurrency"] = json!(concurrency);
    }
    json!({ "StartAt": "Fan", "States": { "Fan": state } }).to_string()
}

/// The blocks of consecutive `MapIterationStarted` events in `types`, in order: one
/// figure per block, which is how many iterations were dispatched together.
fn dispatch_blocks(types: &[String]) -> Vec<usize> {
    let mut blocks = Vec::new();
    let mut run = 0usize;
    for event in types {
        if event == ITERATION_STARTED {
            run += 1;
            continue;
        }
        if run > 0 {
            blocks.push(run);
            run = 0;
        }
    }
    if run > 0 {
        blocks.push(run);
    }
    blocks
}

/// Every iteration the concurrency admits is started before any iteration body runs.
#[tokio::test]
async fn an_inline_map_dispatches_every_admitted_iteration_before_running_one() {
    // (case, item count, declared MaxConcurrency, the blocks the started events form)
    let rows: Vec<(&str, usize, Option<i64>, Vec<usize>)> = vec![
        (
            "two items and no declared concurrency, so both at once",
            2,
            None,
            vec![2],
        ),
        (
            "three items and no declared concurrency",
            3,
            None,
            vec![3],
        ),
        (
            "a single item, which is the whole dispatch",
            1,
            None,
            vec![1],
        ),
        (
            "a concurrency of one, which dispatches them one at a time",
            3,
            Some(1),
            vec![1, 1, 1],
        ),
        (
            "a concurrency the items divide evenly into",
            4,
            Some(2),
            vec![2, 2],
        ),
        (
            "a concurrency the items do NOT divide evenly into",
            5,
            Some(2),
            vec![2, 2, 1],
        ),
        (
            "a concurrency larger than the item count, which admits them all",
            3,
            Some(10),
            vec![3],
        ),
        (
            "the documented no-limit figure, which admits them all too",
            4,
            Some(0),
            vec![4],
        ),
    ];

    let emulator = Emulator::new();
    let mut started = Vec::with_capacity(rows.len());
    for (index, (_, items, concurrency, _)) in rows.iter().enumerate() {
        let machine = create_machine(
            &emulator,
            &format!("dispatch-{index}"),
            &definition(*concurrency),
        )
        .await;
        let input = json!({ "items": (0..*items).collect::<Vec<usize>>() }).to_string();
        started.push(start_execution(&emulator, &machine, "only", &input).await);
    }

    for ((case, items, _, blocks), execution) in rows.iter().zip(started) {
        let described = settled(&emulator, case, &execution).await;
        assert_eq!(
            described.get("status").and_then(Value::as_str),
            Some("SUCCEEDED"),
            "case {case:?}: expected the Map to have run every item, got {described:?}"
        );
        let types: Vec<String> = history(&emulator, &execution, json!({}))
            .await
            .iter()
            .map(|event| string(event, "type"))
            .collect();
        assert_eq!(
            &dispatch_blocks(&types),
            blocks,
            "case {case:?}: expected the iterations to be dispatched in blocks of \
             {blocks:?}, got {types:?}"
        );
        // One started and one succeeded event per item, however they were blocked.
        for event in [ITERATION_STARTED, ITERATION_SUCCEEDED] {
            let counted = types.iter().filter(|found| *found == event).count();
            assert_eq!(
                counted, *items,
                "case {case:?}: expected {items} {event} events, got {counted} in {types:?}"
            );
        }
        // And the FIRST block opens the state: nothing of an iteration's body sits
        // between the state starting and the iterations it dispatched.
        let opened: Vec<&String> = types
            .iter()
            .skip_while(|event| *event != STATE_STARTED)
            .skip(1)
            .take(blocks[0])
            .collect();
        assert!(
            opened.iter().all(|event| *event == ITERATION_STARTED),
            "case {case:?}: expected the state to open with its dispatch, got {types:?}"
        );
    }
}
