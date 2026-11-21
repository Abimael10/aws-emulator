// SPDX-License-Identifier: CC0-1.0

//! What a stack whose FAILED CREATE was kept takes next, and the teardown that follows,
//! end to end over the awsQuery wire.
//!
//! RULE under test: **the failed state of a create is kept either because the request named
//! the failure ACTION (`OnFailure=DO_NOTHING`) or because it stated the FLAG that disables the
//! rollback (`DisableRollback=true`), and which of the two it used decides what the stack takes
//! next: a
//! stack kept by the ACTION takes no deployment at all — neither `UpdateStack` nor the
//! execution of a change set, whatever the request asks — and is refused `400 ValidationError`
//! "This stack cannot be updated from [&lt;StackStatus&gt;] state because it contains
//! [on-failure: &lt;action&gt;]. Please use disable-rollback option instead during stack
//! creation.", while a stack kept by the FLAG takes a deployment that states that flag again
//! and is otherwise refused the non-terminal-state sentence. `Stack$DisableRollback` reports
//! the flag the request stated, so it is `false` for a stack kept by the action. And since a
//! refused deployment starts nothing, the stack stays SETTLED: the `DeleteStack` a teardown
//! then makes is accepted with an empty result and really removes the stack, after which its
//! name resolves nothing (`Stack with id &lt;name&gt; does not exist`).**
//!
//! Why it matters: a caller that cannot update such a stack has to recreate it, and the
//! teardown that recreation starts with is the `DeleteStack` every `cdk destroy` and
//! `aws cloudformation delete-stack` makes. A refusal that instead starts an update AWS would
//! never have started leaves an operation in flight on the stack, which refuses that delete
//! and leaks the stack and every resource it holds.
//!
//! The rows differ along what the rule generalises over: which statement kept the failed state
//! (the action, the flag), what the next request asks for (nothing, `DisableRollback=true`,
//! `DisableRollback=false`), and which call would deploy it (`UpdateStack`, `ExecuteChangeSet`).

use std::time::{Duration, Instant};

use serde_json::json;
use service_com_amazonaws_cloudformation_cloudformation::state::status;
use smithy_server_runtime::MultiServiceRouter;

use crate::change_set_lifecycle::{create_change_set, settle_the_store};
use crate::harness::{
    call, create_stack, member, refusal, router_with_engine, xml_blocks, xml_text,
};
use crate::stack_lifecycle::describe_stack;

/// This service's one wire code for a request it refuses on input grounds.
const VALIDATION_ERROR: &str = "ValidationError";

/// The failure ACTION that keeps a failed create's state, as `CreateStack`'s `OnFailure`
/// spells it.
const DO_NOTHING: &str = "DO_NOTHING";

/// How long a poll waits for a condition the emulator owes: generous, since nothing here is
/// timed — every wait below looks again until the condition holds.
const DEADLINE: Duration = Duration::from_secs(120);

/// How long a poll waits before looking again, which is how a client watches a stack.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The template every row deploys: one parameter the create makes, and one restating that
/// same name, which CloudFormation itself refuses — so the create fails with a resource
/// already made, which is the state the rows are about.
fn failing_template(name: &str) -> String {
    json!({"Resources": {
        "Made": {
            "Type": "AWS::SSM::Parameter",
            "Properties": {"Name": name, "Type": "String", "Value": "made-by-the-create"}
        },
        "Clash": {
            "Type": "AWS::SSM::Parameter",
            "DependsOn": "Made",
            "Properties": {"Name": name, "Type": "String", "Value": "the-same-name-twice"}
        }
    }})
    .to_string()
}

/// A template the same stack could be corrected with: one parameter under a name of its own,
/// so a deployment of it has work to carry out.
fn corrected_template(name: &str) -> String {
    json!({"Resources": {
        "Made": {
            "Type": "AWS::SSM::Parameter",
            "Properties": {"Name": name, "Type": "String", "Value": "corrected"}
        }
    }})
    .to_string()
}

/// The sentence a deployment onto a stack kept by its failure ACTION is refused with.
fn kept_by_its_action(stack_status: &str, action: &str) -> String {
    format!(
        "This stack cannot be updated from [{stack_status}] state because it contains \
         [on-failure: {action}]. Please use disable-rollback option instead during stack \
         creation."
    )
}

/// The sentence a deployment onto a stack kept by the FLAG is refused with when the request
/// does not state that flag again.
fn non_terminal(stack_status: &str) -> String {
    format!(
        "This stack is currently in a non-terminal [{stack_status}] state. To update the stack \
         from this state, please use the disable-rollback parameter with update-stack API. To \
         rollback to the last known good state, use the rollback-stack API"
    )
}

/// What the next deployment onto the kept stack gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Next {
    /// Refused with the sentence for a stack kept by its failure action.
    KeptByTheAction,
    /// Refused with the non-terminal-state sentence.
    NonTerminal,
    /// Carried out.
    Accepted,
}

