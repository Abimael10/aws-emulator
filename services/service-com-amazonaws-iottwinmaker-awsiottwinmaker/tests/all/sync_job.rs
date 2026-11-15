// SPDX-License-Identifier: CC0-1.0

//! The sync job lifecycle, end to end over HTTP.
//!
//! Every case drives the real request path — `MultiServiceRouter::dispatch`, the
//! same entry the served emulator uses — so a route, a label, a query
//! parameter, a status code and a serialized body are all exercised, not just a
//! handler body.
//!
//! The suites assert RULES over a table of inputs rather than one recorded
//! case: what a `CreateSyncJob` persists is exactly what `GetSyncJob` reports,
//! whatever role and tags the create carried; the create answers the
//! non-terminal `CREATING` and the CLOCK — not a caller — brings the link up
//! through `INITIALIZING` to the standing `ACTIVE`; a sync job is identified
//! inside its workspace by its sync source, so it is reachable only through that
//! workspace and a second create for the same source conflicts; a source the
//! service does not support is refused and stores nothing; and a workspace's
//! `linkedServices` follows the sync jobs it holds.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use service_com_amazonaws_iottwinmaker_awsiottwinmaker::ops::types::SYNC_JOB_STATE_VALUES;
use service_com_amazonaws_iottwinmaker_awsiottwinmaker::state::{
    SYNC_JOB_ACTIVE_SPAN, SYNC_JOB_SETTLE_SPAN, SYNC_JOB_STATE_ACTIVE, SYNC_JOB_STATE_CREATING,
    SYNC_JOB_STATE_INITIALIZING, SYNC_SOURCE_SITEWISE,
};

use crate::harness::{sync_job_arn, sync_job_in_state, Emulator, Resp, REGION};

/// The `status.state` a response reports for a job.
fn reported_state(resp: &Resp) -> Option<&str> {
    resp.member("status")["state"].as_str()
}

/// Age the stored sync job `workspace_id`/`sync_source` by `elapsed`, as if that
/// much time had passed since the create, and answer the creation instant it
/// then carries.
///
/// The clock is what moves a job on, and the instants it owes are ON the record,
/// so a case about the state a given amount of elapsed time makes due drives
/// that time here rather than waiting for it: every instant the record carries
/// moves back by `elapsed`, which is the same record a create that happened that
/// long ago would have left. Reached through the seam a handler reads state by
/// (`harness::Emulator::state`), so what the reads below report is what the one
/// store holds.
fn age_sync_job(
    emulator: &Emulator,
    workspace_id: &str,
    sync_source: &str,
    elapsed: Duration,
) -> SystemTime {
    let state = emulator.state();
    let mut state = state.lock();
    let job = state
        .region_mut(REGION)
        .workspaces
        .get_mut(workspace_id)
        .and_then(|workspace| workspace.sync_jobs.get_mut(sync_source))
        .unwrap_or_else(|| panic!("the sync job {workspace_id:?}/{sync_source:?} must exist"));
    job.creation_date_time -= elapsed;
    job.update_date_time -= elapsed;
    job.settles_at -= elapsed;
    job.active_at -= elapsed;
    job.creation_date_time
}

/// `at` as the epoch seconds a timestamp member carries it in.
fn seconds_of(at: SystemTime) -> f64 {
    at.duration_since(UNIX_EPOCH)
        .expect("an instant after the epoch")
        .as_secs_f64()
}

/// An emulator holding the workspaces `workspace_ids` names.
async fn emulator_with_workspaces(workspace_ids: &[&str]) -> Emulator {
    let emulator = Emulator::new();
    for workspace_id in workspace_ids {
        assert_eq!(
            emulator.create_workspace(workspace_id).await.status,
            200,
            "the workspace {workspace_id:?} the cases name must exist"
        );
    }
    emulator
}

