// SPDX-License-Identifier: CC0-1.0

//! The dependency graph a client builds with `dependsOn`, as a WHOLE: two parents and the two
//! jobs waiting behind them, submitted together and then only polled.
//!
//! The RULE under test is the one [`crate::batch_job_dependency_gate`] states over its table —
//! a dependent is held in `PENDING`, released when every job it names has `SUCCEEDED`, and
//! failed undispatched when one of them `FAILED`. What this suite adds is the SHAPE of a
//! client's whole workflow: both branches of the graph in one estate, settling without any
//! call but `DescribeJobs`, with containers whose programs STATE a wait between their steps —
//! the form a batch workload is written in (`sleep …; exit …`) and the form that would hold
//! the gate for as long as a caller's own words if a stated duration were spent.
//!
//! The helpers, the estate and the assertions' sources are that suite's; only the graph is
//! this one's.

use std::time::Instant;

use serde_json::Value;

use crate::batch_job_dependency_gate::{
    described, estate, instant, network_of, register_definition, router, submit, terminal,
    until_left_submitted, until_terminal, DEADLINE, ESSENTIAL_CONTAINER_EXITED, FAILED, PENDING,
    POLL_INTERVAL, SUCCEEDED,
};

/// The two job definitions of this graph: one whose container exits 0 after stating a wait,
/// one whose container exits 9 after stating one. Both state the wait BETWEEN their steps, so
/// each job settles on the status its container exited with rather than on the caller's
/// seconds.
const SUCCEEDING: &str = "jd-graph-succeeding";
const FAILING: &str = "jd-graph-failing";

/// RULE: both branches of a `dependsOn` graph reach a verdict — the dependent of the parent
/// that succeeds runs after it, the dependent of the parent that fails is failed without ever
/// being dispatched — and the graph settles whatever duration the containers' programs state.
#[tokio::test]
async fn both_branches_of_a_dependency_graph_reach_their_verdict() {
    let router = router();
    let (subnet, security_group) = network_of(&router).await;
    estate(&router, &subnet, &security_group).await;
    // The waits are stated between the program's steps, and they are longer than this suite's
    // whole deadline: a graph that SPENT them could not settle inside it at all.
    register_definition(
        &router,
        SUCCEEDING,
        &["/bin/sh", "-c", "sleep 300; exit 0"],
    )
    .await;
    register_definition(&router, FAILING, &["/bin/sh", "-c", "sleep 300; exit 9"]).await;

    let case = "a graph of two branches";
    let good_parent = submit(&router, case, "good-parent", SUCCEEDING, &[]).await;
    let good_child = submit(
        &router,
        case,
        "good-child",
        SUCCEEDING,
        std::slice::from_ref(&good_parent),
    )
    .await;
    let bad_parent = submit(&router, case, "bad-parent", FAILING, &[]).await;
    let bad_child = submit(
        &router,
        case,
        "bad-child",
        SUCCEEDING,
        std::slice::from_ref(&bad_parent),
    )
    .await;

    // Each child leaves SUBMITTED into PENDING, echoing the parent it waits on and reporting
    // nothing of a run — while that parent is still UNFINISHED, which is what the hold is
    // observed against (the assertion [`crate::batch_job_dependency_gate`] makes over its own
    // table): a child is held by the submit that named the dependency, so the read that finds
    // one held can be earlier than its parent's own first move.
    for (child, parent) in [(&good_child, &good_parent), (&bad_child, &bad_parent)] {
        let (held, parent_status) = until_left_submitted(&router, case, child, parent).await;
        assert_eq!(
            held["status"].as_str(),
            Some(PENDING),
            "case {case:?}: a child whose parent has not succeeded is held in {PENDING}, got \
             {held}"
        );
        assert!(
            !terminal(&parent_status),
            "case {case:?}: the parent must still have been unfinished while its child was \
             held, got {parent_status:?}"
        );
        assert_eq!(
            held["dependsOn"][0]["jobId"].as_str(),
            Some(parent.as_str()),
            "case {case:?}: the held child echoes the parent it waits on, got {held}"
        );
        assert_eq!(
            instant(&held, "startedAt"),
            None,
            "case {case:?}: a held child has not started, got {held}"
        );
    }

    // The parent that exits 0 succeeds, and its child then runs its own container: one
    // attempt, exit status 0, and a start no earlier than the parent's stop.
    let parent_detail = until_terminal(&router, case, &good_parent).await;
    assert_eq!(
        parent_detail["status"].as_str(),
        Some(SUCCEEDED),
        "case {case:?}: a parent whose container exits 0 succeeds, got {parent_detail}"
    );
    let parent_stop = instant(&parent_detail, "stoppedAt").unwrap_or_else(|| {
        panic!("case {case:?}: a job that ran reports stoppedAt, got {parent_detail}")
    });
    let child_detail = until_terminal(&router, case, &good_child).await;
    assert_eq!(
        child_detail["status"].as_str(),
        Some(SUCCEEDED),
        "case {case:?}: the released child runs and succeeds, got {child_detail}"
    );
    assert_eq!(
        child_detail["container"]["exitCode"].as_i64(),
        Some(0),
        "case {case:?}: the released child reports the status its container exited with, got \
         {child_detail}"
    );
    let child_start = instant(&child_detail, "startedAt").unwrap_or_else(|| {
        panic!("case {case:?}: a child that ran reports startedAt, got {child_detail}")
    });
    assert!(
        child_start >= parent_stop,
        "case {case:?}: the child starts no earlier than its parent stopped, got startedAt \
         {child_start} against {parent_stop}"
    );

    // The parent that exits 9 fails, and its child is failed by the GATE: nothing of it ran.
    let failed_parent = until_terminal(&router, case, &bad_parent).await;
    assert_eq!(
        failed_parent["status"].as_str(),
        Some(FAILED),
        "case {case:?}: a parent whose container exits non-zero fails, got {failed_parent}"
    );
    assert_eq!(
        failed_parent["container"]["exitCode"].as_i64(),
        Some(9),
        "case {case:?}: the failing parent reports the status its container exited with, got \
         {failed_parent}"
    );
    let blocked = until_terminal(&router, case, &bad_child).await;
    assert_eq!(
        blocked["status"].as_str(),
        Some(FAILED),
        "case {case:?}: a child whose parent failed fails too, got {blocked}"
    );
    assert_eq!(
        blocked["attempts"].as_array().map(Vec::len),
        Some(0),
        "case {case:?}: a child the gate failed was never dispatched, got {blocked}"
    );
    assert_eq!(
        instant(&blocked, "startedAt"),
        None,
        "case {case:?}: a child the gate failed never started, got {blocked}"
    );
    assert!(
        blocked["container"]["exitCode"].is_null(),
        "case {case:?}: no exit code is reported for a container that never ran, got {blocked}"
    );
    let reason = blocked["statusReason"].as_str().unwrap_or_default();
    assert!(
        !reason.is_empty() && reason != ESSENTIAL_CONTAINER_EXITED,
        "case {case:?}: the failure explains the dependency rather than reporting a container \
         that exited, got {reason:?}"
    );

    // And the graph is at REST: every job of it is terminal, and a later read says the same.
    let deadline = Instant::now() + DEADLINE;
    loop {
        let mut settled = Vec::new();
        for job in [&good_parent, &good_child, &bad_parent, &bad_child] {
            let detail: Value = described(&router, case, job).await;
            settled.push(terminal(detail["status"].as_str().unwrap_or_default()));
        }
        if settled.iter().all(|done| *done) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: every job of the graph must come to rest, got {settled:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}
