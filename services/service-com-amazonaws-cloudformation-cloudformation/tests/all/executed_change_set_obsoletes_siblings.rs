// SPDX-License-Identifier: CC0-1.0

//! What becomes of a stack's OTHER change sets when one of them is executed.
//!
//! RULE: executing a change set TAKES AWAY the other outstanding plans of the stack it
//! belongs to — "when you execute a change set, CloudFormation deletes all other change sets
//! associated with the stack because they aren't valid for the updated stack" — so each such
//! record stops being addressable: `DescribeChangeSet` for it is answered with the modelled
//! `ChangeSetNotFound` and `ListChangeSets` no longer lists it. Only a PLAN goes: a change set
//! whose own execution has begun carries the record of the change the stack took rather than a
//! plan for one, so it stays describable and listed, and another stack's change sets are not
//! this stack's plans at all.
//!
//! The table below spans the states a sibling can be in when the execution runs — a plan
//! ready to execute (two shapes of it), a creation whose outcome left nothing to execute, a
//! creation still in flight — and the second phase covers a stack whose plans a FIRST
//! execution had already taken away, so the recorded call this suite was written for is one
//! row of it. Every read goes through the awsQuery wire.

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

/// One sibling of the executed change set: the state its own creation leaves it in, which is
/// what the rows span — every one of them is an outstanding PLAN, so every one of them goes.
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
}

fn siblings() -> Vec<Sibling> {
    vec![
        Sibling {
            case: "a plan ready to execute",
            name: "superseded-ready",
            adds: Some("C"),
            creation_concluded: true,
        },
        Sibling {
            case: "a second plan ready to execute, adding a resource of its own",
            name: "superseded-ready-other",
            adds: Some("D"),
            creation_concluded: true,
        },
        Sibling {
            case: "a creation whose outcome left nothing to execute",
            name: "superseded-no-changes",
            adds: None,
            creation_concluded: true,
        },
        Sibling {
            case: "a creation still in flight when the execution ran",
            name: "superseded-in-flight",
            adds: Some("E"),
            creation_concluded: false,
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

/// Assert that `change_set` is no longer a change set of `stack`: the read is refused with the
/// modelled `ChangeSetNotFound`, and so is a call asking for it to be executed.
async fn assert_taken_away(router: &MultiServiceRouter, case: &str, change_set: &str, stack: &str) {
    for (what, (code, body)) in [
        (
            "DescribeChangeSet",
            describe_change_set(router, change_set, Some(stack)).await,
        ),
        (
            "ExecuteChangeSet",
            execute_change_set(router, change_set, Some(stack)).await,
        ),
    ] {
        assert_eq!(
            code, 404,
            "case {case:?}: expected {what} of a superseded plan to be refused, got {code} \
             {body}"
        );
        let (reported_code, _) = refusal(&body);
        assert_eq!(
            reported_code, "ChangeSetNotFound",
            "case {case:?}: expected {what} to answer ChangeSetNotFound, got {reported_code:?} \
             {body}"
        );
    }
}

/// The change set names `ListChangeSets` reports for one stack.
async fn listed(router: &MultiServiceRouter, stack: &str) -> Vec<String> {
    let (code, body) = call(router, "ListChangeSets", &[member("StackName", stack)]).await;
    assert_eq!(code, 200, "ListChangeSets must succeed, got {code} {body}");
    xml_texts(&body, "ChangeSetName")
}

#[tokio::test]
async fn executing_one_change_set_takes_away_its_siblings() {
    let (router, engine) = router_with_engine();
    let stack = "superseded-siblings";
    let elsewhere = "superseded-siblings-other";
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

    // Every sibling is addressable right up to the execution.
    for sibling in &table {
        let (code, body) = describe_change_set(&router, sibling.name, Some(stack)).await;
        assert_eq!(
            code, 200,
            "case {:?}: the sibling must be describable before the execution, got {code} {body}",
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

    // ...and every other plan of the stack is gone, whatever state its own creation left it
    // in.
    for sibling in &table {
        assert_taken_away(&router, sibling.case, sibling.name, stack).await;
    }

    // The only record the stack still holds is the one that was executed.
    assert_eq!(
        listed(&router, stack).await,
        vec!["executed-set".to_string()],
        "expected the executed change set to be the stack's only remaining record"
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

    // A SECOND execution on the stack: the change set the FIRST execution carried out keeps
    // the record of that execution rather than going with the plans, and the plan this one
    // supersedes — created after the first execution — goes the same way its siblings did.
    for (name, template) in [("second-set", adding("F")), ("spare-set", adding("G"))] {
        let (code, body) = create_updating_change_set(&router, stack, name, &template).await;
        assert_eq!(code, 200, "the change set {name} must be created, got {code} {body}");
    }
    settle_the_store(&router, &engine).await;
    let (code, body) = execute_change_set(&router, "second-set", Some(stack)).await;
    assert_eq!(code, 200, "the second change set must execute, got {code} {body}");
    settle_the_store(&router, &engine).await;

    assert_taken_away(
        &router,
        "a plan outstanding when the second execution ran",
        "spare-set",
        stack,
    )
    .await;
    for (case, name) in [
        ("the change set the first execution carried out", "executed-set"),
        ("the change set this execution carried out", "second-set"),
    ] {
        let (_, reported_execution, _) = described(&router, case, name, stack).await;
        assert_eq!(
            reported_execution,
            execution_status::EXECUTE_COMPLETE,
            "case {case:?}: expected ExecutionStatus \
             {:?}, got {reported_execution:?}",
            execution_status::EXECUTE_COMPLETE
        );
    }
}