/// The table every round-trip row is built from:
/// `(case, workspaceId, syncRole, tags)`.
///
/// One workspace per row, because a sync source identifies at most one job
/// inside a workspace. The rows differ along what the rule generalises over —
/// which partition the `syncRole` ARN names and how deep its role path runs
/// (the modelled `RoleArn` is `@length(20..2048)` and
/// `^arn:((aws)|(aws-cn)|(aws-us-gov)):iam::[0-9]{12}:role/`, from
/// `smithy_query(query="operation",operation="CreateSyncJob")`), and whether the
/// create carries no `tags` member, an empty one, or several entries — never
/// along a mere rename.
fn rows() -> Vec<(&'static str, &'static str, String, Option<Value>)> {
    vec![
        (
            "aws partition, no tags member",
            "syncWorkspaceA",
            "arn:aws:iam::000000000000:role/TwinMakerSyncRole".to_string(),
            None,
        ),
        (
            "aws-cn partition, empty tag map",
            "syncWorkspaceB",
            "arn:aws-cn:iam::123456789012:role/service-role/plant/SyncRole".to_string(),
            Some(json!({})),
        ),
        (
            "aws-us-gov partition, one tag",
            "syncWorkspaceC",
            "arn:aws-us-gov:iam::999999999999:role/s".to_string(),
            Some(json!({"team": "plant"})),
        ),
        (
            "long role path, several tags",
            "syncWorkspaceD",
            format!("arn:aws:iam::000000000000:role/{}", "deep/".repeat(40)),
            Some(json!({"env": "test", "owner": "sync", "tier": "2"})),
        ),
    ]
}

/// RULE: a create persists one sync job under the source that identifies it in
/// the workspace the request names, and a read of that workspace and source
/// reports exactly what was persisted — the minted ARN, the workspace, the
/// source, the caller's role and the creation instant — with the job's tags
/// reachable under its own ARN through this service's one tagging surface.
#[tokio::test]
async fn a_sync_job_reads_back_exactly_what_was_created() {
    let workspaces: Vec<&str> = rows().iter().map(|row| row.1).collect();
    let emulator = emulator_with_workspaces(&workspaces).await;

    for (case, workspace_id, sync_role, tags) in rows() {
        let mut request = json!({"syncRole": sync_role});
        if let Some(tags) = tags.clone() {
            request["tags"] = tags;
        }
        let created = emulator
            .create_sync_job(workspace_id, SYNC_SOURCE_SITEWISE, request)
            .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: expected 200 from CreateSyncJob, got {created:?}"
        );
        assert_eq!(
            created.member("arn").as_str(),
            Some(sync_job_arn(workspace_id, SYNC_SOURCE_SITEWISE).as_str()),
            "case {case:?}: expected the minted ARN, got {created:?}"
        );
        assert_eq!(
            created.member("state").as_str(),
            Some(SYNC_JOB_STATE_CREATING),
            "case {case:?}: a create answers the non-terminal CREATING, got {created:?}"
        );
        let creation_date_time = created.member("creationDateTime").as_f64();
        assert!(
            creation_date_time.is_some_and(|seconds| seconds > 0.0),
            "case {case:?}: expected an epoch-seconds creationDateTime, got {created:?}"
        );

        let read = emulator
            .get_sync_job(Some(workspace_id), SYNC_SOURCE_SITEWISE)
            .await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: expected 200 from GetSyncJob, got {read:?}"
        );
        assert_eq!(
            (
                read.member("arn").as_str(),
                read.member("workspaceId").as_str(),
                read.member("syncSource").as_str(),
                read.member("syncRole").as_str(),
            ),
            (
                Some(sync_job_arn(workspace_id, SYNC_SOURCE_SITEWISE).as_str()),
                Some(workspace_id),
                Some(SYNC_SOURCE_SITEWISE),
                Some(sync_role.as_str()),
            ),
            "case {case:?}: expected the created identity and role, got {read:?}"
        );
        assert_eq!(
            reported_state(&read),
            Some(SYNC_JOB_STATE_CREATING),
            "case {case:?}: a read inside the settle span still reports CREATING, got {read:?}"
        );
        assert!(
            reported_state(&read).is_some_and(|state| SYNC_JOB_STATE_VALUES.contains(&state)),
            "case {case:?}: the reported state must be a modelled SyncJobState, got {read:?}"
        );
        assert_eq!(
            read.member("status")["error"],
            json!({}),
            "case {case:?}: a job that failed at nothing carries an empty error, got {read:?}"
        );
        assert_eq!(
            read.member("creationDateTime").as_f64(),
            creation_date_time,
            "case {case:?}: the read must report the create's own instant, got {read:?}"
        );
        assert_eq!(
            read.member("updateDateTime").as_f64(),
            creation_date_time,
            "case {case:?}: nothing has moved the job yet, so updateDateTime is the creation \
             instant, got {read:?}"
        );

        // The create's tags are reachable under the job's own ARN, through the
        // one tagging surface every resource of this service shares.
        let listed = emulator
            .list_tags_for_resource(&sync_job_arn(workspace_id, SYNC_SOURCE_SITEWISE), json!({}))
            .await;
        assert_eq!(
            (listed.status, listed.member("tags")),
            (200, &tags.unwrap_or_else(|| json!({}))),
            "case {case:?}: a sync job ARN must be taggable and report its created tags, \
             got {listed:?}"
        );
    }
}

