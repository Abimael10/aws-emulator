// SPDX-License-Identifier: CC0-1.0

//! WHEN a Recycle Bin retention rule decides a `DeleteVolume`, and WHAT the volume it decides
//! about carries — driven end to end over both services' own wires through the shared
//! cross-service harness [`super::rbin_retention_rules`].
//!
//! [`super::volume_retention_rules`] covers which KINDS of rule cover a delete, with every
//! volume tagged at its create and every delete the first one. The rule this suite pins is the
//! other half of the same sentence: a retention rule retains "resources (of the specified
//! type) that have at least one of these tag key and value pairs" and a Region-level rule
//! retains everything of the type except that "resources that have any of these tags are not
//! retained by the retention rule upon deletion" (`rbin`'s `CreateRuleRequest$ResourceTags` /
//! `$ExcludeResourceTags`) — a statement about the tags the deleted resource carries and the
//! rules in force AT THE DELETE, not about either as they stood when the volume was minted.
//! So:
//!
//! 1. a delete is decided by the tags the volume carries WHEN IT IS DELETED, however they got
//!    there: a pair written by `CreateTags` after the create covers the delete exactly as one
//!    carried from the create does, a pair `DeleteTags` took away covers it no longer, and a
//!    value `CreateTags` overwrote is read as the value it now has;
//! 2. every delete is decided afresh by the rules in force at it, so a volume
//!    `RestoreVolumeFromRecycleBin` gave back is a volume of the account again — deletable,
//!    and permanently deleted when the rule that retained it is gone.
//!
//! Each row of the table below differs along what rule 1 generalises over: the kind of rule
//! (tag-level, Region-level with exclusion tags), where the deciding tag came from (the
//! create, a later `CreateTags`, a `DeleteTags`, a `CreateTags` that overwrote a value), and
//! whether the delete is retained or permanent — so no row is the specification.

use service_com_amazonaws_ec2_amazonec2 as ec2;

use ec2::state::{
    INVALID_VOLUME_ID_MALFORMED, INVALID_VOLUME_ID_NOT_FOUND, VOLUME_STATE_AVAILABLE,
    VOLUME_STATE_DELETED,
};
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{
    call, element, elements, error_code, error_message, indexed, tag_specification, tagging,
};
use super::rbin_retention_rules::{
    RESOURCE_TYPE_VOLUME, RetentionRule, create_available_rule, delete_rule,
    emulator_with_retention_rules,
};
use super::volume_lifecycle::{REGION, is_volume_id, p, past_the_transition, zone_name};

/// The `ResourceType` a volume is tagged under, as `TagSpecification$ResourceType` names it
/// for this resource ("volume").
const TAGGED_RESOURCE_TYPE: &str = "volume";

/// The retention period, in days, the rows that retain use — inside "EBS volumes: 1 - 7 days"
/// (`rbin`'s `RetentionPeriod$RetentionPeriodValue`), whose two ends
/// [`super::volume_retention_rules`] already drives; this suite varies the TAGS, so one
/// period in the middle of that range serves every row.
const RETENTION_DAYS: i64 = 2;

/// One row: the rule the account holds, how the deleted volume came to carry the tags it
/// carries at the delete, and whether that delete is therefore retained.
struct Row {
    case: &'static str,
    rule: RetentionRule,
    /// `TagSpecification` on the `CreateVolume` that mints the volume.
    tags_at_create: &'static [(&'static str, &'static str)],
    /// `CreateTags` written onto the volume after the create and before the delete: "adds or
    /// overwrites only the specified tags for the specified Amazon EC2 resource"
    /// (`CreateTags`).
    tags_added: &'static [(&'static str, &'static str)],
    /// `DeleteTags` applied after the create and before the delete.
    tags_removed: &'static [(&'static str, &'static str)],
    /// Whether the Recycle Bin must hold the volume once it is deleted.
    retained: bool,
}

/// A tag-level rule for EBS volumes in the Region this suite's EC2 calls are signed for.
const fn tag_level_rule(
    resource_tags: &'static [(&'static str, &'static str)],
) -> RetentionRule {
    RetentionRule {
        resource_type: RESOURCE_TYPE_VOLUME,
        retention_days: RETENTION_DAYS,
        resource_tags,
        exclude_resource_tags: &[],
        region: REGION,
    }
}

