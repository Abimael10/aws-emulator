// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHICH scaling configuration an Amazon SQS event source mapping's
//! drain is bounded by once an update has changed it.
//!
//! RULE: the invocations of its function a queue event source mapping has running at one
//! instant are bounded by the `ScalingConfig$MaximumConcurrency` the mapping CURRENTLY
//! carries — "limits the number of concurrent instances that the Amazon SQS event source can
//! invoke" (`smithy_query(query="shape", shape="ScalingConfig")`) — so an
//! `UpdateEventSourceMapping` stating a scaling configuration is what bounds every pass the
//! mapping makes after it, whichever way it moved the bound, and the backlog still drains with
//! every message invoked exactly once.
//!
//! What that bound DOES over a drain is the rule of
//! [`crate::sqs_event_source_maximum_concurrency_drain`], whose harness — the handler, the two
//! side-channel queues, the spans and the budgets — this suite shares rather than copies. What
//! this suite pins is the other half: the bound in force is the STORED one, so a mapping polls
//! under the configuration it carries now and not under the one it was created with.
//!
//! Each row drives real HTTP requests through one router carrying Lambda and Amazon SQS on ONE
//! engine. The mapping is created with polling paused — "when false, Lambda pauses polling and
//! invocation" — so the whole backlog and the whole reconfiguration are in place before
//! anything is polled: how many invocations the drain then runs is the mapping's own decision
//! about its current configuration rather than a question of how fast the messages arrived or
//! of which pass saw which setting. The invocations are observed the way an independent client
//! would: the handler reports each record it was handed on a SECOND queue the moment it starts
//! and then stays in its invocation until this suite releases it through a THIRD queue, so
//! "how many invocations are running at once" is a fact this suite controls rather than one it
//! times.
//!
//! The rows differ along what the rule generalises over: which way the update moved the bound
//! (given to a mapping that configured none, lowered, raised), the value it moved it to (the
//! published minimum of `MaximumConcurrency` — `smithy_query(query="shape",
//! shape="MaximumConcurrency")`: `@range(min: 2, max: 1000)` — and a value inside that range)
//! and how deep the backlog is. Every row expects the UPDATED bound, which differs from the
//! bound the mapping was created with, so a mapping still polling under its created
//! configuration fails here.
//!
//! Nothing here waits a span of its own: every wait looks again until what it waits for is
//! there or a generous deadline passes, and the window that asserts no FURTHER invocation
//! started is a poll loop over that window.

use std::time::Instant;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mappings::{
    await_mapping_state, call, create_mapping, create_queue, create_running_function, get_mapping,
    queue_held, router_with_queues, send_message, taken_bodies, Answer, LOOK_AGAIN, MAPPINGS_URI,
    STATE_DISABLED, STATE_ENABLED,
};
use crate::invoke_request_id::one_invocation_stream_at_a_time;
use crate::sqs_event_source_maximum_concurrency_drain::{
    BUDGET, HANDLER_SOURCE, QUIET_WINDOW, RELEASE, RELEASE_QUEUE_URL, STARTED_QUEUE_URL,
    TIMEOUT_SECONDS,
};

/// How many release messages a row sends per message of its backlog once it has observed the
/// bound: more than one each, so no invocation of the rest of the drain waits on a release
/// another one is holding.
const RELEASES_PER_MESSAGE: u32 = 2;

/// One row: what the mapping was created with, what an update then states, how deep the
/// backlog is — and the name every resource of the row is spelled from.
struct Row {
    case: &'static str,
    /// The prefix every resource of the row is named from.
    name: &'static str,
    /// `ScalingConfig$MaximumConcurrency` the mapping is CREATED with, or `None` for a mapping
    /// created configuring none.
    created: Option<i64>,
    /// The `ScalingConfig$MaximumConcurrency` an update then states, which is the bound the
    /// drain must then run under: the invocations the mapping may have running at one instant.
    updated: i64,
    /// How many messages are waiting before anything polls the queue, deeper than either
    /// bound, so the drain takes more than one hand-over.
    backlog: u32,
}