/// RULE: an accepted job is reported in the non-terminal `CREATING` state until
/// the instant the next state of bringing its link up falls due, and the CLOCK —
/// not a caller's poll — then brings it through `INITIALIZING` to the standing
/// `ACTIVE`, with no error; the standing job is stable across reads.
#[tokio::test]
async fn an_accepted_sync_job_reaches_the_standing_state_the_clock_owes() {
    // The rows differ in whether a reader observed the job during the first
    // span, because the transitions are the clock's and not a read's.
    let rows = [
        ("polled", "polledWorkspace", true),
        ("unpolled", "unpolledWorkspace", false),
    ];
    let workspaces: Vec<&str> = rows.iter().map(|row| row.1).collect();
    let emulator = emulator_with_workspaces(&workspaces).await;

    for (case, workspace_id, poll_while_creating) in rows {
        let created = emulator
            .create_sync_job(
                workspace_id,
                SYNC_SOURCE_SITEWISE,
                json!({"syncRole": "arn:aws:iam::000000000000:role/TwinMakerSyncRole"}),
            )
            .await;
        assert_eq!(
            created.member("state").as_str(),
            Some(SYNC_JOB_STATE_CREATING),
            "case {case:?}: a create answers CREATING, got {created:?}"
        );
        if poll_while_creating {
            let during = emulator
                .get_sync_job(Some(workspace_id), SYNC_SOURCE_SITEWISE)
                .await;
            assert_eq!(
                reported_state(&during),
                Some(SYNC_JOB_STATE_CREATING),
                "case {case:?}: a read in the same breath as the create still reports CREATING, \
                 got {during:?}"
            );
        }
    }

    for (case, workspace_id, _) in rows {
        // The standing state the clock owes, waited for as a CONDITION and
        // never for a span.
        let settled =
            sync_job_in_state(&emulator, workspace_id, SYNC_SOURCE_SITEWISE, SYNC_JOB_STATE_ACTIVE)
                .await;
        assert_eq!(
            settled.member("status")["error"],
            json!({}),
            "case {case:?}: a job whose synchronisation landed carries no error, got {settled:?}"
        );
        assert!(
            reported_state(&settled).is_some_and(|state| SYNC_JOB_STATE_VALUES.contains(&state)),
            "case {case:?}: the reported state must be a modelled SyncJobState, got {settled:?}"
        );
        // The transition fell due at the stored instant, not when a caller
        // asked, so two reads of a settled job report the same `updateDateTime`
        // — and it is later than the creation instant.
        let update_date_time = settled.member("updateDateTime").as_f64();
        assert!(
            update_date_time > settled.member("creationDateTime").as_f64(),
            "case {case:?}: reaching the standing state is an update, so updateDateTime moved \
             past creationDateTime, got {settled:?}"
        );
        let again = emulator
            .get_sync_job(Some(workspace_id), SYNC_SOURCE_SITEWISE)
            .await;
        assert_eq!(
            (
                reported_state(&again),
                again.member("updateDateTime").as_f64(),
            ),
            (Some(SYNC_JOB_STATE_ACTIVE), update_date_time),
            "case {case:?}: a read must not move a settled job again, got {again:?}"
        );
    }
}

