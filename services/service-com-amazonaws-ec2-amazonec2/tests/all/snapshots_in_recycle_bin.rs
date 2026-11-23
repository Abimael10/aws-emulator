// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `RestoreSnapshotFromRecycleBin`, driven over the wire the service
//! actually speaks — `ec2Query`, through the crate's shared [`super::ec2_query`] harness.
//!
//! The rules under test are:
//!
//! 1. A RESTORE GIVES BACK THE SNAPSHOT THE RECYCLE BIN HELD, not one that resembles it:
//!    every member the bin kept — the id, the owner, the description, the volume and the size
//!    it was taken of, its encryption, its tags and the state the clock had reached — reads
//!    back through `DescribeSnapshots` exactly as it read before the snapshot was recycled,
//!    and the call itself answers that snapshot's own members;
//! 2. a restore is a MOVE between the two stores, so a snapshot is in exactly one of them:
//!    while it is in the bin `DescribeSnapshots` refuses its id, and once it is restored a
//!    second restore of the same id is refused;
//! 3. a snapshot id the bin holds no entry for is refused with `InvalidSnapshotID.Malformed`
//!    (HTTP 400) whatever the reason — the recorded service answers that one code even for a
//!    well-formed id, so this operation does not distinguish "no such entry" from "not a
//!    snapshot id" — and nothing is written;
//! 4. `DryRun` answers `DryRunOperation` and performs nothing: the snapshot is still in the
//!    bin, with the retention period it had;
//! 5. the end of a retention period is the CLOCK's: once the exit instant has passed the
//!    snapshot is permanently deleted, whether or not anybody called, so it can no longer be
//!    restored and the bytes it was charged go back to the store;
//! 6. WHAT PUTS A SNAPSHOT IN THE BIN is a delete a Recycle Bin retention rule covers:
//!    "resources […] that have one or more of the specified tag key and value pairs are
//!    automatically retained in the Recycle Bin upon deletion", and a Region-level rule
//!    "retains all deleted resources of the specified resource type in the Region in which
//!    the rule is created, even if the resources are not tagged"
//!    (`rbin`'s `CreateRuleRequest$ResourceTags`) — so a covered `DeleteSnapshot` retains the
//!    snapshot for that rule's retention period, and a delete no rule covers is permanent;
//! 7. THE BIN'S OWN REPORT answers the entries it holds in `SnapshotId` order, narrowed to
//!    the ids the request names — an id it holds no entry for being a refusal rather than an
//!    absent row — and cut into `MaxResults`-sized pages whose `NextToken` continues from the
//!    end of the last one and is honoured only if this service issued it.
//!
//! Each table row is a distinct input along what the rules generalise over — the size of the
//! volume the snapshot was taken of, whether that volume is encrypted, whether the snapshot
//! carries a description and tags, the retention period the bin keeps it for, which way an id
//! can fail to name an entry, and which kind of rule covers (or does not cover) the delete —
//! so no row is the specification.
//!
//! The suites that are about the RESTORE alone retain a snapshot the way a covered delete
//! does, by calling the state primitive the bin is written through
//! ([`Ec2State::move_snapshot_to_recycle_bin`]), so each of them is one call away from the
//! case it is about; RULE 6's own suite drives the whole path instead, creating the retention
//! rule through the `rbin` service that OWNS it, mounted on the same engine — exactly as the
//! assembled emulator mounts them.

use std::time::{Duration, Instant, SystemTime};

use service_com_amazonaws_ec2_amazonec2 as ec2;

use ec2::state::{
    ComAmazonawsEc2Amazonec2State as Ec2State, INVALID_SNAPSHOT_ID_MALFORMED,
    INVALID_SNAPSHOT_ID_NOT_FOUND, MAX_RESOURCE_IDS, SNAPSHOT_PROGRESS_COMPLETED,
    SNAPSHOT_RECYCLE_BIN_PAGE_SIZE_RANGE, SNAPSHOT_STATE_COMPLETED,
};
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, elements, error_code, error_message, indexed, unescaped};
use super::rbin_retention_rules::{
    OTHER_REGION, RESOURCE_TYPE_SNAPSHOT, RESOURCE_TYPE_VOLUME, RetentionRule,
    emulator_with_retention_rules, poll_until,
};
use super::volume_lifecycle::{REGION, p, zone_name};

/// The HTTP status EC2 answers `DryRunOperation` with: the request was refused because a
/// precondition the CALLER set holds, not because it was malformed.
const DRY_RUN_STATUS: u16 = 412;

/// A retention period long enough that nothing in this suite reaches its end. Stands for the
/// `RetentionPeriod` of the Recycle Bin rule that retained the snapshot, which is the `rbin`
/// service's value rather than one this crate owns.
pub(super) const LONG_RETENTION: Duration = Duration::from_secs(3_600);

/// One row of the restore table: the case, the size of the volume the snapshot is taken of,
/// whether that volume is encrypted, and the description and tags the snapshot carries.
pub(super) struct SnapshotRow {
    pub(super) case: &'static str,
    pub(super) volume_size: i32,
    pub(super) encrypted: bool,
    pub(super) description: Option<&'static str>,
    pub(super) tags: &'static [(&'static str, &'static str)],
}

/// One call of `action` signed for the Region the snapshot suites use.
async fn invoke(
    router: &MultiServiceRouter,
    action: &str,
    params: &[(String, String)],
) -> (u16, String) {
    call(router, REGION, action, params).await
}

/// Mint one volume of `size` GiB, encrypted or not, and answer its id — the volume the row's
/// snapshot is taken of.
async fn create_volume(
    router: &MultiServiceRouter,
    case: &str,
    size: i32,
    encrypted: bool,
) -> String {
    let mut params = vec![
        p("AvailabilityZone", zone_name(0)),
        p("Size", size.to_string()),
    ];
    if encrypted {
        params.push(p("Encrypted", "true"));
    }
    let (status, body) = invoke(router, "CreateVolume", &params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateVolume, got {status} with body {body}"
    );
    element(&body, "volumeId")
        .unwrap_or_else(|| panic!("case {case:?}: no volumeId in {body}"))
}

