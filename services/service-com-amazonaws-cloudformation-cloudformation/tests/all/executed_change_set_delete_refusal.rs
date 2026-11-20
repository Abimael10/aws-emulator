// SPDX-License-Identifier: CC0-1.0

//! What `DeleteChangeSet` does with a change set its stack has already acted on, driven the
//! whole way over the awsQuery wire.
//!
//! The rule every case here holds the service to: **`DeleteChangeSet` discards a PLAN, so a
//! change set is deletable only while it still is one — `ExecutionStatus` `UNAVAILABLE` or
//! `AVAILABLE`. In every other execution status something has already become of the plan
//! (CloudFormation is applying it, it has been applied however that application ended, or
//! another execution superseded it), and the call is refused with HTTP 400, the modelled
//! `InvalidChangeSetStatus` and `Cannot delete ChangeSet in execution status
//! <ExecutionStatus>` — fixed text followed by the status the change set is in, read off this
//! service's own state. A refusal writes nothing: the change set is still resolved by a
//! following `DescribeChangeSet`, in the very status that refused the delete.**
//!
//! `change_set_status_refusals.rs` holds the same rule over every `ExecutionStatus` the model
//! declares, by putting a change set in each; this suite reaches the one the REAL path produces
//! — the execution the stack carries through — for each change-set type whose execution changes
//! a stack, addressing the change set both ways a request may (by ARN, and by name beside its
//! stack), with a plan nothing has executed as the positive control.

use std::time::{Duration, Instant};

use service_com_amazonaws_cloudformation_cloudformation::state::execution_status;
use smithy_server_runtime::MultiServiceRouter;

use crate::change_set_lifecycle::{
    create_change_set, describe_change_set, execute_change_set, settle_the_store,
};
use crate::harness::{call, create_stack, member, router_with_engine, xml_text};

/// The template the stack is created from, and the ones its change sets plan: a second
/// resource beside the topic, and a renamed topic, so each change set plans a real change.
const TOPIC: &str = r#"{"Resources":{"A":{"Type":"AWS::SNS::Topic"}}}"#;
const TOPIC_AND_GROUP: &str =
    r#"{"Resources":{"A":{"Type":"AWS::SNS::Topic"},"G":{"Type":"AWS::Logs::LogGroup"}}}"#;
const RENAMED_TOPIC: &str =
    r#"{"Resources":{"A":{"Type":"AWS::SNS::Topic","Properties":{"DisplayName":"renamed"}}}}"#;

/// How long a case waits for a change set to reach the execution status the stack operation
/// it started owes it, and how often it looks. Far longer than the spans this emulator gives
/// a transition, so neither a loaded host nor a busy test run can decide a case: what is
/// asserted is the CONDITION, never the time it took.
const EXECUTION_DEADLINE: Duration = Duration::from_secs(180);
const EXECUTION_POLL_INTERVAL: Duration = Duration::from_millis(50);

async fn delete_change_set(
    router: &MultiServiceRouter,
    change_set: &str,
    stack_name: Option<&str>,
) -> (u16, String) {
    let mut members = vec![member("ChangeSetName", change_set)];
    if let Some(stack_name) = stack_name {
        members.push(member("StackName", stack_name));
    }
    call(router, "DeleteChangeSet", &members).await
}

/// How a request addresses the change set it is about: the two forms `ChangeSetName` admits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AddressedBy {
    /// The change set's own ARN, which names it on its own.
    Arn,
    /// Its name, with the stack it belongs to beside it.
    NameAndStack,
}

/// The `(Code, Message)` of a refusal envelope.
fn refused(body: &str) -> (String, String) {
    (
        xml_text(body, "Code").unwrap_or_default(),
        xml_text(body, "Message").unwrap_or_default(),
    )
}

/// The refusal a delete of a change set in `execution` must answer, as CloudFormation words
/// it.
fn cannot_delete(execution: &str) -> (String, String) {
    (
        "InvalidChangeSetStatus".to_string(),
        format!("Cannot delete ChangeSet in execution status {execution}"),
    )
}

/// Create one change set of `stack_name` of the type the case is about, and answer the ARN it
/// reports for itself.
async fn plan(
    case: &str,
    router: &MultiServiceRouter,
    engine: &std::sync::Arc<engine::InMemoryEngine>,
    stack_name: &str,
    change_set_name: &str,
    change_set_type: &str,
    template: &str,
) -> String {
    let (code, body) = create_change_set(
        router,
        stack_name,
        change_set_name,
        &[
            member("ChangeSetType", change_set_type),
            member("TemplateBody", template),
        ],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: the change set must be created, got {code} {body}"
    );
    settle_the_store(router, engine).await;
    let (code, described) = describe_change_set(router, change_set_name, Some(stack_name)).await;
    assert_eq!(
        code, 200,
        "case {case:?}: the created change set must be readable, got {code} {described}"
    );
    xml_text(&described, "ChangeSetId")
        .unwrap_or_else(|| panic!("case {case:?}: a described change set carries its ARN"))
}

