// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the EBS snapshot STORAGE TIER slice: `ModifySnapshotTier` and
//! `DescribeSnapshotTierStatus`, driven over the wire the service actually speaks — `ec2Query`,
//! through the crate's shared [`super::ec2_query`] harness.
//!
//! The rules under test are:
//!
//! 1. A STORAGE TIER STATUS DESCRIBES AN ARCHIVE OR RESTORE ACTION, so a snapshot nothing has
//!    archived has no row at all — whatever that snapshot is, however it was created and
//!    whether or not the account holds others;
//! 2. ARCHIVING A SNAPSHOT RECORDS THAT ACTION, and the clock — not a caller — carries it from
//!    `archival-in-progress` to `archival-completed`, from which the snapshot is in the
//!    `archive` tier; every member of the reported row is read off the snapshot or off the
//!    action, and `DescribeSnapshots` reports the SAME tier for the same snapshot;
//! 3. THE READ REPORTS AND CHANGES NOTHING: two reads one after the other of a settled action
//!    answer the same row;
//! 4. THE DOCUMENTED FILTERS NARROW THE ROWS and an undocumented one is refused, whether or not
//!    any row would have matched;
//! 5. THE ROWS PAGE in `SnapshotId` order, with a token exactly while another row follows;
//! 6. A TIER CHANGE THE SERVICE CANNOT SERVE IS REFUSED WITH EC2'S OWN CODE and NOTHING is
//!    recorded — an id of no form EC2 issues, a well-formed id the account holds no snapshot
//!    for, a request that names no tier, a snapshot that does not yet hold all of its data, a
//!    snapshot already archived, and a `DryRun` probe;
//! 7. A SNAPSHOT THAT LEAVES THE INVENTORY TAKES ITS TIER STATUS WITH IT.
//!
//! Each table row is a distinct input along what the rule generalises over — the volume's size,
//! its encryption state, whether the snapshot is described and tagged, how many snapshots the
//! account holds, which filter and value, which page size, and which refusal — so no row is the
//! specification.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use ec2::state::{
    DRY_RUN_OPERATION, INCORRECT_STATE, INVALID_PARAMETER_VALUE, INVALID_SNAPSHOT_ID_MALFORMED,
    INVALID_SNAPSHOT_ID_NOT_FOUND, MISSING_PARAMETER, SNAPSHOT_STATE_COMPLETED,
    SNAPSHOT_STORAGE_TIER_ARCHIVE, SNAPSHOT_STORAGE_TIER_STANDARD,
    TIERING_OPERATION_ARCHIVAL_COMPLETED, TIERING_OPERATION_ARCHIVAL_IN_PROGRESS,
    TIERING_PROGRESS_COMPLETED, TIERING_PROGRESS_IN_PROGRESS,
};

use super::ec2_query::{as_timestamp, element, error_code, filters, member};
use super::volume_lifecycle::{REGION, p, zone_name};

/// The tier the model lets a request ask for: "you must specify `archive`"
/// (`ModifySnapshotTierRequest$StorageTier`), the only member of the `TargetStorageTier` catalog.
const TARGET_STORAGE_TIER: &str = SNAPSHOT_STORAGE_TIER_ARCHIVE;

/// How long a poll below waits for a move the clock owes before it gives up. Generous: what is
/// under test is the state the move leaves behind, not how fast the host runs it, so the
/// deadline only turns a hang into a failure.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// One look inside that deadline, so the deadline costs many looks rather than one.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// One `action` call signed for the Region the suite works in.
async fn op(router: &MultiServiceRouter, action: &str, params: &[(String, String)]) -> (u16, String) {
    super::ec2_query::call(router, REGION, action, params).await
}

/// One row of the table: the volume a snapshot is taken of, and what the request says about the
/// snapshot.
struct SnapshotCase {
    case: &'static str,
    size: i32,
    encrypted: bool,
    description: Option<&'static str>,
    tags: &'static [(&'static str, &'static str)],
}

/// The class the rules cover: a snapshot of any volume of the account, described and tagged
/// however the request chose.
fn snapshot_cases() -> Vec<SnapshotCase> {
    vec![
        SnapshotCase {
            case: "a one GiB plain volume, no description and no tags",
            size: 1,
            encrypted: false,
            description: None,
            tags: &[],
        },
        SnapshotCase {
            case: "an eight GiB encrypted volume, described and tagged",
            size: 8,
            encrypted: true,
            description: Some("quarterly archive"),
            tags: &[("Name", "quarterly"), ("owner", "data")],
        },
        SnapshotCase {
            case: "a 100 GiB plain volume, described and tagged with one tag",
            size: 100,
            encrypted: false,
            description: Some("cold copy"),
            tags: &[("retention", "cold")],
        },
    ]
}

