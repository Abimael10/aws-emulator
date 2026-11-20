// SPDX-License-Identifier: CC0-1.0

//! Where an `aws:branch` step sends the walk, and in particular where it sends it when NONE
//! of its choices hold.
//!
//! "Default — (optional) the name of a step the automation should send the workflow to in
//! the event that none of the Choices can be evaluated to true" (`aws:branch`): the member
//! is OPTIONAL, and a branch that declares none names nowhere of its own — so the step takes
//! the ordering every step of a runbook takes when nothing names a successor, "the next step
//! in the mainSteps section". A branch is a step like any other: it succeeds, and `nextStep`,
//! `isEnd` and the declaration order decide what runs after it.
//!
//! The consequence the cases below pin is the one a branching runbook is written for: the
//! step the branch reaches runs, and the step it did not reach does not — so the other
//! branch's effect is absent, whichever way the branch went and whether it went by a choice,
//! by its `Default` or by falling through.
//!
//! Every case goes through the front door a client uses — the shared harness in
//! `all/parameter_store.rs` ([`call`], [`router_with_engine`]) dispatches `POST /` with
//! `X-Amz-Target: AmazonSSM.<Operation>` through the multi-service router — and the runbook
//! is created through `CreateDocument` first. The one thing reached through the store is the
//! CLOCK: the pickup a start owes is moved into the past rather than waited for.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_ssm_amazonssm::state::ComAmazonawsSsmAmazonssmState;
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{call, router_with_engine, REGION};

/// How many reads a case may take before it stops waiting for a walk to end. Generous: the
/// walk advances on each read and nothing here sleeps, so this is a deadline on the loop
/// rather than a span anything is timed against.
const MAX_READS: usize = 64;

/// An instant well in the past, which the pickup is moved to.
fn long_ago() -> SystemTime {
    SystemTime::now() - Duration::from_secs(3_600)
}

/// Let the clock reach the pickup every execution of [`REGION`] is waiting for, so a case
/// asserting what the walk did never waits for a span to pass.
fn pick_up(engine: &Arc<engine::InMemoryEngine>) {
    let state = engine.state::<ComAmazonawsSsmAmazonssmState>();
    let mut store = state.lock();
    for ((held, _), execution) in store.automation_executions.iter_mut() {
        if held == REGION && execution.runnable_at.is_some() {
            execution.runnable_at = Some(long_ago());
        }
    }
}

