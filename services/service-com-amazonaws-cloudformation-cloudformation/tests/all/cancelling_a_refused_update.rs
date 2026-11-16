// SPDX-License-Identifier: CC0-1.0

//! `CancelUpdateStack` against an update whose own deployment has ALREADY been refused, end
//! to end over the awsQuery wire.
//!
//! **The rule every case here holds the service to: an update is IN FLIGHT from the call that
//! started it until the clock makes that update's conclusion due, whichever way that
//! conclusion went — so the stack reports `UPDATE_IN_PROGRESS` for that whole span and
//! `CancelUpdateStack` is answered throughout it, with HTTP 200 and the empty
//! `CancelUpdateStackResponse` envelope, and the update is then reverted as a cancelled
//! update is.** A deployment is performed by the call that submits it — it asks the services
//! that own the resources for what the template declares — so that call can come back knowing
//! the update cannot be carried through; that is not an update that concluded sooner, and the
//! failure it reached is reported when the conclusion falls due.
//!
//! The rows span the class the rule generalises over — HOW the update's conclusion was
//! reached before the clock made it due: a resource whose change the owning service refused, a
//! deployment CloudFormation refused before asking any owning service at all, the caller that
//! asked for the failed state to be kept (`DisableRollback`), and an update nothing refused at
//! all as the control. The same rule is held for an update a change set EXECUTES, which is the
//! other call that puts one in flight.
//!
//! Nothing here sleeps for the span the clock owes: the conclusion's own due instant is moved
//! — out, to hold the update open for the span the case acts in, and into the past, to settle
//! it — so what a row asserts is the state the service reports, never how fast the host runs.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use service_com_amazonaws_cloudformation_cloudformation::state::status;
use smithy_server_runtime::MultiServiceRouter;

use crate::change_set_lifecycle::settle_the_store;
use crate::stack_lifecycle::{
    call, create_stack, describe_stack, member, router_with_engine, settled_stack, store,
    xml_text, REGION,
};

/// The `StackStatusReason` a cancelled update and the rollback it owes carry, as the service
/// words it — spelled out here, because the sentence is the requirement.
const CANCELLED_BY_USER: &str = "Stack update cancelled by user.";

/// How far out a case moves the due instant of the conclusion the clock owes, to hold the
/// update open while it acts.
///
/// Generous: the span is the emulator's own and a loaded host must not decide any row, so what
/// is held open outlasts every call a case makes inside it by a wide margin.
const HELD_OPEN: Duration = Duration::from_secs(600);

/// A bucket declared under `name`, with whatever else the stage declares for it.
fn bucket(name: &str, extra: &str) -> String {
    format!(
        r#"{{"Resources":{{"Store":{{"Type":"AWS::S3::Bucket","Properties":{{
             "BucketName":"{name}"{extra}}}}}}}}}"#
    )
}

/// An SSM parameter declared under `value`.
fn parameter(name: &str, value: &str) -> String {
    format!(
        r#"{{"Resources":{{"Param":{{"Type":"AWS::SSM::Parameter","Properties":{{
             "Name":"{name}","Type":"String","Value":"{value}"}}}}}}}}"#
    )
}

/// Move the due instant of the transition the stack owes OUT, so the operation in flight stays
/// in flight while the case acts, and answer whether there was one to move.
///
/// The conclusion of a stack operation is due at an instant this service records, so a case
/// about the span before that instant moves the instant rather than racing it. A stack with no
/// transition owed has nothing to hold — which is itself what a row asserts against, through
/// the status the service then reports.
fn hold_the_conclusion_open(engine: &Arc<engine::InMemoryEngine>, stack: &str) -> bool {
    let state = store(engine);
    let mut held = state.lock();
    let Some(stored) = held.resolve_mut(REGION, stack) else {
        return false;
    };
    let Some(pending) = stored.pending.as_mut() else {
        return false;
    };
    pending.due_at = SystemTime::now() + HELD_OPEN;
    true
}

