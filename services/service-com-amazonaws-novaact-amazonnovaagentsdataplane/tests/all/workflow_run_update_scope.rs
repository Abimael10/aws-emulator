// SPDX-License-Identifier: CC0-1.0

//! `UpdateWorkflowRun`: the write lands on exactly the one STORED run the path names, and
//! every reader of a run reports that one write.
//!
//! The rule these cases assert is not "this one request answers these bytes": the update
//! resolves the definition the path names and then the run under it, writes the status the
//! request carries plus the instant the finish happened onto that record alone, and leaves
//! every other member of it and every other stored run exactly as they were — so the
//! committed status and `endedAt` read back identically through `GetWorkflowRun` and through
//! `ListWorkflowRuns`, a sibling run under the same definition stays as it was, and a run id
//! minted under one definition does not resolve under another (404, reporting the run as the
//! missing resource under the definition the path named).
//!
//! So the tables below vary what that rule generalises over: which of the statuses that
//! finish a run is asked for, how many sibling runs sit under the same definition, how many
//! other definitions hold runs of their own, which reader the stored run is read back
//! through, and which definition a run id is named under. The recorded "update one run to
//! SUCCEEDED" case is one row among them.

use serde_json::{json, Value};

use crate::workflow_lifecycle::{text, Emulator};

/// The definition each case hangs its target run off.
const DEFINITION: &str = "run-update-scope";

/// The kind name a refusal of this service reports for a workflow run.
const RUN_RESOURCE_TYPE: &str = "workflow-run";

/// The values of `WorkflowRunStatus` that FINISH a run: it ran to the end, it failed, or it
/// ran out of time. Only a run in one of them reports an `endedAt`
/// (`smithy_query(query="shape",shape="WorkflowRunStatus")`).
const FINISHING_STATUSES: [&str; 3] = ["SUCCEEDED", "FAILED", "TIMED_OUT"];

/// The members a finished run reports through BOTH readers of a run: `GetWorkflowRun`'s
/// response and the `WorkflowRunSummary` a listing carries
/// (`smithy_query(query="shape",shape="GetWorkflowRunResponse,WorkflowRunSummary")`).
const SHARED_RUN_MEMBERS: [&str; 5] = [
    "workflowRunArn",
    "workflowRunId",
    "status",
    "startedAt",
    "endedAt",
];

/// The `UpdateWorkflowRun` / `GetWorkflowRun` path: both identifiers are `@httpLabel`s
/// (`smithy_query(query="operation",operation="UpdateWorkflowRun")`).
fn run_path(definition: &str, run_id: &str) -> String {
    format!("/workflow-definitions/{definition}/workflow-runs/{run_id}")
}

/// One definition to hang runs off.
async fn definition(emulator: &Emulator, name: &str) {
    let (status, body) = emulator.create_definition(name).await;
    assert_eq!(
        status, 201,
        "the definition {name} must be created, got {status} {body}"
    );
}

/// One new run under `definition_name`, created `RUNNING` as `CreateWorkflowRun` stores it.
async fn new_run(emulator: &Emulator, definition_name: &str) -> String {
    let (status, body) = emulator.create_run(definition_name).await;
    assert_eq!(
        status, 201,
        "a run under {definition_name} must be created, got {status} {body}"
    );
    text(&body, "workflowRunId")
}

/// One `UpdateWorkflowRun` call asking for `status`.
async fn move_to(
    emulator: &Emulator,
    definition_name: &str,
    run_id: &str,
    status: &str,
) -> (u16, Value) {
    emulator
        .call(
            "PUT",
            &run_path(definition_name, run_id),
            Some(json!({ "status": status })),
        )
        .await
}

/// The stored run read back through `GetWorkflowRun`.
async fn get_run(emulator: &Emulator, definition_name: &str, run_id: &str) -> Value {
    let (status, body) = emulator
        .call("GET", &run_path(definition_name, run_id), None)
        .await;
    assert_eq!(
        status, 200,
        "the read of {run_id} under {definition_name} must answer, got {status} {body}"
    );
    body
}

/// The summaries `ListWorkflowRuns` reports for one definition. `sortOrder` travels in the
/// body and `maxResults` is an `@httpQuery` member this call leaves to its default, which is
/// wider than any case below stores
/// (`smithy_query(query="operation",operation="ListWorkflowRuns")`).
async fn list_runs(emulator: &Emulator, definition_name: &str) -> Vec<Value> {
    let (status, body) = emulator
        .call(
            "POST",
            &format!("/workflow-definitions/{definition_name}/workflow-runs"),
            Some(json!({})),
        )
        .await;
    assert_eq!(
        status, 200,
        "the listing of {definition_name} must answer, got {status} {body}"
    );
    body["workflowRunSummaries"]
        .as_array()
        .unwrap_or_else(|| panic!("workflowRunSummaries must be a list in {body}"))
        .clone()
}