/// Take one snapshot of a freshly minted volume, as the row describes it, and answer the
/// snapshot's id and the id of the volume it was taken of.
pub(super) async fn create_snapshot(
    router: &MultiServiceRouter,
    row: &SnapshotRow,
) -> (String, String) {
    let case = row.case;
    let volume_id = create_volume(router, case, row.volume_size, row.encrypted).await;
    let mut params = vec![p("VolumeId", &volume_id)];
    if let Some(description) = row.description {
        params.push(p("Description", description));
    }
    if !row.tags.is_empty() {
        params.push(p("TagSpecification.1.ResourceType", "snapshot"));
        for (index, (key, value)) in row.tags.iter().enumerate() {
            let at = index + 1;
            params.push(p(&format!("TagSpecification.1.Tag.{at}.Key"), *key));
            params.push(p(&format!("TagSpecification.1.Tag.{at}.Value"), *value));
        }
    }
    let (status, body) = invoke(router, "CreateSnapshot", &params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateSnapshot, got {status} with body {body}"
    );
    let snapshot_id = element(&body, "snapshotId")
        .unwrap_or_else(|| panic!("case {case:?}: no snapshotId in {body}"));
    assert!(
        snapshot_id.starts_with("snap-"),
        "case {case:?}: expected the id EC2 mints for a snapshot, got {snapshot_id:?}"
    );
    (snapshot_id, volume_id)
}

/// `DescribeSnapshots` for one snapshot id: the HTTP status and the body.
pub(super) async fn describe(router: &MultiServiceRouter, snapshot_id: &str) -> (u16, String) {
    invoke(router, "DescribeSnapshots", &indexed("SnapshotId", &[snapshot_id])).await
}

/// The whole `<snapshotSet>` `DescribeSnapshots` reports for `snapshot_id` — every member of
/// the snapshot as the service renders it, so comparing two of these compares the WHOLE
/// snapshot rather than the handful of members an assertion remembered to name.
async fn described_snapshot(router: &MultiServiceRouter, case: &str, snapshot_id: &str) -> String {
    let (status, body) = describe(router, snapshot_id).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeSnapshots, got {status} with body {body}"
    );
    element(&body, "snapshotSet").unwrap_or_else(|| {
        panic!("case {case:?}: expected DescribeSnapshots to report a snapshotSet, got {body}")
    })
}

/// Wait until the clock has moved `snapshot_id` to `completed`, by looking at what a read of
/// it reports rather than by sleeping out the service's span.
async fn wait_until_completed(router: &MultiServiceRouter, case: &str, snapshot_id: &str) {
    poll_until(case, "the snapshot to be completed", || async {
        let (status, body) = describe(router, snapshot_id).await;
        status == 200 && element(&body, "status").as_deref() == Some(SNAPSHOT_STATE_COMPLETED)
    })
    .await;
}

/// Retain `snapshot_id` in the Recycle Bin for `retention`, the way a `DeleteSnapshot`
/// covered by a retention rule will — through the state primitive the bin is written through.
pub(super) fn recycle(
    engine: &engine::InMemoryEngine,
    case: &str,
    snapshot_id: &str,
    retention: Duration,
) {
    let store = engine.state::<Ec2State>();
    let mut guard = store.lock();
    assert!(
        guard
            .move_snapshot_to_recycle_bin(snapshot_id, SystemTime::now(), retention)
            .is_some(),
        "case {case:?}: expected {snapshot_id} to be a stored snapshot the bin could take"
    );
}

/// Whether the Recycle Bin holds `snapshot_id` right now.
fn bin_holds(engine: &engine::InMemoryEngine, snapshot_id: &str) -> bool {
    engine
        .state::<Ec2State>()
        .lock()
        .snapshots_in_recycle_bin
        .contains_key(snapshot_id)
}

/// How much of `snapshot_id`'s retention period is left, or `None` when the bin does not hold
/// it at all.
fn retention_left(engine: &engine::InMemoryEngine, snapshot_id: &str) -> Option<Duration> {
    let now = Instant::now();
    engine
        .state::<Ec2State>()
        .lock()
        .snapshots_in_recycle_bin
        .get(snapshot_id)
        .map(|record| record.retention_left(now))
}

/// The caller-chosen bytes the store is holding right now.
fn stored_bytes(engine: &engine::InMemoryEngine) -> usize {
    engine.state::<Ec2State>().lock().stored_bytes()
}

/// A restore of `snapshot_id`, with whatever extra members the case needs.
pub(super) async fn restore(
    router: &MultiServiceRouter,
    snapshot_id: &str,
    extra: &[(String, String)],
) -> (u16, String) {
    let mut params = vec![p("SnapshotId", snapshot_id)];
    params.extend(extra.iter().cloned());
    invoke(router, "RestoreSnapshotFromRecycleBin", &params).await
}

/// Four snapshots that differ along everything the restore rule generalises over: the size of
/// the volume they were taken of, that volume's encryption state, and whether the snapshot
/// carries a description, tags, or both.
fn snapshot_rows() -> Vec<SnapshotRow> {
    vec![
        SnapshotRow {
            case: "a snapshot of the smallest volume, with no description and no tags",
            volume_size: 1,
            encrypted: false,
            description: None,
            tags: &[],
        },
        SnapshotRow {
            case: "a described snapshot of an eight GiB volume",
            volume_size: 8,
            encrypted: false,
            description: Some("the nightly backup"),
            tags: &[],
        },
        SnapshotRow {
            case: "a tagged snapshot of an encrypted volume",
            volume_size: 4,
            encrypted: true,
            description: None,
            tags: &[("Name", "retained"), ("Team", "A")],
        },
        SnapshotRow {
            case: "a described and tagged snapshot of a 1024 GiB volume",
            volume_size: 1_024,
            encrypted: true,
            description: Some("the quarterly archive"),
            tags: &[("Name", "archive")],
        },
    ]
}

