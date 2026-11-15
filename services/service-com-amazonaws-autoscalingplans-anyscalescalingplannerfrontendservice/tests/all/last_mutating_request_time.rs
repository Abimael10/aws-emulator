// SPDX-License-Identifier: CC0-1.0

//! `LastMutatingRequestTime`: when the last request that WROTE a plan was made.
//!
//! The rule pinned here: every `ScalingPlan` a `DescribeScalingPlans` 200 returns carries
//! `LastMutatingRequestTime`, an epoch-seconds timestamp of the last request that mutated
//! that plan — the `CreateScalingPlan` that minted it, or the latest `UpdateScalingPlan`
//! or `DeleteScalingPlan` applied to it — in every lifecycle phase the plan can be read
//! in. RECORDED behaviour: the member is not declared by the on-disk `ScalingPlan` shape
//! (`smithy_query(query="shape",shape="ScalingPlan")` lists `ScalingPlanName`,
//! `ScalingPlanVersion`, `ApplicationSource`, `ScalingInstructions`, `StatusCode`,
//! `StatusMessage`, `StatusStartTime` and `CreationTime`), and AWS emits it anyway.
//!
//! Why it has to be there, and has to track the WRITE: it is what a caller polling a plan
//! compares against the time of its own mutating call, to tell a read that has seen that
//! write from a pre-write snapshot. A caller that reads it as null cannot converge, and
//! one that reads a value moved by anything other than a write converges on the wrong
//! snapshot — so this suite also pins what must NOT move it: a read, and a transition the
//! clock settles on its own.
//!
//! The VALUE is a clock reading, so nothing here asserts a particular instant: what is
//! asserted is the member's presence, its type and units, what it equals in state (the
//! plan's own `CreationTime` right after the create), that it is per plan, and that it
//! never moves backwards or without a write.
//!
//! The harness (router, dispatch, lifecycle clock) is reused from
//! [`crate::scaling_plan_lifecycle`]; nothing here builds a second one.

use serde_json::{json, Value};

use crate::scaling_plan_lifecycle::{
    now_secs, simple_instructions, stack_source, Emulator, DAY_SECONDS,
};

/// The member this suite is about.
const MEMBER: &str = "LastMutatingRequestTime";

/// How far from the test's own clock a stamped instant may sit and still be a reading of
/// the same clock. Generous on purpose: this suite never measures elapsed time, it only
/// rules out a value that is not a current epoch-seconds reading at all (a zero, a
/// millisecond count, a date years out).
const CLOCK_WINDOW_SECONDS: i64 = DAY_SECONDS;

/// The plan every case writes to.
const PLAN: &str = "my-plan";

/// Create `plan` and leave its creation still being applied.
async fn create(emulator: &Emulator, plan: &str, source: usize) {
    emulator
        .ok(
            "CreateScalingPlan",
            json!({
                "ScalingPlanName": plan,
                "ApplicationSource": stack_source(source),
                "ScalingInstructions": simple_instructions(),
            }),
        )
        .await;
}

/// The one plan `plan`, as `DescribeScalingPlans` reports it.
async fn described(emulator: &Emulator, plan: &str) -> Value {
    let body = emulator
        .ok("DescribeScalingPlans", json!({ "ScalingPlanNames": [plan] }))
        .await;
    body["ScalingPlans"]
        .as_array()
        .and_then(|plans| plans.first().cloned())
        .unwrap_or_else(|| panic!("no plan named {plan:?} in {body}"))
}

/// The `LastMutatingRequestTime` of `plan`, as a number of seconds since the epoch, with
/// the member's presence and units checked on the way through.
async fn stamp(emulator: &Emulator, case: &str, plan: &str) -> f64 {
    let entry = described(emulator, plan).await;
    let member = entry
        .get(MEMBER)
        .unwrap_or_else(|| panic!("case {case:?}: {MEMBER} is missing from {entry}"));
    let seconds = member.as_f64().unwrap_or_else(|| {
        panic!("case {case:?}: {MEMBER} must be an epoch-seconds number, got {member}")
    });
    let now = now_secs();
    assert!(
        (seconds - now as f64).abs() < CLOCK_WINDOW_SECONDS as f64,
        "case {case:?}: {MEMBER} must be a reading of the same clock as the caller's \
         ({now} s since the epoch), got {seconds}"
    );
    seconds
}

