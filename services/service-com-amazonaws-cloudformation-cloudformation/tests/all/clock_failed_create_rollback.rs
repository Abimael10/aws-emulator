// SPDX-License-Identifier: CC0-1.0

//! A create the CLOCK fails while nothing is calling: what the stack settles on, and what
//! becomes of the resources it had already made.
//!
//! RULE: "ROLLBACK — Roll back the stack creation. This is the default"
//! (`CreateStackInput$OnFailure`), so a create that fails — including one failed by the clock,
//! because a wait condition's `Timeout` ran out while no client was calling — does not rest in
//! `CREATE_FAILED`: CloudFormation rolls it back, which removes in the owning services what the
//! create had already made, and the stack settles on `ROLLBACK_COMPLETE`. `CREATE_FAILED` is
//! the status of the create whose caller asked for the failed state to be KEPT
//! (`OnFailure=DO_NOTHING`, `DisableRollback`), and only of that one; it is never what a stack
//! is left in because the failure happened to be noticed by a read rather than by the call
//! that could carry the rollback out. A removal is a CALL to the service that owns the
//! resource, so a failure materialised with the store locked owes that call and cannot be
//! concluded there — concluding it there would leave a create resting in `CREATE_FAILED` with
//! the resources it had made standing and no operation left to remove them.
//!
//! The rows differ along what the rule generalises over: what the create asked for on failure
//! (the default, `OnFailure=DO_NOTHING`, and `DisableRollback=true`), which is the one thing
//! that decides whether the failed state is kept.
//!
//! Each row drives the whole sequence over the awsQuery wire, materialises the clock's own
//! settle of the store directly between reads — the pass every request of this service makes
//! with the store locked, which is what a create whose wait ran out during its own call races
//! — and reads the resource back through the service that OWNS it (AWS Systems Manager), so
//! what it asserts is what a client sees rather than what this store recorded.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use serde_json::json;
use service_com_amazonaws_cloudformation_cloudformation::state::status;
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{router_with_engine, store, REGION};
use crate::stack_lifecycle::{create_stack, member, settled_stack, xml_text};
use crate::update_rollback_recovery::{parameter, parameter_value, template, GATE_TIMEOUT};

/// How long a row gives the clock to make the expiry due, and how often it looks: the wait
/// asks for [`GATE_TIMEOUT`] seconds, and the deadline is far longer so a loaded host cannot
/// fail a row. What is asserted is the CONDITION, never how long it took.
const EXPIRY_DEADLINE: Duration = Duration::from_secs(60);
/// See [`EXPIRY_DEADLINE`].
const EXPIRY_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// How many more times the clock's own settle runs once the wait has run out, before a row
/// reads what that settle alone left the stack in: a failure the store could conclude under
/// its lock would be concluded by the first of them.
const SETTLES_AFTER_THE_EXPIRY: usize = 5;

/// One row: what the create asked for on failure, and whether the failed state is kept.
struct Row {
    case: &'static str,
    stack: &'static str,
    parameter: &'static str,
    /// The request members that say what the create wants done when it fails.
    on_failure: &'static [(&'static str, &'static str)],
    /// The `StackStatus` the create settles on.
    settles_on: &'static str,
    /// Whether the resource the create had made is still held by the service that owns it.
    keeps_the_resource: bool,
}

const ROWS: &[Row] = &[
    Row {
        case: "the default, which rolls the create back",
        stack: "clockfail-default",
        parameter: "/clockfail/default",
        on_failure: &[],
        settles_on: status::ROLLBACK_COMPLETE,
        keeps_the_resource: false,
    },
    Row {
        case: "OnFailure=ROLLBACK, stated",
        stack: "clockfail-rollback",
        parameter: "/clockfail/rollback",
        on_failure: &[("OnFailure", "ROLLBACK")],
        settles_on: status::ROLLBACK_COMPLETE,
        keeps_the_resource: false,
    },
    Row {
        case: "OnFailure=DO_NOTHING, which keeps the failed state",
        stack: "clockfail-do-nothing",
        parameter: "/clockfail/do-nothing",
        on_failure: &[("OnFailure", "DO_NOTHING")],
        settles_on: status::CREATE_FAILED,
        keeps_the_resource: true,
    },
    Row {
        case: "DisableRollback=true, which keeps the failed state",
        stack: "clockfail-disable-rollback",
        parameter: "/clockfail/disable-rollback",
        on_failure: &[("DisableRollback", "true")],
        settles_on: status::CREATE_FAILED,
        keeps_the_resource: true,
    },
];

/// The template every row deploys: a parameter of Parameter Store, and behind it a wait
/// condition nothing will ever signal, so the create is failed by the clock alone once its
/// `Timeout` runs out.
fn template_with_an_unsignalled_gate(parameter_name: &str) -> String {
    template(json!({
        "Held": parameter(parameter_name, "made-by-the-create"),
        "Gate": { "Type": "AWS::CloudFormation::WaitConditionHandle" },
        "Wait": {
            "Type": "AWS::CloudFormation::WaitCondition",
            "DependsOn": "Held",
            "Properties": {
                "Handle": { "Ref": "Gate" },
                "Timeout": GATE_TIMEOUT,
                "Count": "1",
            },
        },
    }))
}

