// SPDX-License-Identifier: CC0-1.0

//! What a RECYCLE BIN RETENTION RULE does to `DeleteVolume`, driven end to end over both
//! services' own wires through the shared cross-service harness
//! [`super::rbin_retention_rules`].
//!
//! The rule under test is the one `CreateRule` publishes: "resources (of the specified type)
//! that have at least one of these tag key and value pairs are automatically retained in the
//! Recycle Bin upon deletion", while a rule naming no resource tags is a Region-level rule
//! and "retains all deleted resources of the specified resource type in the Region in which
//! the rule is created, even if the resources are not tagged" — except for what its
//! exclusion tags leave out, "resources that have any of these tags are not retained by the
//! retention rule upon deletion" (`rbin`'s `CreateRuleRequest$ResourceTags` /
//! `$ExcludeResourceTags`), and only a rule "in the `available` state" retains anything
//! (`CreateRuleResponse$Status`). So:
//!
//! 1. a `DeleteVolume` an available rule of the SIGNED REGION covers retains the volume: it
//!    stops being one of the account's volumes, `ListVolumesInRecycleBin` reports it with the
//!    two instants its retention period spans — the exit instant being the enter instant plus
//!    that rule's retention period — and `RestoreVolumeFromRecycleBin` gives back the very
//!    same volume while the period lasts;
//! 2. a delete NO rule covers is permanent: the bin holds nothing and there is nothing to
//!    restore. A rule of another resource type, a rule another Region holds, a tag-level rule
//!    whose pairs the volume does not carry, a Region-level rule whose exclusion tags it
//!    does, and a rule that has been deleted again are each no cover at all;
//! 3. a volume several rules cover is kept for the LONGEST of their retention periods, the
//!    bin holding the volume once;
//! 4. a `DryRun` of a covered delete retains nothing: it reports what the call would have
//!    done, and the volume is still the account's.
//!
//! Each table row is a distinct input along what the rule generalises over — the kind of rule
//! (Region-level, tag-level, with and without exclusion tags), its resource type, the Region
//! it was created in, how many rules the account holds, whether the rule still exists, the
//! retention period at each end of the range the service publishes for this resource type,
//! and the tags the deleted volume carries — so no row is the specification.
//!
//! What happens at the END of a retention period — the permanent deletion the clock owes —
//! is the Recycle Bin's own rule rather than the retention rule's, and
//! [`super::volumes_in_recycle_bin`] covers it: the shortest period a rule can carry is one
//! day, so no rule can drive that case.

use service_com_amazonaws_ec2_amazonec2 as ec2;

use ec2::state::{
    INVALID_VOLUME_ID_MALFORMED, INVALID_VOLUME_ID_NOT_FOUND, VOLUME_STATE_DELETED,
};
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, elements, error_code, indexed};
use super::rbin_retention_rules::{
    MAX_RETENTION_DAYS_VOLUME, OTHER_REGION, RESOURCE_TYPE_SNAPSHOT, RESOURCE_TYPE_VOLUME,
    RetentionRule, create_available_rule, delete_rule, emulator_with_retention_rules,
};
use super::volume_lifecycle::{REGION, is_volume_id, p, past_the_transition, zone_name};

/// The HTTP status `DryRunOperation` carries: the request was refused because a precondition
/// the CALLER set holds, not because anything in it was wrong.
const DRY_RUN_STATUS: u16 = 412;

/// The shortest retention period a rule may carry: "the retention period […] measured in
/// days" is bounded below by the modelled `@range(min: 1, max: 3650)` on
/// `RetentionPeriod$RetentionPeriodValue`, and the published per-type range starts at the
/// same day — "EBS volumes: 1 - 7 days" (`RetentionPeriod$RetentionPeriodValue`).
const SHORTEST_RETENTION_DAYS: i64 = 1;

/// The longest retention period a rule for this resource type may carry — "EBS volumes: 1 - 7
/// days" (`RetentionPeriod$RetentionPeriodValue`), as the owning service's own catalog states
/// it ([`MAX_RETENTION_DAYS_VOLUME`]).
const LONGEST_RETENTION_DAYS: i64 = MAX_RETENTION_DAYS_VOLUME;

