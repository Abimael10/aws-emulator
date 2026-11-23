// SPDX-License-Identifier: CC0-1.0

//! WHICH SNAPSHOT a Recycle Bin retention rule covers, driven end to end over both services'
//! own wires through the shared cross-service harness [`super::rbin_retention_rules`].
//!
//! The rule under test is the pair-matching half of what `CreateRule` publishes: "for each
//! retention rule, you specify one or more tag key and value pairs. Resources (of the
//! specified type) that have at least one of these tag key and value pairs are automatically
//! retained in the Recycle Bin upon deletion", and "if a resource is deleted, but it does not
//! have any of the specified tag key and value pairs, it is immediately deleted without being
//! retained by the retention rule" (`rbin`'s `CreateRuleRequest$ResourceTags`). So a delete of
//! an EBS snapshot is retained exactly when a rule of the signed Region, for that resource
//! type and in the one state that retains, names a PAIR — the tag key together with its value
//! — that the snapshot carries AT THE MOMENT IT IS DELETED, whichever pair of the rule that
//! is; and a snapshot several rules cover waits for the LONGEST of their periods.
//!
//! Sources: `smithy_query(query="doc",shape="CreateRuleRequest",member="ResourceTags")` for
//! which resources a rule retains, `smithy_query(query="shape",shape="ResourceTag")` for the
//! key and the (optional, possibly empty) value a pair is made of, and
//! `smithy_query(query="shape",shape="RetentionPeriod")` for the period's unit and its
//! published per-type bounds.
//!
//! The rows differ along what the matching generalises over: WHICH of a rule's several pairs
//! the snapshot carries, a key carried with another value, a pair differing from the
//! snapshot's only in letter case, a pair whose value is empty, a pair the snapshot gained
//! after it was created, a pair it lost before it was deleted, two rules covering at once, and
//! a rule deleted again before the snapshot was — so no row is the rule.
//!
//! The KINDS of rule (Region-level, exclusion tags, another resource type, another Region) are
//! [`super::snapshots_in_recycle_bin`]'s own table, and what happens at the END of a retention
//! period is the bin's own rule, covered there too; nothing of either is repeated here.

use service_com_amazonaws_ec2_amazonec2 as ec2;
use service_com_amazonaws_rbin_amazonrecyclebin as rbin;

use ec2::state::{INVALID_SNAPSHOT_ID_MALFORMED, INVALID_SNAPSHOT_ID_NOT_FOUND};
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{
    call, element, elements, error_code, error_message, indexed, tag_report, tag_specification,
    tagging,
};
use super::rbin_retention_rules::{
    RESOURCE_TYPE_SNAPSHOT, RetentionRule, create_available_rule, delete_rule,
    emulator_with_retention_rules,
};
use super::volume_lifecycle::{REGION, p, zone_name};

/// The longest retention period a rule for this resource type may carry — "EBS snapshots and
/// EBS-backed AMIs: 1 - 365 days" (`RetentionPeriod$RetentionPeriodValue`), as the service
/// that OWNS retention rules states it ([`rbin::state::MAX_RETENTION_DAYS_SNAPSHOT_OR_IMAGE`])
/// rather than as a figure this suite repeats.
const LONGEST_RETENTION_DAYS: i64 = rbin::state::MAX_RETENTION_DAYS_SNAPSHOT_OR_IMAGE as i64;

/// The resource type a `TagSpecification` of `CreateSnapshot` names, as EC2 spells it for the
/// one resource that call mints.
const SNAPSHOT_RESOURCE_TYPE: &str = "snapshot";