/// Run the pass the CLOCK owes this store — the one every request of this service makes with
/// the store locked — and answer the `StackStatus` it left `stack` in, plus whether the wait
/// it is held at has run out.
///
/// Called directly, rather than through a request, because a request would also drive the
/// path that CAN carry a rollback out: what a row holds to the rule is what the locked store's
/// own settle leaves behind on its own.
fn settle_the_clock(
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    stack: &str,
) -> (String, bool) {
    let state = store(engine);
    let mut held = state.lock();
    let now = SystemTime::now();
    held.settle(now);
    let found = held.resolve(REGION, stack).unwrap_or_else(|| {
        panic!("case {case:?}: expected the store to hold the stack {stack:?}")
    });
    let expired = found
        .wait_conditions
        .iter()
        .any(|barrier| barrier.has_expired(now));
    (found.status.clone(), expired)
}

/// Settle the clock until the row's wait has run out, answering every `StackStatus` that
/// settle left the stack in along the way, newest last.
///
/// The wait is for the CONDITION — the barrier's own `Timeout` having passed — under
/// [`EXPIRY_DEADLINE`], so nothing here depends on how fast the host runs.
async fn settle_until_the_wait_runs_out(
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    stack: &str,
) -> Vec<String> {
    let deadline = Instant::now() + EXPIRY_DEADLINE;
    let mut seen: Vec<String> = Vec::new();
    loop {
        let (status, expired) = settle_the_clock(engine, case, stack);
        if seen.last().map(String::as_str) != Some(status.as_str()) {
            seen.push(status.clone());
        }
        // A status the create has already left is no longer waiting at anything, so the
        // expiry this waits for can no longer be observed on the stack.
        if expired || !status.ends_with("_IN_PROGRESS") {
            return seen;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the wait to run out, saw {seen:?}"
        );
        tokio::time::sleep(EXPIRY_POLL_INTERVAL).await;
    }
}

/// Poll `DescribeStacks` until the stack reports a status that is not an operation in flight,
/// and answer it: the status the stack SETTLED on, once the operations that can reach the
/// owning services have run.
async fn settled_status(router: &MultiServiceRouter, case: &str, stack: &str) -> String {
    let described = settled_stack(router, case, stack).await;
    xml_text(&described, "StackStatus").unwrap_or_else(|| {
        panic!("case {case:?}: expected a StackStatus for {stack:?}, got {described}")
    })
}

/// Every row: a create the clock fails is rolled back unless its caller asked for the failed
/// state to be kept — and the clock's own settle of the locked store never leaves a create
/// that is to be rolled back resting in `CREATE_FAILED`, which would strand the resources it
/// had made.
#[tokio::test]
async fn a_create_the_clock_fails_is_rolled_back_unless_its_caller_kept_the_failed_state() {
    for row in ROWS {
        let case = row.case;
        let (router, engine) = router_with_engine();
        let template = template_with_an_unsignalled_gate(row.parameter);
        let extra: Vec<(String, String)> = row
            .on_failure
            .iter()
            .map(|(key, value)| member(key, value))
            .collect();
        let (http_status, body) = create_stack(&router, row.stack, &template, &extra).await;
        assert_eq!(
            http_status, 200,
            "case {case:?}: CreateStack({}) must answer 200, got {http_status} {body}",
            row.stack
        );
        assert_eq!(
            parameter_value(&router, row.parameter).await.as_deref(),
            Some("made-by-the-create"),
            "case {case:?}: expected the create to have made {} before the gate holds it",
            row.parameter
        );

        // The clock's own pass over the locked store, run until the wait has run out and a
        // few times after that: a create whose failure owes a removal cannot be concluded
        // there, so it is still in flight; one whose failed state the caller asked to keep
        // owes nothing and may rest in it.
        let mut seen = settle_until_the_wait_runs_out(&engine, case, row.stack).await;
        for _ in 0..SETTLES_AFTER_THE_EXPIRY {
            let (status, _) = settle_the_clock(&engine, case, row.stack);
            if seen.last().map(String::as_str) != Some(status.as_str()) {
                seen.push(status.clone());
            }
        }
        if row.settles_on == status::ROLLBACK_COMPLETE {
            assert!(
                !seen.iter().any(|status| status == status::CREATE_FAILED),
                "case {case:?}: a create that is to be rolled back must not be left resting in \
                 {}, saw {seen:?}",
                status::CREATE_FAILED
            );
        }

        // What a client reads: the status the stack settled on, and whether the service that
        // owns the resource the create had made still holds it.
        let settled = settled_status(&router, case, row.stack).await;
        assert_eq!(
            settled, row.settles_on,
            "case {case:?}: expected the create the clock failed to settle on {:?}, got \
             {settled:?} after {seen:?}",
            row.settles_on
        );
        let held = parameter_value(&router, row.parameter).await;
        assert_eq!(
            held.is_some(),
            row.keeps_the_resource,
            "case {case:?}: expected Parameter Store to hold {} = {}, got {held:?}",
            row.parameter,
            row.keeps_the_resource
        );
    }
}