/// A Region-level rule for EBS volumes whose exclusion tags leave `exclude_resource_tags` out.
const fn region_level_rule(
    exclude_resource_tags: &'static [(&'static str, &'static str)],
) -> RetentionRule {
    RetentionRule {
        resource_type: RESOURCE_TYPE_VOLUME,
        retention_days: RETENTION_DAYS,
        resource_tags: &[],
        exclude_resource_tags,
        region: REGION,
    }
}

/// The rows: every way the tags that decide a delete can have reached the volume.
fn rows() -> Vec<Row> {
    vec![
        // The CONTROL row of the table: the tag reaches the volume the one way
        // [`super::volume_retention_rules`] drives, so every other row differs from this one
        // only in where the deciding tag came from.
        Row {
            case: "the pair the rule names, carried from the create",
            rule: tag_level_rule(&[("Backup", "yes")]),
            tags_at_create: &[("Backup", "yes")],
            tags_added: &[],
            tags_removed: &[],
            retained: true,
        },
        Row {
            case: "the pair the rule names, written by CreateTags after the create",
            rule: tag_level_rule(&[("Backup", "yes")]),
            tags_at_create: &[],
            tags_added: &[("Backup", "yes")],
            tags_removed: &[],
            retained: true,
        },
        Row {
            case: "the pair the rule names, taken away by DeleteTags before the delete",
            rule: tag_level_rule(&[("Backup", "yes")]),
            tags_at_create: &[("Backup", "yes"), ("Team", "A")],
            tags_added: &[],
            tags_removed: &[("Backup", "yes")],
            retained: false,
        },
        Row {
            case: "the rule's key overwritten by CreateTags to a value the rule does not name",
            rule: tag_level_rule(&[("Backup", "yes")]),
            tags_at_create: &[("Backup", "yes")],
            tags_added: &[("Backup", "no")],
            tags_removed: &[],
            retained: false,
        },
        Row {
            case: "the rule's key overwritten by CreateTags to the value the rule names",
            rule: tag_level_rule(&[("Backup", "yes")]),
            tags_at_create: &[("Backup", "no")],
            tags_added: &[("Backup", "yes")],
            tags_removed: &[],
            retained: true,
        },
        Row {
            case: "one of several pairs the rule names, written by CreateTags",
            rule: tag_level_rule(&[("Backup", "yes"), ("Keep", "forever")]),
            tags_at_create: &[("Team", "A")],
            tags_added: &[("Keep", "forever")],
            tags_removed: &[],
            retained: true,
        },
        Row {
            case: "a Region-level rule whose exclusion tag CreateTags wrote after the create",
            rule: region_level_rule(&[("Temporary", "true")]),
            tags_at_create: &[("Name", "keep")],
            tags_added: &[("Temporary", "true")],
            tags_removed: &[],
            retained: false,
        },
        Row {
            case: "a Region-level rule whose exclusion tag DeleteTags took away before the delete",
            rule: region_level_rule(&[("Temporary", "true")]),
            tags_at_create: &[("Temporary", "true")],
            tags_added: &[],
            tags_removed: &[("Temporary", "true")],
            retained: true,
        },
    ]
}

/// One EC2 call signed for the Region this suite's volumes and rules live in.
async fn invoke(
    router: &MultiServiceRouter,
    action: &str,
    params: &[(String, String)],
) -> (u16, String) {
    call(router, REGION, action, params).await
}

/// Mint one `gp3` volume carrying `tags`, settled past the transition its create owes, and
/// answer its id.
async fn settled_volume(
    router: &MultiServiceRouter,
    case: &str,
    tags: &[(&str, &str)],
) -> String {
    let mut params = vec![
        p("AvailabilityZone", zone_name(0)),
        p("VolumeType", "gp3"),
        p("Size", "1"),
    ];
    params.extend(tag_specification(TAGGED_RESOURCE_TYPE, tags));
    let (status, body) = invoke(router, "CreateVolume", &params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateVolume, got {status} with body {body}"
    );
    let volume_id = element(&body, "volumeId").unwrap_or_default();
    assert!(
        is_volume_id(&volume_id),
        "case {case:?}: expected a minted volume id, got {volume_id:?}"
    );
    past_the_transition().await;
    volume_id
}