/// Create an Automation runbook of `name` with `content`, asserting it succeeded.
async fn runbook(router: &MultiServiceRouter, name: &str, content: &Value) {
    let (status, created) = call(
        router,
        "CreateDocument",
        &json!({
            "Name": name,
            "DocumentType": "Automation",
            "Content": content.to_string(),
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the runbook {name} must be created first, got {status} {created}"
    );
}

/// A runbook that branches on its `Path` parameter and then declares `left` and `right` in
/// that order, each ending the automation. `default` is the branch's own `Default`, or
/// `None` for a branch that declares none.
fn branching_runbook(default: Option<&str>) -> Value {
    let mut inputs = json!({
        "Choices": [
            {"NextStep": "left", "Variable": "{{ Path }}", "StringEquals": "left"},
            {"NextStep": "right", "Variable": "{{ Path }}", "StringEquals": "right"},
        ],
    });
    if let Some(default) = default {
        inputs["Default"] = json!(default);
    }
    json!({
        "schemaVersion": "0.3",
        "description": "Branches on one parameter.",
        "parameters": {"Path": {"type": "String", "default": "neither"}},
        "mainSteps": [
            {"name": "decide", "action": "aws:branch", "inputs": inputs},
            {"name": "left", "action": "aws:sleep", "inputs": {"Duration": "PT0S"}, "isEnd": true},
            {"name": "right", "action": "aws:sleep", "inputs": {"Duration": "PT0S"}, "isEnd": true},
        ],
    })
}

/// Start the runbook `document` with `Path` bound to `path`, let the clock reach the pickup
/// and read the execution back until its walk has ended, answering the execution body.
async fn walked(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    document: &str,
    path: &str,
) -> Value {
    let (status, started) = call(
        router,
        "StartAutomationExecution",
        &json!({"DocumentName": document, "Parameters": {"Path": [path]}}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the start must succeed, got {status} {started}"
    );
    let execution_id = started["AutomationExecutionId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the start reports an id, got {started}"))
        .to_string();
    pick_up(engine);
    let mut last = Value::Null;
    for _ in 0..MAX_READS {
        let (status, response) = call(
            router,
            "GetAutomationExecution",
            &json!({"AutomationExecutionId": execution_id}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the read must be served, got {status} {response}"
        );
        last = response["AutomationExecution"].clone();
        let status = last["AutomationExecutionStatus"].as_str().unwrap_or_default();
        if !matches!(status, "Pending" | "InProgress" | "Waiting") {
            return last;
        }
    }
    panic!("case {case:?}: the walk never ended, last read {last}");
}

/// Each step of an execution as `(name, status)`, in the runbook's own order.
fn steps(execution: &Value) -> Vec<(String, String)> {
    execution["StepExecutions"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|step| {
            (
                step["StepName"].as_str().unwrap_or("<unset>").to_string(),
                step["StepStatus"].as_str().unwrap_or("<unset>").to_string(),
            )
        })
        .collect()
}

/// The `ValidNextSteps` one step of an execution reports ("strings that describe the step's
/// valid next steps", `StepExecution$ValidNextSteps`).
fn valid_next_steps(execution: &Value, name: &str) -> Vec<String> {
    execution["StepExecutions"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .find(|step| step["StepName"].as_str() == Some(name))
        .and_then(|step| step["ValidNextSteps"].as_array().cloned())
        .unwrap_or_default()
        .iter()
        .map(|next| next.as_str().unwrap_or("<unset>").to_string())
        .collect()
}

/// One row of the walk table: what a failing assertion calls the case, the runbook to start,
/// the `Path` value to bind, and the `(step, status)` pairs the walk must leave behind.
type BranchRow = (
    &'static str,
    &'static str,
    &'static str,
    [(&'static str, &'static str); 3],
);

/// RULE: a branch goes where a holding choice points, where its `Default` points when none
/// holds, and — when none holds and it declares no `Default` — on to the step declared after
/// it. The step it did not reach never runs. The rows differ along which choice holds,
/// whether the branch declares a `Default`, and which of the two following steps the walk
/// therefore reaches.
#[tokio::test]
async fn a_branch_whose_choices_do_not_hold_falls_through_to_the_next_declared_step() {
    let (router, engine) = router_with_engine();
    runbook(&router, "BranchWithDefault", &branching_runbook(Some("right"))).await;
    runbook(&router, "BranchWithoutDefault", &branching_runbook(None)).await;

    // (case, the runbook, the Path value, the step statuses the walk must leave)
    let rows: [BranchRow; 6] = [
        (
            "a holding choice, with a Default declared",
            "BranchWithDefault",
            "left",
            [("decide", "Success"), ("left", "Success"), ("right", "Pending")],
        ),
        (
            "the second holding choice, with a Default declared",
            "BranchWithDefault",
            "right",
            [("decide", "Success"), ("left", "Pending"), ("right", "Success")],
        ),
        (
            "no choice holds, with a Default declared",
            "BranchWithDefault",
            "neither",
            [("decide", "Success"), ("left", "Pending"), ("right", "Success")],
        ),
        (
            "a holding choice, with no Default declared",
            "BranchWithoutDefault",
            "left",
            [("decide", "Success"), ("left", "Success"), ("right", "Pending")],
        ),
        (
            "the second holding choice, with no Default declared",
            "BranchWithoutDefault",
            "right",
            [("decide", "Success"), ("left", "Pending"), ("right", "Success")],
        ),
        (
            "no choice holds and no Default is declared",
            "BranchWithoutDefault",
            "neither",
            [("decide", "Success"), ("left", "Success"), ("right", "Pending")],
        ),
    ];

    for (case, document, path, expected) in rows {
        let execution = walked(&router, &engine, case, document, path).await;
        assert_eq!(
            execution["AutomationExecutionStatus"].as_str(),
            Some("Success"),
            "case {case:?}: the execution must end Success, got {execution}"
        );
        let walked_steps = steps(&execution);
        let expected: Vec<(String, String)> = expected
            .iter()
            .map(|(name, status)| ((*name).to_string(), (*status).to_string()))
            .collect();
        assert_eq!(
            walked_steps, expected,
            "case {case:?}: expected {expected:?}, got {walked_steps:?}"
        );
    }
}

/// The same rule where there IS no step after the branch: a branch that declares no
/// `Default`, whose choices do not hold and that is the last step declared succeeds, and the
/// execution ends with it — the walk has nowhere left to go, which is the same ordering rule
/// read at the end of the runbook.
#[tokio::test]
async fn a_branch_that_falls_through_off_the_end_of_the_runbook_ends_the_execution() {
    let (router, engine) = router_with_engine();
    let content = json!({
        "schemaVersion": "0.3",
        "parameters": {"Path": {"type": "String", "default": "neither"}},
        "mainSteps": [
            {"name": "only", "action": "aws:sleep", "inputs": {"Duration": "PT0S"}},
            {
                "name": "decide",
                "action": "aws:branch",
                "inputs": {
                    "Choices": [
                        {"NextStep": "only", "Variable": "{{ Path }}", "StringEquals": "again"},
                    ],
                },
            },
        ],
    });
    runbook(&router, "BranchAtTheEnd", &content).await;

    let execution = walked(
        &router,
        &engine,
        "a branch at the end of the runbook",
        "BranchAtTheEnd",
        "neither",
    )
    .await;
    assert_eq!(
        execution["AutomationExecutionStatus"].as_str(),
        Some("Success"),
        "the execution must end Success, got {execution}"
    );
    assert_eq!(
        steps(&execution),
        vec![
            ("only".to_string(), "Success".to_string()),
            ("decide".to_string(), "Success".to_string()),
        ],
        "both steps must have run once, got {execution}"
    );
    assert_eq!(
        valid_next_steps(&execution, "decide"),
        vec!["only".to_string()],
        "the branch's one choice is the only step it could enter, got {execution}"
    );
}

/// The step PROJECTION agrees with the walk: the successor a defaultless branch falls
/// through to is one of the branch step's own `ValidNextSteps`, and the step the walk did not
/// enter is still the one it did not enter. The rows differ along whether the branch declares
/// a `Default`, with the fall-through step DISTINCT from every choice's target so the two
/// answers cannot coincide.
#[tokio::test]
async fn a_branch_reports_the_successor_it_falls_through_to_among_its_valid_next_steps() {
    let (router, engine) = router_with_engine();

    /// A runbook whose branch chooses `chosen` and whose NEXT DECLARED step is `fallthrough`.
    fn content(default: Option<&str>) -> Value {
        let mut inputs = json!({
            "Choices": [
                {"NextStep": "chosen", "Variable": "{{ Path }}", "StringEquals": "chosen"},
            ],
        });
        if let Some(default) = default {
            inputs["Default"] = json!(default);
        }
        json!({
            "schemaVersion": "0.3",
            "parameters": {"Path": {"type": "String", "default": "neither"}},
            "mainSteps": [
                {"name": "decide", "action": "aws:branch", "inputs": inputs},
                {
                    "name": "fallthrough",
                    "action": "aws:sleep",
                    "inputs": {"Duration": "PT0S"},
                    "isEnd": true,
                },
                {
                    "name": "chosen",
                    "action": "aws:sleep",
                    "inputs": {"Duration": "PT0S"},
                    "isEnd": true,
                },
            ],
        })
    }

    runbook(&router, "FallThroughWithDefault", &content(Some("chosen"))).await;
    runbook(&router, "FallThroughWithoutDefault", &content(None)).await;

    // (case, the runbook, the branch's ValidNextSteps, the step the walk must enter, the step
    // it must leave alone)
    let rows: [(&str, &str, &[&str], &str, &str); 2] = [
        (
            "no Default declared",
            "FallThroughWithoutDefault",
            &["chosen", "fallthrough"],
            "fallthrough",
            "chosen",
        ),
        (
            "a Default declared",
            "FallThroughWithDefault",
            &["chosen"],
            "chosen",
            "fallthrough",
        ),
    ];

    for (case, document, reported, entered, untouched) in rows {
        let execution = walked(&router, &engine, case, document, "neither").await;
        let projected = valid_next_steps(&execution, "decide");
        assert_eq!(
            projected,
            reported
                .iter()
                .map(|name| (*name).to_string())
                .collect::<Vec<String>>(),
            "case {case:?}: expected ValidNextSteps {reported:?}, got {projected:?}"
        );
        let walked_steps = steps(&execution);
        assert!(
            walked_steps.contains(&(entered.to_string(), "Success".to_string())),
            "case {case:?}: {entered} must have run, got {walked_steps:?}"
        );
        assert!(
            walked_steps.contains(&(untouched.to_string(), "Pending".to_string())),
            "case {case:?}: {untouched} must not have run, got {walked_steps:?}"
        );
    }
}
