// SPDX-License-Identifier: CC0-1.0

//! The lifecycle a DRY RUN has of its own — end to end over the wire, with the services
//! that own the resources a run uses mounted beside Synthetics.
//!
//! The rule asserted here: a dry run is a run of its own, addressed by its `DryRunId`
//! (`GetCanaryRequest$DryRunId`: "You can use this DryRunId to retrieve information about
//! the dry run"), so a read carrying that id reports THAT dry run's `CanaryStatus` and
//! `CanaryTimeline` — an in-progress `CanaryState` while its run executes and a terminal one
//! once that run has a verdict — while the same read WITHOUT the id keeps reporting the
//! canary's own state and timeline, whatever the canary's schedule is doing meanwhile.
//!
//! The rows below vary along what that rule generalises over: the verdict the dry run's run
//! reaches (and so the terminal state the dry run settles to), the form of the grant the
//! execution role carries, and the state the canary itself is in while the dry run runs.
//! Every transition is waited OUT with the target's poll helper rather than slept for.

use serde_json::{json, Value};

use crate::canary_lifecycle::{poll_until, Answer, Synthetics};
use crate::canary_run_outcomes::{
    canary_body, create_bucket, create_role, ready_canary, statement, ARTIFACT_WRITE_ACTION,
    BUCKET, REGION,
};
use crate::group_membership::ok;

/// The `CanaryState` members that say work on the resource is still going.
///
/// The enum is `CREATING | READY | STARTING | RUNNING | UPDATING | STOPPING | STOPPED |
/// ERROR | DELETING` (`smithy_query(query="shape",shape="CanaryState")`); the three states
/// that are not listed here — `READY`, `STOPPED` and `ERROR` — are the settled ones, so a
/// poll for "has it finished" is a poll for a state outside this set.
const IN_PROGRESS_STATES: [&str; 6] = [
    "CREATING",
    "STARTING",
    "RUNNING",
    "UPDATING",
    "STOPPING",
    "DELETING",
];

/// The reason code the model documents for artifacts that could not be saved.
const EXECUTION_FAILURE: &str = "EXECUTION_FAILURE";

/// Whether `state` is one of the states work is still in flight in.
fn is_in_progress(state: &str) -> bool {
    IN_PROGRESS_STATES.contains(&state)
}

/// `POST /canary/{Name}/dry-run/start`.
async fn start_dry_run(aws: &Synthetics, name: &str) -> Answer {
    aws.call(
        "POST",
        &format!("/canary/{name}/dry-run/start"),
        Some(json!({})),
    )
    .await
}

/// `GET /canary/{Name}?dryRunId=...` — the read that addresses one dry run.
async fn get_for_dry_run(aws: &Synthetics, name: &str, dry_run_id: &str) -> Answer {
    aws.call("GET", &format!("/canary/{name}?dryRunId={dry_run_id}"), None)
        .await
}

/// Wait until the dry run `dry_run_id` reports a settled `CanaryState`, and answer that
/// read.
async fn settled_dry_run(aws: &Synthetics, case: &str, name: &str, dry_run_id: &str) -> Answer {
    poll_until(
        &format!("{case}/settled"),
        || get_for_dry_run(aws, name, dry_run_id),
        |answer| !is_in_progress(&answer.string_at("Canary.Status.State")),
    )
    .await
}