/// One row: the rules the account holds when the snapshot is deleted, the tags the snapshot
/// carries by then — the ones it was created with, the ones written onto it afterwards and the
/// ones taken off it again — and for how many days the Recycle Bin must then keep it. `None`
/// is a permanent deletion, which is what a delete no rule covers is.
struct Row {
    case: &'static str,
    rules: &'static [RetentionRule],
    /// Which of `rules` are deleted again, through the service that owns them, before the
    /// snapshot is deleted: an index into `rules`.
    rules_deleted_again: &'static [usize],
    /// `CreateSnapshot`'s `TagSpecifications`.
    created_with: &'static [(&'static str, &'static str)],
    /// Tags written onto the snapshot with `CreateTags` after it was created.
    tagged_after: &'static [(&'static str, &'static str)],
    /// Tags taken off the snapshot with `DeleteTags` before it is deleted.
    untagged_before: &'static [(&'static str, &'static str)],
    retained_for_days: Option<i64>,
}

/// A tag-level rule of the signed Region for EBS snapshots, retaining for `retention_days`
/// whatever carries one of `resource_tags`.
const fn tag_level(
    retention_days: i64,
    resource_tags: &'static [(&'static str, &'static str)],
) -> RetentionRule {
    RetentionRule {
        resource_type: RESOURCE_TYPE_SNAPSHOT,
        retention_days,
        resource_tags,
        exclude_resource_tags: &[],
        region: REGION,
    }
}

/// The pairs a rule of the rows below names, written once so the rows that match them and the
/// rows that miss them differ only in the snapshot's own tags.
const NIGHTLY: (&str, &str) = ("backup", "nightly");
const KEEP: (&str, &str) = ("Name", "keep");
const TEAM_B: (&str, &str) = ("Team", "B");

/// The rows, differing along everything the pair matching generalises over.
const ROWS: &[Row] = &[
    Row {
        case: "a rule naming three pairs, the snapshot carrying only the last of them",
        rules: &[tag_level(1, &[KEEP, TEAM_B, NIGHTLY])],
        rules_deleted_again: &[],
        created_with: &[NIGHTLY],
        tagged_after: &[],
        untagged_before: &[],
        retained_for_days: Some(1),
    },
    Row {
        case: "a rule naming three pairs, the snapshot carrying each of their keys with \
               another value",
        rules: &[tag_level(7, &[KEEP, TEAM_B, NIGHTLY])],
        rules_deleted_again: &[],
        created_with: &[("Name", "drop"), ("Team", "A"), ("backup", "weekly")],
        tagged_after: &[],
        untagged_before: &[],
        retained_for_days: None,
    },
    Row {
        case: "a pair whose key differs from the snapshot's only in letter case",
        rules: &[tag_level(7, &[("Backup", "nightly")])],
        rules_deleted_again: &[],
        created_with: &[NIGHTLY],
        tagged_after: &[],
        untagged_before: &[],
        retained_for_days: None,
    },
    Row {
        case: "a pair whose value differs from the snapshot's only in letter case",
        rules: &[tag_level(7, &[("backup", "Nightly")])],
        rules_deleted_again: &[],
        created_with: &[NIGHTLY],
        tagged_after: &[],
        untagged_before: &[],
        retained_for_days: None,
    },
    Row {
        case: "a pair whose value is empty, the snapshot carrying that key with no value, at \
               the longest period the type takes",
        rules: &[tag_level(LONGEST_RETENTION_DAYS, &[("archive", "")])],
        rules_deleted_again: &[],
        created_with: &[("archive", ""), KEEP],
        tagged_after: &[],
        untagged_before: &[],
        retained_for_days: Some(LONGEST_RETENTION_DAYS),
    },
    Row {
        case: "a pair the snapshot was tagged with only after it was created",
        rules: &[tag_level(2, &[NIGHTLY])],
        rules_deleted_again: &[],
        created_with: &[("Name", "nightly")],
        tagged_after: &[NIGHTLY],
        untagged_before: &[],
        retained_for_days: Some(2),
    },
    Row {
        case: "a pair the snapshot was created with and untagged of before the delete",
        rules: &[tag_level(7, &[NIGHTLY])],
        rules_deleted_again: &[],
        created_with: &[NIGHTLY, KEEP],
        tagged_after: &[],
        untagged_before: &[NIGHTLY],
        retained_for_days: None,
    },
    Row {
        case: "two tag-level rules whose pairs the snapshot both carries",
        rules: &[tag_level(3, &[NIGHTLY]), tag_level(30, &[KEEP])],
        rules_deleted_again: &[],
        created_with: &[NIGHTLY, KEEP],
        tagged_after: &[],
        untagged_before: &[],
        // The longer of the two periods, not the one of the rule created first.
        retained_for_days: Some(30),
    },
    Row {
        case: "the only rule whose pair the snapshot carries, deleted again first",
        rules: &[tag_level(7, &[NIGHTLY])],
        rules_deleted_again: &[0],
        created_with: &[NIGHTLY],
        tagged_after: &[],
        untagged_before: &[],
        retained_for_days: None,
    },
];

