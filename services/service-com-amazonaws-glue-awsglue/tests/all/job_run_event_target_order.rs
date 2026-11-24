// SPDX-License-Identifier: CC0-1.0

//! The `Glue Job State Change` event of a job run that FAILED reaches each SQS target of its
//! EventBridge rule by that target's OWN queue policy, whatever ORDER the targets were
//! installed in and however many of them there are.
//!
//! The rule under test: EventBridge invokes an Amazon SQS target as its own service principal
//! and "relies on resource-based policies" (`PutTargets`), so each target of a matched rule is
//! decided by the policy of ITS OWN queue — independently of every other target of the same
//! rule, and so independently of the position it occupies in that rule's target list. A queue
//! whose policy grants `events.amazonaws.com` `sqs:SendMessage` for the invoking rule receives
//! the envelope; a sibling queue that has granted that principal nothing receives none of it,
//! and withholding that one delivery withholds nothing from the targets beside it.
//!
//! Why this suite exists beside [`crate::job_run_event_target_permission`], which pins the
//! permission boundary itself, and [`crate::job_run_state_change_events`], which pins the
//! publication: those two install the PERMITTED target first, so neither can tell a per-target
//! decision from one that stops the rule's fan-out at the first target it withholds. The
//! recorded case installs the unpermitted target FIRST (`PutTargets(Ids=['denied','allowed'])`),
//! which is one row here; the rows differ along what the rule generalises over — the position
//! of the withheld target among its siblings and how many targets the rule carries.
//!
//! Every run here is a `pythonshell` run whose script exits non-zero, so the state it comes to
//! rest in is the modelled failure state and the rule each row installs is narrowed to that
//! job and that state, exactly as a client that wires post-failure work to a job run does. The
//! role the runs are performed as is created through IAM's own API, carrying both the managed
//! policy AWS publishes for a Glue job's role and the inline grants that let the run read its
//! script, so each run is admitted against records `CreateRole`, `AttachRolePolicy` and
//! `PutRolePolicy` wrote.
//!
//! Nothing here waits on the clock: the event reaching the PERMITTED queue of the same rule is
//! what bounds every negative assertion — the deliveries of one published event are decided in
//! one pass, so a queue still empty once its sibling holds that event is a queue the delivery
//! was withheld from — and the one wait there is polls for that arrival against a generous
//! deadline. No assertion reads elapsed time.

use std::time::Instant;

use serde_json::{json, Value};
use service_com_amazonaws_glue_awsglue::state::JOB_RUN_STATE_FAILED;
use smithy_server_runtime::MultiServiceRouter;

use crate::job_run_event_target_permission::{
    delivered_envelope, drain, emulator_with_a_bus_and_iam, events, given_role,
    grant_delivery_from, iam, DELIVERY_DEADLINE, DELIVERY_POLL, EVENT_SOURCE, EXITS_NON_ZERO,
    JOB_STATE_CHANGE,
};
use crate::job_run_state_change_events::given_queue;
use crate::job_script_execution::{create_bucket, stage_script, SCRIPTS};
use crate::wire::ok;

/// The policy AWS publishes for a Glue job's role, which the recorded case attaches to the
/// role beside its inline grants.
const GLUE_SERVICE_ROLE_POLICY: &str = "arn:aws:iam::aws:policy/service-role/AWSGlueServiceRole";

/// What one target of the row's rule is: a queue whose policy admits the rule's deliveries, or
/// a sibling that has granted EventBridge nothing at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    /// Its queue policy grants `events.amazonaws.com` `sqs:SendMessage` for the rule.
    Permitted,
    /// Its queue carries no resource policy, so the delivery to it is dropped.
    Withheld,
}

/// One row: the rule's target list, in the order `PutTargets` installs it.
struct Row {
    /// The case name every assertion of the row reports.
    case: &'static str,
    /// The targets of the row's rule, in order.
    targets: &'static [Target],
}