/// How deep every row's backlog is: deeper than any bound a row configures, so each row needs
/// more than one hand-over to drain, and shallow enough that a row holds only a handful of
/// this process's run slots while it observes them.
const BACKLOG: u32 = 4;

/// The catalog: a concurrency GIVEN to a mapping that configured none (whose created
/// configuration bounded it by its backlog alone), one LOWERED by an update, and one RAISED by
/// it — two of them to the published minimum of `MaximumConcurrency` and one to a value inside
/// the published range. Every row's backlog is deeper than the bound its update states, and
/// deeper than or equal to the bound its create stated, so the wave a row observes tells the
/// two bounds apart.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a concurrency given to a mapping created with none",
            name: "esm-updated-given",
            created: None,
            updated: 2,
            backlog: BACKLOG,
        },
        Row {
            case: "a concurrency lowered by an update",
            name: "esm-updated-lower",
            created: Some(4),
            updated: 2,
            backlog: BACKLOG,
        },
        Row {
            case: "a concurrency raised by an update",
            name: "esm-updated-raise",
            created: Some(2),
            updated: 3,
            backlog: BACKLOG,
        },
    ]
}

/// `UpdateEventSourceMapping` stating `change`, waited out to the terminal `state` the mapping
/// settles into — the change is ACCEPTED (202) and applied by the clock, so what it asked for
/// is in force once the mapping reports that state again.
async fn update_mapping(
    router: &MultiServiceRouter,
    case: &str,
    uuid: &str,
    change: &Value,
    state: &str,
) -> Answer {
    let answered = call(router, "PUT", &format!("{MAPPINGS_URI}/{uuid}"), change).await;
    assert_eq!(
        answered.status,
        202,
        "case {case:?}: expected UpdateEventSourceMapping({change}) to answer 202, got {} {}",
        answered.status,
        answered.text()
    );
    await_mapping_state(router, case, uuid, state).await;
    answered
}

