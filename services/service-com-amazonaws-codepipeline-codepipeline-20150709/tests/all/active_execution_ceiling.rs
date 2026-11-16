// SPDX-License-Identifier: CC0-1.0

//! The per-pipeline ACTIVE-EXECUTION ceiling, end to end over the wire the service
//! speaks: how many executions of one pipeline may be in flight at once, what the call
//! past that is answered, and when the slots come back.
//!
//! The rule under test: a pipeline admits a further execution while FEWER than
//! `MAX_CONCURRENT_EXECUTIONS_PER_PIPELINE` of its executions are in a non-terminal
//! status — each admitted start reporting a `pipelineExecutionId` of its own that
//! `ListPipelineExecutions` reports — and the start that would exceed that count is
//! refused `ConcurrentPipelineExecutionsLimitExceededException` (HTTP 400) having
//! recorded nothing; the count is of the executions still in flight and NOT a
//! high-water mark, so every execution that reaches a terminal status gives its slot
//! back and a later start is admitted into it.
//!
//! What the rows span is what the rule generalises over: the pipeline's
//! `executionMode` — the two modes whose executions accumulate (`PARALLEL`, where every
//! execution runs on its own, and `QUEUED`, where they enter a stage in turn) reach the
//! ceiling, while `SUPERSEDED` retires the execution that is waiting and so never does
//! — and whether a slot is given back by a stop or held by work nobody has reported.
//!
//! The suites beside this one pin the WORDING of that refusal
//! (`concurrent_execution_limit_wording`) and that a dispatched invoke action holds a
//! slot (`lambda_invoke_job_results`); what this one pins is the ACCOUNTING: the count,
//! the reconciliation of the refused start against the execution history, and the
//! recovery as executions retire.

use serde_json::{json, Value};

use service_com_amazonaws_codepipeline_codepipeline_20150709::state::MAX_CONCURRENT_EXECUTIONS_PER_PIPELINE;

use crate::execution_engine::Emulator;
use crate::pipeline_lifecycle::is_execution_id;
use crate::probe::{pipeline, NON_TERMINAL};

/// The sentence `StartPipelineExecution` refuses a start past the ceiling with. Fixed
/// text with no variable part, written out here so a reword fails this suite.
const AT_THE_LIMIT: &str =
    "The limit for concurrent pipeline executions has been exceeded. Please try again later.";

/// How many pipeline executions one page of this suite's listings carries.
///
/// Well under the number of executions a pipeline at its ceiling holds, so every
/// reconciliation below goes through the paginator rather than through one page.
/// `MaxResults` is modelled `@range(min: 1, max: 100)`
/// (`smithy_query(query="shape", shape="MaxResults")`).
const PER_PAGE: usize = 20;

/// How many of the pipeline's slots the retirement rows give back before starting again.
///
/// More than one, so that what is admitted afterwards is a COUNT rather than a single
/// slot, and fewer than the ceiling, so the pipeline is still holding executions while
/// the next starts are admitted.
const RETIRED: usize = 3;

/// The pipeline the rows drive: its source action is a `Custom` one, whose work is a job
/// this account's own worker reports the outcome of, so an admitted execution stays
/// non-terminal until something retires it and the clock changes no count here.
///
/// The declaration is the shared one from the `probe` suite with this row's
/// `executionMode` in place of its own.
fn declaration(name: &str, execution_mode: &str) -> Value {
    let mut declaration = pipeline(name, "Custom", "MySource");
    declaration["executionMode"] = json!(execution_mode);
    json!({"pipeline": declaration})
}

/// One `StartPipelineExecution` call.
async fn start(emulator: &Emulator, name: &str) -> (u16, Value) {
    emulator
        .cp("StartPipelineExecution", &json!({"name": name}))
        .await
}

/// Every summary `ListPipelineExecutions` reports for the pipeline, across all of its
/// pages — the independent read of the pipeline's execution history.
async fn summaries(emulator: &Emulator, name: &str) -> Vec<Value> {
    let mut summaries: Vec<Value> = Vec::new();
    let mut next: Option<String> = None;
    loop {
        let mut request = json!({"pipelineName": name, "maxResults": PER_PAGE});
        if let Some(token) = &next {
            request["nextToken"] = json!(token);
        }
        let page = emulator.ok("ListPipelineExecutions", &request).await;
        summaries.extend(
            page["pipelineExecutionSummaries"]
                .as_array()
                .cloned()
                .unwrap_or_default(),
        );
        match page["nextToken"].as_str() {
            Some(token) => next = Some(token.to_string()),
            None => return summaries,
        }
    }
}