/// RULES 1 and 2, over the whole table: a snapshot the Recycle Bin holds is refused by
/// `DescribeSnapshots`, a restore answers that snapshot's own members and gives back the SAME
/// snapshot — byte for byte the `snapshotSet` the describe reported before it was recycled —
/// and a second restore of the same id is refused because the bin no longer holds it.
#[tokio::test]
async fn restoring_a_recycled_snapshot_gives_back_the_snapshot_the_bin_held() {
    let (router, engine) = super::ec2_query::emulator();

    for row in snapshot_rows() {
        let case = row.case;
        let (snapshot_id, volume_id) = create_snapshot(&router, &row).await;
        // The `pending` -> `completed` move is the CLOCK's, so the suite waits for the state
        // it reports rather than for a span: the reading taken here and the one taken after
        // the restore are then of a settled snapshot, and the comparison is not a comparison
        // of two clocks.
        wait_until_completed(&router, case, &snapshot_id).await;
        let before = described_snapshot(&router, case, &snapshot_id).await;

        recycle(&engine, case, &snapshot_id, LONG_RETENTION);
        assert!(
            bin_holds(&engine, &snapshot_id),
            "case {case:?}: expected the Recycle Bin to hold {snapshot_id}"
        );
        // RULE 2: the two stores partition the account's snapshots, so while the bin holds it
        // the describe cannot see it — the same refusal as for a snapshot that never existed.
        let (status, body) = describe(&router, &snapshot_id).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 from DescribeSnapshots for a recycled snapshot, \
             got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            INVALID_SNAPSHOT_ID_NOT_FOUND,
            "case {case:?}: expected {INVALID_SNAPSHOT_ID_NOT_FOUND}, got {body}"
        );

        let (status, body) = restore(&router, &snapshot_id, &[]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from RestoreSnapshotFromRecycleBin, got {status} \
             with body {body}"
        );

        // RULE 1, the call's own answer: the members it reports are the restored snapshot's,
        // read off the record the bin gave back.
        for (member, expected) in [
            ("snapshotId", snapshot_id.clone()),
            ("volumeId", volume_id.clone()),
            ("volumeSize", row.volume_size.to_string()),
            ("ownerId", engine::emulator_account_id().to_string()),
            ("encrypted", row.encrypted.to_string()),
            ("status", SNAPSHOT_STATE_COMPLETED.to_string()),
            ("progress", SNAPSHOT_PROGRESS_COMPLETED.to_string()),
            (
                "description",
                row.description.unwrap_or_default().to_string(),
            ),
        ] {
            let actual = element(&body, member).unwrap_or_default();
            assert_eq!(
                actual, expected,
                "case {case:?}: expected the restore to report {member} {expected:?}, got \
                 {actual:?} in {body}"
            );
        }
        assert!(
            element(&body, "startTime").is_some(),
            "case {case:?}: expected the restore to report the snapshot's start time, got {body}"
        );

        // RULE 1: the snapshot is back, and it is the one the bin held.
        let after = described_snapshot(&router, case, &snapshot_id).await;
        assert_eq!(
            after, before,
            "case {case:?}: expected the restored snapshot to read back exactly as it did \
             before it was recycled, got {after} instead of {before}"
        );
        assert!(
            !bin_holds(&engine, &snapshot_id),
            "case {case:?}: expected the Recycle Bin to have given {snapshot_id} up"
        );

        // RULE 2: the bin holds it no more, so restoring it again is a refusal rather than a
        // second success.
        let (status, body) = restore(&router, &snapshot_id, &[]).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 from a second restore, got {status} / {body}"
        );
        assert_eq!(
            error_code(&body),
            INVALID_SNAPSHOT_ID_MALFORMED,
            "case {case:?}: expected {INVALID_SNAPSHOT_ID_MALFORMED}, got {body}"
        );
    }
}

/// RULE 3: every way a `SnapshotId` can fail to name an entry of the bin — a value that is
/// not a snapshot id at all, in each of the ways an id can be misshapen; a well-formed id of
/// either published form naming nothing; the all-zero placeholder; and a snapshot the account
/// still holds — is refused with the one code this operation answers for all of them, and
/// nothing is written.
#[tokio::test]
async fn a_snapshot_id_the_recycle_bin_holds_no_entry_for_is_refused_as_malformed() {
    let (router, engine) = super::ec2_query::emulator();

    // A snapshot the account still has: it is not in the bin, so it is not restorable.
    let live_row = SnapshotRow {
        case: "a snapshot the account still holds",
        volume_size: 1,
        encrypted: false,
        description: None,
        tags: &[],
    };
    let (live, _) = create_snapshot(&router, &live_row).await;
    // A snapshot the bin holds, so the table also covers the one id that WOULD be restored.
    let retained_row = SnapshotRow {
        case: "a snapshot the bin holds",
        volume_size: 1,
        encrypted: false,
        description: None,
        tags: &[],
    };
    let (retained, _) = create_snapshot(&router, &retained_row).await;
    recycle(&engine, retained_row.case, &retained, LONG_RETENTION);

    // A snapshot id is `snap-` followed by 8 or 17 lowercase hexadecimal digits, of which not
    // all are `0` (`crate::state::snapshot_id_is_well_formed`), so each row below either is
    // not one or names no entry of the bin.
    let rows: Vec<(&str, String)> = vec![
        ("a suffix that is not hexadecimal", "snap-notasnap".to_string()),
        ("a suffix of neither length", "snap-0123456789".to_string()),
        ("one digit past the long form", format!("snap-{}", "0".repeat(18))),
        ("the long form in upper case", "snap-0123456789ABCDEF0".to_string()),
        ("the prefix with no digits", "snap-".to_string()),
        ("an id of another resource kind", format!("vol-{}", "1".repeat(17))),
        ("no prefix at all", "0123456789abcdef0".to_string()),
        // The all-zero id of a published length: well formed structurally, yet no mint can
        // ever issue it.
        ("the all-zero placeholder", format!("snap-{}", "0".repeat(17))),
        // Well formed, both published id forms, naming nothing the bin holds. The recorded
        // service answers the SAME code for these as for a misshapen id: the operation
        // resolves against the bin and reports one failure for every id it cannot resolve
        // there.
        ("a well-formed long id naming nothing", "snap-0123456789abcdef0".to_string()),
        ("a well-formed short id naming nothing", "snap-000000ff".to_string()),
        ("a snapshot the account still holds", live.clone()),
    ];

    for (case, snapshot_id) in rows {
        let (status, body) = restore(&router, &snapshot_id, &[]).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            INVALID_SNAPSHOT_ID_MALFORMED,
            "case {case:?}: expected {INVALID_SNAPSHOT_ID_MALFORMED}, got {body}"
        );
        // The message is built from the request: it names the value the caller sent, so the
        // request can be fixed from the message alone.
        let message = unescaped(&element(&body, "Message").unwrap_or_default());
        assert_eq!(
            message,
            format!("The snapshot ID '{snapshot_id}' is malformed"),
            "case {case:?}: expected a message naming {snapshot_id:?}, got {message:?}"
        );
        // Nothing was written: the live snapshot is still the account's, and the retained one
        // is still in the bin.
        assert!(
            bin_holds(&engine, &retained),
            "case {case:?}: a refused restore must leave the bin as it was"
        );
        let (status, _) = describe(&router, &live).await;
        assert_eq!(
            status, 200,
            "case {case:?}: a refused restore must leave the account's snapshots alone"
        );
    }
}

