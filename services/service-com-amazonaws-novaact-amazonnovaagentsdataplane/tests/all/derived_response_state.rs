// SPDX-License-Identifier: CC0-1.0

//! Response members this service DERIVES rather than publishes: the status an act is
//! created in, and the status plus end instant a workflow run reports once it has ended.
//!
//! The rule these cases assert is not "this one request answers these bytes". It is that a
//! response member describing a resource's execution state is read off that resource's
//! stored record, and the record's value comes from what the service knows about the
//! resource at the time:
//!
//! * an act is driven by its CLIENT — the only operation that takes an act's next step is
//!   the caller's, and it carries the results of tool calls the caller executed — so an act
//!   that has only just been created has had no step taken on it and the next move is the
//!   caller's, which is the status that says so, whatever work the act declares;
//! * a workflow run reports `RUNNING` and no `endedAt` while it is under way, and once it
//!   has ended it reports the status it ended in together with the instant it ended at —
//!   `endedAt` is optional precisely because it belongs to a run that has ended
//!   (`smithy_query(query="operation",operation="GetWorkflowRun")`).
//!
//! So the tables below vary what those rules generalise over: for an act, whether
//! `toolSpecs` is absent, present but empty, or carries one or several specs, and the task
//! it declares; for a run, which of the statuses that finish a run it ends in.

use aws_smithy_types::date_time::Format;
use aws_smithy_types::DateTime;
use serde_json::{json, Value};

use crate::act_status_update::{
    chain, move_to, summary, Chain, FINISHED_STATUSES, INITIAL_ACT_STATUS,
};
use crate::workflow_lifecycle::{text, Emulator};
use crate::workflow_run_status_update::{move_to as move_run_to, read_run};

/// The definition each case builds its chain under.
const DEFINITION: &str = "derived-response-state";

/// The statuses that FINISH a workflow run, i.e. the ones that owe an `endedAt`
/// (`smithy_query(query="shape",shape="WorkflowRunStatus")` lists `RUNNING`, `SUCCEEDED`,
/// `FAILED`, `TIMED_OUT` and `DELETING`; `RUNNING` is a run still under way and `DELETING`
/// one on its way out).
const RUN_FINISHING_STATUSES: [&str; 3] = ["SUCCEEDED", "FAILED", "TIMED_OUT"];

/// One `ToolSpec` as the model shapes it: a name, a description and an input schema, all
/// three required (`smithy_query(query="shape",shape="ToolSpec")`).
fn tool_spec(name: &str) -> Value {
    json!({
        "name": name,
        "description": format!("what {name} does"),
        "inputSchema": { "json": { "type": "object", "properties": { "q": { "type": "string" } } } },
    })
}

/// RULE: a created act is stored in the status that hands the next move to its caller, and
/// every read of that act reports the stored status back.
#[tokio::test]
async fn a_created_act_is_stored_waiting_on_its_client_whatever_work_it_declares() {
    let emulator = Emulator::new("us-west-2");
    // The definition -> run -> session chain comes from the sibling act suite's harness, so
    // this suite drives the same setup rather than a second copy of it.
    let chain = chain(&emulator, DEFINITION).await;

    // Rows differ along what the rule generalises over: whether `toolSpecs` is sent at all,
    // whether it is sent empty, how many specs it carries, and the task the act declares.
    let rows: [(&str, &str, Option<Vec<Value>>); 5] = [
        ("no toolSpecs member at all", "search for a product", None),
        ("an empty toolSpecs list", "summarise the page", Some(vec![])),
        (
            "one tool spec",
            "click the first result",
            Some(vec![tool_spec("search")]),
        ),
        (
            "three tool specs",
            "search, click and report",
            Some(vec![
                tool_spec("search"),
                tool_spec("click"),
                tool_spec("report"),
            ]),
        ),
        (
            "the shortest legal task, with a tool spec",
            "x",
            Some(vec![tool_spec("a_tool-1")]),
        ),
    ];

    let mut created: Vec<(String, String)> = Vec::new();
    for (case, task, tool_specs) in rows {
        let mut request = json!({ "task": task });
        if let Some(tool_specs) = tool_specs {
            request["toolSpecs"] = Value::Array(tool_specs);
        }
        let (status, body) = create_act(&emulator, &chain, request).await;
        assert_eq!(
            status, 201,
            "case {case:?}: expected HTTP 201, got {status} {body}"
        );
        let reported = text(&body, "status");
        assert_eq!(
            reported, INITIAL_ACT_STATUS,
            "case {case:?}: expected the created act to report {INITIAL_ACT_STATUS}, got \
             {reported:?}"
        );
        created.push((case.to_string(), text(&body, "actId")));
    }

    // READ BACK: the listing reports the status that was STORED for each act, so the create
    // answered state and not a value built for that one response. The read goes through the
    // sibling harness's `ListActs` reader, the implemented reader of an act.
    for (case, act_id) in &created {
        let summary = summary(&emulator, &chain, act_id).await;
        let reported = text(&summary, "status");
        assert_eq!(
            reported, INITIAL_ACT_STATUS,
            "case {case:?}: expected the stored act to report {INITIAL_ACT_STATUS}, got \
             {summary}"
        );
        assert!(
            summary.get("endedAt").is_none(),
            "case {case:?}: an act that has not finished reports no endedAt, got {summary}"
        );
    }

    // The status an act is created in is one the act can still be moved OUT of, so the
    // statuses that finish an act are all reachable from it.
    for (index, (case, act_id)) in created.iter().enumerate() {
        let finishing = FINISHED_STATUSES[index % FINISHED_STATUSES.len()];
        let (status, answer) = move_to(&emulator, &chain, act_id, finishing).await;
        assert_eq!(
            status, 200,
            "case {case:?}: an act waiting on its client takes {finishing}, got {status} \
             {answer}"
        );
        let summary = summary(&emulator, &chain, act_id).await;
        let reported = text(&summary, "status");
        assert_eq!(
            reported, finishing,
            "case {case:?}: expected the finished act to report {finishing}, got {summary}"
        );
    }
}