/// Create one volume of `size` GiB in the Region's first zone and answer its id.
async fn create_volume(router: &MultiServiceRouter, case: &str, size: i32, encrypted: bool) -> String {
    let mut params = vec![p("AvailabilityZone", zone_name(0)), p("Size", size.to_string())];
    if encrypted {
        params.push(p("Encrypted", "true"));
    }
    let (status, body) = op(router, "CreateVolume", &params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateVolume must succeed, got {status} with body {body}"
    );
    element(&body, "volumeId")
        .unwrap_or_else(|| panic!("case {case:?}: no volumeId in {body}"))
}

/// Take one snapshot of `volume_id` and answer its id, once the clock has carried it to
/// `completed` — the state a snapshot has to be in before it can be archived.
async fn completed_snapshot(
    router: &MultiServiceRouter,
    case: &str,
    volume_id: &str,
    description: Option<&str>,
    tags: &[(&str, &str)],
) -> String {
    let mut params = vec![p("VolumeId", volume_id)];
    if let Some(description) = description {
        params.push(p("Description", description));
    }
    params.extend(super::ec2_query::tag_specification("snapshot", tags));
    let (status, body) = op(router, "CreateSnapshot", &params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateSnapshot must succeed, got {status} with body {body}"
    );
    let snapshot_id = element(&body, "snapshotId")
        .unwrap_or_else(|| panic!("case {case:?}: no snapshotId in {body}"));
    poll_until_snapshot_completed(router, case, &snapshot_id).await;
    snapshot_id
}

