// SPDX-License-Identifier: CC0-1.0

//! What a client that watches a domain configuration change STAGE BY STAGE observes,
//! driven over the real wire path — a `restJson1` request through the multi-service
//! router into the mounted service — so what is asserted is what a polling client sees.
//!
//! ONE rule, over every call that applies a configuration change to a provisioned
//! domain:
//!
//! > a change the service has BEGUN APPLYING to a domain's cluster is applied by
//! > deploying that cluster anew, so the progress of the change reports that
//! > deployment's stages — "the specific stages that the domain is going through to
//! > perform the configuration change", a non-empty list whose entries each carry a
//! > `Name`, a `Status`, a `Description` and a `LastUpdated`, counted by
//! > `TotalNumberOfStages`, performed in order and advancing only forward until every
//! > one of them has completed with the change. A change the service has NOT begun
//! > applying — the one a create starts, which waits for the cluster there is nothing to
//! > deploy yet — goes through no such deployment and reports no stage.
//!
//! So the assertions span the inputs the rule generalises over: every option container
//! in the shared change table (a cluster structure, an EBS structure, a policy string,
//! two maps, an enum, a snapshot structure and a software-update structure — four of
//! them changes that reshape the cluster and four settings on the running one), the
//! create that mints the domain, and a change stopped by a cancel. The stage list is
//! read both implicitly and by the change's own `ChangeId`, and on both sides of the
//! span the clock owes the change.
//!
//! The harness, the baseline `CreateDomain` body, the shared change table and the poll
//! helpers come from [`crate::domain_lifecycle`]; the progress call itself from
//! [`super`] and the cancel from [`crate::config_change_cancellation`] — this suite
//! adds no second copy of any of them.

use serde_json::{json, Value};

use crate::domain_lifecycle::{
    change_rows, create_baseline, poll_until, settled_domain, string_member, Emulator,
    CHANGE_LOG_GROUP,
};

/// The values `ChangeProgressStage$Status` takes.
///
/// The member is a free `String` of 1..256 characters in this service's model, so the
/// value set is the one the ingestion service's model publishes as an enum for the same
/// concept (`smithy_query(query="shape",
/// shape="com.amazonaws.osis#ChangeProgressStageStatuses")`): a stage is waiting, being
/// performed, performed, or not going to be performed.
const STAGE_STATUSES: [&str; 4] = ["PENDING", "IN_PROGRESS", "COMPLETED", "FAILED"];

/// The states a stage that IS being performed walks, in the order it walks them. The
/// fourth value above is off this progression — a stage reported as failed has not
/// advanced along it but stopped — so a case that observed one is not ranked here.
const STAGE_PROGRESSION: [&str; 3] = ["PENDING", "IN_PROGRESS", "COMPLETED"];

/// `ChangeProgressStage$Name`'s own `@length(min: 1, max: 64)`.
const STAGE_NAME_BOUNDS: (usize, usize) = (1, 64);

/// How far along [`STAGE_PROGRESSION`] the state `status` is.
fn rank(status: &str, case: &str) -> usize {
    STAGE_PROGRESSION
        .iter()
        .position(|state| *state == status)
        .unwrap_or_else(|| {
            panic!(
                "case {case}: expected one of the states a stage being performed walks \
                 ({STAGE_PROGRESSION:?}), got {status:?}"
            )
        })
}

/// One reported stage of a change: its name, its status and the instant its status last
/// moved.
#[derive(Debug, Clone, PartialEq)]
struct Stage {
    name: String,
    status: String,
    last_updated: f64,
}

/// The stages a `ChangeProgressStatusDetails` reports, each checked against the members
/// the model declares for it, with `TotalNumberOfStages` checked against their count.
fn reported_stages(status: &Value, case: &str) -> Vec<Stage> {
    let listed = status["ChangeProgressStages"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case}: expected a ChangeProgressStages list, got {status:?}"))
        .clone();
    assert_eq!(
        status["TotalNumberOfStages"].as_i64(),
        Some(i64::try_from(listed.len()).unwrap_or(i64::MAX)),
        "case {case}: the total number of stages is the number of stages reported, got \
         {status:?}"
    );
    listed
        .iter()
        .map(|entry| {
            let name = string_member(entry, "Name", case);
            assert!(
                (STAGE_NAME_BOUNDS.0..=STAGE_NAME_BOUNDS.1).contains(&name.chars().count()),
                "case {case}: a stage name is {STAGE_NAME_BOUNDS:?} characters, got {name:?} in \
                 {entry:?}"
            );
            let reported = string_member(entry, "Status", case);
            assert!(
                STAGE_STATUSES.contains(&reported.as_str()),
                "case {case}: expected one of {STAGE_STATUSES:?} as the status of {name:?}, got \
                 {reported:?}"
            );
            let description = string_member(entry, "Description", case);
            assert!(
                !description.is_empty(),
                "case {case}: expected a description of the stage {name:?}, got {entry:?}"
            );
            let last_updated = entry["LastUpdated"].as_f64().unwrap_or_else(|| {
                panic!("case {case}: expected a LastUpdated timestamp for {name:?}, got {entry:?}")
            });
            Stage {
                name,
                status: reported,
                last_updated,
            }
        })
        .collect()
}

