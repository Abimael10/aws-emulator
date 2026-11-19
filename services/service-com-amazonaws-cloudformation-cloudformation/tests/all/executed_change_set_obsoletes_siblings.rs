// SPDX-License-Identifier: CC0-1.0

//! What becomes of a stack's OTHER change sets when one of them is executed.
//!
//! RULE: executing a change set INVALIDATES the other outstanding plans of the stack it
//! belongs to without making them unaddressable — each such record stays describable and
//! listable and reports the modelled `OBSOLETE` execution status ("a change set might be …
//! in an `OBSOLETE` state because the stack was already updated",
//! `DescribeChangeSetOutput$ExecutionStatus`), so a later `ExecuteChangeSet` on one of them
//! is refused with the modelled `InvalidChangeSetStatus` on that status rather than answered
//! not-found. Only a plan that could still have been executed is invalidated: a change set
//! whose own execution has begun keeps the record of the change the stack took, a creation
//! that left no plan at all keeps reporting `UNAVAILABLE`, and another stack's change sets
//! are not this stack's plans at all.
//!
//! The table below spans the states a sibling can be in when the execution runs — a plan
//! ready to execute (two shapes of it), a creation whose outcome left nothing to execute, a
//! creation still in flight — and the second phase covers a sibling the FIRST execution had
//! already invalidated and one it had already executed, so the recorded call this suite was
//! written for is one row of it. Every read goes through the awsQuery wire.

use service_com_amazonaws_cloudformation_cloudformation::state::{
    change_set_status, execution_status, status,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::change_set_lifecycle::{
    create_change_set, describe_change_set, execute_change_set, settle_the_store,
};
use crate::harness::{
    call, create_stack, member, refusal, router_with_engine, xml_text, xml_texts,
};
use crate::stack_lifecycle::describe_stack;

/// The template the stack is created with, and so the one in force when the change sets
/// below are planned.
const IN_FORCE: &str = r#"{"Resources":{"A":{"Type":"AWS::SNS::Topic"}}}"#;

/// A template that declares the stack's topic and one more under `logical_id`: a plan that
/// ADDS a resource, which is a change against [`IN_FORCE`].
fn adding(logical_id: &str) -> String {
    format!(
        r#"{{"Resources":{{"A":{{"Type":"AWS::SNS::Topic"}},"{logical_id}":{{"Type":"AWS::SNS::Topic"}}}}}}"#
    )
}

/// One sibling of the executed change set: the state its own creation leaves it in, and what
/// it must report once another change set of its stack has been executed.
struct Sibling {
    case: &'static str,
    name: &'static str,
    /// The logical id its plan would add, or `None` for a plan that declares exactly what is
    /// already in force and so finds nothing to change.
    adds: Option<&'static str>,
    /// Whether its creation has concluded BEFORE the execution that supersedes it: a
    /// creation still in flight is computing its plan against the configuration that
    /// execution supersedes.
    creation_concluded: bool,
    /// The `ChangeSetStatus` it reports afterwards.
    change_set_status: &'static str,
    /// The `ExecutionStatus` it reports afterwards.
    execution_status: &'static str,
}

fn siblings() -> Vec<Sibling> {
    vec![
        Sibling {
            case: "a plan ready to execute",
            name: "obsolete-ready",
            adds: Some("C"),
            creation_concluded: true,
            change_set_status: change_set_status::CREATE_COMPLETE,
            execution_status: execution_status::OBSOLETE,
        },
        Sibling {
            case: "a second plan ready to execute, adding a resource of its own",
            name: "obsolete-ready-other",
            adds: Some("D"),
            creation_concluded: true,
            change_set_status: change_set_status::CREATE_COMPLETE,
            execution_status: execution_status::OBSOLETE,
        },
        Sibling {
            case: "a creation whose outcome left nothing to execute",
            name: "obsolete-no-changes",
            adds: None,
            creation_concluded: true,
            change_set_status: change_set_status::FAILED,
            execution_status: execution_status::UNAVAILABLE,
        },
        Sibling {
            case: "a creation still in flight when the execution ran",
            name: "obsolete-in-flight",
            adds: Some("E"),
            creation_concluded: false,
            change_set_status: change_set_status::CREATE_COMPLETE,
            execution_status: execution_status::OBSOLETE,
        },
    ]
}

/// `CreateChangeSet` for an UPDATE of a stack that exists.
async fn create_updating_change_set(
    router: &MultiServiceRouter,
    stack: &str,
    change_set: &str,
    template: &str,
) -> (u16, String) {
    create_change_set(
        router,
        stack,
        change_set,
        &[
            member("ChangeSetType", "UPDATE"),
            member("TemplateBody", template),
        ],
    )
    .await
}

/// The `(ChangeSetStatus, ExecutionStatus, ChangeSetId)` one `DescribeChangeSet` reports,
/// asserting the change set is still addressable at all.
async fn described(
    router: &MultiServiceRouter,
    case: &str,
    change_set: &str,
    stack: &str,
) -> (String, String, String) {
    let (code, body) = describe_change_set(router, change_set, Some(stack)).await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected {change_set} to stay describable, got {code} {body}"
    );
    (
        xml_text(&body, "Status").unwrap_or_default(),
        xml_text(&body, "ExecutionStatus").unwrap_or_default(),
        xml_text(&body, "ChangeSetId").unwrap_or_default(),
    )
}

