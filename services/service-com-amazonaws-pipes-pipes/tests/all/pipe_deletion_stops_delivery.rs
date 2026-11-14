// SPDX-License-Identifier: CC0-1.0

//! What a pipe the service no longer holds does to its source and its target: NOTHING.
//!
//! The RULE asserted over the table below, in one sentence: a pipe's source is read and its
//! target delivered to only while the service holds a record for that pipe that rests in
//! `RUNNING`, so a pipe whose record `DeletePipe` removed takes nothing more out of its
//! source and hands nothing more to its target — the records sent after the delete stay on
//! the queue, visible and counted, for whichever consumer takes them next — while every
//! OTHER pipe keeps delivering, because the record each poller reads is its own pipe's.
//! (`DeletePipe` "delete an existing pipe", and a pipe is RUNNING or not per
//! `smithy_query(query="shape", shape="PipeState,RequestedPipeState")`; the data movement
//! itself is not modelled, so it is asserted here where a client sees it.)
//!
//! Every call goes through `MultiServiceRouter::dispatch` — the one path a socket-served
//! request takes — with pipes and Amazon SQS on ONE engine, so the source the pipe reads and
//! the target it writes are the queues a client created and reads. Nothing in this suite
//! delivers anything itself: every message on a target queue was put there by the pipe.
//!
//! The rows differ along what the rule generalises over: how many records the pipe moved
//! before it was deleted (none, one batch, several batches), the `BatchSize` it moved them
//! under (one, a divisor of the backlog, a ceiling above it and the member's absence), how
//! many records are sent afterwards, and whether the pipe was already `STOPPED` when the
//! delete arrived.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_pipes_pipes::state::ComAmazonawsPipesPipesState;
use smithy_server_runtime::MultiServiceRouter;

use crate::pipe_data_plane::{
    collect_at_least, collect_during, emulator_with_engine, given_queue, markers, send, visible,
    LOOK_AGAIN, QUIET,
};
use crate::pipe_execution_role::wait_for_state;
use crate::pipe_lifecycle::{call, describe, Reply, REGION};

/// The role every pipe below names. Nothing in this suite turns on its policies: no IAM
/// store is mounted, so no stage is refused on the strength of one.
const ROLE: &str = "arn:aws:iam::000000000000:role/pipes-deletion";

/// How long a wait for work the pipe has to do — settling into a state, moving a batch — is
/// given before the row is a failure. Well above the poll interval and the settle span, so a
/// row fails only when the work did not happen at all.
const BUDGET: Duration = Duration::from_secs(30);

/// Pipes and Amazon SQS on one engine, as `core/router/src/lib.rs` mounts them, with the
/// engine handed back: the suite reads the pipes store directly to see when a deleted pipe's
/// poller has ended ([`await_poller_ended`]), which no client call reports. The assembly is
/// the data-plane suite's own ([`emulator_with_engine`]).
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    emulator_with_engine()
}

/// The name a row's pipe and queues are built from: the case with nothing in it a pipe name
/// (`^[\.\-_A-Za-z0-9]+$`) or an Amazon SQS queue name does not admit.
fn slug(case: &str) -> String {
    case.replace(' ', "-")
}

/// `CreatePipe` of a pipe whose source and target are the queues `source_arn` and
/// `target_arn` name, polling under `batch_size` when the row names one.
async fn create_pipe(
    router: &MultiServiceRouter,
    name: &str,
    source_arn: &str,
    target_arn: &str,
    batch_size: Option<i64>,
) -> Reply {
    let mut queue_parameters = serde_json::Map::new();
    if let Some(batch_size) = batch_size {
        queue_parameters.insert("BatchSize".to_string(), json!(batch_size));
    }
    let body = json!({
        "Source": source_arn,
        "Target": target_arn,
        "RoleArn": ROLE,
        "DesiredState": "RUNNING",
        "SourceParameters": { "SqsQueueParameters": Value::Object(queue_parameters) },
    });
    call(
        router,
        "POST",
        &format!("/v1/pipes/{name}"),
        Some(body),
        REGION,
    )
    .await
}