/// One call of `action` signed for the Region this suite's EC2 calls use.
async fn invoke(
    router: &MultiServiceRouter,
    action: &str,
    params: &[(String, String)],
) -> (u16, String) {
    call(router, REGION, action, params).await
}

/// One volume of the account for the row's snapshot to be taken of.
async fn create_volume(router: &MultiServiceRouter, case: &str) -> String {
    let (status, body) = invoke(
        router,
        "CreateVolume",
        &[
            p("AvailabilityZone", zone_name(0)),
            p("VolumeType", "gp3"),
            p("Size", "1"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateVolume, got {status} with body {body}"
    );
    element(&body, "volumeId")
        .unwrap_or_else(|| panic!("case {case:?}: no volumeId in {body}"))
}

/// One snapshot of `volume_id` carrying `tags` from its create.
async fn create_snapshot(
    router: &MultiServiceRouter,
    case: &str,
    volume_id: &str,
    tags: &[(&str, &str)],
) -> String {
    let mut params = vec![p("VolumeId", volume_id)];
    params.extend(tag_specification(SNAPSHOT_RESOURCE_TYPE, tags));
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
    snapshot_id
}

/// `DescribeSnapshots` for one snapshot id: the HTTP status and the body.
async fn describe(router: &MultiServiceRouter, snapshot_id: &str) -> (u16, String) {
    invoke(
        router,
        "DescribeSnapshots",
        &indexed("SnapshotId", &[snapshot_id]),
    )
    .await
}

/// `ListSnapshotsInRecycleBin` with `params`: the HTTP status and the body.
async fn list_bin(router: &MultiServiceRouter, params: &[(String, String)]) -> (u16, String) {
    invoke(router, "ListSnapshotsInRecycleBin", params).await
}

/// Whether the Recycle Bin holds `snapshot_id` right now, read off the store the handler wrote
/// rather than off a report, so an assertion about retention does not depend on a reader.
fn bin_holds(engine: &engine::InMemoryEngine, snapshot_id: &str) -> bool {
    engine
        .state::<ec2::state::ComAmazonawsEc2Amazonec2State>()
        .lock()
        .snapshots_in_recycle_bin
        .contains_key(snapshot_id)
}

/// One timestamp the bin reports, as the instant it names.
fn reported_instant(case: &str, member: &str, body: &str) -> chrono::DateTime<chrono::FixedOffset> {
    let text =
        element(body, member).unwrap_or_else(|| panic!("case {case:?}: no {member} in {body}"));
    chrono::DateTime::parse_from_rfc3339(&text).unwrap_or_else(|error| {
        panic!("case {case:?}: {member} {text:?} is not a timestamp ({error})")
    })
}

/// THE RULE over the whole table: a `DeleteSnapshot` is retained exactly when a rule the
/// account holds names a pair the snapshot carries when it is deleted — the bin then keeping
/// it for the longest covering rule's period, with its tags, restorable until that period runs
/// out — and is permanent when no rule names such a pair.
#[tokio::test]
async fn the_pairs_a_rule_names_decide_whether_a_deleted_snapshot_is_retained() {
    for row in ROWS {
        let case = row.case;
        // One emulator per row, so the rules of one row decide only that row's delete.
        let (router, engine) = emulator_with_retention_rules();
        let mut identifiers = Vec::new();
        for rule in row.rules {
            identifiers.push(create_available_rule(&router, case, rule).await);
        }
        for &index in row.rules_deleted_again {
            delete_rule(&router, case, row.rules[index].region, &identifiers[index]).await;
        }

        let volume_id = create_volume(&router, case).await;
        let snapshot_id = create_snapshot(&router, case, &volume_id, row.created_with).await;
        if !row.tagged_after.is_empty() {
            let (status, body) =
                invoke(&router, "CreateTags", &tagging(&snapshot_id, row.tagged_after)).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200 from CreateTags, got {status} with body {body}"
            );
        }
        if !row.untagged_before.is_empty() {
            let (status, body) = invoke(
                &router,
                "DeleteTags",
                &tagging(&snapshot_id, row.untagged_before),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200 from DeleteTags, got {status} with body {body}"
            );
        }
        // What the snapshot carries at the moment it is deleted, which is what the rules are
        // asked about — and what a restore has to give back with it.
        let tags_at_delete = tag_report(&router, REGION, case, &snapshot_id).await;

        let (status, body) = invoke(&router, "DeleteSnapshot", &[p("SnapshotId", &snapshot_id)])
            .await;
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
            "case {case:?}: expected the wording for an absent snapshot, got {body}"
        );

        let Some(retention_days) = row.retained_for_days else {
            // No rule names a pair the snapshot carried, so the delete was permanent: the bin
            // holds nothing and there is nothing to restore.
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
            let (status, body) = invoke(
                &router,
                "RestoreSnapshotFromRecycleBin",
                &[p("SnapshotId", &snapshot_id)],
            )
            .await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected a permanently deleted snapshot to be unrestorable, got \
                 {status} with body {body}"
            );
            assert_eq!(
                error_code(&body),
                INVALID_SNAPSHOT_ID_MALFORMED,
                "case {case:?}: expected {INVALID_SNAPSHOT_ID_MALFORMED}, got {body}"
            );
            continue;
        };

        // A rule named a pair the snapshot carried, so the snapshot is in the bin — reported
        // by its id, with the volume it was taken of and the two instants its retention period
        // spans.
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
        assert!(
            bin_holds(&engine, &snapshot_id),
            "case {case:?}: expected the covered delete to retain {snapshot_id}"
        );
        // "The date and time when the snapshot is to be permanently deleted from the Recycle
        // Bin" is when it entered plus the retention period of the rule that covered it, so
        // the span between the two instants the bin reports IS that rule's period — the
        // LONGEST one, for a snapshot more than one rule covers.
        let entered = reported_instant(case, "recycleBinEnterTime", &listed);
        let exits = reported_instant(case, "recycleBinExitTime", &listed);
        assert_eq!(
            exits - entered,
            chrono::TimeDelta::try_days(retention_days).expect("a representable period"),
            "case {case:?}: expected the bin to keep the snapshot for the rule's \
             {retention_days} day(s), got {entered} to {exits}"
        );

        // And it is restorable while the period lasts: the restore gives the snapshot back to
        // the account, carrying the tags it was deleted with — the very tags the rules were
        // asked about.
        let (status, body) = invoke(
            &router,
            "RestoreSnapshotFromRecycleBin",
            &[p("SnapshotId", &snapshot_id)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 restoring the retained {snapshot_id}, got {status} \
             with body {body}"
        );
        let (status, body) = describe(&router, &snapshot_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the restored snapshot to be the account's again, got \
             {status} with body {body}"
        );
        assert_eq!(
            element(&body, "volumeId").as_deref(),
            Some(volume_id.as_str()),
            "case {case:?}: expected the restored snapshot to report its source volume, got \
             {body}"
        );
        let tags_after_restore = tag_report(&router, REGION, case, &snapshot_id).await;
        assert_eq!(
            tags_after_restore, tags_at_delete,
            "case {case:?}: expected the restored snapshot to carry the tags it was deleted \
             with, got {tags_after_restore:?} instead of {tags_at_delete:?}"
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