/// `CancelUpdateStack`, answered with the HTTP status and the XML body.
async fn cancel_update(router: &MultiServiceRouter, stack: &str) -> (u16, String) {
    call(router, "CancelUpdateStack", &[member("StackName", stack)]).await
}

/// The `StackStatus` and `StackStatusReason` the stack reports right now.
async fn reported(case: &str, router: &MultiServiceRouter, stack: &str) -> (String, String) {
    let (http_status, body) = describe_stack(router, stack).await;
    assert_eq!(
        http_status, 200,
        "case {case:?}: expected DescribeStacks to answer 200, got {http_status} {body}"
    );
    (
        xml_text(&body, "StackStatus").unwrap_or_default(),
        xml_text(&body, "StackStatusReason").unwrap_or_default(),
    )
}

/// The cancel itself: it must be answered with the empty result in this operation's own
/// envelope, which is what an SDK reads the call's success out of.
async fn assert_the_cancel_is_answered(case: &str, router: &MultiServiceRouter, stack: &str) {
    let (http_status, body) = cancel_update(router, stack).await;
    assert_eq!(
        http_status, 200,
        "case {case:?}: expected CancelUpdateStack to be answered, got {http_status} {body}"
    );
    assert!(
        body.contains("<CancelUpdateStackResponse"),
        "case {case:?}: expected the operation's own response envelope, got {body:?}"
    );
    let request_id = xml_text(&body, "RequestId").unwrap_or_default();
    assert!(
        !request_id.is_empty(),
        "case {case:?}: expected the envelope to carry a RequestId, got {body:?}"
    );
}

/// Every stack-level event of `stack` that reports `reported`, counted: the stack's own
/// transitions carry its name as their `LogicalResourceId`.
///
/// What this pins is that ONE rollback was opened — the cancellation's. A conclusion reported
/// a second time would open a second one, and the terminal status alone cannot tell the two
/// apart.
async fn stack_transitions(
    case: &str,
    router: &MultiServiceRouter,
    stack: &str,
    reported: &str,
) -> usize {
    crate::update_rollback_recovery::stack_events(case, router, stack)
        .await
        .into_iter()
        .filter(|(logical_id, event_status)| logical_id == stack && event_status == reported)
        .count()
}

/// One row: the stack the update is made against, the update that is then cancelled, and where
/// the cancel leaves the stack.
struct Row {
    case: &'static str,
    stack: &'static str,
    created: String,
    updated: String,
    /// `DisableRollback`, exactly as the update carries it.
    disable_rollback: bool,
    /// The status the stack reports as soon as the cancel is answered.
    cancelling: &'static str,
    /// ...and the one it settles in once the clock makes that transition due.
    settles_to: &'static str,
    /// How many `UPDATE_ROLLBACK_IN_PROGRESS` transitions the whole walk may record: the one
    /// the cancellation opened, and none at all for the caller that kept the failed state.
    rollbacks_opened: usize,
}