/// Assert the stages of a change the service is APPLYING: there is at least one, the
/// deployment has begun, each is named once, and they are performed IN ORDER — no stage
/// is ahead of the one before it.
fn assert_stages_in_flight(stages: &[Stage], case: &str) {
    assert!(
        !stages.is_empty(),
        "case {case}: a change being applied reports the stages it is going through, got none"
    );
    let mut names: Vec<&str> = stages.iter().map(|stage| stage.name.as_str()).collect();
    let listed = names.len();
    names.sort_unstable();
    names.dedup();
    assert_eq!(
        names.len(),
        listed,
        "case {case}: every stage of a change is reported once, got {stages:?}"
    );
    assert!(
        stages.iter().any(|stage| stage.status != "PENDING"),
        "case {case}: the deployment applying the change has begun, so a stage of it is \
         under way or done, got {stages:?}"
    );
    for (earlier, later) in stages.iter().zip(stages.iter().skip(1)) {
        assert!(
            rank(&earlier.status, case) >= rank(&later.status, case),
            "case {case}: the stages are performed in order, so {:?} cannot be ahead of {:?}, \
             got {stages:?}",
            later.name,
            earlier.name
        );
    }
}

/// RULE: a change the service is applying to a provisioned cluster reports the stages of
/// the deployment applying it, advancing only forward to all-completed when the change
/// lands.
#[tokio::test]
async fn a_change_being_applied_reports_the_stages_it_is_going_through() {
    let emulator = Emulator::with_log_groups();
    // The log-publishing row of the shared table publishes to a log group, which has to
    // exist before an option enabling it is accepted.
    emulator.created_log_group(CHANGE_LOG_GROUP).await;
    let rows = change_rows();
    // One domain per row, created first so ONE wait settles every one of them: a domain
    // still being provisioned has work in flight, which the update under test refuses.
    for index in 0..rows.len() {
        create_baseline(&emulator, &format!("stages-{index}")).await;
    }
    for index in 0..rows.len() {
        settled_domain(&emulator, &format!("stages-{index}")).await;
    }

    // (the row's option, the domain, the change it started, the stages seen in flight)
    let mut in_flight: Vec<(&str, String, String, Vec<Stage>)> = Vec::new();
    for (index, (option, value, deployment)) in rows.into_iter().enumerate() {
        let name = format!("stages-{index}");
        let updated = emulator
            .update_config(&name, json!({ option: value.clone() }))
            .await;
        assert_eq!(
            updated.status, 200,
            "case UpdateDomainConfig({option}): expected 200, got {} with {:?}",
            updated.status, updated.body
        );

        let case = format!("DescribeDomainChangeProgress({option}, {deployment}, in flight)");
        let status = super::progress_status(&emulator, &name, None).await;
        let stages = reported_stages(&status, &case);
        assert_stages_in_flight(&stages, &case);

        // The same change read by its own id reports the same stages: the stages belong
        // to the CHANGE, not to the way a caller addressed it.
        let change_id = string_member(&status, "ChangeId", &case);
        let by_id = super::progress_status(&emulator, &name, Some(&change_id)).await;
        let named = reported_stages(&by_id, &format!("{case}, by id"));
        assert_eq!(
            named.iter().map(|stage| &stage.name).collect::<Vec<_>>(),
            stages.iter().map(|stage| &stage.name).collect::<Vec<_>>(),
            "case {case}: the change read by its id reports the same stages, got {named:?} \
             after {stages:?}"
        );

        in_flight.push((option, name, change_id, stages));
    }

    for (option, name, change_id, seen) in in_flight {
        // The clock, not the reader, completes the change — so the condition is waited
        // for rather than a span slept through.
        let case = format!("DescribeDomainChangeProgress({option}, settled)");
        poll_until(&case, || async {
            super::progress_status(&emulator, &name, None).await["ConfigChangeStatus"].as_str()
                == Some("Completed")
        })
        .await;
        let settled = super::progress_status(&emulator, &name, None).await;
        assert_eq!(
            settled["ChangeId"].as_str(),
            Some(change_id.as_str()),
            "case {case}: expected the change that was in flight, got {settled:?}"
        );
        let stages = reported_stages(&settled, &case);
        assert_eq!(
            stages.iter().map(|stage| &stage.name).collect::<Vec<_>>(),
            seen.iter().map(|stage| &stage.name).collect::<Vec<_>>(),
            "case {case}: the stages of a change do not change identity or order, got \
             {stages:?} after {seen:?}"
        );
        for (before, after) in seen.iter().zip(stages.iter()) {
            assert_eq!(
                after.status.as_str(),
                "COMPLETED",
                "case {case}: every stage of a change that has landed has completed, got \
                 {after:?}"
            );
            assert!(
                after.last_updated >= before.last_updated,
                "case {case}: a stage's LastUpdated only moves forward, got {after:?} after \
                 {before:?}"
            );
        }
    }
}