/// RULE 4: an authorized dry run answers `DryRunOperation` and performs nothing — the
/// snapshot is still in the bin with the retention period it had, and a real call after it
/// still restores.
#[tokio::test]
async fn a_dry_run_leaves_the_snapshot_in_the_recycle_bin() {
    let (router, engine) = super::ec2_query::emulator();

    let row = SnapshotRow {
        case: "a dry run of a restorable snapshot",
        volume_size: 2,
        encrypted: false,
        description: Some("the one the dry run leaves alone"),
        tags: &[],
    };
    let case = row.case;
    let (snapshot_id, _) = create_snapshot(&router, &row).await;
    recycle(&engine, case, &snapshot_id, LONG_RETENTION);
    let before = retention_left(&engine, &snapshot_id)
        .unwrap_or_else(|| panic!("case {case:?}: expected the bin to hold {snapshot_id}"));

    for attempt in 1..=2 {
        let (status, body) = restore(&router, &snapshot_id, &[p("DryRun", "true")]).await;
        assert_eq!(
            status, DRY_RUN_STATUS,
            "attempt {attempt}: expected HTTP {DRY_RUN_STATUS}, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            "DryRunOperation",
            "attempt {attempt}: expected DryRunOperation, got {body}"
        );
        let left = retention_left(&engine, &snapshot_id).unwrap_or_else(|| {
            panic!("attempt {attempt}: expected the bin to still hold {snapshot_id}")
        });
        // The retention period only ever runs DOWN, and only the clock runs it: a dry run
        // neither restores the snapshot nor gives it a new period.
        assert!(
            left <= before,
            "attempt {attempt}: expected the retention period to run down with the clock \
             only, got {left:?} against {before:?}"
        );
        let (status, body) = describe(&router, &snapshot_id).await;
        assert_eq!(
            status, 400,
            "attempt {attempt}: a dry run must not give the snapshot back, got {status} / \
             {body}"
        );
    }

    // The dry runs changed nothing, so the real call still has a snapshot to restore.
    let (status, body) = restore(&router, &snapshot_id, &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 after the dry runs, got {status} with body {body}"
    );
    let (status, body) = describe(&router, &snapshot_id).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the restored snapshot to be the account's again, got {status} \
         with body {body}"
    );
}

/// RULE 5: the end of a retention period belongs to the clock. A snapshot whose exit instant
/// has passed is permanently deleted without anybody calling, so it cannot be restored, it is
/// not one of the account's snapshots either, and the bytes it was charged are back.
///
/// The rows differ along the retention period the bin keeps the snapshot for — the rule's
/// value, which nothing but the clock ends.
#[tokio::test]
async fn a_retention_period_that_has_run_out_deletes_the_snapshot_permanently() {
    let (router, engine) = super::ec2_query::emulator();

    for (case, retention) in [
        (
            "a retention period of a tenth of a second",
            Duration::from_millis(100),
        ),
        (
            "a retention period of a quarter of a second",
            Duration::from_millis(250),
        ),
    ] {
        let row = SnapshotRow {
            case,
            volume_size: 3,
            encrypted: false,
            description: Some("the one whose retention runs out"),
            tags: &[("Name", "expiring")],
        };
        let (snapshot_id, _) = create_snapshot(&router, &row).await;
        // The figure the snapshot's own bytes are measured against: what the store holds with
        // the volume created and the snapshot taken, minus what the snapshot is charged for.
        // Read AFTER the create, so the volume's own charge is not part of the comparison.
        let charged = stored_bytes(&engine);
        recycle(&engine, case, &snapshot_id, retention);
        assert!(
            retention_left(&engine, &snapshot_id).is_some(),
            "case {case:?}: expected the bin to hold {snapshot_id} once it was retained"
        );

        // A read of the account's snapshots is the first caller after the exit instant, so it
        // is what materialises the deletion the clock owed: the entry is gone from the bin
        // without any caller having asked for its removal. The suite looks again until the
        // retention period has run out rather than sleeping it out.
        poll_until(case, "the retention period to run out", || async {
            retention_left(&engine, &snapshot_id).is_none_or(|left| left.is_zero())
        })
        .await;

        let (status, body) = restore(&router, &snapshot_id, &[]).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 for a snapshot whose retention period ended, got \
             {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            INVALID_SNAPSHOT_ID_MALFORMED,
            "case {case:?}: expected {INVALID_SNAPSHOT_ID_MALFORMED}, got {body}"
        );
        assert!(
            !bin_holds(&engine, &snapshot_id),
            "case {case:?}: expected the bin to have given {snapshot_id} up at its exit instant"
        );
        let (status, body) = describe(&router, &snapshot_id).await;
        assert_eq!(
            status, 400,
            "case {case:?}: a permanently deleted snapshot is not the account's either, got \
             {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            INVALID_SNAPSHOT_ID_NOT_FOUND,
            "case {case:?}: expected {INVALID_SNAPSHOT_ID_NOT_FOUND}, got {body}"
        );
        // The snapshot's own bytes — its description and its tags — are back with the store,
        // so the budget is the sum of what it actually holds.
        assert!(
            stored_bytes(&engine) < charged,
            "case {case:?}: expected the permanently deleted snapshot's bytes to go back to \
             the store's budget, still holding {} against {charged}",
            stored_bytes(&engine)
        );
    }
}