/// The dry runs of `name`, newest first, as `GetCanaryRuns(RunType=DRY_RUN)` reports them.
async fn dry_runs(aws: &Synthetics, case: &str, name: &str) -> Vec<Value> {
    let answer = ok(
        &format!("{case}/dry-runs"),
        aws.runs(name, json!({ "RunType": "DRY_RUN" })).await,
    );
    answer
        .at("CanaryRuns")
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// How many runs of the canary's OWN schedule `name` has.
async fn canary_run_count(aws: &Synthetics, case: &str, name: &str) -> usize {
    let answer = ok(
        &format!("{case}/canary-runs"),
        aws.runs(name, json!({ "RunType": "CANARY_RUN" })).await,
    );
    answer
        .at("CanaryRuns")
        .as_array()
        .map(Vec::len)
        .unwrap_or_default()
}

/// One case of the rule the four tests below share.
struct Row {
    case: &'static str,
    /// The canary's name, which is also the key prefix its artifacts go under.
    name: &'static str,
    /// The inline permissions policy the dry run's execution role carries.
    statements: Value,
    /// Whether the canary's own schedule is started before the dry run, so the dry run is
    /// asserted against a canary in a different state than `READY`.
    start_canary: bool,
    /// The state the canary itself reports throughout — a dry run changes none of it.
    canary_state: &'static str,
    /// The terminal `CanaryState` this dry run settles to.
    dry_run_state: &'static str,
    /// The `CanaryRunState` the run this dry run started settles to.
    run_state: &'static str,
}

/// RULE (one test per case below, so no case waits on another): a dry run carries its own
/// `CanaryStatus` and `CanaryTimeline` — in progress while its run executes, terminal once
/// that run has a verdict — and the canary's own status and timeline are untouched by it.
///
/// The cases differ along what the rule generalises over: the verdict the dry run's run
/// reaches (and so the terminal state it settles to), the form of the grant the execution
/// role carries, and the state the canary itself is in while the dry run runs.
#[tokio::test]
async fn a_dry_run_of_a_ready_canary_with_every_action_allowed_stops() {
    assert_dry_run_lifecycle(Row {
        case: "sound role, every action allowed, canary READY",
        name: "ready-allow-all",
        statements: json!([statement("Allow", json!("*"), json!("*"))]),
        start_canary: false,
        canary_state: "READY",
        dry_run_state: "STOPPED",
        run_state: "PASSED",
    })
    .await;
}

/// The same rule where the role grants only the artifact write, on the bucket's objects.
#[tokio::test]
async fn a_dry_run_whose_role_grants_only_the_artifact_write_stops() {
    assert_dry_run_lifecycle(Row {
        case: "sound role, the write allowed on the bucket's objects, canary READY",
        name: "ready-scoped-grant",
        statements: json!([statement(
            "Allow",
            json!(ARTIFACT_WRITE_ACTION),
            json!(format!("arn:aws:s3:::{BUCKET}/*")),
        )]),
        start_canary: false,
        canary_state: "READY",
        dry_run_state: "STOPPED",
        run_state: "PASSED",
    })
    .await;
}

/// The same rule where the dry run's run produces no verdict, because the role withholds the
/// write its artifacts need: the dry run ends in `ERROR`, carrying that run's reason.
#[tokio::test]
async fn a_dry_run_whose_role_withholds_the_write_ends_in_error() {
    assert_dry_run_lifecycle(Row {
        case: "role withholds the write, canary READY",
        name: "ready-no-grant",
        statements: json!([]),
        start_canary: false,
        canary_state: "READY",
        dry_run_state: "ERROR",
        run_state: "FAILED",
    })
    .await;
}

/// The same rule against a canary that is RUNNING its own schedule: the dry run settles while
/// the canary holds `RUNNING`, which is the independence `StartCanaryDryRun` exists for.
#[tokio::test]
async fn a_dry_run_of_a_running_canary_settles_while_the_canary_keeps_running() {
    assert_dry_run_lifecycle(Row {
        case: "sound role, canary RUNNING its own schedule",
        name: "running-allow-all",
        statements: json!([statement("Allow", json!("*"), json!("*"))]),
        start_canary: true,
        canary_state: "RUNNING",
        dry_run_state: "STOPPED",
        run_state: "PASSED",
    })
    .await;
}

/// Drive one case of the rule end to end, on its own emulator with its own account
/// resources, and assert every half of it.
async fn assert_dry_run_lifecycle(row: Row) {
    let (aws, iam, s3) = Synthetics::with_account_services(REGION);
    create_bucket(&s3).await;

    {
        let case = row.case;
        let role = create_role(&iam, &format!("{}-role", row.name), row.statements).await;
        let location = format!("s3://{BUCKET}/{}", row.name);
        ready_canary(
            &aws,
            case,
            canary_body(row.name, &role, &location, "rate(5 minutes)"),
        )
        .await;
        if row.start_canary {
            ok(&format!("{case}/start"), aws.start(row.name).await);
        }

        let before = ok(&format!("{case}/before"), aws.get(row.name).await);
        assert_eq!(
            before.string_at("Canary.Status.State"),
            row.canary_state,
            "case {case}: expected the canary to be {} before the dry run, got {:?}",
            row.canary_state,
            before.at("Canary.Status")
        );
        let canary_timeline = before.at("Canary.Timeline").clone();

        let started = ok(&format!("{case}/dry-run"), start_dry_run(&aws, row.name).await);
        let dry_run_id = started.string_at("DryRunConfig.DryRunId");

        // Read straight after the start: whatever the host's speed, this answer is the DRY
        // RUN's own lifecycle — its in-progress state while its run executes, or the terminal
        // one this row settles to — never the canary's, and its timeline agrees with it.
        let open = ok(
            &format!("{case}/open"),
            get_for_dry_run(&aws, row.name, &dry_run_id).await,
        );
        let open_state = open.string_at("Canary.Status.State");
        assert!(
            is_in_progress(&open_state) || open_state == row.dry_run_state,
            "case {case}: expected the dry run's own lifecycle — in progress, or the terminal \
             {} it settles to — got {:?}",
            row.dry_run_state,
            open.at("Canary.Status")
        );
        assert!(
            open.at("Canary.Timeline.Created").is_number()
                && open.at("Canary.Timeline.LastStarted").is_number(),
            "case {case}: expected the dry run's own start, got {:?}",
            open.at("Canary.Timeline")
        );
        assert_eq!(
            open.at("Canary.Timeline.LastStopped").is_null(),
            is_in_progress(&open_state),
            "case {case}: expected a stop instant exactly when the dry run has finished \
             ({open_state}), got {:?}",
            open.at("Canary.Timeline")
        );
        assert_eq!(
            open.at("Canary.DryRunConfig.DryRunId").as_str(),
            Some(dry_run_id.as_str()),
            "case {case}: expected the read to report the dry run asked for, got {:?}",
            open.at("Canary.DryRunConfig")
        );

        // The very same read WITHOUT the id is the canary's own: the two are different
        // answers, which is the whole point of addressing a dry run by its id.
        let during = ok(&format!("{case}/during"), aws.get(row.name).await);
        assert_eq!(
            during.string_at("Canary.Status.State"),
            row.canary_state,
            "case {case}: expected the canary to keep its own state while the dry run runs, \
             got {:?}",
            during.at("Canary.Status")
        );
        assert_eq!(
            during.at("Canary.Timeline"),
            &canary_timeline,
            "case {case}: expected the canary's own timeline to be untouched by the dry run, \
             got {:?}",
            during.at("Canary.Timeline")
        );

        let settled = settled_dry_run(&aws, case, row.name, &dry_run_id).await;
        assert_eq!(
            settled.string_at("Canary.Status.State"),
            row.dry_run_state,
            "case {case}: expected the dry run to settle to {}, got {:?}",
            row.dry_run_state,
            settled.at("Canary.Status")
        );
        let stopped = settled.at("Canary.Timeline.LastStopped").as_f64();
        let dry_run_started = settled.at("Canary.Timeline.LastStarted").as_f64();
        assert!(
            matches!((stopped, dry_run_started), (Some(end), Some(begin)) if end >= begin),
            "case {case}: expected the dry run to report a stop at or after its start, got \
             {:?}",
            settled.at("Canary.Timeline")
        );

        // The run the dry run started, and the verdict the dry run's terminal state follows.
        let runs = dry_runs(&aws, case, row.name).await;
        assert_eq!(
            runs.len(),
            1,
            "case {case}: expected exactly the one run this dry run started, got {runs:?}"
        );
        let run = &runs[0];
        assert_eq!(
            run["Status"]["State"].as_str(),
            Some(row.run_state),
            "case {case}: expected the dry run's run to be {}, got {:?}",
            row.run_state,
            run["Status"]
        );
        assert!(
            run["Timeline"]["Completed"].is_number(),
            "case {case}: expected the finished run to carry a completion instant, got {:?}",
            run["Timeline"]
        );
        assert_eq!(
            run["DryRunConfig"]["DryRunId"].as_str(),
            Some(dry_run_id.as_str()),
            "case {case}: expected the run to report the dry run it belongs to, got {run:?}"
        );
        if row.run_state == "PASSED" {
            assert!(
                run["Status"]["StateReason"].is_null()
                    && run["Status"]["StateReasonCode"].is_null(),
                "case {case}: a passing run carries no failure reason, got {:?}",
                run["Status"]
            );
            assert!(
                settled.at("Canary.Status.StateReason").is_null(),
                "case {case}: a dry run that finished carries no failure reason, got {:?}",
                settled.at("Canary.Status")
            );
        } else {
            assert_eq!(
                run["Status"]["StateReasonCode"].as_str(),
                Some(EXECUTION_FAILURE),
                "case {case}: expected {EXECUTION_FAILURE} for a run that could not store \
                 its artifacts, got {:?}",
                run["Status"]
            );
            assert_eq!(
                settled.at("Canary.Status.StateReason").as_str(),
                run["Status"]["StateReason"].as_str(),
                "case {case}: expected the dry run's reason to be the one its run reported, \
                 got {:?}",
                settled.at("Canary.Status")
            );
        }
        assert_eq!(
            settled.at("Canary.DryRunConfig.LastDryRunExecutionStatus").as_str(),
            Some(row.run_state),
            "case {case}: expected the last execution status to be the run's own, got {:?}",
            settled.at("Canary.DryRunConfig")
        );

        // A dry run is not a run of the canary's schedule, so it adds nothing to that
        // history: only a started canary has one there.
        let own_runs = canary_run_count(&aws, case, row.name).await;
        let expected_own_runs = usize::from(row.start_canary);
        assert_eq!(
            own_runs, expected_own_runs,
            "case {case}: expected {expected_own_runs} run(s) of the canary's own schedule, \
             got {own_runs}"
        );

        // And the canary itself came out of the dry run exactly as it went in.
        let after = ok(&format!("{case}/after"), aws.get(row.name).await);
        assert_eq!(
            after.string_at("Canary.Status.State"),
            row.canary_state,
            "case {case}: expected the canary to keep its own state after the dry run, got \
             {:?}",
            after.at("Canary.Status")
        );
        assert_eq!(
            after.at("Canary.Timeline"),
            &canary_timeline,
            "case {case}: expected the canary's own timeline to be unchanged by the dry run, \
             got {:?}",
            after.at("Canary.Timeline")
        );
    }
}

/// RULE: each dry run of one canary carries its OWN lifecycle, so one that has finished
/// keeps reporting its terminal state and its timeline while a later dry run of the same
/// canary runs and settles — and the canary stays `READY` through both.
#[tokio::test]
async fn each_dry_run_of_one_canary_settles_on_its_own() {
    let (aws, iam, s3) = Synthetics::with_account_services(REGION);
    create_bucket(&s3).await;
    let name = "two-dry-runs";
    let role = create_role(
        &iam,
        "two-dry-runs-role",
        json!([statement(
            "Allow",
            json!(ARTIFACT_WRITE_ACTION),
            json!(format!("arn:aws:s3:::{BUCKET}/*")),
        )]),
    )
    .await;
    ready_canary(
        &aws,
        "two",
        canary_body(
            name,
            &role,
            &format!("s3://{BUCKET}/{name}"),
            "rate(5 minutes)",
        ),
    )
    .await;

    let first = ok("first/start", start_dry_run(&aws, name).await).string_at("DryRunConfig.DryRunId");
    let first_settled = settled_dry_run(&aws, "first", name, &first).await;
    assert_eq!(
        first_settled.string_at("Canary.Status.State"),
        "STOPPED",
        "case first: expected the first dry run to settle to STOPPED, got {:?}",
        first_settled.at("Canary.Status")
    );
    let first_stopped = first_settled.at("Canary.Timeline.LastStopped").as_f64();
    assert!(
        first_stopped.is_some(),
        "case first: expected the finished dry run to report its stop, got {:?}",
        first_settled.at("Canary.Timeline")
    );

    let second =
        ok("second/start", start_dry_run(&aws, name).await).string_at("DryRunConfig.DryRunId");
    assert_ne!(
        second, first,
        "case second: expected a dry run of its own, got the first one's id {first:?}"
    );
    let second_settled = settled_dry_run(&aws, "second", name, &second).await;
    assert_eq!(
        second_settled.string_at("Canary.Status.State"),
        "STOPPED",
        "case second: expected the second dry run to settle to STOPPED on its own, got {:?}",
        second_settled.at("Canary.Status")
    );
    // Its own start, which is after the first one had finished — the second dry run does not
    // report the first's instants.
    let second_started = second_settled.at("Canary.Timeline.LastStarted").as_f64();
    assert!(
        matches!((second_started, first_stopped), (Some(begin), Some(end)) if begin >= end),
        "case second: expected the second dry run to start at or after the first one stopped, \
         got {:?} after {:?}",
        second_settled.at("Canary.Timeline"),
        first_settled.at("Canary.Timeline")
    );

    // The first one is not touched by the second: same state, same timeline.
    let first_again = ok("first/again", get_for_dry_run(&aws, name, &first).await);
    assert_eq!(
        first_again.string_at("Canary.Status.State"),
        "STOPPED",
        "case first again: expected a finished dry run to stay finished, got {:?}",
        first_again.at("Canary.Status")
    );
    assert_eq!(
        first_again.at("Canary.Timeline"),
        first_settled.at("Canary.Timeline"),
        "case first again: expected the finished dry run's timeline to be unchanged, got {:?}",
        first_again.at("Canary.Timeline")
    );

    // The canary's own read is neither of them.
    let canary = ok("canary", aws.get(name).await);
    assert_eq!(
        canary.string_at("Canary.Status.State"),
        "READY",
        "case canary: expected the canary to stay READY through both dry runs, got {:?}",
        canary.at("Canary.Status")
    );

    let runs = dry_runs(&aws, "both", name).await;
    assert_eq!(
        runs.len(),
        2,
        "case both: expected one run per dry run, got {runs:?}"
    );
    assert!(
        runs.iter()
            .all(|run| run["Status"]["State"].as_str() == Some("PASSED")),
        "case both: expected both dry runs' runs to have passed, got {runs:?}"
    );
}
