// SPDX-License-Identifier: CC0-1.0

//! What `DeleteChangeSet` does with a change set its stack has already acted on, driven the
//! whole way over the awsQuery wire.
//!
//! The rule every case here holds the service to: **`DeleteChangeSet` takes away the PLAN. A
//! change set nothing has executed is taken away outright, and one whose execution has been
//! carried out has no plan left to take away: the call succeeds, no read of the stack's change
//! sets reports the record any more — `DescribeChangeSet` for it answers the modelled
//! `ChangeSetNotFound` — and the one thing the delete makes sure of is that nobody executes
//! that plan again, so `ExecuteChangeSet` for it is still answered on the execution it carried
//! out. The only change set CloudFormation will not discard is one it has an operation of its
//! own in flight on, which it refuses with HTTP 400, the modelled `InvalidChangeSetStatus` and
//! `Cannot delete ChangeSet in execution status <ExecutionStatus>` — fixed text followed by the
//! status read off this service's own state. A refusal writes nothing, and a delete that
//! succeeds takes only the change set it addressed: the stack whose change the executed plan
//! carried out is left exactly where that execution left it.**
//!
//! `change_set_status_refusals.rs` holds the same rule over every `ExecutionStatus` the model
//! declares, by putting a change set in each; this suite reaches the one the REAL path produces
//! — the execution the stack carries through — for each change-set type whose execution changes
//! a stack, addressing the change set both ways a request may (by ARN, and by name beside its
//! stack), with the execution still in flight as the refusing control.

use std::time::{Duration, Instant};

use service_com_amazonaws_cloudformation_cloudformation::state::execution_status;
use smithy_server_runtime::MultiServiceRouter;

use crate::change_set_lifecycle::{
    create_change_set, describe_change_set, execute_change_set, settle_the_store,
};
use crate::harness::{call, create_stack, member, router_with_engine, xml_text};
use crate::stack_lifecycle::describe_stack;

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

/// The refusal a delete of a change set CloudFormation has an operation in flight on must
/// answer, as CloudFormation words it.
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

/// The rule over the statuses the real path reaches, for each change-set type whose execution
/// changes a stack and each way a request may address the change set: the execution still being
/// applied is refused and stays readable, the record of the execution that was carried out is
/// discarded and reads back as the modelled not-found, and so is a plan nothing executed —
/// while the stack that execution changed is left exactly where it was.
#[tokio::test]
async fn an_executed_change_sets_record_is_discarded_once_its_execution_has_concluded() {
    for addressed_by in [AddressedBy::Arn, AddressedBy::NameAndStack] {
        // Both change-set types whose execution changes a stack: the one that brings the stack
        // into existence, and the one that updates a stack that already exists.
        for change_set_type in ["CREATE", "UPDATE"] {
            let (router, engine) = router_with_engine();
            let case = format!("{addressed_by:?}/{change_set_type}");
            let stack_name = format!("delrecord-{addressed_by:?}-{change_set_type}").to_lowercase();
            // The template the executed plan carries, and the one the second case plans on top
            // of what that execution left in force.
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

            let (addressed, stack) = match addressed_by {
                AddressedBy::Arn => (applied_arn.clone(), None),
                AddressedBy::NameAndStack => (applied_name.clone(), Some(stack_name.as_str())),
            };

            // While the execution is being applied CloudFormation has an operation of its own
            // in flight on the record: the delete is refused naming that execution status, and
            // the record is still there afterwards reporting it.
            let (code, body) = describe_change_set(&router, &applied_arn, None).await;
            assert_eq!(
                (code, xml_text(&body, "ExecutionStatus").unwrap_or_default()),
                (200, execution_status::EXECUTE_IN_PROGRESS.to_string()),
                "case {case:?}: the execution is in flight with the stack, got {code} {body}"
            );
            let (code, body) = delete_change_set(&router, &addressed, stack).await;
            assert_eq!(
                code, 400,
                "case {case:?}: the delete must be refused while the execution is applied, got \
                 {code} {body}"
            );
            assert_eq!(
                refused(&body),
                cannot_delete(execution_status::EXECUTE_IN_PROGRESS),
                "case {case:?}: expected the modelled refusal naming the execution status, got \
                 {body}"
            );
            let (code, body) = describe_change_set(&router, &applied_arn, None).await;
            assert_eq!(
                (code, xml_text(&body, "ExecutionStatus").unwrap_or_default()),
                (200, execution_status::EXECUTE_IN_PROGRESS.to_string()),
                "case {case:?}: a refused delete leaves the change set exactly where it was, \
                 got {code} {body}"
            );

            // Once the stack has carried the change through, the record of that execution is
            // discardable like any other: the delete succeeds and the record is gone.
            awaiting_execution_status(
                &case,
                &router,
                &engine,
                &applied_arn,
                execution_status::EXECUTE_COMPLETE,
            )
            .await;
            let (_, before) = describe_stack(&router, &stack_name).await;
            let stack_status_before = xml_text(&before, "StackStatus").unwrap_or_default();
            let (code, body) = delete_change_set(&router, &addressed, stack).await;
            assert_eq!(
                code, 200,
                "case {case:?}: the record of a concluded execution is discardable, got {code} \
                 {body}"
            );
            let (code, body) = describe_change_set(&router, &applied_arn, None).await;
            assert_eq!(
                (code, refused(&body)),
                (
                    404,
                    (
                        "ChangeSetNotFound".to_string(),
                        format!("ChangeSet [{applied_arn}] does not exist"),
                    ),
                ),
                "case {case:?}: expected the discarded record to read back as not found, got \
                 {body}"
            );
            // ...and what the delete made sure of is that nobody executes that plan again: a
            // request to execute the discarded record is answered on the execution it carried
            // out rather than as a change set that never existed.
            let (code, body) = execute_change_set(&router, &applied_arn, None).await;
            assert_eq!(
                (code, refused(&body)),
                (
                    400,
                    (
                        "InvalidChangeSetStatus".to_string(),
                        format!(
                            "ChangeSet [{applied_arn}] cannot be executed in its current \
                             execution status of [{}]",
                            execution_status::EXECUTE_COMPLETE
                        ),
                    ),
                ),
                "case {case:?}: expected the execution that happened to refuse the call, got \
                 {code} {body}"
            );
            // The delete took only the change set it addressed: the stack the execution
            // changed is left in the status that execution left it in.
            let (code, after) = describe_stack(&router, &stack_name).await;
            assert_eq!(
                (code, xml_text(&after, "StackStatus").unwrap_or_default()),
                (200, stack_status_before.clone()),
                "case {case:?}: deleting a change set leaves its stack alone, got {code} {after}"
            );
            assert!(
                stack_status_before.ends_with("_COMPLETE"),
                "case {case:?}: the execution must have carried the change through, got \
                 {stack_status_before:?}"
            );

            // The same on the other side of the execution: a plan nothing has executed —
            // created against the template that execution left in force — is deleted just as
            // cleanly.
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
            // A plan the delete took away outright has no execution to be answered on, so the
            // same call is the modelled not-found here.
            let (code, body) = execute_change_set(&router, &control_arn, None).await;
            assert_eq!(
                (code, refused(&body).0),
                (404, "ChangeSetNotFound".to_string()),
                "case {case:?}: expected a discarded plan to answer the modelled not-found, \
                 got {code} {body}"
            );
        }
    }
}