/// How many of the pipeline's executions are still in flight, read the way a client
/// reads it: the summaries whose status is one an execution can still leave.
async fn active(emulator: &Emulator, name: &str) -> usize {
    summaries(emulator, name)
        .await
        .iter()
        .filter(|summary| {
            summary["status"]
                .as_str()
                .is_some_and(|status| NON_TERMINAL.contains(&status))
        })
        .count()
}

/// Retire one execution by stopping it with its in-progress actions abandoned, which
/// settles it at a terminal status at once.
async fn retire(emulator: &Emulator, name: &str, execution_id: &str, case: &str) {
    let (status, answered) = emulator
        .cp(
            "StopPipelineExecution",
            &json!({
                "pipelineName": name,
                "pipelineExecutionId": execution_id,
                "abandon": true,
                "reason": "retiring the slot",
            }),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: an in-flight execution can be stopped, got {status} with {answered}"
    );
}

/// Fill every one of the pipeline's slots with starts, and report the ids they minted.
///
/// One slot is already taken by the run the pipeline's creation started, so the starts
/// that fit inside the bound are one fewer than the bound.
async fn fill(emulator: &Emulator, name: &str, case: &str) -> Vec<String> {
    let mut accepted: Vec<String> = Vec::new();
    for index in 0..MAX_CONCURRENT_EXECUTIONS_PER_PIPELINE - 1 {
        let (status, answered) = start(emulator, name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: start {index} is inside the bound of \
             {MAX_CONCURRENT_EXECUTIONS_PER_PIPELINE}, got {status} with {answered}"
        );
        let id = answered["pipelineExecutionId"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {case:?}: start {index} reports an execution id, got {answered}")
            })
            .to_string();
        assert!(
            is_execution_id(&id),
            "case {case:?}: start {index} reports a modelled execution id, got {id:?}"
        );
        assert!(
            !accepted.contains(&id),
            "case {case:?}: start {index} reports an id of its own, got {id} again"
        );
        accepted.push(id);
    }
    accepted
}

/// The modes whose executions ACCUMULATE, so the ceiling is reachable in them: the two
/// `ExecutionMode` values a V2 pipeline runs more than one execution of
/// (`smithy_query(query="shape", shape="ExecutionMode")` -> QUEUED, SUPERSEDED,
/// PARALLEL; `SUPERSEDED` retires the execution that is waiting, which the row below
/// covers).
const ACCUMULATING_MODES: [&str; 2] = ["PARALLEL", "QUEUED"];

#[tokio::test]
async fn a_start_past_the_active_execution_ceiling_is_refused_and_records_nothing() {
    for mode in ACCUMULATING_MODES {
        let case = format!("executionMode {mode}, filled by sequential starts");
        let emulator = Emulator::new();
        let name = "ceiling";
        emulator
            .ok("CreatePipeline", &declaration(name, mode))
            .await;

        let accepted = fill(&emulator, name, &case).await;
        let in_flight = active(&emulator, name).await;
        assert_eq!(
            in_flight, MAX_CONCURRENT_EXECUTIONS_PER_PIPELINE,
            "case {case:?}: every admitted execution waits for its job worker, so every slot \
             is taken, got {in_flight}"
        );

        // The start past the ceiling: the modelled refusal, in the sentence that call
        // states the condition with.
        let (status, refused) = start(&emulator, name).await;
        assert_eq!(
            (
                status,
                refused["__type"].as_str(),
                refused["message"].as_str()
            ),
            (
                400,
                Some("ConcurrentPipelineExecutionsLimitExceededException"),
                Some(AT_THE_LIMIT)
            ),
            "case {case:?}: the start past the bound is refused, got {status} with {refused}"
        );

        // And it created nothing: the history reports every accepted id and nothing but
        // them beside the run the pipeline's creation started.
        let listed = summaries(&emulator, name).await;
        let ids: Vec<&str> = listed
            .iter()
            .filter_map(|summary| summary["pipelineExecutionId"].as_str())
            .collect();
        assert_eq!(
            ids.len(),
            MAX_CONCURRENT_EXECUTIONS_PER_PIPELINE,
            "case {case:?}: a refused start records no execution, got {ids:?}"
        );
        for id in &accepted {
            assert!(
                ids.contains(&id.as_str()),
                "case {case:?}: every accepted execution is reported, {id} is not among {ids:?}"
            );
        }
        assert_eq!(
            ids.len() - accepted.len(),
            1,
            "case {case:?}: exactly one execution beyond the accepted ones — the run the \
             pipeline's creation started — is reported, got {ids:?}"
        );
    }
}

