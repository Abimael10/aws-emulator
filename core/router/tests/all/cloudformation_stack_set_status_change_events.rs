// SPDX-License-Identifier: CC0-1.0

//! The rule an OPERATOR writes on the default event bus for a stack set: "tell me when a
//! rollout FAILS", observed where a client observes it — on the Amazon SQS queue that rule
//! delivers to.
//!
//! The rule pinned here: **a stack-set status-change event goes through the BUS, so a rule
//! whose `EventPattern` requires `detail.status-details.status` `FAILED` receives the
//! conclusion of an operation whose instance's stack failed and NOTHING else of that
//! operation — not its RUNNING start, and not the instance's own change, which states
//! `status` `OUTDATED` (its `detailed-status` is what is `FAILED`) and so matches no pattern
//! on `status`.** The alarm therefore fires exactly once, when the rollout fails. A pattern
//! is matched against what the event STATES, so this is the same publication the suite beside
//! this one reads in full ([`crate::cloudformation_stackset_failed_instance_events`], whose
//! wire harness this suite drives) seen through the filter a client really writes.
//!
//! Why it matters: the point of publishing a rollout's statuses is that nobody has to poll
//! for them. An alarm on failure is a rule with a status pattern, and it is worth nothing if
//! the pattern matches the start as well as the conclusion, or if the conclusion never
//! arrives.

use serde_json::{json, Value};

use crate::cloudformation_stackset_failed_instance_events::{
    assert_envelope, collect_until, deployed_stack_set, drain, operation_event, router,
    settled_operation, status_of, unsignalled_wait_template, wired_queue, FAILED,
    INSTANCE_CHANGE, OPERATION_CHANGE, RUNNING, SOURCE, SUCCEEDED,
};
use crate::query_wire::opt_element;

/// The pattern an operator's "tell me when a rollout failed" rule carries: this service's
/// stack-set events, narrowed to the ones reporting a FAILED status.
fn only_failed_events() -> Value {
    json!({
        "source": [SOURCE],
        "detail-type": [OPERATION_CHANGE, INSTANCE_CHANGE],
        "detail": { "status-details": { "status": [FAILED] } },
    })
}

/// A rule matching only FAILED receives the failed operation's conclusion, and the start of
/// that same operation never reaches it.
#[tokio::test]
async fn a_failed_only_rule_receives_the_conclusion_and_not_the_start() {
    let case = "an operator's rule matching only a FAILED status";
    let stack_set = "events-failed-only-rule";
    let router = router();
    let alarm = wired_queue(
        &router,
        case,
        &format!("{stack_set}-alarm"),
        &only_failed_events(),
    )
    .await;
    let (stack_set_arn, operation_id) =
        deployed_stack_set(&router, case, stack_set, &unsignalled_wait_template()).await;

    // The operation concludes FAILED, which is the change the rule is waiting for.
    let settled = settled_operation(&router, case, stack_set, &operation_id).await;
    let reported = opt_element(&settled, "Status").unwrap_or_default();
    assert_eq!(
        reported, FAILED,
        "case {case:?}: expected the operation to conclude {FAILED}, got {settled}"
    );

    let mut seen: Vec<Value> = Vec::new();
    collect_until(&router, case, &alarm, &mut seen, |seen| {
        operation_event(seen, &operation_id, FAILED).is_some()
    })
    .await;
    let concluded = operation_event(&seen, &operation_id, FAILED)
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected the {OPERATION_CHANGE:?} reporting {FAILED} for \
                 {operation_id} to reach the rule's queue, got {seen:?}"
            )
        })
        .clone();
    assert_envelope(case, &concluded, &stack_set_arn);

    // Everything the bus had to deliver has been delivered — the conclusion above arrived, and
    // EventBridge matched each event of this operation against every rule of the bus as it was
    // published — so what this rule has not received, it never will.
    seen.extend(drain(&router, case, &alarm).await);
    let delivered: Vec<&str> = seen.iter().map(status_of).collect();
    assert!(
        !delivered.contains(&RUNNING) && !delivered.contains(&SUCCEEDED),
        "case {case:?}: a rule matching only {FAILED} must receive no other status, got \
         {delivered:?}"
    );
}