/// The rows: where the withheld target sits among its siblings, and how many targets the rule
/// carries. The recorded case is the first of them.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the withheld target installed first",
            targets: &[Target::Withheld, Target::Permitted],
        },
        Row {
            case: "the withheld target installed last",
            targets: &[Target::Permitted, Target::Withheld],
        },
        Row {
            case: "a withheld target on either side of the permitted one",
            targets: &[Target::Withheld, Target::Permitted, Target::Withheld],
        },
        Row {
            case: "two permitted targets behind a withheld one",
            targets: &[Target::Withheld, Target::Permitted, Target::Permitted],
        },
    ]
}

/// Create, through IAM's own API, the role every run here is performed as: the trust and inline
/// grants its sibling suite stages, plus the managed policy AWS publishes for a Glue job's role.
/// Answers its ARN.
async fn given_role_with_the_published_policy(router: &MultiServiceRouter) -> String {
    let role = given_role(router).await;
    let name = role
        .rsplit('/')
        .next()
        .unwrap_or_else(|| panic!("the role ARN must name the role, got {role}"))
        .to_string();
    iam(
        router,
        "AttachRolePolicy",
        &format!("RoleName={name}&PolicyArn={GLUE_SERVICE_ROLE_POLICY}"),
    )
    .await;
    role
}

/// Put an ENABLED rule on the default bus matching the terminal state change of `job_name`, and
/// answer the rule's own ARN — the ARN a target's queue policy names as the source of the
/// delivery.
async fn given_failed_state_rule(
    router: &MultiServiceRouter,
    name: &str,
    job_name: &str,
) -> String {
    let pattern = json!({
        "source": [EVENT_SOURCE],
        "detail-type": [JOB_STATE_CHANGE],
        "detail": { "jobName": [job_name], "state": [JOB_RUN_STATE_FAILED] },
    });
    let put = events(
        router,
        "PutRule",
        json!({ "Name": name, "EventPattern": pattern.to_string(), "State": "ENABLED" }),
    )
    .await;
    put["RuleArn"]
        .as_str()
        .unwrap_or_else(|| panic!("PutRule must answer a RuleArn, got {put}"))
        .to_string()
}

/// Point the rule `name` at `targets`, each under the id it is listed with, and assert the
/// service accepted every one of them — a target whose queue admits nothing is accepted like
/// any other, and no error is surfaced for it.
async fn given_targets(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    targets: &[(String, String)],
) {
    let entries: Vec<Value> = targets
        .iter()
        .map(|(id, arn)| json!({ "Id": id, "Arn": arn }))
        .collect();
    let pointed = events(
        router,
        "PutTargets",
        json!({ "Rule": name, "Targets": entries }),
    )
    .await;
    assert_eq!(
        pointed["FailedEntryCount"],
        json!(0),
        "case {case:?}: PutTargets must accept every target, got {pointed}"
    );
    assert!(
        pointed["FailedEntries"]
            .as_array()
            .is_none_or(Vec::is_empty),
        "case {case:?}: PutTargets must report no failed entry, got {pointed}"
    );
}

/// The state the run `run_id` of `job_name` has come to rest in, polled until it has come to
/// rest at all against a generous deadline.
async fn rested_state(
    router: &MultiServiceRouter,
    case: &str,
    job_name: &str,
    run_id: &str,
) -> String {
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        let read = ok(
            router,
            "GetJobRun",
            &json!({ "JobName": job_name, "RunId": run_id }),
        )
        .await;
        let state = read["JobRun"]["JobRunState"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if ["SUCCEEDED", "FAILED", "TIMEOUT", "ERROR", "STOPPED"].contains(&state.as_str()) {
            return state;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the run did not come to rest before the deadline, last state \
             {state:?}"
        );
        tokio::time::sleep(DELIVERY_POLL).await;
    }
}

