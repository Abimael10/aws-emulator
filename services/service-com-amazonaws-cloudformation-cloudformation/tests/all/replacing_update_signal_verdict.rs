// SPDX-License-Identifier: CC0-1.0

//! The UPDATE side of the computed signal status: an `UpdatePolicy`
//! `AutoScalingReplacingUpdate` whose REPLACEMENT group's instances compute a FAILURE.
//!
//! RULE: **the status a signalled resource's instance asks its stack for is the status
//! PERFORMING that instance's boot configuration asks for**, which
//! `computed_instance_signal_status.rs` states in full and pins over the create. This suite is
//! that same rule on the operation the policy exists for: `AutoScalingReplacingUpdate` with
//! `WillReplace: true` builds a NEW group for the update and holds the logical resource at the
//! replacement's own `CreationPolicy` signals, so a replacement whose instances compute a
//! FAILURE is rejected — the update fails and is rolled back, the stack reports the FAILURE
//! signal on the group, and the logical resource points back at the group the stack had — while
//! a success quorum would commit it.
//!
//! Why a client cares: `WillReplace` exists precisely so a bad revision is caught before it
//! takes over, and a deployment pipeline that reads `UPDATE_COMPLETE` as "the new fleet
//! reported healthy" draws the opposite conclusion from the one the fleet reported.

use crate::computed_instance_signal_status::{
    computes_the_status_from_an_exit_code, event_trail, group_template, physical_id,
    settled_status, signals_of_status, GATE,
};
use crate::stack_lifecycle::{call, create_stack, member, router};

#[tokio::test]
async fn a_replacement_whose_bootstrap_computes_a_failure_is_rejected() {
    const CASE: &str = "a replacing update whose replacement group's instances compute a \
                        FAILURE from their own exit code";
    let stack = "computed-signal-replacing-update";
    let router = router();
    // The configuration the stack is CREATED with: a bootstrap whose own exit code is zero, so
    // the create's gate is released by its instances and the update starts from a group that is
    // really serving.
    let created = group_template(&computes_the_status_from_an_exit_code("0"));
    let (code, body) = create_stack(&router, stack, &created, &[]).await;
    assert_eq!(
        code, 200,
        "case {CASE:?}: expected CreateStack to succeed, got {code} {body}"
    );
    let status = settled_status(CASE, &router, stack).await;
    assert_eq!(
        status, "CREATE_COMPLETE",
        "case {CASE:?}: expected the signalled create to complete, got {status:?}; events {:?}",
        event_trail(&router, stack).await
    );
    let group_before = physical_id(&router, stack, GATE).await;
    assert!(
        !group_before.is_empty(),
        "case {CASE:?}: expected the created stack to report a group"
    );

    // The update rewrites the launch template's `UserData`, which changes the group's
    // `LaunchTemplate` property — the change the `UpdatePolicy` governs — and the replacement's
    // instances compute a FAILURE from their own exit code.
    let changed = group_template(&computes_the_status_from_an_exit_code("1"));
    let (code, body) = call(
        &router,
        "UpdateStack",
        &[member("StackName", stack), member("TemplateBody", &changed)],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {CASE:?}: expected UpdateStack to be accepted, got {code} {body}"
    );
    let status = settled_status(CASE, &router, stack).await;
    assert_eq!(
        status, "UPDATE_ROLLBACK_COMPLETE",
        "case {CASE:?}: expected the rejected replacement to roll the update back, got \
         {status:?}; events {:?}",
        event_trail(&router, stack).await
    );

    // ...and the FAILURE the replacement's instances reported is what rejected it: the signal
    // is on the group's own events, under the id of the instance that sent it.
    let failed = signals_of_status(&router, stack, GATE, "FAILURE").await;
    assert!(
        !failed.is_empty(),
        "case {CASE:?}: expected the replacement's FAILURE signal to be reported for {GATE}, \
         got the events {:?}",
        event_trail(&router, stack).await
    );
    for unique_id in &failed {
        assert!(
            crate::harness::is_service_assigned_id(unique_id, "i"),
            "case {CASE:?}: expected the failure signal's UniqueId to be the instance id that \
             sent it, got {unique_id:?}"
        );
    }
    // The logical resource points back at the group the stack had: the rejected replacement is
    // not the group the stack reports.
    let group_after = physical_id(&router, stack, GATE).await;
    assert_eq!(
        group_after, group_before,
        "case {CASE:?}: expected the rejected replacement to leave the original group in \
         force, got {group_after:?} against {group_before:?}; events {:?}",
        event_trail(&router, stack).await
    );
}