/// The change set names `ListChangeSets` reports for one stack.
async fn listed(router: &MultiServiceRouter, stack: &str) -> Vec<String> {
    let (code, body) = call(router, "ListChangeSets", &[member("StackName", stack)]).await;
    assert_eq!(code, 200, "ListChangeSets must succeed, got {code} {body}");
    xml_texts(&body, "ChangeSetName")
}

#[tokio::test]
async fn executing_one_change_set_leaves_its_siblings_obsolete_and_still_addressable() {
    let (router, engine) = router_with_engine();
    let stack = "obsolete-siblings";
    let elsewhere = "obsolete-siblings-other";
    for name in [stack, elsewhere] {
        let (code, body) = create_stack(&router, name, IN_FORCE, &[]).await;
        assert_eq!(code, 200, "the stack {name} must be created, got {code} {body}");
    }
    settle_the_store(&router, &engine).await;

    // Another stack's plan: nothing this stack's execution does may touch it.
    let (code, body) =
        create_updating_change_set(&router, elsewhere, "untouched-set", &adding("B")).await;
    assert_eq!(
        code, 200,
        "the other stack's change set must be created, got {code} {body}"
    );

    let table = siblings();
    for sibling in table.iter().filter(|sibling| sibling.creation_concluded) {
        let template = sibling
            .adds
            .map(adding)
            .unwrap_or_else(|| IN_FORCE.to_string());
        let (code, body) =
            create_updating_change_set(&router, stack, sibling.name, &template).await;
        assert_eq!(
            code, 200,
            "case {:?}: the sibling must be created, got {code} {body}",
            sibling.case
        );
    }
    // The change set this phase EXECUTES, created and settled with the siblings whose
    // creations conclude before the execution.
    let (code, body) =
        create_updating_change_set(&router, stack, "executed-set", &adding("B")).await;
    assert_eq!(code, 200, "the executed change set must be created, got {code} {body}");
    settle_the_store(&router, &engine).await;

    // ...and the siblings whose creation is STILL IN FLIGHT when the execution runs: created
    // after that settling and left unsettled.
    for sibling in table.iter().filter(|sibling| !sibling.creation_concluded) {
        let template = sibling
            .adds
            .map(adding)
            .unwrap_or_else(|| IN_FORCE.to_string());
        let (code, body) =
            create_updating_change_set(&router, stack, sibling.name, &template).await;
        assert_eq!(
            code, 200,
            "case {:?}: the sibling must be created, got {code} {body}",
            sibling.case
        );
    }

    let (code, body) = execute_change_set(&router, "executed-set", Some(stack)).await;
    assert_eq!(code, 200, "the change set must execute, got {code} {body}");
    settle_the_store(&router, &engine).await;

    // The execution went through: the stack took the change and the change set reports it.
    let (code, body) = describe_stack(&router, stack).await;
    assert_eq!(code, 200, "the stack must be describable, got {code} {body}");
    assert_eq!(
        xml_text(&body, "StackStatus").as_deref(),
        Some(status::UPDATE_COMPLETE),
        "expected the executed plan to update the stack, got {body}"
    );
    let (_, executed_execution, _) =
        described(&router, "the executed change set", "executed-set", stack).await;
    assert_eq!(
        executed_execution, execution_status::EXECUTE_COMPLETE,
        "expected the executed change set to report its execution, got \
         {executed_execution:?}"
    );

    for sibling in &table {
        let case = sibling.case;
        let (reported_status, reported_execution, change_set_id) =
            described(&router, case, sibling.name, stack).await;
        assert_eq!(
            reported_status, sibling.change_set_status,
            "case {case:?}: expected ChangeSetStatus {:?}, got {reported_status:?}",
            sibling.change_set_status
        );
        assert_eq!(
            reported_execution, sibling.execution_status,
            "case {case:?}: expected ExecutionStatus {:?}, got {reported_execution:?}",
            sibling.execution_status
        );
        // ...and asking for the invalidated plan to be executed is refused on the status it
        // is in — a 400 status conflict naming the change set and that status — rather than
        // answered as a change set that never existed.
        let (code, body) = execute_change_set(&router, sibling.name, Some(stack)).await;
        assert_eq!(
            code, 400,
            "case {case:?}: expected the execution to be refused, got {code} {body}"
        );
        let (reported_code, message) = refusal(&body);
        assert_eq!(
            reported_code, "InvalidChangeSetStatus",
            "case {case:?}: expected InvalidChangeSetStatus, got {reported_code:?} {body}"
        );
        let expected = if sibling.change_set_status == change_set_status::CREATE_COMPLETE {
            format!(
                "ChangeSet [{change_set_id}] cannot be executed in its current execution \
                 status of [{}]",
                sibling.execution_status
            )
        } else {
            format!(
                "ChangeSet [{change_set_id}] cannot be executed in its current status of [{}]",
                sibling.change_set_status
            )
        };
        assert_eq!(
            message, expected,
            "case {case:?}: expected the refusal {expected:?}, got {message:?}"
        );
        // The refused call changed nothing: the record is where it was.
        let (again_status, again_execution, _) = described(&router, case, sibling.name, stack).await;
        assert_eq!(
            (again_status.as_str(), again_execution.as_str()),
            (sibling.change_set_status, sibling.execution_status),
            "case {case:?}: expected the refused execution to leave the record where it was, \
             got {again_status:?} {again_execution:?}"
        );
    }

    // Every record the stack holds is still listed, in creation order.
    let mut expected_names: Vec<String> = table
        .iter()
        .filter(|sibling| sibling.creation_concluded)
        .map(|sibling| sibling.name.to_string())
        .collect();
    expected_names.push("executed-set".to_string());
    expected_names.extend(
        table
            .iter()
            .filter(|sibling| !sibling.creation_concluded)
            .map(|sibling| sibling.name.to_string()),
    );
    assert_eq!(
        listed(&router, stack).await,
        expected_names,
        "expected every record of the stack to stay listed"
    );

    // The other stack's plan is untouched, and still executes.
    let (status_elsewhere, execution_elsewhere, _) = described(
        &router,
        "another stack's change set",
        "untouched-set",
        elsewhere,
    )
    .await;
    assert_eq!(
        (status_elsewhere.as_str(), execution_elsewhere.as_str()),
        (
            change_set_status::CREATE_COMPLETE,
            execution_status::AVAILABLE
        ),
        "case \"another stack's change set\": expected it to stay ready to execute, got \
         {status_elsewhere:?} {execution_elsewhere:?}"
    );
    let (code, body) = execute_change_set(&router, "untouched-set", Some(elsewhere)).await;
    assert_eq!(
        code, 200,
        "case \"another stack's change set\": expected it to still execute, got {code} {body}"
    );

    // A SECOND execution on the stack: the plan it supersedes is already obsolete and stays
    // so, and the change set the first execution carried out keeps the record of that
    // execution rather than being invalidated with the plans.
    let (code, body) =
        create_updating_change_set(&router, stack, "second-set", &adding("F")).await;
    assert_eq!(code, 200, "the second change set must be created, got {code} {body}");
    settle_the_store(&router, &engine).await;
    let (code, body) = execute_change_set(&router, "second-set", Some(stack)).await;
    assert_eq!(code, 200, "the second change set must execute, got {code} {body}");
    settle_the_store(&router, &engine).await;

    for (case, name, expected_execution) in [
        (
            "a plan the first execution had already invalidated",
            "obsolete-ready",
            execution_status::OBSOLETE,
        ),
        (
            "a creation that left nothing to execute",
            "obsolete-no-changes",
            execution_status::UNAVAILABLE,
        ),
        (
            "the change set the first execution carried out",
            "executed-set",
            execution_status::EXECUTE_COMPLETE,
        ),
        (
            "the change set this execution carried out",
            "second-set",
            execution_status::EXECUTE_COMPLETE,
        ),
    ] {
        let (_, reported_execution, _) = described(&router, case, name, stack).await;
        assert_eq!(
            reported_execution, expected_execution,
            "case {case:?}: expected ExecutionStatus {expected_execution:?}, got \
             {reported_execution:?}"
        );
    }
}