/// A retention period whose end is not a representable instant REFUSES the move instead of
/// unwinding part way through it: the snapshot is still the account's, the bin holds nothing,
/// and the byte budget is unchanged — every fallible step of the move is taken before its
/// first mutation.
///
/// Driven through the state primitive, because no retention rule carries a period onto this
/// path: a rule's period is bounded by `RetentionPeriod$RetentionPeriodValue`'s own
/// `@range(min: 1, max: 3650)`, so a delete can never ask for one this long.
#[tokio::test]
async fn a_retention_period_with_no_representable_end_refuses_the_move() {
    let (router, engine) = super::ec2_query::emulator();

    let row = SnapshotRow {
        case: "a retention period with no representable end",
        volume_size: 1,
        encrypted: false,
        description: Some("the one that stays put"),
        tags: &[],
    };
    let case = row.case;
    let (snapshot_id, _) = create_snapshot(&router, &row).await;
    let charged = stored_bytes(&engine);

    // Two periods past the end of both clocks, so neither row is the boundary itself.
    for retention in [Duration::MAX, Duration::MAX - Duration::from_secs(1)] {
        let store = engine.state::<Ec2State>();
        let mut guard = store.lock();
        assert!(
            guard
                .move_snapshot_to_recycle_bin(&snapshot_id, SystemTime::now(), retention)
                .is_none(),
            "case {case:?}: expected a retention of {retention:?} to refuse the move"
        );
    }

    assert!(
        !bin_holds(&engine, &snapshot_id),
        "case {case:?}: a refused move must store no entry, yet the bin holds {snapshot_id}"
    );
    let (status, body) = describe(&router, &snapshot_id).await;
    assert_eq!(
        status, 200,
        "case {case:?}: a refused move must leave the snapshot the account's, got {status} \
         with body {body}"
    );
    let reported = elements(&body, "snapshotId");
    assert_eq!(
        reported,
        vec![snapshot_id.clone()],
        "case {case:?}: expected the account's snapshots to be unchanged, got {reported:?}"
    );
    assert_eq!(
        stored_bytes(&engine),
        charged,
        "case {case:?}: a refused move must leave the byte budget as it was"
    );
}

// =============================================================================
// RULE 6: what a Recycle Bin retention rule does to a delete
// =============================================================================

/// One row of the retention table: the rule the account holds, the Region it was created in,
/// the tags the deleted snapshot carries, and whether that rule retains the delete.
struct RuleRow {
    case: &'static str,
    resource_type: &'static str,
    retention_days: i64,
    /// `CreateRuleRequest$ResourceTags` — a rule naming none is a Region-level rule.
    resource_tags: &'static [(&'static str, &'static str)],
    /// `CreateRuleRequest$ExcludeResourceTags` — Region-level rules only.
    exclude_resource_tags: &'static [(&'static str, &'static str)],
    rule_region: &'static str,
    snapshot_tags: &'static [(&'static str, &'static str)],
    retained: bool,
}

/// Create the row's retention rule through the service that OWNS retention rules, wait until
/// it is in the one state that retains anything, and answer its identifier — both through the
/// cross-service harness every suite about a retention rule shares
/// ([`super::rbin_retention_rules`]).
async fn create_available_rule(router: &MultiServiceRouter, row: &RuleRow) -> String {
    super::rbin_retention_rules::create_available_rule(
        router,
        row.case,
        &RetentionRule {
            resource_type: row.resource_type,
            retention_days: row.retention_days,
            resource_tags: row.resource_tags,
            exclude_resource_tags: row.exclude_resource_tags,
            region: row.rule_region,
        },
    )
    .await
}

/// `ListSnapshotsInRecycleBin` with `params`: the HTTP status and the body.
pub(super) async fn list_bin(
    router: &MultiServiceRouter,
    params: &[(String, String)],
) -> (u16, String) {
    invoke(router, "ListSnapshotsInRecycleBin", params).await
}

/// One timestamp the bin reports, as the instant it names.
fn reported_instant(case: &str, member: &str, body: &str) -> chrono::DateTime<chrono::FixedOffset> {
    let text = element(body, member)
        .unwrap_or_else(|| panic!("case {case:?}: no {member} in {body}"));
    chrono::DateTime::parse_from_rfc3339(&text).unwrap_or_else(|error| {
        panic!("case {case:?}: {member} {text:?} is not a timestamp ({error})")
    })
}