/// The one summary a listing carries for `run_id`.
fn summary_of(case: &str, summaries: &[Value], run_id: &str) -> Value {
    let found: Vec<&Value> = summaries
        .iter()
        .filter(|summary| summary["workflowRunId"] == run_id)
        .collect();
    assert_eq!(
        found.len(),
        1,
        "case {case}: the listing must carry exactly one summary for {run_id}, got {summaries:?}"
    );
    found[0].clone()
}

/// A modelled timestamp member as epoch seconds. This model's `DateTimestamp` carries
/// `@timestampFormat("date-time")`, so the wire form is the RFC 3339 string; the numeric form
/// every other Smithy timestamp takes is accepted too, so this helper reads the member rather
/// than the wire form.
fn timestamp_seconds(case: &str, body: &Value, member: &str) -> f64 {
    match &body[member] {
        Value::Number(seconds) => seconds
            .as_f64()
            .unwrap_or_else(|| panic!("case {case}: member {member} must be finite in {body}")),
        Value::String(rendered) => ::aws_smithy_types::DateTime::from_str(
            rendered,
            ::aws_smithy_types::date_time::Format::DateTime,
        )
        .unwrap_or_else(|error| {
            panic!("case {case}: member {member} must be an RFC 3339 date-time: {error}")
        })
        .as_secs_f64(),
        other => panic!("case {case}: member {member} must be a timestamp, got {other}"),
    }
}

/// One update writes the run the path names and NOTHING else the store holds.
#[tokio::test]
async fn an_update_writes_only_the_run_the_path_names() {
    // The rows differ in which finishing status is asked for, how many sibling runs sit
    // under the same definition, and how many other definitions hold a run of their own.
    // Every definition name below is a `WorkflowDefinitionName` (`^[a-zA-Z0-9_-]{1,40}$`).
    let rows: [(&str, &str, usize, &[&str]); 5] = [
        ("the only run in the store", "SUCCEEDED", 0, &[]),
        ("one sibling under the same definition", "FAILED", 1, &[]),
        (
            "three siblings under the same definition",
            "TIMED_OUT",
            3,
            &[],
        ),
        (
            "a sibling and a run under another definition",
            "SUCCEEDED",
            1,
            &["other-1"],
        ),
        (
            "runs under two other definitions",
            "FAILED",
            2,
            &["other_a", "other-b-2"],
        ),
    ];

    for (case, requested, siblings, others) in rows {
        let emulator = Emulator::new("us-east-1");
        definition(&emulator, DEFINITION).await;
        for other in others {
            definition(&emulator, other).await;
        }

        let target = new_run(&emulator, DEFINITION).await;
        // Every run the update must leave alone, with the definition it lives under.
        let mut untouched: Vec<(String, String)> = Vec::new();
        for _ in 0..siblings {
            let sibling = new_run(&emulator, DEFINITION).await;
            untouched.push((DEFINITION.to_string(), sibling));
        }
        for other in others {
            let run_id = new_run(&emulator, other).await;
            untouched.push(((*other).to_string(), run_id));
        }

        let before_target = get_run(&emulator, DEFINITION, &target).await;
        let mut before: Vec<Value> = Vec::new();
        for (definition_name, run_id) in &untouched {
            before.push(get_run(&emulator, definition_name, run_id).await);
        }

        let (status, answer) = move_to(&emulator, DEFINITION, &target, requested).await;
        assert_eq!(
            status, 200,
            "case {case}: expected HTTP 200, got {status} {answer}"
        );
        // `UpdateWorkflowRunResponse` declares no members, so the success carries nothing.
        assert!(
            answer.is_null() || answer == json!({}),
            "case {case}: expected the modelled empty body, got {answer}"
        );

        let after_target = get_run(&emulator, DEFINITION, &target).await;
        assert_eq!(
            after_target["status"], requested,
            "case {case}: expected the target to report {requested}, got {after_target}"
        );
        assert!(
            !after_target["endedAt"].is_null(),
            "case {case}: a finished run reports endedAt, got {after_target}"
        );
        // The status and the finish are the ONLY members the update wrote.
        for member in ["workflowRunId", "workflowRunArn", "startedAt", "modelId"] {
            assert_eq!(
                after_target[member], before_target[member],
                "case {case}: member {member} must not move, got {after_target} \
                 after {before_target}"
            );
        }

        for ((definition_name, run_id), was) in untouched.iter().zip(before) {
            let now = get_run(&emulator, definition_name, run_id).await;
            assert_eq!(
                now, was,
                "case {case}: the run {run_id} under {definition_name} must be untouched, \
                 got {now}"
            );
            assert_eq!(
                now["status"], "RUNNING",
                "case {case}: the run {run_id} under {definition_name} must still be RUNNING, \
                 got {now}"
            );
        }
    }
}