#[tokio::test]
async fn every_described_plan_carries_the_time_of_its_last_mutating_request() {
    // Every phase a plan can be read in, in the order one plan passes through them: the
    // two transient phases are held open rather than raced, so each read happens in a
    // known phase.
    let emulator = Emulator::new();
    create(&emulator, PLAN, 1).await;
    emulator.hold_pending(PLAN);

    let creating = described(&emulator, PLAN).await;
    assert_eq!(
        creating.get("StatusCode"),
        Some(&json!("CreationInProgress")),
        "the plan under test must still be being created, got {creating}"
    );
    let while_creating = stamp(&emulator, "while being created", PLAN).await;
    // The create IS the plan's last mutating request, so the two instants the plan
    // reports for it agree.
    let creation_time = creating
        .get("CreationTime")
        .and_then(Value::as_f64)
        .unwrap_or_else(|| panic!("no CreationTime in {creating}"));
    assert_eq!(
        while_creating, creation_time,
        "case \"while being created\": the create is the last mutating request, so \
         {MEMBER} must be the plan's CreationTime, got {while_creating} against \
         {creation_time}"
    );

    // A transition the CLOCK settles is not a request, so it must not move the stamp.
    emulator.make_all_due();
    let created = described(&emulator, PLAN).await;
    assert_eq!(
        created.get("StatusCode"),
        Some(&json!("Active")),
        "the plan under test must have been applied, got {created}"
    );
    let after_creating = stamp(&emulator, "applied create", PLAN).await;
    assert_eq!(
        after_creating, while_creating,
        "case \"applied create\": the clock settling the owed transition is not a \
         mutating request, so {MEMBER} must not move, got {after_creating} after \
         {while_creating}"
    );

    // Neither does a read: two reads of one unchanged plan report one value.
    let read_again = stamp(&emulator, "read again", PLAN).await;
    assert_eq!(
        read_again, after_creating,
        "case \"read again\": a read is not a mutating request, so {MEMBER} must not \
         move, got {read_again} after {after_creating}"
    );

    // An update is a mutating request: the stamp is its instant, and the clock applying
    // the update afterwards leaves it where the request left it.
    emulator
        .ok(
            "UpdateScalingPlan",
            json!({
                "ScalingPlanName": PLAN,
                "ScalingPlanVersion": 1,
                "ScalingInstructions": simple_instructions(),
            }),
        )
        .await;
    emulator.hold_pending(PLAN);
    let updating = described(&emulator, PLAN).await;
    assert_eq!(
        updating.get("StatusCode"),
        Some(&json!("UpdateInProgress")),
        "the plan under test must be being updated, got {updating}"
    );
    let while_updating = stamp(&emulator, "while being updated", PLAN).await;
    assert!(
        while_updating >= after_creating,
        "case \"while being updated\": {MEMBER} tracks the LATEST mutating request, so it \
         cannot move backwards, got {while_updating} after {after_creating}"
    );
    emulator.make_all_due();
    let after_updating = stamp(&emulator, "applied update", PLAN).await;
    assert_eq!(
        after_updating, while_updating,
        "case \"applied update\": the clock applying the update is not a request, so \
         {MEMBER} must stay at the update's own instant, got {after_updating} after \
         {while_updating}"
    );

    // A delete is a mutating request too, and the plan is still readable while its
    // deletion is being applied — which is exactly where a caller polling for its own
    // write looks.
    emulator
        .ok(
            "DeleteScalingPlan",
            json!({ "ScalingPlanName": PLAN, "ScalingPlanVersion": 1 }),
        )
        .await;
    emulator.hold_pending(PLAN);
    let deleting = described(&emulator, PLAN).await;
    assert_eq!(
        deleting.get("StatusCode"),
        Some(&json!("DeletionInProgress")),
        "the plan under test must be being deleted, got {deleting}"
    );
    let while_deleting = stamp(&emulator, "while being deleted", PLAN).await;
    assert!(
        while_deleting >= after_updating,
        "case \"while being deleted\": {MEMBER} tracks the LATEST mutating request, so it \
         cannot move backwards, got {while_deleting} after {after_updating}"
    );
}

#[tokio::test]
async fn a_plans_stamp_moves_only_when_that_plan_is_written() {
    // Two plans, created in a known order, then one of them updated: the stamp of the
    // plan nobody wrote stays where it was, and the written plan's stamp is no older than
    // the one minted after it. Stated as orderings rather than as instants, because the
    // values are clock readings.
    let emulator = Emulator::new();
    create(&emulator, "first", 1).await;
    create(&emulator, "second", 2).await;
    emulator.make_all_due();

    let first_created = stamp(&emulator, "first created", "first").await;
    let second_created = stamp(&emulator, "second created", "second").await;
    assert!(
        second_created >= first_created,
        "case \"two creates\": the plan created later carries the later stamp, got \
         {second_created} against {first_created}"
    );

    emulator
        .ok(
            "UpdateScalingPlan",
            json!({
                "ScalingPlanName": "first",
                "ScalingPlanVersion": 1,
                "ScalingInstructions": simple_instructions(),
            }),
        )
        .await;
    emulator.make_all_due();

    let first_updated = stamp(&emulator, "first updated", "first").await;
    assert!(
        first_updated >= second_created,
        "case \"one of two plans updated\": the updated plan's stamp is the time of ITS \
         update, which is later than the other plan's create, got {first_updated} \
         against {second_created}"
    );
    let second_untouched = stamp(&emulator, "second untouched", "second").await;
    assert_eq!(
        second_untouched, second_created,
        "case \"one of two plans updated\": writing one plan must not move another \
         plan's stamp, got {second_untouched} after {second_created}"
    );
}

#[tokio::test]
async fn every_entry_of_a_paged_read_carries_the_member() {
    // The member is on every entry of the page, not only on a single-plan read: a caller
    // listing plans converges on each one from the same response.
    let emulator = Emulator::new();
    for index in 0..5 {
        create(&emulator, &format!("plan-{index}"), index).await;
    }
    emulator.make_all_due();

    let mut token: Option<String> = None;
    let mut seen = 0_usize;
    for page in 0..5 {
        let mut request = json!({ "MaxResults": 2 });
        if let Some(token) = token.as_deref() {
            request["NextToken"] = json!(token);
        }
        let body = emulator.ok("DescribeScalingPlans", request).await;
        let plans = body["ScalingPlans"].as_array().cloned().unwrap_or_default();
        for entry in &plans {
            let member = entry.get(MEMBER).unwrap_or_else(|| {
                panic!("case \"page {page}\": {MEMBER} is missing from {entry}")
            });
            assert!(
                member.as_f64().is_some(),
                "case \"page {page}\": {MEMBER} must be an epoch-seconds number, got \
                 {member}"
            );
            seen += 1;
        }
        token = body
            .get("NextToken")
            .and_then(Value::as_str)
            .map(str::to_string);
        if token.is_none() {
            break;
        }
    }
    assert_eq!(
        seen, 5,
        "every stored plan must be reported with the member, got {seen} of 5"
    );
}