/// RULE 6: a `DeleteSnapshot` a retention rule covers RETAINS the snapshot — it leaves the
/// account's snapshots, the bin reports it with the two instants its retention period spans,
/// and a restore gives it back — while a delete no rule covers is permanent.
///
/// The rows differ along the kind of rule the account holds: a Region-level rule, a tag-level
/// rule the snapshot matches, a tag-level rule it does not, a Region-level rule whose
/// exclusion tag the snapshot does not carry, one whose exclusion tag it does, a rule for
/// another resource type, and a rule another Region holds — so no row is the rule.
#[tokio::test]
async fn a_delete_a_retention_rule_covers_retains_the_snapshot_in_the_recycle_bin() {
    let rows: Vec<RuleRow> = vec![
        RuleRow {
            case: "a Region-level rule for EBS snapshots",
            resource_type: RESOURCE_TYPE_SNAPSHOT,
            retention_days: 7,
            resource_tags: &[],
            exclude_resource_tags: &[],
            rule_region: REGION,
            snapshot_tags: &[],
            retained: true,
        },
        RuleRow {
            case: "a tag-level rule one of the snapshot's tags matches",
            resource_type: RESOURCE_TYPE_SNAPSHOT,
            retention_days: 1,
            resource_tags: &[("Name", "keep")],
            exclude_resource_tags: &[],
            rule_region: REGION,
            snapshot_tags: &[("Team", "A"), ("Name", "keep")],
            retained: true,
        },
        RuleRow {
            case: "a tag-level rule none of the snapshot's tags match",
            resource_type: RESOURCE_TYPE_SNAPSHOT,
            retention_days: 7,
            resource_tags: &[("Name", "keep")],
            exclude_resource_tags: &[],
            rule_region: REGION,
            snapshot_tags: &[("Name", "something-else")],
            retained: false,
        },
        RuleRow {
            case: "a Region-level rule whose exclusion tag the snapshot does not carry",
            resource_type: RESOURCE_TYPE_SNAPSHOT,
            // The longest period this resource type supports
            // (`rbin::state::MAX_RETENTION_DAYS_SNAPSHOT_OR_IMAGE`).
            retention_days: 365,
            resource_tags: &[],
            exclude_resource_tags: &[("Temporary", "true")],
            rule_region: REGION,
            snapshot_tags: &[("Name", "keep")],
            retained: true,
        },
        RuleRow {
            case: "a Region-level rule whose exclusion tag the snapshot carries",
            resource_type: RESOURCE_TYPE_SNAPSHOT,
            retention_days: 365,
            resource_tags: &[],
            exclude_resource_tags: &[("Temporary", "true")],
            rule_region: REGION,
            snapshot_tags: &[("Temporary", "true")],
            retained: false,
        },
        RuleRow {
            case: "a rule for another resource type",
            resource_type: RESOURCE_TYPE_VOLUME,
            retention_days: 7,
            resource_tags: &[],
            exclude_resource_tags: &[],
            rule_region: REGION,
            snapshot_tags: &[],
            retained: false,
        },
        RuleRow {
            case: "a rule another Region holds",
            resource_type: RESOURCE_TYPE_SNAPSHOT,
            retention_days: 7,
            resource_tags: &[],
            exclude_resource_tags: &[],
            rule_region: OTHER_REGION,
            snapshot_tags: &[],
            retained: false,
        },
    ];

    for row in rows {
        let case = row.case;
        // One emulator per row, so the rules of one row decide only that row's delete.
        let (router, engine) = emulator_with_retention_rules();
        create_available_rule(&router, &row).await;

        let snapshot_row = SnapshotRow {
            case,
            volume_size: 1,
            encrypted: false,
            description: Some("the one the retention rule decides"),
            tags: row.snapshot_tags,
        };
        let (snapshot_id, volume_id) = create_snapshot(&router, &snapshot_row).await;
        let before = described_snapshot(&router, case, &snapshot_id).await;

        let (status, body) = invoke(&router, "DeleteSnapshot", &[p("SnapshotId", &snapshot_id)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 deleting {snapshot_id}, got {status} with body \
             {body}"
        );

        // Either way the snapshot has left the account's snapshots: a retained snapshot has
        // been deleted, and the bin is where it waits.
        let (status, body) = describe(&router, &snapshot_id).await;
        assert_eq!(
            status, 400,
            "case {case:?}: a deleted snapshot is not the account's, got {status} / {body}"
        );
        assert_eq!(
            error_code(&body),
            INVALID_SNAPSHOT_ID_NOT_FOUND,
            "case {case:?}: expected {INVALID_SNAPSHOT_ID_NOT_FOUND}, got {body}"
        );
        assert_eq!(
            error_message(&body),
            format!("The snapshot '{snapshot_id}' does not exist."),
            "case {case:?}: expected the not-found wording, got {body}"
        );

        if !row.retained {
            // No rule covers the delete, so it was permanent: the bin holds nothing and the
            // restore has nothing to give back.
            let (status, listed) = list_bin(&router, &[]).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200 from ListSnapshotsInRecycleBin, got {status} \
                 with body {listed}"
            );
            assert!(
                elements(&listed, "snapshotId").is_empty(),
                "case {case:?}: expected an empty Recycle Bin, got {listed}"
            );
            assert!(
                !bin_holds(&engine, &snapshot_id),
                "case {case:?}: expected the delete of {snapshot_id} to be permanent"
            );
            let (status, body) = restore(&router, &snapshot_id, &[]).await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected a permanently deleted snapshot to be unrestorable, \
                 got {status} with body {body}"
            );
            assert_eq!(
                error_code(&body),
                INVALID_SNAPSHOT_ID_MALFORMED,
                "case {case:?}: expected {INVALID_SNAPSHOT_ID_MALFORMED}, got {body}"
            );
            continue;
        }

        // The rule covers the delete, so the snapshot is in the bin — reported by its id,
        // with the description and source volume it carried and the two instants its
        // retention period spans.
        let (status, listed) = list_bin(&router, &indexed("SnapshotId", &[&snapshot_id])).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from ListSnapshotsInRecycleBin, got {status} with \
             body {listed}"
        );
        assert_eq!(
            elements(&listed, "snapshotId"),
            vec![snapshot_id.clone()],
            "case {case:?}: expected the bin to report exactly {snapshot_id}, got {listed}"
        );
        assert_eq!(
            element(&listed, "volumeId").as_deref(),
            Some(volume_id.as_str()),
            "case {case:?}: expected the bin to report the source volume, got {listed}"
        );
        assert_eq!(
            element(&listed, "description").as_deref(),
            snapshot_row.description,
            "case {case:?}: expected the bin to report the snapshot's description, got {listed}"
        );
        // "The date and time when the snapshot is to be permanently deleted from the Recycle
        // Bin" is when it entered plus the retention period of the rule that covered it, so
        // the span between the two instants the bin reports IS that rule's period.
        let entered = reported_instant(case, "recycleBinEnterTime", &listed);
        let exits = reported_instant(case, "recycleBinExitTime", &listed);
        assert_eq!(
            exits - entered,
            chrono::TimeDelta::try_days(row.retention_days).expect("a representable period"),
            "case {case:?}: expected the bin to keep the snapshot for the rule's \
             {} day(s), got {entered} to {exits}",
            row.retention_days
        );

        // And it is restorable until that period runs out: the restore gives back the same
        // snapshot the describe reported before the delete.
        let (status, body) = restore(&router, &snapshot_id, &[]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 restoring the retained {snapshot_id}, got \
             {status} with body {body}"
        );
        let after = described_snapshot(&router, case, &snapshot_id).await;
        assert_eq!(
            after, before,
            "case {case:?}: expected the restored snapshot to read back as it did before the \
             delete, got {after} instead of {before}"
        );
        // The bin gave it up, so it reports nothing any more.
        let (status, listed) = list_bin(&router, &[]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from ListSnapshotsInRecycleBin, got {status} with \
             body {listed}"
        );
        assert!(
            elements(&listed, "snapshotId").is_empty(),
            "case {case:?}: expected the bin to be empty after the restore, got {listed}"
        );
    }
}