/// One row: how the failed create's state was kept, what the next request states about the
/// flag, and what that request gets.
struct Row {
    case: &'static str,
    stack: &'static str,
    /// The `CreateStack` members that keep the failed state.
    kept_by: Vec<(String, String)>,
    /// Whether the create named the failure action, which is what the refusal then states.
    action: Option<&'static str>,
    /// `DisableRollback` the next request states, where it states it.
    states: Option<&'static str>,
    next: Next,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "kept by OnFailure=DO_NOTHING, the update asks for nothing",
            stack: "kept-action-unasked",
            kept_by: vec![member("OnFailure", DO_NOTHING)],
            action: Some(DO_NOTHING),
            states: None,
            next: Next::KeptByTheAction,
        },
        Row {
            case: "kept by OnFailure=DO_NOTHING, the update asks for the flag",
            stack: "kept-action-flagged",
            kept_by: vec![member("OnFailure", DO_NOTHING)],
            action: Some(DO_NOTHING),
            states: Some("true"),
            next: Next::KeptByTheAction,
        },
        Row {
            case: "kept by OnFailure=DO_NOTHING, the update states the flag as false",
            stack: "kept-action-unflagged",
            kept_by: vec![member("OnFailure", DO_NOTHING)],
            action: Some(DO_NOTHING),
            states: Some("false"),
            next: Next::KeptByTheAction,
        },
        Row {
            case: "kept by DisableRollback=true, the update asks for nothing",
            stack: "kept-flag-unasked",
            kept_by: vec![member("DisableRollback", "true")],
            action: None,
            states: None,
            next: Next::NonTerminal,
        },
        Row {
            case: "kept by DisableRollback=true, the update states the flag as false",
            stack: "kept-flag-unflagged",
            kept_by: vec![member("DisableRollback", "true")],
            action: None,
            states: Some("false"),
            next: Next::NonTerminal,
        },
        Row {
            case: "kept by DisableRollback=true, the update asks for the flag again",
            stack: "kept-flag-flagged",
            kept_by: vec![member("DisableRollback", "true")],
            action: None,
            states: Some("true"),
            next: Next::Accepted,
        },
    ]
}

/// Deploy the failing template and answer the stack's id once the create has settled in the
/// kept failure status.
async fn a_kept_failed_create(
    router: &MultiServiceRouter,
    engine: &std::sync::Arc<engine::InMemoryEngine>,
    case: &str,
    stack: &str,
    kept_by: &[(String, String)],
) -> String {
    let template = failing_template(&format!("/{stack}/made"));
    let (code, body) = create_stack(router, stack, &template, kept_by).await;
    assert_eq!(
        code, 200,
        "case {case:?}: CreateStack must be accepted, got {code} {body}"
    );
    settle_the_store(router, engine).await;
    let (code, described) = describe_stack(router, stack).await;
    assert_eq!(
        code, 200,
        "case {case:?}: DescribeStacks must answer 200, got {code} {described}"
    );
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some(status::CREATE_FAILED),
        "case {case:?}: expected the failed state to be kept, got {described}"
    );
    xml_text(&described, "StackId")
        .unwrap_or_else(|| panic!("case {case:?}: expected a StackId, got {described}"))
}

/// Assert one refused call: the status, the code and the whole sentence.
fn assert_refused(case: &str, answered: (u16, String), expected: &str) {
    let (code, body) = answered;
    assert_eq!(
        code, 400,
        "case {case:?}: expected the refusal, got {code} {body}"
    );
    let (wire_code, message) = refusal(&body);
    assert_eq!(
        (wire_code.as_str(), message.as_str()),
        (VALIDATION_ERROR, expected),
        "case {case:?}: expected {VALIDATION_ERROR} {expected:?}, got {body}"
    );
}

#[tokio::test]
async fn a_failed_create_kept_by_its_action_takes_no_deployment() {
    for row in rows() {
        let case = row.case;
        let (router, engine) = router_with_engine();
        let stack_id =
            a_kept_failed_create(&router, &engine, case, row.stack, &row.kept_by).await;

        // `Stack$DisableRollback` is the flag the REQUEST stated: a create that named the
        // failure action stated no flag, however the action behaves.
        let (_, described) = describe_stack(&router, row.stack).await;
        let expected_flag = if row.action.is_some() { "false" } else { "true" };
        assert_eq!(
            xml_text(&described, "DisableRollback").as_deref(),
            Some(expected_flag),
            "case {case:?}: expected DisableRollback {expected_flag}, got {described}"
        );

        let mut members = vec![
            member("StackName", row.stack),
            member("TemplateBody", &corrected_template(&format!("/{}/fix", row.stack))),
        ];
        if let Some(states) = row.states {
            members.push(member("DisableRollback", states));
        }
        let answered = call(&router, "UpdateStack", &members).await;

        match row.next {
            Next::KeptByTheAction => {
                let action = row.action.expect("a row refused for its action names one");
                assert_refused(
                    case,
                    answered,
                    &kept_by_its_action(status::CREATE_FAILED, action),
                );
            }
            Next::NonTerminal => {
                assert_refused(case, answered, &non_terminal(status::CREATE_FAILED));
            }
            Next::Accepted => {
                let (code, body) = answered;
                assert_eq!(
                    code, 200,
                    "case {case:?}: expected the update to be accepted, got {code} {body}"
                );
            }
        }

        // A refused deployment starts nothing: the stack is still resting in the status its
        // create left it in, which is what makes the teardown below possible.
        let (_, described) = describe_stack(&router, &stack_id).await;
        let expected_status = if row.next == Next::Accepted {
            status::UPDATE_IN_PROGRESS
        } else {
            status::CREATE_FAILED
        };
        assert_eq!(
            xml_text(&described, "StackStatus").as_deref(),
            Some(expected_status),
            "case {case:?}: expected the stack to be {expected_status}, got {described}"
        );
    }
}