/// One row of the retention table: the rules the account holds when the volume is deleted,
/// the volume that is deleted, and for how many days the Recycle Bin must then keep it —
/// `None` when no rule covers the delete, which is a permanent deletion.
struct Row {
    case: &'static str,
    rules: &'static [RetentionRule],
    /// The identifiers of which of `rules` are deleted again before the volume is: an index
    /// into `rules`.
    rules_deleted_again: &'static [usize],
    /// `CreateVolume`'s `VolumeType` and `Size` for the volume under test, so the rows differ
    /// in the volume they delete as well as in the rules that decide it.
    volume_type: &'static str,
    volume_size: &'static str,
    volume_tags: &'static [(&'static str, &'static str)],
    retained_for_days: Option<i64>,
}

/// One call of `action` signed for the Region this suite's EC2 calls use.
async fn invoke(
    router: &MultiServiceRouter,
    action: &str,
    params: &[(String, String)],
) -> (u16, String) {
    call(router, REGION, action, params).await
}

/// The `TagSpecification` members that tag a created volume.
fn tag_specification(tags: &[(&str, &str)]) -> Vec<(String, String)> {
    let mut members = vec![p("TagSpecification.1.ResourceType", "volume")];
    for (index, (key, value)) in tags.iter().enumerate() {
        members.push(p(&format!("TagSpecification.1.Tag.{}.Key", index + 1), *key));
        members.push(p(
            &format!("TagSpecification.1.Tag.{}.Value", index + 1),
            *value,
        ));
    }
    members
}

/// Mint one volume of the row's type, size and tags, and answer its id.
async fn create_volume(
    router: &MultiServiceRouter,
    case: &str,
    volume_type: &str,
    size: &str,
    tags: &[(&str, &str)],
) -> String {
    let mut params = vec![
        p("AvailabilityZone", zone_name(0)),
        p("VolumeType", volume_type),
        p("Size", size),
    ];
    if !tags.is_empty() {
        params.extend(tag_specification(tags));
    }
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
    volume_id
}

/// `DescribeVolumes` for one volume id: the HTTP status and the body.
async fn describe(router: &MultiServiceRouter, volume_id: &str) -> (u16, String) {
    invoke(router, "DescribeVolumes", &indexed("VolumeId", &[volume_id])).await
}

/// The whole `<volumeSet>` `DescribeVolumes` reports for `volume_id` — every member of the
/// volume as the service renders it, so comparing two of these compares the WHOLE volume
/// rather than the handful of members an assertion remembered to name.
async fn described_volume(router: &MultiServiceRouter, case: &str, volume_id: &str) -> String {
    let (status, body) = describe(router, volume_id).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeVolumes, got {status} with body {body}"
    );
    element(&body, "volumeSet").unwrap_or_else(|| {
        panic!("case {case:?}: expected DescribeVolumes to report a volumeSet, got {body}")
    })
}

/// One timestamp the bin reports, as the instant it names.
fn reported_instant(case: &str, member: &str, body: &str) -> chrono::DateTime<chrono::FixedOffset> {
    let text =
        element(body, member).unwrap_or_else(|| panic!("case {case:?}: no {member} in {body}"));
    chrono::DateTime::parse_from_rfc3339(&text)
        .unwrap_or_else(|error| panic!("case {case:?}: {member} {text:?} is not a timestamp ({error})"))
}