/// Wait until no data-plane poller of the pipe `name` is running any more.
///
/// A poller's claim on its pipe is held for exactly as long as that poller runs and is given
/// up inside the critical section the pass that found the pipe gone decided in
/// (`ComAmazonawsPipesPipesState::claim_poller`), so TAKING that claim is the one way to
/// observe that the background task has ended — nothing a client can call reports it. The
/// claim is then this suite's and the pipe is gone, so no later poller is kept out of
/// anything.
async fn await_poller_ended(engine: &Arc<engine::InMemoryEngine>, case: &str, name: &str) {
    let deadline = Instant::now() + BUDGET;
    loop {
        {
            let state = engine.state::<ComAmazonawsPipesPipesState>();
            let mut store = state.lock();
            if store.claim_poller(name) {
                return;
            }
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the poller of the deleted pipe {name:?} is still running"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The bodies `delivered` carries, as the set of the source records they were derived from:
/// an Amazon SQS target receives one record envelope per message, whose `body` is the text
/// its producer sent.
fn delivered_bodies(case: &str, delivered: &[String]) -> BTreeSet<String> {
    delivered
        .iter()
        .map(|body| {
            let parsed: Value = serde_json::from_str(body).unwrap_or_else(|error| {
                panic!("case {case:?}: a delivered body is not JSON ({error}): {body:?}")
            });
            parsed["body"].as_str().unwrap_or_default().to_string()
        })
        .collect()
}

/// One row: how many records the pipe moves before the delete, how many are sent after it,
/// the `BatchSize` it polls under and whether it was stopped before being deleted.
struct Row {
    case: &'static str,
    before: usize,
    after: usize,
    /// `SourceParameters.SqsQueueParameters.BatchSize`, or `None` for the member's absence.
    batch_size: Option<i64>,
    stopped_first: bool,
}

/// RULE: a pipe reads its source and delivers to its target only while the service holds a
/// record for it that rests in `RUNNING` — so once `DeletePipe` has removed that record
/// nothing more is taken from the source and nothing more reaches the target, whatever the
/// pipe had moved before and whatever it was configured to move it under.
#[tokio::test]
async fn a_deleted_pipe_takes_nothing_more_from_its_source() {
    let rows = [
        Row {
            case: "deleted after a backlog of three",
            before: 3,
            after: 2,
            batch_size: None,
            stopped_first: false,
        },
        Row {
            case: "one record per batch",
            before: 2,
            after: 3,
            batch_size: Some(1),
            stopped_first: false,
        },
        Row {
            case: "nothing was ever delivered",
            before: 0,
            after: 4,
            batch_size: Some(2),
            stopped_first: false,
        },
        Row {
            case: "stopped before the delete",
            before: 2,
            after: 2,
            batch_size: Some(5),
            stopped_first: true,
        },
        Row {
            case: "a batch ceiling above the backlog",
            before: 5,
            after: 1,
            batch_size: Some(10),
            stopped_first: false,
        },
    ];
    for row in &rows {
        let case = row.case;
        let name = slug(case);
        let (router, engine) = emulator();
        let (source_url, source_arn) = given_queue(&router, &format!("{name}-source")).await;
        let (target_url, target_arn) = given_queue(&router, &format!("{name}-target")).await;

        let created = create_pipe(
            &router,
            &name,
            &source_arn,
            &target_arn,
            row.batch_size,
        )
        .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: CreatePipe answered {} {}",
            created.status, created.body
        );

        // What the pipe moves while it still exists: the control that tells a deleted pipe
        // from one that never worked.
        if row.before > 0 {
            let sent = markers(&format!("{name}-before"), row.before);
            send(&router, &source_url, &sent).await;
            let delivered = collect_at_least(&router, &target_url, sent.len(), BUDGET).await;
            let bodies = delivered_bodies(case, &delivered);
            let expected: BTreeSet<String> = sent.iter().cloned().collect();
            assert_eq!(
                bodies, expected,
                "case {case:?}: a RUNNING pipe must deliver its source records, got {bodies:?}"
            );
        }

        if row.stopped_first {
            let stopped = call(
                &router,
                "POST",
                &format!("/v1/pipes/{name}/stop"),
                None,
                REGION,
            )
            .await;
            assert_eq!(
                stopped.status, 200,
                "case {case:?}: StopPipe answered {} {}",
                stopped.status, stopped.body
            );
            let settled = wait_for_state(&router, &name, "STOPPED", BUDGET).await;
            assert_eq!(
                settled, "STOPPED",
                "case {case:?}: the stopped pipe must settle into STOPPED, it reports \
                 {settled:?}"
            );
        }

        let deleted = call(&router, "DELETE", &format!("/v1/pipes/{name}"), None, REGION).await;
        assert_eq!(
            deleted.status, 200,
            "case {case:?}: DeletePipe answered {} {}",
            deleted.status, deleted.body
        );
        let gone = describe(&router, &name).await;
        assert_eq!(
            (gone.status, gone.error_code.as_str()),
            (404, "NotFoundException"),
            "case {case:?}: a deleted pipe must be gone from every reader, got {} {}",
            gone.status,
            gone.body
        );
        await_poller_ended(&engine, case, &name).await;

        // The records that arrive after the delete: the queue's own, and nobody else's.
        let after = markers(&format!("{name}-after"), row.after);
        send(&router, &source_url, &after).await;
        let delivered = collect_during(&router, &target_url, QUIET).await;
        assert!(
            delivered.is_empty(),
            "case {case:?}: a deleted pipe's target must receive nothing, got {delivered:?}"
        );
        let still = visible(&router, &source_url).await;
        assert_eq!(
            still,
            i64::try_from(after.len()).expect("a row's record count fits"),
            "case {case:?}: the {} record(s) sent after the delete must stay visible on the \
             source, {still} are",
            after.len()
        );
    }
}

/// RULE, the other half: the record a poller reads is its OWN pipe's, so deleting one pipe
/// is not a shut-down of the data plane — a pipe nobody deleted keeps polling its source and
/// delivering to its target while the deleted one's source fills up.
#[tokio::test]
async fn deleting_one_pipe_leaves_every_other_pipe_delivering() {
    let case = "one of two pipes deleted";
    let (router, engine) = emulator();
    let (deleted_source, deleted_source_arn) = given_queue(&router, "kept-apart-source").await;
    let (deleted_target, deleted_target_arn) = given_queue(&router, "kept-apart-target").await;
    let (live_source, live_source_arn) = given_queue(&router, "live-pipe-source").await;
    let (live_target, live_target_arn) = given_queue(&router, "live-pipe-target").await;

    for (name, source_arn, target_arn) in [
        ("deleted-pipe", &deleted_source_arn, &deleted_target_arn),
        ("live-pipe", &live_source_arn, &live_target_arn),
    ] {
        let created = create_pipe(&router, name, source_arn, target_arn, Some(10)).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: CreatePipe {name:?} answered {} {}",
            created.status, created.body
        );
    }

    // Both pipes work to begin with, so what changes afterwards is the delete and nothing
    // else.
    for (label, source_url, target_url) in [
        ("deleted-pipe", &deleted_source, &deleted_target),
        ("live-pipe", &live_source, &live_target),
    ] {
        let sent = markers(&format!("{label}-first"), 2);
        send(&router, source_url, &sent).await;
        let delivered = collect_at_least(&router, target_url, sent.len(), BUDGET).await;
        let bodies = delivered_bodies(case, &delivered);
        let expected: BTreeSet<String> = sent.iter().cloned().collect();
        assert_eq!(
            bodies, expected,
            "case {case:?}: the pipe {label:?} must deliver its source records before the \
             delete, got {bodies:?}"
        );
    }

    let deleted = call(
        &router,
        "DELETE",
        "/v1/pipes/deleted-pipe",
        None,
        REGION,
    )
    .await;
    assert_eq!(
        deleted.status, 200,
        "case {case:?}: DeletePipe answered {} {}",
        deleted.status, deleted.body
    );
    await_poller_ended(&engine, case, "deleted-pipe").await;

    // The live pipe is untouched by the other pipe's delete: it moves the records sent to
    // its own source, while the deleted pipe's source keeps everything sent to it.
    let held = markers("deleted-pipe-after", 3);
    send(&router, &deleted_source, &held).await;
    let moved = markers("live-pipe-after", 3);
    send(&router, &live_source, &moved).await;

    let delivered = collect_at_least(&router, &live_target, moved.len(), BUDGET).await;
    let bodies = delivered_bodies(case, &delivered);
    let expected: BTreeSet<String> = moved.iter().cloned().collect();
    assert_eq!(
        bodies, expected,
        "case {case:?}: the pipe nobody deleted must keep delivering, got {bodies:?}"
    );
    let stranded = collect_during(&router, &deleted_target, QUIET).await;
    assert!(
        stranded.is_empty(),
        "case {case:?}: the deleted pipe's target must receive nothing, got {stranded:?}"
    );
    let still = visible(&router, &deleted_source).await;
    assert_eq!(
        still,
        i64::try_from(held.len()).expect("a row's record count fits"),
        "case {case:?}: the deleted pipe's source must still hold its {} record(s), {still} \
         are visible",
        held.len()
    );
}