/// RULE: a job bringing its link up passes through every state the model
/// declares for it, in order — `CREATING` while nothing is due, `INITIALIZING`
/// once the first transition is, and the standing `ACTIVE` once the second is —
/// and each state is reported from the instant it fell due, never from the
/// instant a caller asked.
///
/// The rows differ along HOW MUCH of the bring-up the clock has already owed the
/// job, which is what the rule generalises over; time is driven by ageing the
/// stored record ([`age_sync_job`]) rather than by waiting, so every row is
/// decided by the rule and not by how fast the host runs.
#[tokio::test]
async fn a_sync_job_reports_each_state_from_the_instant_it_falls_due() {
    const WORKSPACE: &str = "bringUpWorkspace";
    // `(case, how long ago the create happened, the state that is then due)`.
    let rows: Vec<(&str, Duration, &str)> = vec![
        ("nothing due yet", Duration::ZERO, SYNC_JOB_STATE_CREATING),
        (
            "just short of the first transition",
            SYNC_JOB_SETTLE_SPAN - Duration::from_millis(1),
            SYNC_JOB_STATE_CREATING,
        ),
        (
            "the first transition is due",
            SYNC_JOB_SETTLE_SPAN,
            SYNC_JOB_STATE_INITIALIZING,
        ),
        (
            "just short of the standing state",
            SYNC_JOB_ACTIVE_SPAN - Duration::from_millis(1),
            SYNC_JOB_STATE_INITIALIZING,
        ),
        (
            "the standing state is due",
            SYNC_JOB_ACTIVE_SPAN,
            SYNC_JOB_STATE_ACTIVE,
        ),
        (
            "long past the standing state",
            SYNC_JOB_ACTIVE_SPAN * 20,
            SYNC_JOB_STATE_ACTIVE,
        ),
    ];
    for (case, aged_by, expected) in rows {
        let emulator = emulator_with_workspaces(&[WORKSPACE]).await;
        assert_eq!(
            emulator
                .create_sync_job(
                    WORKSPACE,
                    SYNC_SOURCE_SITEWISE,
                    json!({"syncRole": "arn:aws:iam::000000000000:role/TwinMakerSyncRole"}),
                )
                .await
                .status,
            200,
            "case {case:?}: the job the read reports must be created"
        );
        let creation_date_time = age_sync_job(&emulator, WORKSPACE, SYNC_SOURCE_SITEWISE, aged_by);
        let read = emulator
            .get_sync_job(Some(WORKSPACE), SYNC_SOURCE_SITEWISE)
            .await;
        assert_eq!(
            reported_state(&read),
            Some(expected),
            "case {case:?}: expected the state the clock owes, got {read:?}"
        );
        assert_eq!(
            read.member("status")["error"],
            json!({}),
            "case {case:?}: a job whose synchronisation landed carries no error, got {read:?}"
        );
        // `updateDateTime` is the instant the state that is due fell due — the
        // creation instant plus that state's span — and not the instant of this
        // read, so two reads of one job report it identically.
        let due_after = match expected {
            SYNC_JOB_STATE_CREATING => Duration::ZERO,
            SYNC_JOB_STATE_INITIALIZING => SYNC_JOB_SETTLE_SPAN,
            _ => SYNC_JOB_ACTIVE_SPAN,
        };
        let expected_update = seconds_of(creation_date_time + due_after);
        assert!(
            read.member("updateDateTime")
                .as_f64()
                .is_some_and(|reported| (reported - expected_update).abs() < 0.001),
            "case {case:?}: expected updateDateTime {expected_update}, got {read:?}"
        );
    }
}