/// The rows: every way a rule can cover a delete, and every way it can fail to.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a Region-level rule for EBS volumes, at the longest period the type takes",
            rules: &[RetentionRule {
                resource_type: RESOURCE_TYPE_VOLUME,
                retention_days: LONGEST_RETENTION_DAYS,
                resource_tags: &[],
                exclude_resource_tags: &[],
                region: REGION,
            }],
            rules_deleted_again: &[],
            volume_type: "gp3",
            volume_size: "1",
            volume_tags: &[],
            retained_for_days: Some(LONGEST_RETENTION_DAYS),
        },
        Row {
            case: "a tag-level rule one of the volume's tags matches, at the shortest period",
            rules: &[RetentionRule {
                resource_type: RESOURCE_TYPE_VOLUME,
                retention_days: SHORTEST_RETENTION_DAYS,
                resource_tags: &[("Name", "keep")],
                exclude_resource_tags: &[],
                region: REGION,
            }],
            rules_deleted_again: &[],
            volume_type: "gp2",
            volume_size: "8",
            volume_tags: &[("Team", "A"), ("Name", "keep")],
            retained_for_days: Some(SHORTEST_RETENTION_DAYS),
        },
        Row {
            case: "a tag-level rule none of the volume's tags match",
            rules: &[RetentionRule {
                resource_type: RESOURCE_TYPE_VOLUME,
                retention_days: 3,
                resource_tags: &[("Name", "keep"), ("Team", "B")],
                exclude_resource_tags: &[],
                region: REGION,
            }],
            rules_deleted_again: &[],
            volume_type: "st1",
            volume_size: "125",
            volume_tags: &[("Name", "something-else")],
            retained_for_days: None,
        },
        Row {
            case: "a Region-level rule whose exclusion tag the volume does not carry",
            rules: &[RetentionRule {
                resource_type: RESOURCE_TYPE_VOLUME,
                retention_days: 4,
                resource_tags: &[],
                exclude_resource_tags: &[("Temporary", "true")],
                region: REGION,
            }],
            rules_deleted_again: &[],
            volume_type: "gp3",
            volume_size: "40",
            volume_tags: &[("Name", "keep")],
            retained_for_days: Some(4),
        },
        Row {
            case: "a Region-level rule whose exclusion tag the volume carries",
            rules: &[RetentionRule {
                resource_type: RESOURCE_TYPE_VOLUME,
                retention_days: 5,
                resource_tags: &[],
                exclude_resource_tags: &[("Temporary", "true")],
                region: REGION,
            }],
            rules_deleted_again: &[],
            volume_type: "standard",
            volume_size: "2",
            volume_tags: &[("Temporary", "true")],
            retained_for_days: None,
        },
        Row {
            case: "a rule for another resource type",
            rules: &[RetentionRule {
                resource_type: RESOURCE_TYPE_SNAPSHOT,
                retention_days: 6,
                resource_tags: &[],
                exclude_resource_tags: &[],
                region: REGION,
            }],
            rules_deleted_again: &[],
            volume_type: "gp3",
            volume_size: "1",
            volume_tags: &[("Name", "keep")],
            retained_for_days: None,
        },
        Row {
            case: "a rule another Region holds",
            rules: &[RetentionRule {
                resource_type: RESOURCE_TYPE_VOLUME,
                retention_days: 7,
                resource_tags: &[],
                exclude_resource_tags: &[],
                region: OTHER_REGION,
            }],
            rules_deleted_again: &[],
            volume_type: "gp3",
            volume_size: "1",
            volume_tags: &[],
            retained_for_days: None,
        },
        Row {
            case: "two rules that both cover the delete",
            rules: &[
                RetentionRule {
                    resource_type: RESOURCE_TYPE_VOLUME,
                    retention_days: 2,
                    resource_tags: &[],
                    exclude_resource_tags: &[],
                    region: REGION,
                },
                RetentionRule {
                    resource_type: RESOURCE_TYPE_VOLUME,
                    retention_days: 6,
                    resource_tags: &[("Name", "keep")],
                    exclude_resource_tags: &[],
                    region: REGION,
                },
            ],
            rules_deleted_again: &[],
            volume_type: "sc1",
            volume_size: "125",
            volume_tags: &[("Name", "keep")],
            // RULE 3: the longer of the two periods, not the one of the rule created first.
            retained_for_days: Some(6),
        },
        Row {
            case: "the only rule that covered the delete, deleted again first",
            rules: &[RetentionRule {
                resource_type: RESOURCE_TYPE_VOLUME,
                retention_days: 7,
                resource_tags: &[],
                exclude_resource_tags: &[],
                region: REGION,
            }],
            rules_deleted_again: &[0],
            volume_type: "gp3",
            volume_size: "1",
            volume_tags: &[],
            retained_for_days: None,
        },
        Row {
            case: "one of two rules deleted again, the surviving one covering the delete",
            rules: &[
                RetentionRule {
                    resource_type: RESOURCE_TYPE_VOLUME,
                    retention_days: 7,
                    resource_tags: &[],
                    exclude_resource_tags: &[],
                    region: REGION,
                },
                RetentionRule {
                    resource_type: RESOURCE_TYPE_VOLUME,
                    retention_days: 3,
                    resource_tags: &[("Name", "keep")],
                    exclude_resource_tags: &[],
                    region: REGION,
                },
            ],
            rules_deleted_again: &[0],
            volume_type: "gp3",
            volume_size: "1",
            volume_tags: &[("Name", "keep")],
            retained_for_days: Some(3),
        },
    ]
}