/// The class the rule covers: any update in flight, however its own deployment went.
fn rows() -> Vec<Row> {
    vec![
        // A property no Amazon S3 call this emulator makes writes on a bucket that exists, so
        // the owning service refuses the change while the update's own call is still running.
        Row {
            case: "a resource whose change the owning service refused",
            stack: "cancel-refused-resource",
            created: bucket("cancel-refused-resource-store", ""),
            updated: bucket(
                "cancel-refused-resource-store",
                r#","AccessControl":"PublicRead""#,
            ),
            disable_rollback: false,
            cancelling: status::UPDATE_ROLLBACK_IN_PROGRESS,
            settles_to: status::UPDATE_ROLLBACK_COMPLETE,
            rollbacks_opened: 1,
        },
        // "DisableRollback: Preserve the state of previously provisioned resources when an
        // operation fails": the caller said what it wants left behind, so the cancelled update
        // rests in the failed status instead of being reverted.
        Row {
            case: "the same refusal, with the failed state kept by DisableRollback",
            stack: "cancel-refused-kept",
            created: bucket("cancel-refused-kept-store", ""),
            updated: bucket(
                "cancel-refused-kept-store",
                r#","AccessControl":"PublicRead""#,
            ),
            disable_rollback: true,
            cancelling: status::UPDATE_FAILED,
            settles_to: status::UPDATE_FAILED,
            rollbacks_opened: 0,
        },
        // "The intrinsic function Fn::ImportValue returns the value of an output exported by
        // another stack": a name no live stack of the Region publishes has no value, so
        // CloudFormation refuses the deployment before asking any owning service for anything.
        Row {
            case: "a deployment CloudFormation refused before asking any owning service",
            stack: "cancel-refused-deployment",
            created: bucket("cancel-refused-deployment-store", ""),
            updated: bucket(
                "cancel-refused-deployment-store",
                r#","Tags":[{"Key":"Table","Value":{"Fn::ImportValue":"never-published-table"}}]"#,
            ),
            disable_rollback: false,
            cancelling: status::UPDATE_ROLLBACK_IN_PROGRESS,
            settles_to: status::UPDATE_ROLLBACK_COMPLETE,
            rollbacks_opened: 1,
        },
        // The control row: an update nothing refused, which is in flight for exactly the same
        // span and cancelled from it the same way.
        Row {
            case: "an update nothing refused",
            stack: "cancel-accepted-update",
            created: parameter("/cancel-accepted-update/p", "before"),
            updated: parameter("/cancel-accepted-update/p", "after"),
            disable_rollback: false,
            cancelling: status::UPDATE_ROLLBACK_IN_PROGRESS,
            settles_to: status::UPDATE_ROLLBACK_COMPLETE,
            rollbacks_opened: 1,
        },
    ]
}

#[tokio::test]
async fn an_update_whose_deployment_was_refused_is_still_cancellable() {
    for row in rows() {
        let case = row.case;
        let (router, engine) = router_with_engine();

        let (http_status, body) = create_stack(&router, row.stack, &row.created, &[]).await;
        assert_eq!(
            http_status, 200,
            "case {case:?}: expected CreateStack to be accepted, got {http_status} {body}"
        );
        settle_the_store(&router, &engine).await;
        let (created, _) = reported(case, &router, row.stack).await;
        assert_eq!(
            created,
            status::CREATE_COMPLETE,
            "case {case:?}: expected the create to complete before the update, got {created:?}"
        );

        let mut members = vec![
            member("StackName", row.stack),
            member("TemplateBody", &row.updated),
        ];
        if row.disable_rollback {
            members.push(member("DisableRollback", "true"));
        }
        let (http_status, body) = call(&router, "UpdateStack", &members).await;
        assert_eq!(
            http_status, 200,
            "case {case:?}: expected UpdateStack to be accepted, got {http_status} {body}"
        );

        // The update is in flight: its conclusion is a transition the clock owes, so it is
        // held open here rather than waited on, and the stack must report the update's own
        // in-progress status for that whole span however its deployment went.
        assert!(
            hold_the_conclusion_open(&engine, row.stack),
            "case {case:?}: expected the update to be in flight with its conclusion owed"
        );
        let (in_flight, _) = reported(case, &router, row.stack).await;
        assert_eq!(
            in_flight,
            status::UPDATE_IN_PROGRESS,
            "case {case:?}: expected the update to be in flight when the cancel arrives, got \
             {in_flight:?}"
        );

        assert_the_cancel_is_answered(case, &router, row.stack).await;

        // The cancel ENDED the update: what follows is the revert it owes, which the caller
        // that asked for the failed state to be kept gets none of — and either way the stack
        // says the cancellation is what put it there.
        hold_the_conclusion_open(&engine, row.stack);
        let (cancelling, reason) = reported(case, &router, row.stack).await;
        assert_eq!(
            cancelling, row.cancelling,
            "case {case:?}: expected the cancel to leave the stack {:?}, got {cancelling:?}",
            row.cancelling
        );
        assert_eq!(
            reason, CANCELLED_BY_USER,
            "case {case:?}: expected the cancelled update's reason to be \
             {CANCELLED_BY_USER:?}, got {reason:?}"
        );

        settle_the_store(&router, &engine).await;
        let settled_body = settled_stack(&router, case, row.stack).await;
        let settled = xml_text(&settled_body, "StackStatus").unwrap_or_default();
        assert_eq!(
            settled, row.settles_to,
            "case {case:?}: expected the cancelled update to settle in {:?}, got {settled:?}",
            row.settles_to
        );
        // ...and the settled stack still says the CANCELLATION is what ended the update: the
        // conclusion the update's own deployment had reached is not reported over it.
        let settled_reason = xml_text(&settled_body, "StackStatusReason").unwrap_or_default();
        assert_eq!(
            settled_reason, CANCELLED_BY_USER,
            "case {case:?}: expected the settled stack's reason to be {CANCELLED_BY_USER:?}, \
             got {settled_reason:?}"
        );
        let opened = stack_transitions(
            case,
            &router,
            row.stack,
            status::UPDATE_ROLLBACK_IN_PROGRESS,
        )
        .await;
        assert_eq!(
            opened, row.rollbacks_opened,
            "case {case:?}: expected {} rollback(s) of this update to be opened, got {opened}",
            row.rollbacks_opened
        );
    }
}