// =============================================================================
// RULE 7: the bin's own report
// =============================================================================

/// The smallest page the report accepts, which is the bottom of the modelled
/// `@range(min: 5, max: 1000)` on `ListSnapshotsInRecycleBinMaxResults`
/// (`ec2::state::SNAPSHOT_RECYCLE_BIN_PAGE_SIZE_RANGE`).
const SMALLEST_PAGE: i32 = 5;

/// How many snapshots the paging rows retain, so a page of [`SMALLEST_PAGE`] leaves a short
/// second page behind it rather than dividing the set exactly.
const RETAINED_FOR_PAGING: usize = 7;

/// RULE 7: the report answers the entries the bin holds in `SnapshotId` order, pages them at
/// the size the request asks for with a `NextToken` that continues from the end of the page,
/// and the whole walk is the same set as the unpaged report.
#[tokio::test]
async fn the_recycle_bin_report_pages_the_entries_it_holds_in_id_order() {
    let (router, engine) = super::ec2_query::emulator();

    let case = "seven retained snapshots";
    let mut retained = Vec::new();
    for index in 0..RETAINED_FOR_PAGING {
        let row = SnapshotRow {
            case,
            volume_size: 1,
            encrypted: false,
            description: None,
            tags: &[],
        };
        let (snapshot_id, _) = create_snapshot(&router, &row).await;
        recycle(&engine, case, &snapshot_id, LONG_RETENTION);
        assert!(
            bin_holds(&engine, &snapshot_id),
            "case {case:?}: expected the bin to hold entry {index}"
        );
        retained.push(snapshot_id);
    }
    // The report's order is the store's `SnapshotId` order, which a minted-id sequence need
    // not be in, so the expectation is the sorted set rather than the creation order.
    let mut expected = retained.clone();
    expected.sort();

    // The whole bin, unpaged.
    let (status, listed) = list_bin(&router, &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from the unpaged report, got {status} / {listed}"
    );
    assert_eq!(
        elements(&listed, "snapshotId"),
        expected,
        "case {case:?}: expected the whole bin in id order, got {listed}"
    );
    assert!(
        element(&listed, "nextToken").is_none(),
        "case {case:?}: a report that fits in one page carries no token, got {listed}"
    );

    // The same set, walked a page at a time: each page is at most the size asked for, the
    // token is present exactly while entries follow, and the walk is the unpaged report.
    let mut walked: Vec<String> = Vec::new();
    let mut token: Option<String> = None;
    for page_number in 1..=3 {
        let mut params = vec![p("MaxResults", SMALLEST_PAGE.to_string())];
        if let Some(cursor) = &token {
            params.push(p("NextToken", cursor.as_str()));
        }
        let (status, listed) = list_bin(&router, &params).await;
        assert_eq!(
            status, 200,
            "page {page_number}: expected HTTP 200 from the paged report, got {status} / \
             {listed}"
        );
        let page = elements(&listed, "snapshotId");
        assert!(
            page.len() <= SMALLEST_PAGE as usize,
            "page {page_number}: expected at most {SMALLEST_PAGE} entries, got {page:?}"
        );
        walked.extend(page.clone());
        token = element(&listed, "nextToken");
        let left = expected.len() - walked.len();
        assert_eq!(
            token.is_some(),
            left > 0,
            "page {page_number}: expected a token exactly while entries follow ({left} left), \
             got {listed}"
        );
        if token.is_none() {
            break;
        }
    }
    assert_eq!(
        walked, expected,
        "case {case:?}: expected the paged walk to be the whole bin in id order, got {walked:?}"
    );

    // A `NextToken` this service did not issue is refused rather than read as a position.
    for bogus in ["not-a-token", "", "0123456789abcdef"] {
        let (status, body) = list_bin(&router, &[p("NextToken", bogus)]).await;
        assert_eq!(
            status, 400,
            "token {bogus:?}: expected HTTP 400 from a token this service did not issue, got \
             {status} / {body}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidPaginationToken",
            "token {bogus:?}: expected InvalidPaginationToken, got {body}"
        );
    }
}