/// RULE: the only sync source this service supports is the one its model
/// documents, so a create naming any other is refused with HTTP 400
/// `ValidationException` naming the member and the supported value, and persists
/// no job and no link.
#[tokio::test]
async fn a_sync_source_the_service_does_not_support_is_refused() {
    const WORKSPACE: &str = "unsupportedSourceWorkspace";
    let emulator = emulator_with_workspaces(&[WORKSPACE]).await;
    let role = json!({"syncRole": "arn:aws:iam::000000000000:role/TwinMakerSyncRole"});

    // Every row satisfies the modelled `SyncSource` pattern
    // (`^[a-zA-Z_0-9]+$`), so what refuses them is the documented support
    // ("Currently the only supported syncSource is SITEWISE") and not a
    // constraint the model could enforce: a different case of the supported
    // value, the service's own name, an unrelated service, and a value the
    // supported one is a prefix of.
    for case in ["sitewise", "SiteWise", "IOTSITEWISE", "KINESIS", "SITEWISE2"] {
        let refused = emulator.create_sync_job(WORKSPACE, case, role.clone()).await;
        assert_eq!(
            (refused.status, refused.error_type.as_deref()),
            (400, Some("ValidationException")),
            "case {case:?}: an unsupported sync source must be refused, got {refused:?}"
        );
        assert!(
            refused.member("message").as_str().is_some_and(|message| {
                message.contains(case) && message.contains(SYNC_SOURCE_SITEWISE)
            }),
            "case {case:?}: the refusal must name the refused source and the supported one, \
             got {refused:?}"
        );
        let read = emulator.get_sync_job(Some(WORKSPACE), case).await;
        assert_eq!(
            (read.status, read.error_type.as_deref()),
            (404, Some("ResourceNotFoundException")),
            "case {case:?}: a refused create must persist nothing, got {read:?}"
        );
        let workspace = emulator.get_workspace(WORKSPACE).await;
        assert!(
            workspace.member("linkedServices").is_null(),
            "case {case:?}: a refused create must link nothing into the workspace, \
             got {workspace:?}"
        );
    }

    // The supported value, on the same workspace, is accepted — so what the
    // rows above were refused for is the source and nothing else.
    let created = emulator
        .create_sync_job(WORKSPACE, SYNC_SOURCE_SITEWISE, role)
        .await;
    assert_eq!(
        created.status, 200,
        "case {SYNC_SOURCE_SITEWISE:?}: the supported sync source must be accepted, \
         got {created:?}"
    );
}