/// The same rule for the other call that puts an update in flight: executing a change set.
/// "Updates a stack using the input information that was provided when the specified change
/// set was created" — so an update a change set executes is cancellable for exactly the span
/// the clock owes its conclusion, however its own deployment went.
#[tokio::test]
async fn an_update_a_change_set_executes_is_cancellable_while_it_is_in_flight() {
    let case = "an executed change set whose resource the owning service refused";
    let stack = "cancel-executed-change-set";
    let store_name = "cancel-executed-change-set-store";
    let (router, engine) = router_with_engine();

    let (http_status, body) = create_stack(&router, stack, &bucket(store_name, ""), &[]).await;
    assert_eq!(
        http_status, 200,
        "case {case:?}: expected CreateStack to be accepted, got {http_status} {body}"
    );
    settle_the_store(&router, &engine).await;

    let (http_status, body) = call(
        &router,
        "CreateChangeSet",
        &[
            member("StackName", stack),
            member("ChangeSetName", "refused-revision"),
            member("ChangeSetType", "UPDATE"),
            member(
                "TemplateBody",
                &bucket(store_name, r#","AccessControl":"PublicRead""#),
            ),
        ],
    )
    .await;
    assert_eq!(
        http_status, 200,
        "case {case:?}: expected CreateChangeSet to be accepted, got {http_status} {body}"
    );
    let change_set = xml_text(&body, "Id").unwrap_or_default();
    settle_the_store(&router, &engine).await;

    let (http_status, body) = call(
        &router,
        "ExecuteChangeSet",
        &[member("ChangeSetName", &change_set)],
    )
    .await;
    assert_eq!(
        http_status, 200,
        "case {case:?}: expected ExecuteChangeSet to be accepted, got {http_status} {body}"
    );

    assert!(
        hold_the_conclusion_open(&engine, stack),
        "case {case:?}: expected the executed update to be in flight with its conclusion owed"
    );
    let (in_flight, _) = reported(case, &router, stack).await;
    assert_eq!(
        in_flight,
        status::UPDATE_IN_PROGRESS,
        "case {case:?}: expected the executed update to be in flight when the cancel arrives, \
         got {in_flight:?}"
    );

    assert_the_cancel_is_answered(case, &router, stack).await;
    hold_the_conclusion_open(&engine, stack);
    let (cancelling, reason) = reported(case, &router, stack).await;
    assert_eq!(
        cancelling,
        status::UPDATE_ROLLBACK_IN_PROGRESS,
        "case {case:?}: expected the cancel to open the update's rollback, got {cancelling:?}"
    );
    assert_eq!(
        reason, CANCELLED_BY_USER,
        "case {case:?}: expected the cancelled update's reason to be {CANCELLED_BY_USER:?}, \
         got {reason:?}"
    );

    settle_the_store(&router, &engine).await;
    let settled_body = settled_stack(&router, case, stack).await;
    let settled = xml_text(&settled_body, "StackStatus").unwrap_or_default();
    assert_eq!(
        settled,
        status::UPDATE_ROLLBACK_COMPLETE,
        "case {case:?}: expected the cancelled update to be rolled back, got {settled:?}"
    );
    let settled_reason = xml_text(&settled_body, "StackStatusReason").unwrap_or_default();
    assert_eq!(
        settled_reason, CANCELLED_BY_USER,
        "case {case:?}: expected the settled stack's reason to be {CANCELLED_BY_USER:?}, got \
         {settled_reason:?}"
    );
    let opened =
        stack_transitions(case, &router, stack, status::UPDATE_ROLLBACK_IN_PROGRESS).await;
    assert_eq!(
        opened, 1,
        "case {case:?}: expected exactly the cancellation's own rollback to be opened, got \
         {opened}"
    );
}

/// The boundary the rule draws: ONCE the clock has made the update's conclusion due, the
/// update is over and the cancel is refused — the stack that was reverted on its own failure
/// is not one with an update left to cancel, and the refusal leaves it exactly where it is.
#[tokio::test]
async fn an_update_whose_conclusion_has_fallen_due_can_no_longer_be_cancelled() {
    let case = "a refused update the clock has already concluded";
    let stack = "cancel-after-the-conclusion";
    let store_name = "cancel-after-the-conclusion-store";
    let (router, engine) = router_with_engine();

    let (http_status, body) = create_stack(&router, stack, &bucket(store_name, ""), &[]).await;
    assert_eq!(
        http_status, 200,
        "case {case:?}: expected CreateStack to be accepted, got {http_status} {body}"
    );
    settle_the_store(&router, &engine).await;

    let (http_status, body) = call(
        &router,
        "UpdateStack",
        &[
            member("StackName", stack),
            member(
                "TemplateBody",
                &bucket(store_name, r#","AccessControl":"PublicRead""#),
            ),
        ],
    )
    .await;
    assert_eq!(
        http_status, 200,
        "case {case:?}: expected UpdateStack to be accepted, got {http_status} {body}"
    );

    // The clock makes the update's conclusion due, and the rollback that failure owes runs to
    // its end: from there the stack has no update in flight at all.
    settle_the_store(&router, &engine).await;
    let settled_body = settled_stack(&router, case, stack).await;
    let settled = xml_text(&settled_body, "StackStatus").unwrap_or_default();
    assert_eq!(
        settled,
        status::UPDATE_ROLLBACK_COMPLETE,
        "case {case:?}: expected the refused update to be rolled back, got {settled:?}"
    );

    let (http_status, body) = cancel_update(&router, stack).await;
    assert_eq!(
        http_status, 400,
        "case {case:?}: expected the cancel to be refused with 400, got {http_status} {body}"
    );
    let code = xml_text(&body, "Code").unwrap_or_default();
    assert_eq!(
        code, "ValidationError",
        "case {case:?}: expected <Code>ValidationError</Code>, got {code:?} {body}"
    );
    let message = xml_text(&body, "Message").unwrap_or_default();
    assert_eq!(
        message, "CancelUpdateStack cannot be called from current stack status",
        "case {case:?}: expected the fixed refusal of the call, got {message:?}"
    );
    let (after, _) = reported(case, &router, stack).await;
    assert_eq!(
        after,
        status::UPDATE_ROLLBACK_COMPLETE,
        "case {case:?}: a refused cancel must leave the stack where it was, got {after:?}"
    );
}