/// Both readers of a run report the one write the update committed, and a read never changes
/// what a later read sees.
#[tokio::test]
async fn every_reader_reports_the_finish_the_update_committed() {
    let emulator = Emulator::new("eu-central-1");
    definition(&emulator, DEFINITION).await;

    for requested in FINISHING_STATUSES {
        let run_id = new_run(&emulator, DEFINITION).await;
        let before = get_run(&emulator, DEFINITION, &run_id).await;
        assert!(
            before["endedAt"].is_null(),
            "case {requested}: a run still under way reports no endedAt, got {before}"
        );

        let (status, answer) = move_to(&emulator, DEFINITION, &run_id, requested).await;
        assert_eq!(
            status, 200,
            "case {requested}: expected HTTP 200, got {status} {answer}"
        );

        let read = get_run(&emulator, DEFINITION, &run_id).await;
        let again = get_run(&emulator, DEFINITION, &run_id).await;
        assert_eq!(
            again, read,
            "case {requested}: a read must not change what a later read reports, got {again}"
        );
        assert_eq!(
            read["startedAt"], before["startedAt"],
            "case {requested}: the instant the run started must not move, got {read}"
        );

        let summary = summary_of(requested, &list_runs(&emulator, DEFINITION).await, &run_id);
        for member in SHARED_RUN_MEMBERS {
            assert_eq!(
                summary[member], read[member],
                "case {requested}: member {member} must read alike through both readers, \
                 got {summary} and {read}"
            );
        }

        // The instant the run finished is the instant of the update, so it cannot precede
        // the instant the run started.
        let started = timestamp_seconds(requested, &read, "startedAt");
        let ended = timestamp_seconds(requested, &read, "endedAt");
        assert!(
            ended >= started,
            "case {requested}: expected endedAt {ended} at or after startedAt {started}, \
             got {read}"
        );
    }
}

/// A run is reached through the definition it was created under: naming its id under another
/// definition resolves nothing, and writes nothing.
#[tokio::test]
async fn a_run_does_not_resolve_under_a_definition_it_was_not_created_under() {
    let region = "ap-northeast-1";
    let emulator = Emulator::new(region);
    // Three definitions, each holding one run, so every (owner, other) pair is a row.
    let definitions: [&str; 3] = ["scope-a", "scope_b", "scope-c-3"];
    let mut runs: Vec<(&str, String)> = Vec::new();
    for name in definitions {
        definition(&emulator, name).await;
    }
    for name in definitions {
        runs.push((name, new_run(&emulator, name).await));
    }

    for (owner, run_id) in &runs {
        for other in definitions {
            if other == *owner {
                continue;
            }
            let case = format!("the run of {owner} named under {other}");
            let (status, answer) = move_to(&emulator, other, run_id, "SUCCEEDED").await;
            assert_eq!(
                status, 404,
                "case {case}: expected HTTP 404, got {status} {answer}"
            );
            assert_eq!(
                answer["__type"], "ResourceNotFoundException",
                "case {case}: expected ResourceNotFoundException, got {answer}"
            );
            assert_eq!(
                answer["resourceType"], RUN_RESOURCE_TYPE,
                "case {case}: expected resourceType {RUN_RESOURCE_TYPE:?}, got {answer}"
            );
            let expected = format!(
                "arn:aws:nova-act:{region}:{}:workflow-definition/{other}/workflow-run/{run_id}",
                engine::EMULATOR_ACCOUNT_ID
            );
            assert_eq!(
                answer["resourceId"], expected,
                "case {case}: expected resourceId {expected:?}, got {answer}"
            );
        }
    }

    // Every run is still exactly where it was: no refused update wrote anything.
    for (owner, run_id) in &runs {
        let read = get_run(&emulator, owner, run_id).await;
        assert_eq!(
            read["status"], "RUNNING",
            "case the run of {owner}: expected it to still be RUNNING, got {read}"
        );
        assert!(
            read["endedAt"].is_null(),
            "case the run of {owner}: expected no endedAt, got {read}"
        );
        let summary = summary_of(owner, &list_runs(&emulator, owner).await, run_id);
        assert_eq!(
            summary["status"], "RUNNING",
            "case the run of {owner}: the listing must report it RUNNING too, got {summary}"
        );
    }
}