/// RULE: the bound a mapping drains its backlog under is the `ScalingConfig$MaximumConcurrency`
/// it carries NOW — the one an update left it with — and the whole backlog still drains, every
/// message invoked exactly once.
#[tokio::test]
async fn an_updated_concurrency_is_the_bound_the_drain_runs_under() {
    // This emulator serves at most `state::execution::MAX_CONCURRENT_RUNS` invocations at once
    // for the whole process, and the rows below keep several of those slots held while they
    // observe them: one stream of invocations, as every suite of this target that runs
    // handlers for these rules takes.
    let _stream = one_invocation_stream_at_a_time().await;

    for row in rows() {
        let case = row.case;
        let router = router_with_queues();
        let (started_url, _) =
            create_queue(&router, case, &format!("{}-started", row.name), &json!({})).await;
        let (release_url, _) =
            create_queue(&router, case, &format!("{}-release", row.name), &json!({})).await;
        let (source_url, source_arn) = create_queue(
            &router,
            case,
            &format!("{}-source", row.name),
            &json!({ "VisibilityTimeout": TIMEOUT_SECONDS.to_string() }),
        )
        .await;
        let function = format!("{}-fn", row.name);
        create_running_function(
            &router,
            case,
            &function,
            HANDLER_SOURCE,
            TIMEOUT_SECONDS,
            &json!({ STARTED_QUEUE_URL: started_url, RELEASE_QUEUE_URL: release_url }),
        )
        .await;

        // Created with polling PAUSED, so nothing is polled while the backlog and the
        // reconfiguration are put in place.
        let mut request = json!({
            "FunctionName": function,
            "EventSourceArn": source_arn,
            "BatchSize": 1,
            "Enabled": false,
        });
        if let Some(concurrency) = row.created {
            request["ScalingConfig"] = json!({ "MaximumConcurrency": concurrency });
        }
        let created = create_mapping(&router, &request).await;
        assert_eq!(
            created.status,
            202,
            "case {case:?}: expected CreateEventSourceMapping to answer 202, got {} {}",
            created.status,
            created.text()
        );
        let uuid = created
            .string("UUID")
            .unwrap_or_else(|| panic!("case {case:?}: no UUID in {}", created.text()));
        await_mapping_state(&router, case, &uuid, STATE_DISABLED).await;

        let mut expected: Vec<String> = (0..row.backlog)
            .map(|index| format!("record-{index}"))
            .collect();
        for body in &expected {
            send_message(&router, case, &source_url, body, None).await;
        }

        // The update, and the proof that it is what the mapping now carries: a client reads the
        // stored configuration back through `GetEventSourceMapping`, which is the bound the
        // passes below are made under.
        let wanted = json!({ "MaximumConcurrency": row.updated });
        update_mapping(
            &router,
            case,
            &uuid,
            &json!({ "ScalingConfig": wanted }),
            STATE_DISABLED,
        )
        .await;
        let read = get_mapping(&router, &uuid).await;
        assert_eq!(
            read.json()["ScalingConfig"],
            wanted,
            "case {case:?}: expected the mapping to carry the scaling configuration the update \
             stated, got {}",
            read.text()
        );

        // Polling begins, under that configuration and no other.
        update_mapping(
            &router,
            case,
            &uuid,
            &json!({ "Enabled": true }),
            STATE_ENABLED,
        )
        .await;

        // Every invocation the mapping starts reports itself and then stays in its run, so the
        // records that have arrived are the invocations that are RUNNING. One record per
        // invocation, because a batch of this mapping holds one message.
        let running = usize::try_from(row.updated).unwrap_or(usize::MAX);
        let mut invoked: Vec<String> = Vec::new();
        let deadline = Instant::now() + BUDGET;
        loop {
            invoked.extend(taken_bodies(&router, case, &started_url).await);
            if invoked.len() >= running {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: expected the updated concurrency of {} to put {running} \
                 invocation(s) of the backlog of {} on the queue, got {invoked:?}",
                row.updated,
                row.backlog
            );
            tokio::time::sleep(LOOK_AGAIN).await;
        }
        let quiet_until = Instant::now() + QUIET_WINDOW;
        while Instant::now() < quiet_until {
            invoked.extend(taken_bodies(&router, case, &started_url).await);
            assert!(
                invoked.len() <= running,
                "case {case:?}: at most {running} invocation(s) may run at once under the \
                 updated concurrency of {} while every one of them is still running, got {} \
                 ({invoked:?})",
                row.updated,
                invoked.len()
            );
            tokio::time::sleep(LOOK_AGAIN).await;
        }
        assert_eq!(
            invoked.len(),
            running,
            "case {case:?}: expected exactly {running} invocation(s) to be running under the \
             updated concurrency of {}, got {invoked:?}",
            row.updated
        );

        // Released, the mapping drains what is left of the backlog, each message invoked once.
        for _ in 0..row.backlog.saturating_mul(RELEASES_PER_MESSAGE) {
            send_message(&router, case, &release_url, RELEASE, None).await;
        }
        let deadline = Instant::now() + BUDGET;
        loop {
            invoked.extend(taken_bodies(&router, case, &started_url).await);
            let held = queue_held(&router, case, &source_url).await;
            if held == 0 && invoked.len() == expected.len() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: expected the backlog of {} message(s) to be drained by one \
                 invocation each, got {held} still held and {invoked:?}",
                row.backlog
            );
            tokio::time::sleep(LOOK_AGAIN).await;
        }
        invoked.sort();
        expected.sort();
        assert_eq!(
            invoked, expected,
            "case {case:?}: expected every message of the backlog to be invoked exactly once, \
             got {invoked:?}"
        );
    }
}