/// RULE: a sync job belongs to the workspace it was created in and is addressed
/// by that workspace and its sync source, so a read that names another
/// workspace, a workspace this service does not hold, or no workspace at all
/// reports no job; and a workspace that still holds one is not deletable.
#[tokio::test]
async fn a_sync_job_belongs_to_its_workspace() {
    const WORKSPACE: &str = "ownerWorkspace";
    const OTHER_WORKSPACE: &str = "otherWorkspace";
    let emulator = emulator_with_workspaces(&[WORKSPACE, OTHER_WORKSPACE]).await;
    let role = json!({"syncRole": "arn:aws:iam::000000000000:role/TwinMakerSyncRole"});

    assert_eq!(
        emulator
            .create_sync_job(WORKSPACE, SYNC_SOURCE_SITEWISE, role.clone())
            .await
            .status,
        200,
        "the sync job the cases read must exist"
    );

    // The rows differ in WHICH workspace the read names: the one that holds the
    // job, a workspace that holds none, a workspace that does not exist, and no
    // workspace at all.
    let rows: Vec<(&str, Option<&str>, u16, &str)> = vec![
        ("the owning workspace", Some(WORKSPACE), 200, ""),
        (
            "a workspace holding no such job",
            Some(OTHER_WORKSPACE),
            404,
            "ResourceNotFoundException",
        ),
        (
            "a workspace the service does not hold",
            Some("absentWorkspace"),
            404,
            "ResourceNotFoundException",
        ),
        ("no workspace at all", None, 400, "ValidationException"),
    ];
    for (case, workspace_id, status, error_type) in rows {
        let read = emulator.get_sync_job(workspace_id, SYNC_SOURCE_SITEWISE).await;
        assert_eq!(
            (read.status, read.error_type.as_deref().unwrap_or("")),
            (status, error_type),
            "case {case:?}: expected {status} {error_type:?}, got {read:?}"
        );
    }

    // A create naming a workspace this service does not hold is refused: this
    // operation models no `ResourceNotFoundException`, so the refusal is the
    // `ValidationException` it does model.
    let orphan = emulator
        .create_sync_job("absentWorkspace", SYNC_SOURCE_SITEWISE, role.clone())
        .await;
    assert_eq!(
        (orphan.status, orphan.error_type.as_deref()),
        (400, Some("ValidationException")),
        "case \"create in an absent workspace\": expected the modelled refusal, got {orphan:?}"
    );

    // The source is the job's identity inside its workspace, so a second create
    // for it conflicts — and changes nothing a later caller sees.
    let before = emulator
        .get_sync_job(Some(WORKSPACE), SYNC_SOURCE_SITEWISE)
        .await;
    let conflict = emulator
        .create_sync_job(
            WORKSPACE,
            SYNC_SOURCE_SITEWISE,
            json!({"syncRole": "arn:aws:iam::000000000000:role/ADifferentRole"}),
        )
        .await;
    assert_eq!(
        (conflict.status, conflict.error_type.as_deref()),
        (409, Some("ConflictException")),
        "case \"re-create a held sync source\": expected the modelled conflict, got {conflict:?}"
    );
    let after = emulator
        .get_sync_job(Some(WORKSPACE), SYNC_SOURCE_SITEWISE)
        .await;
    assert_eq!(
        (
            after.member("syncRole").as_str(),
            after.member("creationDateTime").as_f64(),
        ),
        (
            before.member("syncRole").as_str(),
            before.member("creationDateTime").as_f64(),
        ),
        "case \"re-create a held sync source\": the refused create must change nothing, \
         got {after:?}"
    );

    // Nothing a workspace contains may outlive it, so a workspace still
    // holding a sync job is refused the delete and keeps serving the job.
    let refused = emulator.delete_workspace(WORKSPACE).await;
    assert_eq!(
        (refused.status, refused.error_type.as_deref()),
        (400, Some("ValidationException")),
        "case \"delete a workspace holding a sync job\": expected the modelled refusal, \
         got {refused:?}"
    );
    assert_eq!(
        emulator
            .get_sync_job(Some(WORKSPACE), SYNC_SOURCE_SITEWISE)
            .await
            .status,
        200,
        "case \"delete a workspace holding a sync job\": the refused delete keeps the job"
    );
}