/// RULE (the other side of it): the change a create starts is not being applied to a
/// cluster — there is none to deploy yet — so it goes through no stage, which is the
/// empty list and a total of zero rather than no list at all.
#[tokio::test]
async fn the_change_a_create_starts_goes_through_no_deployment_stage() {
    let emulator = Emulator::new();
    let name = "stages-of-a-create";
    create_baseline(&emulator, name).await;

    let case = "DescribeDomainChangeProgress(the create's change, in flight)";
    let waiting = super::progress_status(&emulator, name, None).await;
    assert_eq!(
        (
            waiting["ConfigChangeStatus"].as_str(),
            reported_stages(&waiting, case),
        ),
        (Some("Pending"), Vec::new()),
        "case {case}: a change waiting for the cluster it would be applied to publishes no \
         stage, got {waiting:?}"
    );

    // And still none once it has landed: the domain's first cluster was provisioned, not
    // deployed anew.
    settled_domain(&emulator, name).await;
    let case = "DescribeDomainChangeProgress(the create's change, settled)";
    let settled = super::progress_status(&emulator, name, None).await;
    assert_eq!(
        (
            settled["ConfigChangeStatus"].as_str(),
            reported_stages(&settled, case),
        ),
        (Some("Completed"), Vec::new()),
        "case {case}: the change a create started published no stage, got {settled:?}"
    );
}

/// RULE (the stopped deployment): a change a cancel stopped is not being applied any
/// more, so no stage of it is left waiting or under way — each one has either been
/// performed or is not going to be.
#[tokio::test]
async fn a_cancelled_change_leaves_no_stage_under_way() {
    let emulator = Emulator::new();
    let name = "stages-cancelled";
    create_baseline(&emulator, name).await;
    settled_domain(&emulator, name).await;

    let updated = emulator
        .update_config(
            name,
            json!({ "ClusterConfig": { "InstanceType": "r6g.xlarge.search", "InstanceCount": 4 } }),
        )
        .await;
    assert_eq!(
        updated.status, 200,
        "case UpdateDomainConfig({name:?}): expected 200, got {} with {:?}",
        updated.status, updated.body
    );
    let case = "DescribeDomainChangeProgress(a cancelled change)";
    let in_flight = super::progress_status(&emulator, name, None).await;
    assert_stages_in_flight(&reported_stages(&in_flight, case), case);

    let cancelled = crate::config_change_cancellation::cancel(&emulator, name, None).await;
    assert_eq!(
        cancelled.status, 200,
        "case CancelDomainConfigChange({name:?}): expected 200, got {} with {:?}",
        cancelled.status, cancelled.body
    );

    let stopped = super::progress_status(&emulator, name, None).await;
    let stages = reported_stages(&stopped, case);
    assert!(
        !stages.is_empty(),
        "case {case}: the change the cancel stopped keeps the stages it was going through, \
         got {stopped:?}"
    );
    for stage in &stages {
        assert!(
            ["COMPLETED", "FAILED"].contains(&stage.status.as_str()),
            "case {case}: a stopped deployment leaves no stage waiting or under way, got \
             {stage:?}"
        );
    }
}