/// RULES 1, 2 and 3 over the whole table: whether a `DeleteVolume` is permanent or retains
/// the volume is decided by the retention rules the account holds in the Region the delete
/// was signed for, and a retained volume waits in the Recycle Bin for the longest covering
/// rule's retention period, restorable until it runs out.
#[tokio::test]
async fn a_delete_a_retention_rule_covers_retains_the_volume_in_the_recycle_bin() {
    for row in rows() {
        let case = row.case;
        // One emulator per row, so the rules of one row decide only that row's delete.
        let (router, engine) = emulator_with_retention_rules();
        let mut identifiers = Vec::new();
        for rule in row.rules {
            identifiers.push(create_available_rule(&router, case, rule).await);
        }
        for index in row.rules_deleted_again {
            delete_rule(&router, case, row.rules[*index].region, &identifiers[*index]).await;
        }

        let volume_id = create_volume(
            &router,
            case,
            row.volume_type,
            row.volume_size,
            row.volume_tags,
        )
        .await;
        // Settled before the delete, so the volume the bin takes in and the volume the
        // restore gives back are read at the same point of the volume's own clock.
        past_the_transition().await;
        let before = described_volume(&router, case, &volume_id).await;

        let (status, body) = invoke(&router, "DeleteVolume", &[p("VolumeId", volume_id.as_str())])
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 deleting {volume_id}, got {status} with body {body}"
        );

        // Either way the volume has left the account's volumes: a retained volume has been
        // deleted, and the bin is where it waits.
        let (status, body) = describe(&router, &volume_id).await;
        assert_eq!(
            status, 400,
            "case {case:?}: a deleted volume is not the account's, got {status} / {body}"
        );
        assert_eq!(
            error_code(&body),
            INVALID_VOLUME_ID_NOT_FOUND,
            "case {case:?}: expected {INVALID_VOLUME_ID_NOT_FOUND}, got {body}"
        );

        let Some(retention_days) = row.retained_for_days else {
            // RULE 2: no rule covers the delete, so it was permanent — the bin holds nothing
            // and the restore has nothing to give back.
            let (status, listed) = invoke(&router, "ListVolumesInRecycleBin", &[]).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200 from ListVolumesInRecycleBin, got {status} \
                 with body {listed}"
            );
            assert!(
                elements(&listed, "volumeId").is_empty(),
                "case {case:?}: expected an empty Recycle Bin, got {listed}"
            );
            assert!(
                !bin_holds(&engine, &volume_id),
                "case {case:?}: expected the delete of {volume_id} to be permanent"
            );
            let (status, body) = invoke(
                &router,
                "RestoreVolumeFromRecycleBin",
                &[p("VolumeId", volume_id.as_str())],
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
            continue;
        };

        // RULE 1: the rule covers the delete, so the volume is in the bin — reported by its
        // id, as a volume that has been deleted, with the two instants its retention period
        // spans.
        let (status, listed) =
            invoke(&router, "ListVolumesInRecycleBin", &indexed("VolumeId", &[&volume_id])).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from ListVolumesInRecycleBin, got {status} with \
             body {listed}"
        );
        assert_eq!(
            elements(&listed, "volumeId"),
            vec![volume_id.clone()],
            "case {case:?}: expected the bin to report exactly {volume_id}, got {listed}"
        );
        assert_eq!(
            element(&listed, "state").as_deref(),
            Some(VOLUME_STATE_DELETED),
            "case {case:?}: expected the bin's entry to report {VOLUME_STATE_DELETED}, got \
             {listed}"
        );
        assert_eq!(
            element(&listed, "size").as_deref(),
            Some(row.volume_size),
            "case {case:?}: expected the bin to report the volume's own size, got {listed}"
        );
        // "The date and time when the volume is to be permanently deleted from the Recycle
        // Bin" is when it entered plus the retention period of the rule that covered it, so
        // the span between the two instants the bin reports IS that rule's period.
        let entered = reported_instant(case, "recycleBinEnterTime", &listed);
        let exits = reported_instant(case, "recycleBinExitTime", &listed);
        assert_eq!(
            exits - entered,
            chrono::TimeDelta::try_days(retention_days).expect("a representable period"),
            "case {case:?}: expected the bin to keep the volume for the rule's \
             {retention_days} day(s), got {entered} to {exits}"
        );

        // And it is restorable until that period runs out: the restore gives back the same
        // volume the describe reported before the delete.
        let (status, body) = invoke(
            &router,
            "RestoreVolumeFromRecycleBin",
            &[p("VolumeId", volume_id.as_str())],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 restoring the retained {volume_id}, got {status} \
             with body {body}"
        );
        let after = described_volume(&router, case, &volume_id).await;
        assert_eq!(
            after, before,
            "case {case:?}: expected the restored volume to read back as it did before the \
             delete, got {after} instead of {before}"
        );
        // The bin gave it up, so it reports nothing any more.
        let (status, listed) = invoke(&router, "ListVolumesInRecycleBin", &[]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from ListVolumesInRecycleBin, got {status} with \
             body {listed}"
        );
        assert!(
            elements(&listed, "volumeId").is_empty(),
            "case {case:?}: expected the bin to be empty after the restore, got {listed}"
        );
    }
}