/// RULE: `GetSyncJob` REPORTS — a read, and a refused read, change nothing a
/// later caller sees: once the outcome the clock owed has been materialised, the
/// job reads back byte-for-byte identically however often it is read, and a read
/// that finds no job materialises none.
///
/// The rows differ along WHAT the refused lookup misses — the workspace holds no
/// job for the source, the workspace is not held at all, and the request names
/// no workspace — because the rule is that none of them writes anything, whatever
/// the refusal is.
#[tokio::test]
async fn reading_a_sync_job_changes_nothing_a_later_caller_sees() {
    const WORKSPACE: &str = "pureReadWorkspace";
    const EMPTY_WORKSPACE: &str = "jobFreeWorkspace";
    let emulator = emulator_with_workspaces(&[WORKSPACE, EMPTY_WORKSPACE]).await;
    assert_eq!(
        emulator
            .create_sync_job(
                WORKSPACE,
                SYNC_SOURCE_SITEWISE,
                json!({"syncRole": "arn:aws:iam::000000000000:role/TwinMakerSyncRole"}),
            )
            .await
            .status,
        200,
        "the sync job the reads report must exist"
    );
    // Past the states the clock owes, so the repeated reads are of a job the
    // clock has already brought up and cannot move again: what remains is the
    // read itself.
    let first = sync_job_in_state(&emulator, WORKSPACE, SYNC_SOURCE_SITEWISE, SYNC_JOB_STATE_ACTIVE)
        .await;
    assert_eq!(
        first.status, 200,
        "the settled job must still be readable, got {first:?}"
    );
    for attempt in 1..=3 {
        let again = emulator
            .get_sync_job(Some(WORKSPACE), SYNC_SOURCE_SITEWISE)
            .await;
        assert_eq!(
            again.body, first.body,
            "read {attempt}: a read must report the same job it reported before, got {again:?}"
        );
    }

    let rows: Vec<(&str, Option<&str>)> = vec![
        ("a workspace holding no such job", Some(EMPTY_WORKSPACE)),
        ("a workspace the service does not hold", Some("absentWorkspace")),
        ("no workspace at all", None),
    ];
    for (case, workspace_id) in &rows {
        let missed = emulator
            .get_sync_job(*workspace_id, SYNC_SOURCE_SITEWISE)
            .await;
        assert!(
            missed.status >= 400,
            "case {case:?}: a read that finds no job must be refused, got {missed:?}"
        );
        assert_eq!(
            emulator
                .state()
                .lock()
                .region(REGION)
                .workspaces
                .values()
                .map(|workspace| workspace.sync_jobs.len())
                .sum::<usize>(),
            1,
            "case {case:?}: a refused read must materialise no sync job"
        );
    }

    let after = emulator
        .get_sync_job(Some(WORKSPACE), SYNC_SOURCE_SITEWISE)
        .await;
    assert_eq!(
        after.body, first.body,
        "the refused reads must leave the stored job exactly as it was, got {after:?}"
    );
}

/// RULE: the services a workspace reports as `linkedServices` are the sources of
/// the sync jobs it holds — omitted while it holds none — and every operation
/// that reports a workspace reports the same list.
#[tokio::test]
async fn a_workspace_reports_the_sources_of_the_sync_jobs_it_holds() {
    const LINKED: &str = "linkedWorkspace";
    const UNLINKED: &str = "unlinkedWorkspace";
    let emulator = emulator_with_workspaces(&[LINKED, UNLINKED]).await;

    // The rows differ in whether the workspace holds a sync job at all, which
    // is what the rule generalises over.
    for case in [LINKED, UNLINKED] {
        let before = emulator.get_workspace(case).await;
        assert!(
            before.member("linkedServices").is_null(),
            "case {case:?}: linkedServices must be omitted while nothing is linked, \
             got {before:?}"
        );
    }

    assert_eq!(
        emulator
            .create_sync_job(
                LINKED,
                SYNC_SOURCE_SITEWISE,
                json!({"syncRole": "arn:aws:iam::000000000000:role/TwinMakerSyncRole"}),
            )
            .await
            .status,
        200,
        "the sync job that links a service into the workspace must be created"
    );

    let expected = json!([SYNC_SOURCE_SITEWISE]);
    let read = emulator.get_workspace(LINKED).await;
    assert_eq!(
        read.member("linkedServices"),
        &expected,
        "case {LINKED:?}: GetWorkspace must report the sources of the jobs it holds, got {read:?}"
    );

    // The same workspace, reported by the listing: one workspace must read the
    // same whichever operation reports it.
    let listed = emulator.list_workspaces(json!({})).await;
    let summaries = listed
        .member("workspaceSummaries")
        .as_array()
        .cloned()
        .unwrap_or_default();
    for summary in &summaries {
        let workspace_id = summary["workspaceId"].as_str().unwrap_or_default();
        let linked = &summary["linkedServices"];
        if workspace_id == LINKED {
            assert_eq!(
                linked, &expected,
                "case {LINKED:?}: the listing must report the same linked services, \
                 got {summary:?}"
            );
        } else {
            assert!(
                linked.is_null(),
                "case {workspace_id:?}: a workspace holding no sync job links nothing, \
                 got {summary:?}"
            );
        }
    }
    assert_eq!(
        summaries.len(),
        2,
        "both workspaces must be listed, got {listed:?}"
    );
}