#[tokio::test]
async fn retired_executions_give_their_slots_back_so_later_starts_are_admitted() {
    for mode in ACCUMULATING_MODES {
        let case = format!("executionMode {mode}, slots given back by a stop");
        let emulator = Emulator::new();
        let name = "recovering";
        emulator
            .ok("CreatePipeline", &declaration(name, mode))
            .await;
        let accepted = fill(&emulator, name, &case).await;

        // Retire some of them: each stop settles its execution at a terminal status, so
        // the count of in-flight executions FALLS by exactly what was retired.
        for id in accepted.iter().take(RETIRED) {
            retire(&emulator, name, id, &case).await;
        }
        let in_flight = active(&emulator, name).await;
        assert_eq!(
            in_flight,
            MAX_CONCURRENT_EXECUTIONS_PER_PIPELINE - RETIRED,
            "case {case:?}: {RETIRED} retired executions give {RETIRED} slots back, got \
             {in_flight} in flight"
        );

        // So exactly that many further starts are admitted, and no more.
        let mut readmitted: Vec<String> = Vec::new();
        for index in 0..RETIRED {
            let (status, answered) = start(&emulator, name).await;
            assert_eq!(
                status, 200,
                "case {case:?}: start {index} into a freed slot is admitted, got {status} with \
                 {answered}"
            );
            let id = answered["pipelineExecutionId"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: start {index} reports an execution id, got {answered}")
                })
                .to_string();
            assert!(
                !accepted.contains(&id) && !readmitted.contains(&id),
                "case {case:?}: start {index} reports an id of its own, got {id} again"
            );
            readmitted.push(id);
        }
        let in_flight = active(&emulator, name).await;
        assert_eq!(
            in_flight, MAX_CONCURRENT_EXECUTIONS_PER_PIPELINE,
            "case {case:?}: the readmitted starts fill the freed slots and no more, got \
             {in_flight} in flight"
        );
        let (status, refused) = start(&emulator, name).await;
        assert_eq!(
            (status, refused["__type"].as_str()),
            (
                400,
                Some("ConcurrentPipelineExecutionsLimitExceededException")
            ),
            "case {case:?}: the pipeline is at its ceiling again, got {status} with {refused}"
        );

        // The retired executions are still reported — they left the slot, not the
        // history — and the readmitted ones are reported beside them.
        let listed = summaries(&emulator, name).await;
        let ids: Vec<&str> = listed
            .iter()
            .filter_map(|summary| summary["pipelineExecutionId"].as_str())
            .collect();
        for id in accepted.iter().chain(readmitted.iter()) {
            assert!(
                ids.contains(&id.as_str()),
                "case {case:?}: {id} is reported in the pipeline's history, got {} entries",
                ids.len()
            );
        }
        assert_eq!(
            ids.len(),
            MAX_CONCURRENT_EXECUTIONS_PER_PIPELINE + RETIRED,
            "case {case:?}: the history holds every execution the pipeline ran, got {ids:?}"
        );
    }
}

#[tokio::test]
async fn the_ceiling_counts_only_the_executions_still_in_flight() {
    // `SUPERSEDED` — the default mode — retires the execution that is waiting to enter a
    // stage as soon as a later one reaches the same boundary, so a pipeline in that mode
    // holds the execution that is RUNNING plus at most the one waiting behind it, however
    // many starts it is sent, and never reaches the ceiling. That is the same accounting
    // read from the other side: what decides a start is the count of non-terminal
    // executions, not how many have ever run.
    let case = "executionMode SUPERSEDED, more starts than the bound";
    // The execution occupying the stage, and at most one waiting to enter it behind it.
    let running_and_one_waiting = 2;
    let emulator = Emulator::new();
    let name = "superseding";
    emulator
        .ok("CreatePipeline", &declaration(name, "SUPERSEDED"))
        .await;

    let past_the_bound = MAX_CONCURRENT_EXECUTIONS_PER_PIPELINE + RETIRED;
    for index in 0..past_the_bound {
        let (status, answered) = start(&emulator, name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: start {index} is admitted because the mode retires the execution \
             that was waiting, got {status} with {answered}"
        );
        let in_flight = active(&emulator, name).await;
        assert!(
            in_flight <= running_and_one_waiting,
            "case {case:?}: after start {index} the pipeline holds the running execution and \
             at most the one waiting behind it — never accumulating towards the ceiling of \
             {MAX_CONCURRENT_EXECUTIONS_PER_PIPELINE} — got {in_flight} in flight"
        );
    }
    let listed = summaries(&emulator, name).await;
    assert_eq!(
        listed.len(),
        past_the_bound + 1,
        "case {case:?}: every start and the creation run are reported, got {} entries",
        listed.len()
    );
}