/// Drive one half of the tag plane over `volume_id` and pin that it succeeded.
async fn retag(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    volume_id: &str,
    tags: &[(&str, &str)],
) {
    if tags.is_empty() {
        return;
    }
    let (status, body) = invoke(router, action, &tagging(volume_id, tags)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action} on {volume_id}, got {status} with \
         body {body}"
    );
}

/// `DeleteVolume` on `volume_id`, which every row expects to be served.
async fn delete_volume(router: &MultiServiceRouter, case: &str, volume_id: &str) {
    let (status, body) = invoke(router, "DeleteVolume", &[p("VolumeId", volume_id)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 deleting {volume_id}, got {status} with body {body}"
    );
}

/// Pin that `volume_id` is no longer one of the account's volumes: a `VolumeIds`-filtered
/// `DescribeVolumes` is refused `InvalidVolume.NotFound` with the sentence that names the id
/// the request asked about.
async fn assert_not_an_account_volume(router: &MultiServiceRouter, case: &str, volume_id: &str) {
    let (status, body) = invoke(router, "DescribeVolumes", &indexed("VolumeId", &[volume_id])).await;
    assert_eq!(
        status, 400,
        "case {case:?}: a deleted volume is not the account's, got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body),
        INVALID_VOLUME_ID_NOT_FOUND,
        "case {case:?}: expected {INVALID_VOLUME_ID_NOT_FOUND}, got {body}"
    );
    assert_eq!(
        error_message(&body),
        format!("The volume '{volume_id}' does not exist."),
        "case {case:?}: expected the sentence that names {volume_id}, got {body}"
    );
}

/// The volume ids the Recycle Bin reports right now, read over the wire.
async fn binned_ids(router: &MultiServiceRouter, case: &str) -> Vec<String> {
    let (status, body) = invoke(router, "ListVolumesInRecycleBin", &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from ListVolumesInRecycleBin, got {status} with \
         body {body}"
    );
    elements(&body, "volumeId")
}

/// Pin that nothing can be restored for `volume_id`: the one code the service answers for
/// every id the bin holds no entry for.
async fn assert_unrestorable(router: &MultiServiceRouter, case: &str, volume_id: &str) {
    let (status, body) = invoke(
        router,
        "RestoreVolumeFromRecycleBin",
        &[p("VolumeId", volume_id)],
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected a permanently deleted volume to be unrestorable, got \
         {status} with body {body}"
    );
    assert_eq!(
        error_code(&body),
        INVALID_VOLUME_ID_MALFORMED,
        "case {case:?}: expected {INVALID_VOLUME_ID_MALFORMED}, got {body}"
    );
    assert_eq!(
        error_message(&body),
        format!("The volume ID '{volume_id}' is malformed"),
        "case {case:?}: expected the sentence that names {volume_id}, got {body}"
    );
}

/// Restore `volume_id` out of the bin and answer the state `DescribeVolumes` then reports for
/// it — the volume being the account's again is the whole point of the restore.
async fn restore(router: &MultiServiceRouter, case: &str, volume_id: &str) -> String {
    let (status, body) = invoke(
        router,
        "RestoreVolumeFromRecycleBin",
        &[p("VolumeId", volume_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 restoring {volume_id}, got {status} with body {body}"
    );
    assert_eq!(
        element(&body, "return").as_deref(),
        Some("true"),
        "case {case:?}: expected RestoreVolumeFromRecycleBin to report true, got {body}"
    );
    let (status, body) = invoke(router, "DescribeVolumes", &indexed("VolumeId", &[volume_id])).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the restored {volume_id} to be describable, got {status} \
         with body {body}"
    );
    element(&body, "status").unwrap_or_default()
}

/// RULE 1: the tags that decide a `DeleteVolume` are the ones the volume carries at the
/// delete, whichever call put them there.
#[tokio::test]
async fn the_tags_a_delete_is_decided_by_are_the_ones_the_volume_carries_at_the_delete() {
    for row in rows() {
        let case = row.case;
        // One emulator per row, so the rule of one row decides only that row's delete.
        let (router, _engine) = emulator_with_retention_rules();
        // The identifier the rule is created under is not needed here: no row of this table
        // deletes its rule again (the second test does).
        let _ = create_available_rule(&router, case, &row.rule).await;

        let volume_id = settled_volume(&router, case, row.tags_at_create).await;
        retag(&router, case, "CreateTags", &volume_id, row.tags_added).await;
        retag(&router, case, "DeleteTags", &volume_id, row.tags_removed).await;

        delete_volume(&router, case, &volume_id).await;
        // Either way the volume has left the account's volumes: a retained volume has been
        // deleted, and the bin is where it waits.
        assert_not_an_account_volume(&router, case, &volume_id).await;

        let listed = binned_ids(&router, case).await;
        if row.retained {
            assert_eq!(
                listed,
                vec![volume_id.clone()],
                "case {case:?}: expected the bin to hold exactly {volume_id}, got {listed:?}"
            );
            let state = restore(&router, case, &volume_id).await;
            assert_eq!(
                state, VOLUME_STATE_AVAILABLE,
                "case {case:?}: expected the restored {volume_id} in state \
                 {VOLUME_STATE_AVAILABLE}, got {state:?}"
            );
            let listed = binned_ids(&router, case).await;
            assert!(
                listed.is_empty(),
                "case {case:?}: expected the bin to be empty after the restore, got {listed:?}"
            );
        } else {
            assert!(
                listed.is_empty(),
                "case {case:?}: expected the delete of {volume_id} to be permanent, yet the \
                 bin reports {listed:?}"
            );
            assert_unrestorable(&router, case, &volume_id).await;
        }
    }
}

/// RULE 2: a restored volume is a volume of the account again, and the NEXT delete of it is
/// decided by the rules in force at that delete — so once the rule that retained it is gone,
/// the same volume is permanently deleted.
#[tokio::test]
async fn a_restored_volume_is_deleted_again_by_the_rules_in_force_at_that_delete() {
    let case = "a volume deleted, restored, and deleted again once the rule is gone";
    let (router, _engine) = emulator_with_retention_rules();
    let rule = region_level_rule(&[]);
    let identifier = create_available_rule(&router, case, &rule).await;

    let volume_id = settled_volume(&router, case, &[("Name", "keep")]).await;

    // The rule is in force, so the first delete retains the volume and the bin reports it as
    // a volume that has been deleted.
    delete_volume(&router, case, &volume_id).await;
    assert_not_an_account_volume(&router, case, &volume_id).await;
    let (status, body) = invoke(
        &router,
        "ListVolumesInRecycleBin",
        &indexed("VolumeId", &[volume_id.as_str()]),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from ListVolumesInRecycleBin, got {status} with \
         body {body}"
    );
    assert_eq!(
        elements(&body, "volumeId"),
        vec![volume_id.clone()],
        "case {case:?}: expected the bin to hold exactly {volume_id}, got {body}"
    );
    assert_eq!(
        element(&body, "state").as_deref(),
        Some(VOLUME_STATE_DELETED),
        "case {case:?}: expected the bin's entry to report {VOLUME_STATE_DELETED}, got {body}"
    );

    let state = restore(&router, case, &volume_id).await;
    assert_eq!(
        state, VOLUME_STATE_AVAILABLE,
        "case {case:?}: expected the restored {volume_id} in state {VOLUME_STATE_AVAILABLE}, \
         got {state:?}"
    );

    // The rule the first delete consulted is deleted through the service that owns it, so the
    // second delete of the very same volume is covered by nothing.
    delete_rule(&router, case, rule.region, &identifier).await;
    delete_volume(&router, case, &volume_id).await;
    assert_not_an_account_volume(&router, case, &volume_id).await;
    let listed = binned_ids(&router, case).await;
    assert!(
        listed.is_empty(),
        "case {case:?}: expected the second delete of {volume_id} to be permanent, yet the \
         bin reports {listed:?}"
    );
    assert_unrestorable(&router, case, &volume_id).await;
}