/// RULE 4: a dry run of a delete a retention rule covers performs NOTHING — it neither
/// deletes the volume nor retains it — and the real call after it still retains.
#[tokio::test]
async fn a_dry_run_of_a_covered_delete_retains_nothing() {
    let case = "a dry run of a delete a Region-level rule covers";
    let (router, engine) = emulator_with_retention_rules();
    create_available_rule(
        &router,
        case,
        &RetentionRule {
            resource_type: RESOURCE_TYPE_VOLUME,
            retention_days: SHORTEST_RETENTION_DAYS,
            resource_tags: &[],
            exclude_resource_tags: &[],
            region: REGION,
        },
    )
    .await;
    let volume_id = create_volume(&router, case, "gp3", "3", &[("Name", "keep")]).await;
    past_the_transition().await;

    for attempt in 1..=2 {
        let (status, body) = invoke(
            &router,
            "DeleteVolume",
            &[p("VolumeId", volume_id.as_str()), p("DryRun", "true")],
        )
        .await;
        assert_eq!(
            status, DRY_RUN_STATUS,
            "attempt {attempt}: expected HTTP {DRY_RUN_STATUS}, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            "DryRunOperation",
            "attempt {attempt}: expected DryRunOperation, got {body}"
        );
        assert!(
            !bin_holds(&engine, &volume_id),
            "attempt {attempt}: a dry run must retain nothing, yet the bin holds {volume_id}"
        );
        let (status, body) = describe(&router, &volume_id).await;
        assert_eq!(
            status, 200,
            "attempt {attempt}: a dry run must leave the volume the account's, got {status} \
             with body {body}"
        );
    }

    // The dry runs changed nothing, so the real delete still has a volume to retain.
    let (status, body) = invoke(&router, "DeleteVolume", &[p("VolumeId", volume_id.as_str())]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from the real delete, got {status} with body {body}"
    );
    assert!(
        bin_holds(&engine, &volume_id),
        "case {case:?}: expected the covered delete to retain {volume_id}"
    );
}

/// Whether the Recycle Bin holds `volume_id` right now, read off the store the handler wrote
/// rather than off a report, so an assertion about retention does not depend on a reader.
fn bin_holds(engine: &engine::InMemoryEngine, volume_id: &str) -> bool {
    engine
        .state::<ec2::state::ComAmazonawsEc2Amazonec2State>()
        .lock()
        .volumes_in_recycle_bin
        .contains_key(volume_id)
}