/// One `CreateAct` call carrying `request`, which this suite shapes itself: the sibling
/// harness's helper sends a task and no tool specs, and whether `toolSpecs` is sent at all is
/// exactly what the rows below vary.
async fn create_act(emulator: &Emulator, chain: &Chain, request: Value) -> (u16, Value) {
    emulator
        .call(
            "PUT",
            &format!(
                "/workflow-definitions/{}/workflow-runs/{}/sessions/{}/acts",
                chain.definition, chain.run_id, chain.session_id
            ),
            Some(request),
        )
        .await
}

/// One `DateTimestamp` member as a generated client reads it: an RFC 3339 string
/// (`DateTimestamp` carries `@timestampFormat("date-time")`).
fn instant(body: &Value, member: &str, case: &str) -> f64 {
    let rendered = text(body, member);
    DateTime::from_str(&rendered, Format::DateTime)
        .unwrap_or_else(|error| {
            panic!("case {case:?}: {member} must be an RFC 3339 date-time, got {rendered:?}: {error}")
        })
        .as_secs_f64()
}

/// RULE: a workflow run that has ENDED reports the status it ended in and the instant it
/// ended at; one still under way reports neither.
#[tokio::test]
async fn a_run_that_has_ended_reports_a_terminal_status_and_the_instant_it_ended() {
    let emulator = Emulator::new("eu-west-1");
    let (status, body) = emulator.create_definition(DEFINITION).await;
    assert_eq!(
        status, 201,
        "the definition must be created, got {status} {body}"
    );

    // One row per status that finishes a run: the end instant belongs to the transition,
    // not to the status it moved to.
    for case in RUN_FINISHING_STATUSES {
        let (status, body) = emulator.create_run(DEFINITION).await;
        assert_eq!(
            status, 201,
            "case {case:?}: the run must be created, got {status} {body}"
        );
        let run_id = text(&body, "workflowRunId");

        // While the run is under way it has no end instant to report. The run reader and the
        // status write are the sibling run suite's, so this suite drives one harness.
        let under_way = read_run(&emulator, DEFINITION, &run_id).await;
        assert_eq!(
            under_way["status"], "RUNNING",
            "case {case:?}: a run still under way reports RUNNING, got {under_way}"
        );
        assert!(
            under_way.get("endedAt").is_none(),
            "case {case:?}: a run still under way reports no endedAt, got {under_way}"
        );
        let started_at = instant(&under_way, "startedAt", case);

        let (status, answer) = move_run_to(&emulator, DEFINITION, &run_id, case).await;
        assert_eq!(
            status, 200,
            "case {case:?}: ending the run must answer HTTP 200, got {status} {answer}"
        );

        // READ BACK: the terminal status and the end instant both come off the stored run.
        let ended = read_run(&emulator, DEFINITION, &run_id).await;
        assert_eq!(
            ended["status"], case,
            "case {case:?}: expected the ended run to report {case}, got {ended}"
        );
        let ended_at = instant(&ended, "endedAt", case);
        assert!(
            ended_at >= started_at,
            "case {case:?}: expected endedAt at or after startedAt {started_at}, got \
             {ended_at}"
        );
        assert_eq!(
            ended["startedAt"], under_way["startedAt"],
            "case {case:?}: the start instant must not move, got {ended}"
        );

        // A read does not change what the next reader sees: the ended run reports the same
        // record again.
        let again = read_run(&emulator, DEFINITION, &run_id).await;
        assert_eq!(
            again, ended,
            "case {case:?}: expected the repeated read to report the same record, got {again}"
        );
    }
}