/// A failing run's state change reaches every target whose own queue policy admits the rule,
/// and no target that has granted EventBridge nothing — whichever position each of them
/// occupies in the rule's target list.
#[tokio::test]
async fn a_withheld_target_withholds_nothing_from_the_targets_beside_it() {
    let router = emulator_with_a_bus_and_iam();
    create_bucket(&router).await;
    let role = given_role_with_the_published_policy(&router).await;

    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let job_name = format!("job_event_order_{index}");
        let location =
            stage_script(&router, &format!("{SCRIPTS}{job_name}.py"), EXITS_NON_ZERO).await;
        let rule = format!("glue-job-failed-order-{index}");
        let rule_arn = given_failed_state_rule(&router, &rule, &job_name).await;

        // One queue per target of the row, created before any of them is pointed at: the
        // permitted ones are granted the rule's deliveries, the withheld ones are left with no
        // resource policy at all, exactly as the recorded case leaves its sibling queue.
        let mut installed: Vec<(String, String)> = Vec::with_capacity(row.targets.len());
        let mut permitted: Vec<String> = Vec::new();
        let mut withheld: Vec<String> = Vec::new();
        for (position, target) in row.targets.iter().enumerate() {
            let (url, arn) =
                given_queue(&router, &format!("glue-job-order-{index}-{position}")).await;
            let id = match target {
                Target::Permitted => format!("allowed-{position}"),
                Target::Withheld => format!("denied-{position}"),
            };
            match target {
                Target::Permitted => {
                    grant_delivery_from(&router, &url, &arn, &rule_arn).await;
                    permitted.push(url);
                }
                Target::Withheld => withheld.push(url),
            }
            installed.push((id, arn));
        }
        given_targets(&router, case, &rule, &installed).await;

        ok(
            &router,
            "CreateJob",
            &json!({
                "Name": job_name,
                "Role": role,
                "Command": {
                    "Name": "pythonshell",
                    "PythonVersion": "3.9",
                    "ScriptLocation": location,
                },
                "MaxCapacity": 0.0625,
                "Timeout": 5,
            }),
        )
        .await;
        let started = ok(&router, "StartJobRun", &json!({ "JobName": job_name })).await;
        let run_id = started["JobRunId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: StartJobRun must answer a JobRunId"))
            .to_string();

        // The run comes to rest in the failure state, which is the state the rule narrows to.
        let rested = rested_state(&router, case, &job_name, &run_id).await;
        assert_eq!(
            rested, JOB_RUN_STATE_FAILED,
            "case {case:?}: expected the run of a script that exits non-zero to come to rest in \
             {JOB_RUN_STATE_FAILED}, got {rested:?}"
        );

        // Every permitted target receives that run's own state change. The arrival of the last
        // of them is what bounds the negative half below: the deliveries of one published event
        // are decided in one pass, so a queue still empty by then was withheld from.
        for (position, url) in permitted.iter().enumerate() {
            let envelope = delivered_envelope(&router, case, url).await;
            assert_eq!(
                envelope["source"],
                json!(EVENT_SOURCE),
                "case {case:?}: permitted target {position} expected source {EVENT_SOURCE:?}, got \
                 {envelope}"
            );
            assert_eq!(
                envelope["detail-type"],
                json!(JOB_STATE_CHANGE),
                "case {case:?}: permitted target {position} expected detail-type \
                 {JOB_STATE_CHANGE:?}, got {envelope}"
            );
            let detail = envelope["detail"].clone();
            assert_eq!(
                detail["jobName"],
                json!(job_name),
                "case {case:?}: permitted target {position} must be told the job, got {detail}"
            );
            assert_eq!(
                detail["jobRunId"],
                json!(run_id),
                "case {case:?}: permitted target {position} must be told the run, got {detail}"
            );
            assert_eq!(
                detail["state"],
                json!(JOB_RUN_STATE_FAILED),
                "case {case:?}: permitted target {position} expected state \
                 {JOB_RUN_STATE_FAILED:?}, got {detail}"
            );
        }

        for (position, url) in withheld.iter().enumerate() {
            let received = drain(&router, url).await;
            assert!(
                received.is_empty(),
                "case {case:?}: withheld target {position} has granted EventBridge nothing, so it \
                 receives nothing, got {received:?}"
            );
        }
    }
}