/// RULE 7, the request's own members: a `DryRun` reports whether the call would have
/// succeeded and lists nothing, and a `SnapshotIds` entry the bin holds no entry for is a
/// refusal — malformed as malformed, well formed as not found — rather than an absent row.
#[tokio::test]
async fn the_recycle_bin_report_refuses_a_request_it_cannot_serve() {
    let (router, engine) = super::ec2_query::emulator();

    let case = "one retained and one live snapshot";
    let retained_row = SnapshotRow {
        case,
        volume_size: 1,
        encrypted: false,
        description: None,
        tags: &[],
    };
    let (retained, _) = create_snapshot(&router, &retained_row).await;
    recycle(&engine, case, &retained, LONG_RETENTION);
    let live_row = SnapshotRow {
        case,
        volume_size: 1,
        encrypted: false,
        description: None,
        tags: &[],
    };
    let (live, _) = create_snapshot(&router, &live_row).await;

    // "Checks whether you have the required permissions for the action, without actually
    // making the request" (`DryRun`): the probe answers that it would have succeeded and
    // reports no entry at all.
    let (status, body) = list_bin(&router, &[p("DryRun", "true")]).await;
    assert_eq!(
        status, DRY_RUN_STATUS,
        "case {case:?}: expected HTTP {DRY_RUN_STATUS} from a dry-run report, got {status} / \
         {body}"
    );
    assert_eq!(
        error_code(&body),
        "DryRunOperation",
        "case {case:?}: expected DryRunOperation, got {body}"
    );
    assert!(
        elements(&body, "snapshotId").is_empty(),
        "case {case:?}: a dry run reports nothing, got {body}"
    );

    // (case, the id the request names, the code the refusal carries)
    let rows: Vec<(&str, String, &str)> = vec![
        (
            "a value that is not a snapshot id",
            "snap-notasnap".to_string(),
            INVALID_SNAPSHOT_ID_MALFORMED,
        ),
        (
            "the all-zero placeholder",
            format!("snap-{}", "0".repeat(17)),
            INVALID_SNAPSHOT_ID_MALFORMED,
        ),
        (
            "a well-formed long id the bin never held",
            "snap-0123456789abcdef0".to_string(),
            INVALID_SNAPSHOT_ID_NOT_FOUND,
        ),
        (
            "a well-formed short id the bin never held",
            "snap-000000ff".to_string(),
            INVALID_SNAPSHOT_ID_NOT_FOUND,
        ),
        (
            "a snapshot the account still holds",
            live.clone(),
            INVALID_SNAPSHOT_ID_NOT_FOUND,
        ),
    ];
    for (row_case, snapshot_id, expected) in rows {
        // Named alone, and named beside the one id the bin DOES hold: one unresolvable entry
        // refuses the whole request either way.
        for (probe, params) in [
            ("alone", indexed("SnapshotId", &[snapshot_id.as_str()])),
            (
                "beside a retained id",
                indexed("SnapshotId", &[retained.as_str(), snapshot_id.as_str()]),
            ),
        ] {
            let (status, body) = list_bin(&router, &params).await;
            assert_eq!(
                status, 400,
                "case {row_case:?} ({probe}): expected HTTP 400, got {status} / {body}"
            );
            assert_eq!(
                error_code(&body),
                expected,
                "case {row_case:?} ({probe}): expected {expected}, got {body}"
            );
            let message = unescaped(&element(&body, "Message").unwrap_or_default());
            let wanted = if expected == INVALID_SNAPSHOT_ID_MALFORMED {
                format!("Value ({snapshot_id}) for snapshot ID is invalid.")
            } else {
                format!("The snapshot '{snapshot_id}' does not exist.")
            };
            assert_eq!(
                message, wanted,
                "case {row_case:?} ({probe}): expected the refusal wording for {snapshot_id:?}, \
                 got {message:?}"
            );
        }
    }

    // Nothing above wrote anything: the retained snapshot is still the bin's, and the live
    // one still the account's.
    // A page size outside the range the model declares for this report is refused before the
    // handler runs: "Valid range: 5 - 1000"
    // (`ec2::state::SNAPSHOT_RECYCLE_BIN_PAGE_SIZE_RANGE`, the modelled `@range` of
    // `ListSnapshotsInRecycleBinMaxResults`), so each bound is probed one past it.
    for max_results in [
        (SMALLEST_PAGE - 1).to_string(),
        "0".to_string(),
        (SNAPSHOT_RECYCLE_BIN_PAGE_SIZE_RANGE.1 + 1).to_string(),
    ] {
        let (status, body) = list_bin(&router, &[p("MaxResults", max_results.as_str())]).await;
        assert_eq!(
            status, 400,
            "page size {max_results}: expected HTTP 400 for a size outside the published \
             range, got {status} / {body}"
        );
    }
    // And each published bound itself is served.
    for max_results in [
        SMALLEST_PAGE.to_string(),
        SNAPSHOT_RECYCLE_BIN_PAGE_SIZE_RANGE.1.to_string(),
    ] {
        let (status, body) = list_bin(&router, &[p("MaxResults", max_results.as_str())]).await;
        assert_eq!(
            status, 200,
            "page size {max_results}: expected a published bound to be served, got {status} / \
             {body}"
        );
        assert_eq!(
            elements(&body, "snapshotId"),
            vec![retained.clone()],
            "page size {max_results}: expected the one retained snapshot, got {body}"
        );
    }

    // More ids than one request may name is refused as a bad parameter value, before any id
    // is resolved — the same ceiling every id list of this service is held to
    // (`ec2::state::MAX_RESOURCE_IDS`).
    let too_many: Vec<String> = (0..=MAX_RESOURCE_IDS)
        .map(|nth| format!("snap-{nth:08x}"))
        .collect();
    let refs: Vec<&str> = too_many.iter().map(String::as_str).collect();
    let (status, body) = list_bin(&router, &indexed("SnapshotId", &refs)).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400 for {} ids, got {status} / {body}",
        refs.len()
    );
    assert_eq!(
        error_code(&body),
        "InvalidParameterValue",
        "case {case:?}: expected InvalidParameterValue for an over-long id list, got {body}"
    );

    let (status, listed) = list_bin(&router, &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from the report, got {status} / {listed}"
    );
    assert_eq!(
        elements(&listed, "snapshotId"),
        vec![retained.clone()],
        "case {case:?}: expected the bin to still hold exactly {retained}, got {listed}"
    );
    let (status, _) = describe(&router, &live).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the live snapshot to be the account's still"
    );
}