/// Look again until the change set at `arn` reports `execution`, settling whatever the store
/// owes between reads: a change set's execution follows the stack operation it started, so
/// what a case waits for is that status appearing.
async fn awaiting_execution_status(
    case: &str,
    router: &MultiServiceRouter,
    engine: &std::sync::Arc<engine::InMemoryEngine>,
    arn: &str,
    execution: &str,
) {
    let deadline = Instant::now() + EXECUTION_DEADLINE;
    loop {
        settle_the_store(router, engine).await;
        let (code, body) = describe_change_set(router, arn, None).await;
        let reported = xml_text(&body, "ExecutionStatus").unwrap_or_default();
        if code == 200 && reported == execution {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the change set to reach {execution}, last read {code} \
             {reported:?}"
        );
        tokio::time::sleep(EXECUTION_POLL_INTERVAL).await;
    }
}

/// The rule over the status the real path reaches, for each change-set type whose execution
/// changes a stack and each way a request may address the change set: the executed plan is
/// refused and stays readable, while a plan nothing has executed is deleted and reads back as
/// the modelled not-found afterwards.
#[tokio::test]
async fn a_change_set_its_stack_has_acted_on_cannot_be_deleted() {
    for addressed_by in [AddressedBy::Arn, AddressedBy::NameAndStack] {
        // Both change-set types whose execution changes a stack: the one that brings the stack
        // into existence, and the one that updates a stack that already exists.
        for change_set_type in ["CREATE", "UPDATE"] {
            let (router, engine) = router_with_engine();
            let case = format!("{addressed_by:?}/{change_set_type}");
            let stack_name =
                format!("delrefuse-{addressed_by:?}-{change_set_type}").to_lowercase();
            // The template the executed plan carries, and the one the control plans on top of
            // what that execution left in force.
            let (applied_template, control_template) = if change_set_type == "UPDATE" {
                let (code, body) = create_stack(&router, &stack_name, TOPIC, &[]).await;
                assert_eq!(
                    code, 200,
                    "case {case:?}: the stack the change set updates must exist, got {code} \
                     {body}"
                );
                settle_the_store(&router, &engine).await;
                (TOPIC_AND_GROUP, RENAMED_TOPIC)
            } else {
                (TOPIC, TOPIC_AND_GROUP)
            };

            let applied_name = format!("{stack_name}-applied");
            let applied_arn = plan(
                &case,
                &router,
                &engine,
                &stack_name,
                &applied_name,
                change_set_type,
                applied_template,
            )
            .await;

            let (code, body) = execute_change_set(&router, &applied_arn, None).await;
            assert_eq!(
                code, 200,
                "case {case:?}: the change set must execute, got {code} {body}"
            );

            // The change set the stack has acted on: the delete is refused naming its
            // execution status, and the record is still there afterwards reporting that same
            // status.
            let execution = execution_status::EXECUTE_COMPLETE;
            awaiting_execution_status(&case, &router, &engine, &applied_arn, execution).await;
            let (addressed, stack) = match addressed_by {
                AddressedBy::Arn => (applied_arn.as_str(), None),
                AddressedBy::NameAndStack => (applied_name.as_str(), Some(stack_name.as_str())),
            };
            let (code, body) = delete_change_set(&router, addressed, stack).await;
            assert_eq!(
                code, 400,
                "case {case:?}: the delete must be refused, got {code} {body}"
            );
            assert_eq!(
                refused(&body),
                cannot_delete(execution),
                "case {case:?}: expected the modelled refusal naming the execution status, got \
                 {body}"
            );
            // The refusal did not take effect: the change set is still resolved, in the very
            // status that refused the call.
            let (code, body) = describe_change_set(&router, &applied_arn, None).await;
            assert_eq!(
                (code, xml_text(&body, "ExecutionStatus").unwrap_or_default()),
                (200, execution.to_string()),
                "case {case:?}: a refused delete leaves the change set exactly where it was, \
                 got {code} {body}"
            );

            // The positive control, on the same stack: a plan nothing has executed — created
            // against the template the execution above left in force — is deleted cleanly, and
            // a read of it then answers the modelled not-found naming what the request named.
            let control_name = format!("{stack_name}-untouched");
            let control_arn = plan(
                &case,
                &router,
                &engine,
                &stack_name,
                &control_name,
                "UPDATE",
                control_template,
            )
            .await;
            let (code, body) = describe_change_set(&router, &control_arn, None).await;
            assert_eq!(
                (code, xml_text(&body, "ExecutionStatus").unwrap_or_default()),
                (200, execution_status::AVAILABLE.to_string()),
                "case {case:?}: expected a plan ready to execute, got {code} {body}"
            );
            let (addressed, stack) = match addressed_by {
                AddressedBy::Arn => (control_arn.as_str(), None),
                AddressedBy::NameAndStack => (control_name.as_str(), Some(stack_name.as_str())),
            };
            let (code, body) = delete_change_set(&router, addressed, stack).await;
            assert_eq!(
                code, 200,
                "case {case:?}: a plan nothing acted on is discardable, got {code} {body}"
            );
            let (code, body) = describe_change_set(&router, &control_arn, None).await;
            assert_eq!(
                (code, refused(&body)),
                (
                    404,
                    (
                        "ChangeSetNotFound".to_string(),
                        format!("ChangeSet [{control_arn}] does not exist"),
                    ),
                ),
                "case {case:?}: expected the discarded plan to read back as not found, got \
                 {body}"
            );
            // ...and the applied plan is still there once the control is gone, so the delete
            // that succeeded took only what it addressed.
            let (code, body) = describe_change_set(&router, &applied_arn, None).await;
            assert_eq!(
                code, 200,
                "case {case:?}: the applied plan outlives the control's delete, got {code} \
                 {body}"
            );
        }
    }
}