/// Look at the snapshot AGAIN until `DescribeSnapshots` reports it `completed`, or a generous
/// deadline passes — the condition is waited for, never a span.
async fn poll_until_snapshot_completed(router: &MultiServiceRouter, case: &str, snapshot_id: &str) {
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let state = described_snapshot_member(router, case, snapshot_id, "status").await;
        if state.as_deref() == Some(SNAPSHOT_STATE_COMPLETED) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: {snapshot_id} must reach {SNAPSHOT_STATE_COMPLETED}, last saw \
             {state:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The member `name` of the one snapshot `DescribeSnapshots` reports for `snapshot_id`.
async fn described_snapshot_member(
    router: &MultiServiceRouter,
    case: &str,
    snapshot_id: &str,
    name: &str,
) -> Option<String> {
    let (status, body) = op(router, "DescribeSnapshots", &[p("SnapshotId.1", snapshot_id)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeSnapshots must report {snapshot_id}, got {status} with body {body}"
    );
    let parsed = super::wire::document(case, &body);
    let item = super::wire::set_items(parsed.root_element(), "snapshotSet")
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: no snapshot reported for {snapshot_id}: {body}"));
    super::wire::child_text(item, name)
}

/// Ask for `snapshot_id` to be archived.
async fn archive(router: &MultiServiceRouter, snapshot_id: &str) -> (u16, String) {
    op(
        router,
        "ModifySnapshotTier",
        &[p("SnapshotId", snapshot_id), p("StorageTier", TARGET_STORAGE_TIER)],
    )
    .await
}

/// One reported tier status row, as the members it carries.
type TierStatusRow = BTreeMap<String, String>;

/// One request's form parameters, as the ec2Query harness takes them.
type Params = Vec<(String, String)>;

/// One row of the filter table: `(case, the filter parameters, the ids it must select)`.
type FilterRow = (&'static str, Params, Vec<String>);

/// One row of the refusal table: `(case, the parameters, the HTTP status, the error code)`.
type RefusalRow = (&'static str, Params, u16, &'static str);

/// The rows `DescribeSnapshotTierStatus` answers for `params`, in the order it reported them,
/// beside the body itself.
async fn tier_statuses(
    router: &MultiServiceRouter,
    case: &str,
    params: &[(String, String)],
) -> (Vec<TierStatusRow>, String) {
    let (status, body) = op(router, "DescribeSnapshotTierStatus", params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeSnapshotTierStatus must succeed, got {status} with body {body}"
    );
    let rows = {
        let parsed = super::wire::document(case, &body);
        super::wire::set_items(parsed.root_element(), "snapshotTierStatusSet")
            .into_iter()
            .map(|item| {
                item.children()
                    .filter(|node| node.is_element())
                    .map(|node| {
                        (
                            node.tag_name().name().to_string(),
                            node.text().unwrap_or_default().to_string(),
                        )
                    })
                    .collect::<TierStatusRow>()
            })
            .collect::<Vec<_>>()
    };
    (rows, body)
}

/// The ids the rows of `params` report, in the order they were reported.
async fn reported_ids(
    router: &MultiServiceRouter,
    case: &str,
    params: &[(String, String)],
) -> Vec<String> {
    let (rows, _body) = tier_statuses(router, case, params).await;
    rows.iter()
        .map(|row| row.get("snapshotId").cloned().unwrap_or_default())
        .collect()
}

/// Look again until the tier status of `snapshot_id` reports `wanted` as its last tiering
/// operation, or a generous deadline passes. Answers the row it then held.
async fn poll_until_tiering_status(
    router: &MultiServiceRouter,
    case: &str,
    snapshot_id: &str,
    wanted: &str,
) -> TierStatusRow {
    let narrowed = filters(&[("snapshot-id", &[snapshot_id])]);
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let (rows, body) = tier_statuses(router, case, &narrowed).await;
        let row = rows.into_iter().next();
        if row
            .as_ref()
            .and_then(|row| row.get("lastTieringOperationStatus"))
            .map(String::as_str)
            == Some(wanted)
        {
            return row.unwrap_or_default();
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the tiering operation of {snapshot_id} must reach {wanted:?}, last \
             saw body {body}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

#[tokio::test]
async fn a_snapshot_nothing_archived_has_no_tier_status() {
    for row in snapshot_cases() {
        let case = row.case;
        let (router, _engine) = super::ec2_query::emulator();
        let volume_id = create_volume(&router, case, row.size, row.encrypted).await;
        let snapshot_id =
            completed_snapshot(&router, case, &volume_id, row.description, row.tags).await;

        // Every narrowing this operation serves answers the same thing for a snapshot with no
        // archive or restore action: there is no action to describe.
        for (narrowing, params) in [
            ("the whole account", Vec::new()),
            (
                "narrowed to the snapshot itself",
                filters(&[("snapshot-id", &[snapshot_id.as_str()])]),
            ),
            (
                "narrowed to its source volume",
                filters(&[("volume-id", &[volume_id.as_str()])]),
            ),
        ] {
            let (rows, body) = tier_statuses(&router, case, &params).await;
            assert!(
                rows.is_empty(),
                "case {case:?} ({narrowing}): a snapshot nothing archived must have no tier \
                 status, got {rows:?} in {body}"
            );
            assert_eq!(
                member(&body, "nextToken"),
                None,
                "case {case:?} ({narrowing}): an empty report has no further page, got {body}"
            );
        }

        // The snapshot itself is still reported by the inventory read, in the tier it was
        // created in: an absent tier STATUS is not an absent snapshot.
        let tier = described_snapshot_member(&router, case, &snapshot_id, "storageTier").await;
        assert_eq!(
            tier.as_deref(),
            Some(SNAPSHOT_STORAGE_TIER_STANDARD),
            "case {case:?}: the snapshot must still be reported in the standard tier, got {tier:?}"
        );
    }
}

#[tokio::test]
async fn archiving_a_snapshot_reports_the_action_the_clock_completes() {
    for row in snapshot_cases() {
        let case = row.case;
        let (router, _engine) = super::ec2_query::emulator();
        let volume_id = create_volume(&router, case, row.size, row.encrypted).await;
        let snapshot_id =
            completed_snapshot(&router, case, &volume_id, row.description, row.tags).await;

        let (status, body) = archive(&router, &snapshot_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: archiving {snapshot_id} must succeed, got {status} with body {body}"
        );
        assert_eq!(
            element(&body, "snapshotId").as_deref(),
            Some(snapshot_id.as_str()),
            "case {case:?}: the archive must answer the snapshot it was asked for, got {body}"
        );
        let tiering_start_time = element(&body, "tieringStartTime")
            .unwrap_or_else(|| panic!("case {case:?}: no tieringStartTime in {body}"));
        assert!(
            as_timestamp(&tiering_start_time).is_some(),
            "case {case:?}: the tiering start time must be a timestamp, got \
             {tiering_start_time:?}"
        );

        // The action as it stands the moment the call answers: still running, nothing
        // transferred, and the snapshot still in the tier it was in — the clock has not carried
        // it anywhere yet.
        let (rows, body) = tier_statuses(&router, case, &[]).await;
        assert_eq!(
            rows.len(),
            1,
            "case {case:?}: exactly the archived snapshot must have a tier status, got {body}"
        );
        let running = rows.first().cloned().unwrap_or_default();
        let reported = |row: &TierStatusRow, name: &str| row.get(name).cloned();
        for (name, expected) in [
            ("snapshotId", Some(snapshot_id.clone())),
            ("volumeId", Some(volume_id.clone())),
            ("status", Some(SNAPSHOT_STATE_COMPLETED.to_string())),
            ("ownerId", Some(engine::emulator_account_id().to_string())),
            ("lastTieringStartTime", Some(tiering_start_time.clone())),
            (
                "lastTieringProgress",
                Some(TIERING_PROGRESS_IN_PROGRESS.to_string()),
            ),
            (
                "lastTieringOperationStatus",
                Some(TIERING_OPERATION_ARCHIVAL_IN_PROGRESS.to_string()),
            ),
            (
                "storageTier",
                Some(SNAPSHOT_STORAGE_TIER_STANDARD.to_string()),
            ),
        ] {
            assert_eq!(
                reported(&running, name),
                expected,
                "case {case:?}: the running archival's {name} must be {expected:?}, got row \
                 {running:?} in {body}"
            );
        }
        assert_eq!(
            reported(&running, "archivalCompleteTime"),
            None,
            "case {case:?}: a running archival has no completion instant, got row {running:?}"
        );

        // The move to `archival-completed` is the CLOCK's: once it has fallen due the action
        // reports all of the data moved, a completion instant, and the snapshot in the archive
        // tier.
        let completed =
            poll_until_tiering_status(&router, case, &snapshot_id, TIERING_OPERATION_ARCHIVAL_COMPLETED)
                .await;
        for (name, expected) in [
            (
                "lastTieringProgress",
                Some(TIERING_PROGRESS_COMPLETED.to_string()),
            ),
            (
                "storageTier",
                Some(SNAPSHOT_STORAGE_TIER_ARCHIVE.to_string()),
            ),
            ("lastTieringStartTime", Some(tiering_start_time.clone())),
        ] {
            assert_eq!(
                reported(&completed, name),
                expected,
                "case {case:?}: the completed archival's {name} must be {expected:?}, got row \
                 {completed:?}"
            );
        }
        let archival_complete_time = reported(&completed, "archivalCompleteTime")
            .unwrap_or_else(|| panic!("case {case:?}: no archivalCompleteTime in {completed:?}"));
        let started = as_timestamp(&tiering_start_time)
            .unwrap_or_else(|| panic!("case {case:?}: {tiering_start_time:?} is no timestamp"));
        let finished = as_timestamp(&archival_complete_time)
            .unwrap_or_else(|| panic!("case {case:?}: {archival_complete_time:?} is no timestamp"));
        assert!(
            finished >= started,
            "case {case:?}: the archival cannot have completed before it started, got \
             {archival_complete_time:?} against {tiering_start_time:?}"
        );

        // The tags the snapshot carries are the row's tags, so the two reads of one snapshot
        // report one set.
        let reported_tags: Vec<String> = row
            .tags
            .iter()
            .map(|(key, _value)| (*key).to_string())
            .collect();
        let (_rows, tagged_body) =
            tier_statuses(&router, case, &filters(&[("snapshot-id", &[snapshot_id.as_str()])])).await;
        for key in reported_tags {
            assert!(
                tagged_body.contains(&key),
                "case {case:?}: the row must carry the snapshot's tag {key:?}, got \
                 {tagged_body}"
            );
        }

        // The inventory read agrees about the tier: one snapshot is in one tier, whichever
        // operation reports it.
        let described = described_snapshot_member(&router, case, &snapshot_id, "storageTier").await;
        assert_eq!(
            described.as_deref(),
            Some(SNAPSHOT_STORAGE_TIER_ARCHIVE),
            "case {case:?}: DescribeSnapshots must report the archived tier too, got {described:?}"
        );

        // A READ CHANGES NOTHING: the same call made again answers the same settled row.
        let (again, again_body) = tier_statuses(&router, case, &[]).await;
        assert_eq!(
            again.first().cloned().unwrap_or_default(),
            completed,
            "case {case:?}: a second read must report the same row, got {again_body}"
        );
    }
}

#[tokio::test]
async fn the_documented_filters_narrow_the_reported_actions() {
    let case = "two completed snapshots of two volumes, one of them archived";
    let (router, _engine) = super::ec2_query::emulator();
    let archived_volume = create_volume(&router, case, 1, false).await;
    let other_volume = create_volume(&router, case, 2, false).await;
    let archived = completed_snapshot(&router, case, &archived_volume, None, &[]).await;
    let other = completed_snapshot(&router, case, &other_volume, None, &[]).await;
    let (status, body) = archive(&router, &archived).await;
    assert_eq!(
        status, 200,
        "case {case:?}: archiving {archived} must succeed, got {status} with body {body}"
    );
    poll_until_tiering_status(&router, case, &archived, TIERING_OPERATION_ARCHIVAL_COMPLETED).await;

    // One row per filter row, along what the rule generalises over: which attribute, a value
    // that matches, a value of the right shape that matches nothing, and several values of one
    // filter (which are OR'd inside it).
    let rows: Vec<FilterRow> = vec![
        (
            "the archived snapshot by id",
            filters(&[("snapshot-id", &[archived.as_str()])]),
            vec![archived.clone()],
        ),
        (
            "the snapshot nothing archived, by id",
            filters(&[("snapshot-id", &[other.as_str()])]),
            Vec::new(),
        ),
        (
            "both snapshot ids, one of which has an action",
            filters(&[("snapshot-id", &[archived.as_str(), other.as_str()])]),
            vec![archived.clone()],
        ),
        (
            "the source volume of the archived snapshot",
            filters(&[("volume-id", &[archived_volume.as_str()])]),
            vec![archived.clone()],
        ),
        (
            "the source volume of the other snapshot",
            filters(&[("volume-id", &[other_volume.as_str()])]),
            Vec::new(),
        ),
        (
            "the completed archival, by the state of the last action",
            filters(&[(
                "last-tiering-operation",
                &[TIERING_OPERATION_ARCHIVAL_COMPLETED],
            )]),
            vec![archived.clone()],
        ),
        (
            "a state no action of this account is in",
            filters(&[(
                "last-tiering-operation",
                &[TIERING_OPERATION_ARCHIVAL_IN_PROGRESS],
            )]),
            Vec::new(),
        ),
        (
            "an id filter and a volume filter that disagree, which are AND'd",
            filters(&[
                ("snapshot-id", &[archived.as_str()]),
                ("volume-id", &[other_volume.as_str()]),
            ]),
            Vec::new(),
        ),
    ];
    for (row, params, expected) in rows {
        let reported = reported_ids(&router, row, &params).await;
        assert_eq!(
            reported, expected,
            "case {row:?}: expected the rows {expected:?}, got {reported:?}"
        );
    }

    // A filter this operation does not document is refused whether or not any row would have
    // matched it, so an unsupported narrowing can never read as "no results".
    for undocumented in ["storage-tier", "tag:Name", "snapshot_id", "Snapshot-Id"] {
        let params = filters(&[(undocumented, &["anything"])]);
        let (status, body) = op(&router, "DescribeSnapshotTierStatus", &params).await;
        assert_eq!(
            status, 400,
            "case {undocumented:?}: an undocumented filter must be refused, got {status} with \
             body {body}"
        );
        assert_eq!(
            error_code(&body),
            INVALID_PARAMETER_VALUE,
            "case {undocumented:?}: expected {INVALID_PARAMETER_VALUE}, got {body}"
        );
    }
}

#[tokio::test]
async fn the_reported_actions_page_in_snapshot_id_order() {
    let case = "three archived snapshots";
    let (router, _engine) = super::ec2_query::emulator();
    let volume_id = create_volume(&router, case, 1, false).await;
    let mut archived = Vec::new();
    for index in 0..3 {
        let snapshot_id = completed_snapshot(&router, case, &volume_id, None, &[]).await;
        let (status, body) = archive(&router, &snapshot_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: archiving snapshot {index} must succeed, got {status} with body {body}"
        );
        archived.push(snapshot_id);
    }
    archived.sort();

    // The whole report first: every action, in `SnapshotId` order, with no further page.
    let (rows, body) = tier_statuses(&router, case, &[]).await;
    let all: Vec<String> = rows
        .iter()
        .map(|row| row.get("snapshotId").cloned().unwrap_or_default())
        .collect();
    assert_eq!(
        all, archived,
        "case {case:?}: the report must carry every action in id order, got {body}"
    );
    assert_eq!(
        member(&body, "nextToken"),
        None,
        "case {case:?}: an untruncated report has no further page, got {body}"
    );

    // Then one page size per row of the table, each walked to the end: a token exactly while
    // another row follows, and every action reported once.
    for page_size in [1_usize, 2, 3, 4] {
        let mut walked: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        let mut pages = 0;
        loop {
            let mut params = vec![p("MaxResults", page_size.to_string())];
            if let Some(token) = token.as_deref() {
                params.push(p("NextToken", token));
            }
            let (rows, body) = tier_statuses(&router, case, &params).await;
            pages += 1;
            assert!(
                rows.len() <= page_size,
                "case page size {page_size}: a page cannot be longer than the size asked for, \
                 got {} rows in {body}",
                rows.len()
            );
            walked.extend(
                rows.iter()
                    .map(|row| row.get("snapshotId").cloned().unwrap_or_default()),
            );
            token = member(&body, "nextToken").filter(|token| !token.is_empty());
            let followed = walked.len() < archived.len();
            assert_eq!(
                token.is_some(),
                followed,
                "case page size {page_size}: a token must be reported exactly while another \
                 action follows ({} of {} walked), got {body}",
                walked.len(),
                archived.len()
            );
            if token.is_none() {
                break;
            }
            assert!(
                pages <= archived.len() + 1,
                "case page size {page_size}: the walk must end, got {pages} pages for {} actions",
                archived.len()
            );
        }
        assert_eq!(
            walked, archived,
            "case page size {page_size}: the walk must report every action once, in id order"
        );
    }

    // A page of NONE is not a page.
    for refused in ["0", "-1"] {
        let (status, body) = op(
            &router,
            "DescribeSnapshotTierStatus",
            &[p("MaxResults", refused)],
        )
        .await;
        assert_eq!(
            status, 400,
            "case MaxResults {refused:?}: expected a refusal, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            INVALID_PARAMETER_VALUE,
            "case MaxResults {refused:?}: expected {INVALID_PARAMETER_VALUE}, got {body}"
        );
    }

    // So is a token that names no position this report left.
    let (status, body) = op(
        &router,
        "DescribeSnapshotTierStatus",
        &[p("NextToken", "not-a-token")],
    )
    .await;
    assert_eq!(
        status, 400,
        "case a bogus page token: expected a refusal, got {status} with body {body}"
    );
}

#[tokio::test]
async fn a_tier_change_the_service_cannot_serve_is_refused() {
    let case = "one completed snapshot of the account";
    let (router, engine) = super::ec2_query::emulator();
    let volume_id = create_volume(&router, case, 1, false).await;
    let completed = completed_snapshot(&router, case, &volume_id, None, &[]).await;

    // A snapshot that is still being written: seeded with a completion the clock owes far
    // enough out that the case tests the state rather than the host's speed.
    let pending = pending_snapshot(&engine, &volume_id);

    // One row per cause, along what the rule generalises over: the form of the id, what the
    // request left out, and the state of the snapshot it names.
    let rows: Vec<RefusalRow> = vec![
        (
            "a value of no id form EC2 issues",
            vec![p("SnapshotId", "snap-bad-id"), p("StorageTier", TARGET_STORAGE_TIER)],
            400,
            INVALID_SNAPSHOT_ID_MALFORMED,
        ),
        (
            "the all-zero placeholder id",
            vec![
                p("SnapshotId", "snap-00000000000000000"),
                p("StorageTier", TARGET_STORAGE_TIER),
            ],
            400,
            INVALID_SNAPSHOT_ID_MALFORMED,
        ),
        (
            "a well-formed id the account holds no snapshot for",
            vec![
                p("SnapshotId", "snap-0123456789abcdef0"),
                p("StorageTier", TARGET_STORAGE_TIER),
            ],
            400,
            INVALID_SNAPSHOT_ID_NOT_FOUND,
        ),
        (
            "a request that names no tier at all",
            vec![p("SnapshotId", completed.clone())],
            400,
            MISSING_PARAMETER,
        ),
        (
            "a snapshot that does not hold all of its volume's data yet",
            vec![p("SnapshotId", pending.clone()), p("StorageTier", TARGET_STORAGE_TIER)],
            400,
            INCORRECT_STATE,
        ),
        (
            "a probe that asks whether the call would have worked",
            vec![
                p("SnapshotId", completed.clone()),
                p("StorageTier", TARGET_STORAGE_TIER),
                p("DryRun", "true"),
            ],
            412,
            DRY_RUN_OPERATION,
        ),
    ];
    for (row, params, expected_status, expected_code) in rows {
        let (status, body) = op(&router, "ModifySnapshotTier", &params).await;
        assert_eq!(
            status, expected_status,
            "case {row:?}: expected HTTP {expected_status}, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            expected_code,
            "case {row:?}: expected {expected_code}, got {body}"
        );
        // NOTHING was recorded: a refused tier change leaves the account with no action to
        // describe.
        let reported = reported_ids(&router, row, &[]).await;
        assert!(
            reported.is_empty(),
            "case {row:?}: a refused tier change must record no action, got {reported:?}"
        );
    }

    // A snapshot already archived is in a state a second archive cannot be served in either —
    // and the first archival is left exactly as it was.
    let (status, body) = archive(&router, &completed).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the first archive must succeed, got {status} with body {body}"
    );
    let first = element(&body, "tieringStartTime")
        .unwrap_or_else(|| panic!("case {case:?}: no tieringStartTime in {body}"));
    let (status, body) = archive(&router, &completed).await;
    assert_eq!(
        status, 400,
        "case a second archive of one snapshot: expected a refusal, got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body),
        INCORRECT_STATE,
        "case a second archive of one snapshot: expected {INCORRECT_STATE}, got {body}"
    );
    let (rows, body) = tier_statuses(&router, case, &[]).await;
    assert_eq!(
        rows.len(),
        1,
        "case a second archive of one snapshot: the account must still hold one action, got {body}"
    );
    assert_eq!(
        rows.first().and_then(|row| row.get("lastTieringStartTime")).cloned(),
        Some(first.clone()),
        "case a second archive of one snapshot: the first archival must be left as it was, got \
         {body}"
    );
}

#[tokio::test]
async fn a_deleted_snapshot_has_no_tier_status() {
    let case = "an archived snapshot that is then deleted";
    let (router, _engine) = super::ec2_query::emulator();
    let volume_id = create_volume(&router, case, 1, false).await;
    let snapshot_id = completed_snapshot(&router, case, &volume_id, None, &[]).await;
    let (status, body) = archive(&router, &snapshot_id).await;
    assert_eq!(
        status, 200,
        "case {case:?}: archiving {snapshot_id} must succeed, got {status} with body {body}"
    );
    poll_until_tiering_status(&router, case, &snapshot_id, TIERING_OPERATION_ARCHIVAL_COMPLETED).await;

    let (status, body) = op(&router, "DeleteSnapshot", &[p("SnapshotId", &snapshot_id)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: deleting {snapshot_id} must succeed, got {status} with body {body}"
    );

    let reported = reported_ids(&router, case, &[]).await;
    assert!(
        reported.is_empty(),
        "case {case:?}: a snapshot that has left the inventory has no tier status, got \
         {reported:?}"
    );
}

/// One snapshot of `volume_id` that is still being written: stored directly, with the
/// `completed` move the clock owes it far enough out that a call made next certainly sees it
/// `pending`.
///
/// Seeded through the store rather than by timing a create, so the case tests the STATE a tier
/// change is refused in and never how fast the host runs.
fn pending_snapshot(engine: &Arc<engine::InMemoryEngine>, volume_id: &str) -> String {
    let state = ec2::state::service_state(&**engine).expect("the EC2 store");
    let mut store = state.lock();
    let view = ec2::state::SnapshotView {
        volume_id: Some(volume_id.to_string()),
        volume_size: Some(1),
        owner_id: Some(engine::emulator_account_id().to_string()),
        encrypted: Some(false),
        storage_tier: Some(SNAPSHOT_STORAGE_TIER_STANDARD.to_string()),
        ..Default::default()
    };
    store.insert_snapshot_completing_at(
        view,
        std::time::SystemTime::now(),
        Instant::now() + POLL_DEADLINE,
    )
}