/// The same gate on the other call that DEPLOYS onto a stack: executing a change set, which
/// a stack kept by its failure action refuses too — so the two cannot disagree about which
/// stack takes a deployment.
#[tokio::test]
async fn an_executed_change_set_is_refused_by_the_same_gate() {
    let case = "an UPDATE change set executed with DisableRollback=true";
    let (router, engine) = router_with_engine();
    let stack = "kept-action-change-set";
    a_kept_failed_create(
        &router,
        &engine,
        case,
        stack,
        &[member("OnFailure", DO_NOTHING)],
    )
    .await;

    let (code, body) = create_change_set(
        &router,
        stack,
        "the-correction",
        &[
            member("ChangeSetType", "UPDATE"),
            member(
                "TemplateBody",
                &corrected_template(&format!("/{stack}/fix")),
            ),
        ],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: CreateChangeSet must be accepted, got {code} {body}"
    );
    settle_the_store(&router, &engine).await;

    assert_refused(
        case,
        call(
            &router,
            "ExecuteChangeSet",
            &[
                member("ChangeSetName", "the-correction"),
                member("StackName", stack),
                member("DisableRollback", "true"),
            ],
        )
        .await,
        &kept_by_its_action(status::CREATE_FAILED, DO_NOTHING),
    );
    // ...and the stack is untouched, so the change set is still there to be executed by a
    // caller that resolves the failure first.
    let (_, described) = describe_stack(&router, stack).await;
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some(status::CREATE_FAILED),
        "case {case:?}: expected the refused execution to leave the stack alone, got {described}"
    );
}

/// The teardown a caller makes once it has been told the stack cannot be updated: the delete
/// is ACCEPTED with an empty result, and the stack really goes — its name resolves nothing
/// afterwards.
#[tokio::test]
async fn the_teardown_of_such_a_stack_is_accepted_and_removes_it() {
    for (case, kept_by) in [
        (
            "kept by OnFailure=DO_NOTHING",
            vec![member("OnFailure", DO_NOTHING)],
        ),
        (
            "kept by DisableRollback=true",
            vec![member("DisableRollback", "true")],
        ),
    ] {
        let (router, engine) = router_with_engine();
        let stack = "kept-teardown";
        let stack_id = a_kept_failed_create(&router, &engine, case, stack, &kept_by).await;

        // The update a client tries first is refused, which is what leaves the stack settled.
        let (code, _) = call(
            &router,
            "UpdateStack",
            &[
                member("StackName", stack),
                member("TemplateBody", &corrected_template(&format!("/{stack}/fix"))),
            ],
        )
        .await;
        assert_eq!(
            code, 400,
            "case {case:?}: the update must be refused for the teardown to follow"
        );

        let (code, deleted) = call(&router, "DeleteStack", &[member("StackName", stack)]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: DeleteStack must be accepted, got {code} {deleted}"
        );
        let result = xml_blocks(&deleted, "DeleteStackResult")
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("case {case:?}: expected a DeleteStackResult, got {deleted}"));
        assert!(
            result.trim().is_empty(),
            "case {case:?}: the deletion reports nothing of its own, got {result:?}"
        );

        // The deletion really removes the stack: its NAME stops resolving, with the sentence
        // every read of a stack this store holds nothing under answers.
        let deadline = Instant::now() + DEADLINE;
        loop {
            settle_the_store(&router, &engine).await;
            let (code, body) = describe_stack(&router, stack).await;
            if code == 400 {
                let (wire_code, message) = refusal(&body);
                assert_eq!(
                    (wire_code.as_str(), message.as_str()),
                    (
                        VALIDATION_ERROR,
                        format!("Stack with id {stack} does not exist").as_str()
                    ),
                    "case {case:?}: expected the not-found refusal, got {body}"
                );
                break;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: expected the deleted stack's name to stop resolving within \
                 {DEADLINE:?}, it still reads {body}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
        // ...and the stack that is gone is the one the delete was made against, which its own
        // id still reports in the terminal delete status.
        let (code, described) = describe_stack(&router, &stack_id).await;
        assert_eq!(
            code, 200,
            "case {case:?}: a deleted stack is still readable by its id, got {code} {described}"
        );
        assert_eq!(
            xml_text(&described, "StackStatus").as_deref(),
            Some(status::DELETE_COMPLETE),
            "case {case:?}: expected the deletion to have completed, got {described}"
        );
    }
}
