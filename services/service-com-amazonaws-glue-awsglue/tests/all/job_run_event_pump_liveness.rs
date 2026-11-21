// SPDX-License-Identifier: CC0-1.0

//! The `Glue Job State Change` event a run that comes to rest owes is published even when the
//! store's single clock pump has to be TAKEN OVER.
//!
//! The rule under test: **the publication a terminal job run owes is made by whichever pump
//! runs next, so the claim that keeps one pump per store from becoming two is held only for as
//! long as a pump that EXISTS is holding it — a claim left behind by a pump that has ended is
//! taken over by the next call that leaves the store owing a publication.** Which is the one
//! invariant the claim needs: the publication is the pump's alone (a client reacting to a run
//! through EventBridge makes no further Glue call), so a claim nobody is holding would park
//! every later publication for the life of the process, and a run would come to rest —
//! reported `SUCCEEDED` by `GetJobRun`, because a reading door materialises what the clock owed
//! — while the event its rule's target is waiting for never left.
//!
//! The rows differ along WHAT the store's claim looks like when the run is started: no claim at
//! all (the ordinary path), a claim whose pump left no token behind, and a claim whose pump's
//! token has been dropped. The last two are the states a pump that ended without releasing its
//! claim leaves — it panicked, or its task was dropped before it finished — and each must
//! leave the run's publication observable all the same.
//!
//! What is NOT asserted here, because it is asserted beside it:
//! [`crate::job_run_state_change_events`] pins what the published envelope carries over every
//! terminal state a run of this emulator reaches. This suite asserts only that the envelope
//! ARRIVES, under each state of the claim.
//!
//! EventBridge, SQS and Amazon S3 are mounted BESIDE Glue on the SAME engine, so the
//! publication travels the real in-process callback, through the real bus, to a real SQS
//! target. Each wait is for the CONDITION it is about — the run at rest, the event on the queue
//! — against a generous deadline, and nothing here sleeps for a span or reads the wall clock.

use std::sync::{Arc, Weak};

use serde_json::{json, Value};
use service_com_amazonaws_glue_awsglue::state::{
    ClockPumpLife, ComAmazonawsGlueAwsglueState, JOB_RUN_STATE_SUCCEEDED,
};

use crate::job_run_state_change_events::{
    delivered_event, emulator_with_a_bus, given_queue, given_rule, grant_delivery,
};
use crate::job_runs::run_at_rest;
use crate::job_script_execution::{create_bucket, stage_script, SCRIPTS};
use crate::wire::ok;

/// The `source` and `detail-type` Glue publishes a job run's terminal state under, spelled out
/// here rather than imported, so a wrong constant in the implementation cannot make this suite
/// pass.
const EVENT_SOURCE: &str = "aws.glue";
const JOB_STATE_CHANGE: &str = "Glue Job State Change";

/// A script that exits cleanly, so every row's run comes to rest in the same terminal state and
/// the rows differ only in the state of the claim.
const EXITS_CLEANLY: &str = r#"
total = sum(index for index in range(4))
assert total == 6, total
"#;

/// The role a row's job names. No IAM is mounted here, so nothing can be shown about it and the
/// run is performed; which role a run is performed as is pinned by the suites that mount IAM.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/glue-job-event-pump";

/// What state the store's pump claim is in when the row's run is started.
#[derive(Debug, Clone, Copy)]
enum Claim {
    /// None has been taken: the ordinary path, where the start claims the pump itself.
    Free,
    /// Taken, with no token kept beside it — the state a pump that ended without releasing its
    /// claim leaves when nothing of it is left to look at.
    HeldWithNoToken,
    /// Taken, with a token that has since been dropped — the state a pump whose task ended
    /// leaves.
    HeldByADroppedToken,
}

impl Claim {
    /// Put the store into this state, as the row's precondition.
    fn install(self, engine: &Arc<engine::InMemoryEngine>) {
        let state = engine.state::<ComAmazonawsGlueAwsglueState>();
        let mut store = state.lock();
        match self {
            Self::Free => {}
            Self::HeldWithNoToken => {
                store.clock_pump_running = true;
                store.clock_pump_life = None;
            }
            Self::HeldByADroppedToken => {
                let dropped: Weak<ClockPumpLife> = {
                    let life = Arc::new(ClockPumpLife);
                    Arc::downgrade(&life)
                };
                store.clock_pump_running = true;
                store.clock_pump_life = Some(dropped);
            }
        }
    }
}

/// One row: the claim the store carries when the run is started.
struct Row {
    /// The case name every assertion of the row reports.
    case: &'static str,
    claim: Claim,
}

/// The rows: every state of the claim a start can find the store in.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "no claim on the pump at all",
            claim: Claim::Free,
        },
        Row {
            case: "a claim whose pump left no token behind",
            claim: Claim::HeldWithNoToken,
        },
        Row {
            case: "a claim whose pump's token has been dropped",
            claim: Claim::HeldByADroppedToken,
        },
    ]
}

/// RULE: a run that comes to rest has its `Glue Job State Change` published onto the default
/// bus and delivered to its rule's granted target, whatever state the store's single pump claim
/// was in when the run started — a claim no pump is holding any more is taken over rather than
/// parking the publication for ever.
#[tokio::test]
async fn a_claim_no_pump_is_holding_does_not_park_the_publication() {
    let (router, engine) = emulator_with_a_bus();
    create_bucket(&router).await;
    let (queue_url, queue_arn) = given_queue(&router, "glue-job-event-pump").await;
    grant_delivery(&router, &queue_url, &queue_arn).await;
    given_rule(
        &router,
        "glue-job-event-pump",
        json!({ "source": [EVENT_SOURCE], "detail-type": [JOB_STATE_CHANGE] }),
        &[queue_arn],
    )
    .await;

    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let job_name = format!("pump_claim_{index}");
        let location = stage_script(&router, &format!("{SCRIPTS}{job_name}.py"), EXITS_CLEANLY)
            .await;
        ok(
            &router,
            "CreateJob",
            &json!({
                "Name": &job_name,
                "Role": ROLE_ARN,
                "Command": {
                    "Name": "pythonshell",
                    "PythonVersion": "3.9",
                    "ScriptLocation": location,
                },
            }),
        )
        .await;

        // The row's precondition goes in LAST, so the start below is the call that finds the
        // claim in that state.
        row.claim.install(&engine);

        let started = ok(&router, "StartJobRun", &json!({ "JobName": &job_name })).await;
        let run_id = started["JobRunId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: StartJobRun must answer a JobRunId"))
            .to_string();
        let run = run_at_rest(&router, &engine, case, &job_name, &run_id).await;
        assert_eq!(
            run["JobRunState"].as_str(),
            Some(JOB_RUN_STATE_SUCCEEDED),
            "case {case:?}: the run of a script that exits cleanly comes to rest in \
             {JOB_RUN_STATE_SUCCEEDED}, got {run}"
        );

        let event = delivered_event(&router, case, &queue_url).await;
        for (pointer, expected) in [
            ("/source", json!(EVENT_SOURCE)),
            ("/detail-type", json!(JOB_STATE_CHANGE)),
            ("/detail/jobName", json!(&job_name)),
            ("/detail/jobRunId", json!(&run_id)),
            ("/detail/state", json!(JOB_RUN_STATE_SUCCEEDED)),
        ] {
            let actual = event.pointer(pointer).cloned().unwrap_or(Value::Null);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {pointer} to be {expected}, got {actual} in {event}"
            );
        }
    }
}
