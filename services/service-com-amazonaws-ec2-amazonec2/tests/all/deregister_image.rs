// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `DeregisterImage`, driven over the wire the service actually speaks
//! — `ec2Query`, through the crate's shared [`super::ec2_query`] harness.
//!
//! The rules under test are:
//!
//! 1. a deregister takes the AMI out of the images the account can see — "a deregistered AMI
//!    can't be used to launch new instances" (`DeregisterImage`) — and nothing of the account
//!    still DEPENDS on it afterwards: the snapshots the AMI restored its devices from can be
//!    deleted, which "you must first deregister the AMI before you can delete the snapshot"
//!    (`DeleteSnapshot`) is the other half of;
//! 2. "when deregistering an EBS-backed AMI, you can optionally delete its associated
//!    snapshots at the same time. However, if a snapshot is associated with multiple AMIs, it
//!    won't be deleted even if specified for deletion, although the AMI will still be
//!    deregistered" (`DeregisterImage`): the per-snapshot result says which of the two
//!    happened for each snapshot, a deregister that asked for no deletion deletes nothing and
//!    reports no results, and a snapshot a Recycle Bin retention rule covers is RETAINED in
//!    the bin rather than permanently deleted, exactly as one `DeleteSnapshot` deletes is;
//! 3. a request this account cannot serve is refused and deregisters nothing — an id that is
//!    no AMI id, a well-formed id the account holds no image under, an image the account does
//!    not own, and a `DryRun` of a request that WOULD have succeeded.
//!
//! Each table row is a distinct input along what the rules generalise over — how many devices
//! the AMI maps, whether another AMI shares a snapshot with it, whether the deletion is asked
//! for, whether the AMI is Amazon EBS-backed or instance store-backed, and which way a
//! request can fail to name an image of this account — so no row is the specification.

use ec2::state::{
    ComAmazonawsEc2Amazonec2State as Ec2State, DRY_RUN_OPERATION, INSTANCE_STORE_ROOT_DEVICE_NAME,
    INVALID_SNAPSHOT_IN_USE, LINUX_ROOT_DEVICE_NAME, SNAPSHOT_RETURN_CODE_SKIPPED,
    SNAPSHOT_RETURN_CODE_SUCCESS,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, elements, error_code, error_message};
use super::rbin_retention_rules::{
    RESOURCE_TYPE_SNAPSHOT, RetentionRule, create_available_rule, emulator_with_retention_rules,
};
use super::volume_lifecycle::{REGION, p, zone_name};
use super::wire::{child_text, document, set_items};

/// One request's form parameters, as the ec2Query harness takes them.
type Params = Vec<(String, String)>;

/// The HTTP status EC2 answers a refusal about the request with.
const REFUSAL_STATUS: u16 = 400;

/// The HTTP status `DryRunOperation` carries: the request was refused because a precondition
/// the CALLER set holds, not because anything in it was wrong.
const DRY_RUN_STATUS: u16 = 412;

/// EC2's code for a value that is not an id of the resource kind the request names it as, and
/// the one this operation answers for a well-formed AMI id the account holds no image under.
const INVALID_AMI_ID_MALFORMED: &str = "InvalidAMIID.Malformed";

/// EC2's code for a request the caller may not make.
const AUTH_FAILURE: &str = "AuthFailure";

/// One `action` call signed for the Region this suite works in.
async fn op(router: &MultiServiceRouter, action: &str, params: &[(String, String)]) -> (u16, String) {
    call(router, REGION, action, params).await
}

/// Mint one snapshot of `size` GiB of this account, through the operations that create one,
/// and answer its id — a snapshot a register's block device mapping restores from.
async fn snapshot_of(router: &MultiServiceRouter, case: &str, size: i32) -> String {
    snapshot_of_with_tags(router, case, size, &[]).await
}

/// [`snapshot_of`], with the tags the snapshot carries — which is what a tag-level Recycle Bin
/// retention rule matches on.
async fn snapshot_of_with_tags(
    router: &MultiServiceRouter,
    case: &str,
    size: i32,
    tags: &[(&str, &str)],
) -> String {
    let (status, body) = op(
        router,
        "CreateVolume",
        &[
            p("AvailabilityZone", zone_name(0)),
            p("Size", size.to_string()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateVolume, got {status} with body {body}"
    );
    let volume_id =
        element(&body, "volumeId").unwrap_or_else(|| panic!("case {case:?}: no volumeId in {body}"));
    let mut params = vec![p("VolumeId", &volume_id)];
    if !tags.is_empty() {
        params.push(p("TagSpecification.1.ResourceType", "snapshot"));
        for (index, (key, value)) in tags.iter().enumerate() {
            let at = index + 1;
            params.push(p(&format!("TagSpecification.1.Tag.{at}.Key"), *key));
            params.push(p(&format!("TagSpecification.1.Tag.{at}.Value"), *value));
        }
    }
    let (status, body) = op(router, "CreateSnapshot", &params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateSnapshot, got {status} with body {body}"
    );
    element(&body, "snapshotId")
        .unwrap_or_else(|| panic!("case {case:?}: no snapshotId in {body}"))
}

/// Register one AMI whose devices restore from `snapshots`, in order, the first being the
/// root device, and answer its `ImageId`.
///
/// An empty `snapshots` registers an instance store-backed AMI, whose root device restores no
/// snapshot at all.
async fn register(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    snapshots: &[String],
) -> String {
    let mut params: Params = vec![p("Name", name)];
    if snapshots.is_empty() {
        params.push(p("RootDeviceName", INSTANCE_STORE_ROOT_DEVICE_NAME));
    } else {
        params.push(p("RootDeviceName", LINUX_ROOT_DEVICE_NAME));
        for (index, snapshot_id) in snapshots.iter().enumerate() {
            // The root device first, then one data device per further snapshot.
            let device_name = if index == 0 {
                LINUX_ROOT_DEVICE_NAME.to_string()
            } else {
                format!("/dev/sd{}", (b'b' + index as u8 - 1) as char)
            };
            params.push(p(
                &format!("BlockDeviceMapping.{}.DeviceName", index + 1),
                device_name,
            ));
            params.push(p(
                &format!("BlockDeviceMapping.{}.Ebs.SnapshotId", index + 1),
                snapshot_id.clone(),
            ));
        }
    }
    let (status, body) = op(router, "RegisterImage", &params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from RegisterImage, got {status} with body {body}"
    );
    element(&body, "imageId").unwrap_or_else(|| panic!("case {case:?}: no imageId in {body}"))
}

/// Every image id the account's own `DescribeImages` reports for the images it OWNS — the set
/// a deregister takes exactly one id out of, and a refused one leaves alone.
async fn own_image_ids(router: &MultiServiceRouter, case: &str) -> Vec<String> {
    let (status, body) = op(router, "DescribeImages", &[p("Owner.1", "self")]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeImages, got {status} with body {body}"
    );
    elements(&body, "imageId")
}

/// Every snapshot id `DescribeSnapshots` reports for the snapshots the account owns.
async fn own_snapshot_ids(router: &MultiServiceRouter, case: &str) -> Vec<String> {
    let (status, body) = op(router, "DescribeSnapshots", &[p("Owner.1", "self")]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeSnapshots, got {status} with body {body}"
    );
    elements(&body, "snapshotId")
}

/// One `DeregisterImage` call carrying `params`.
async fn deregister(
    router: &MultiServiceRouter,
    image_id: &str,
    extra: &[(String, String)],
) -> (u16, String) {
    let mut params = vec![p("ImageId", image_id)];
    params.extend(extra.iter().cloned());
    op(router, "DeregisterImage", &params).await
}

/// The `(snapshotId, returnCode)` pairs a `DeregisterImage` body reports, in document order.
fn snapshot_results(case: &str, body: &str) -> Vec<(String, String)> {
    let parsed = document(case, body);
    set_items(parsed.root_element(), "deleteSnapshotResultSet")
        .into_iter()
        .map(|item| {
            (
                child_text(item, "snapshotId").unwrap_or_default(),
                child_text(item, "returnCode").unwrap_or_default(),
            )
        })
        .collect()
}

/// RULE 1 and 2: a deregister takes the AMI out of the account's images, deletes the
/// snapshots it was asked to delete and skips the ones another AMI still uses, and leaves
/// every snapshot deletable afterwards.
///
/// The rows differ along how many devices the AMI maps, whether another registered AMI shares
/// a snapshot with it, whether the deletion is asked for, and whether the AMI restores any
/// snapshot at all.
#[tokio::test]
async fn a_deregister_takes_the_ami_out_and_settles_its_snapshots() {
    let (router, _engine) = super::ec2_query::emulator();

    // (case, how many snapshots the AMI maps, whether a second AMI shares the first of them,
    //  whether the request asks for the snapshots to be deleted).
    let rows: Vec<(&str, usize, bool, bool)> = vec![
        (
            "a one-device AMI deregistered without deleting its snapshot",
            1,
            false,
            false,
        ),
        (
            "a one-device AMI that deletes its snapshot with it",
            1,
            false,
            true,
        ),
        (
            "a two-device AMI that deletes both its snapshots with it",
            2,
            false,
            true,
        ),
        (
            "a two-device AMI whose first snapshot another AMI also restores",
            2,
            true,
            true,
        ),
        (
            "an instance store-backed AMI, which restores no snapshot",
            0,
            false,
            true,
        ),
    ];

    for (index, (case, devices, shared, delete_snapshots)) in rows.into_iter().enumerate() {
        let mut snapshots = Vec::new();
        for device in 0..devices {
            // Sizes differ per device, so the snapshots of one row are distinct resources.
            let size = 8 + i32::try_from(device).unwrap_or_default();
            snapshots.push(snapshot_of(&router, case, size).await);
        }
        let image_id = register(
            &router,
            case,
            &format!("deregister-image-row-{index}"),
            &snapshots,
        )
        .await;
        // The AMI that keeps the first snapshot alive, where the row has one.
        let sharer = if shared {
            Some(
                register(
                    &router,
                    case,
                    &format!("deregister-image-row-{index}-sharer"),
                    &snapshots[..1],
                )
                .await,
            )
        } else {
            None
        };

        // While the AMI is registered, the snapshot it restores its root device from cannot
        // be deleted: that is what deregistering it has to clear.
        if let Some(root_snapshot) = snapshots.first() {
            let (status, body) =
                op(&router, "DeleteSnapshot", &[p("SnapshotId", root_snapshot)]).await;
            assert_eq!(
                status, REFUSAL_STATUS,
                "case {case:?}: expected HTTP {REFUSAL_STATUS} deleting {root_snapshot} while \
                 {image_id} is registered, got {status} with body {body}"
            );
            assert_eq!(
                error_code(&body),
                INVALID_SNAPSHOT_IN_USE,
                "case {case:?}: expected {INVALID_SNAPSHOT_IN_USE} for the in-use \
                 {root_snapshot}, got {body}"
            );
        }

        let extra = if delete_snapshots {
            vec![p("DeleteAssociatedSnapshots", "true")]
        } else {
            Vec::new()
        };
        let (status, body) = deregister(&router, &image_id, &extra).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DeregisterImage of {image_id}, got {status} \
             with body {body}"
        );
        assert_eq!(
            element(&body, "return").as_deref(),
            Some("true"),
            "case {case:?}: expected <return>true</return>, got {body}"
        );

        // The AMI is no longer one of the images the account can see.
        let images = own_image_ids(&router, case).await;
        assert!(
            !images.contains(&image_id),
            "case {case:?}: expected {image_id} to be gone from the account's images, got \
             {images:?}"
        );
        if let Some(sharer) = sharer.as_deref() {
            assert!(
                images.iter().any(|held| held == sharer),
                "case {case:?}: expected the other AMI {sharer} to survive, got {images:?}"
            );
        }

        // One result per snapshot the AMI restored a device from, and only for a request that
        // asked for them to be deleted: `skipped` for the one another AMI still restores,
        // `success` for every other.
        let results = snapshot_results(case, &body);
        let expected: Vec<(String, String)> = if delete_snapshots {
            snapshots
                .iter()
                .enumerate()
                .map(|(device, snapshot_id)| {
                    let code = if shared && device == 0 {
                        SNAPSHOT_RETURN_CODE_SKIPPED
                    } else {
                        SNAPSHOT_RETURN_CODE_SUCCESS
                    };
                    (snapshot_id.clone(), code.to_string())
                })
                .collect()
        } else {
            Vec::new()
        };
        assert_eq!(
            results, expected,
            "case {case:?}: expected the snapshot results {expected:?}, got {results:?}"
        );

        // The state behind those results: a deleted snapshot is gone from the account's
        // snapshots, and a skipped one is still there.
        let held = own_snapshot_ids(&router, case).await;
        for (device, snapshot_id) in snapshots.iter().enumerate() {
            let deleted = delete_snapshots && !(shared && device == 0);
            assert_eq!(
                !held.contains(snapshot_id),
                deleted,
                "case {case:?}: expected {snapshot_id} deleted={deleted}, got the account's \
                 snapshots {held:?}"
            );
            if deleted {
                continue;
            }
            // A snapshot the deregister left behind is deletable now that no registered AMI
            // restores a device from it — unless another AMI still does, which is the row
            // whose result was `skipped`.
            let (status, body) =
                op(&router, "DeleteSnapshot", &[p("SnapshotId", snapshot_id)]).await;
            let (expected_status, expected_code) = if shared && device == 0 {
                (REFUSAL_STATUS, INVALID_SNAPSHOT_IN_USE)
            } else {
                (200, "")
            };
            assert_eq!(
                status, expected_status,
                "case {case:?}: expected HTTP {expected_status} deleting {snapshot_id} after \
                 the deregister, got {status} with body {body}"
            );
            if !expected_code.is_empty() {
                assert_eq!(
                    error_code(&body),
                    expected_code,
                    "case {case:?}: expected {expected_code} for {snapshot_id}, got {body}"
                );
            }
        }
    }
}

/// One row of the retention table: the case, the rule's resource tags, the tags the snapshot
/// carries, and whether that rule therefore retains it.
type RetentionRow = (
    &'static str,
    &'static [(&'static str, &'static str)],
    &'static [(&'static str, &'static str)],
    bool,
);

/// RULE 2 (the other half): a snapshot a Recycle Bin retention rule covers is RETAINED rather
/// than permanently deleted when a deregister deletes it, exactly as it is when
/// `DeleteSnapshot` deletes it — the delete reports the deletion it carried out, the snapshot
/// stops being one of the account's, and the bin holds it.
///
/// The rows differ along which rule covers the snapshot: a Region-level rule that retains every
/// deleted snapshot of the Region, a tag-level rule the snapshot carries the pair of, and a
/// tag-level rule whose pair it does not carry (the control, which is retained by nothing and
/// is therefore permanently deleted).
#[tokio::test]
async fn a_retained_snapshot_deleted_with_its_ami_moves_into_the_recycle_bin() {
    // (case, the rule's resource tags, the tags the snapshot carries, whether the rule
    //  retains it).
    let rows: Vec<RetentionRow> = vec![
        (
            "a Region-level rule, which retains every deleted snapshot",
            &[],
            &[],
            true,
        ),
        (
            "a tag-level rule whose pair the snapshot carries",
            &[("retain", "yes")],
            &[("retain", "yes")],
            true,
        ),
        (
            "a tag-level rule whose pair the snapshot does not carry",
            &[("retain", "yes")],
            &[("retain", "no")],
            false,
        ),
    ];

    for (index, (case, rule_tags, snapshot_tags, retained)) in rows.into_iter().enumerate() {
        let (router, _engine) = emulator_with_retention_rules();
        create_available_rule(
            &router,
            case,
            &RetentionRule {
                resource_type: RESOURCE_TYPE_SNAPSHOT,
                // Long enough that nothing here reaches its end, and inside the bound the
                // service that owns rules publishes for a snapshot rule.
                retention_days: 1,
                resource_tags: rule_tags,
                exclude_resource_tags: &[],
                region: REGION,
            },
        )
        .await;

        let snapshot_id = snapshot_of_with_tags(&router, case, 8, snapshot_tags).await;
        let image_id = register(
            &router,
            case,
            &format!("deregister-image-retained-{index}"),
            std::slice::from_ref(&snapshot_id),
        )
        .await;

        let (status, body) = deregister(
            &router,
            &image_id,
            &[p("DeleteAssociatedSnapshots", "true")],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DeregisterImage of {image_id}, got {status} \
             with body {body}"
        );
        assert_eq!(
            snapshot_results(case, &body),
            vec![(
                snapshot_id.clone(),
                SNAPSHOT_RETURN_CODE_SUCCESS.to_string()
            )],
            "case {case:?}: expected the deletion of {snapshot_id} to be reported as carried \
             out, got {body}"
        );

        // Either way the snapshot is no longer one of the account's...
        let held = own_snapshot_ids(&router, case).await;
        assert!(
            !held.contains(&snapshot_id),
            "case {case:?}: expected {snapshot_id} to be gone from the account's snapshots, got \
             {held:?}"
        );
        // ...and whether the bin holds it is what the rule decided.
        let (status, body) = op(&router, "ListSnapshotsInRecycleBin", &[]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from ListSnapshotsInRecycleBin, got {status} with \
             body {body}"
        );
        let in_the_bin = elements(&body, "snapshotId");
        assert_eq!(
            in_the_bin.contains(&snapshot_id),
            retained,
            "case {case:?}: expected the Recycle Bin to hold {snapshot_id} = {retained}, got \
             {in_the_bin:?}"
        );
    }
}

/// RULE 3: a request this account cannot serve is refused and deregisters nothing.
///
/// The rows differ along the way the request fails: a value that is no AMI id at all, a
/// well-formed id the account holds no image under, and an image the account can see but does
/// not own — one of the published AMIs. The `DryRun` of a request that WOULD have succeeded is
/// the last case, asserted against the account's own image.
#[tokio::test]
async fn a_deregister_this_account_cannot_serve_is_refused() {
    let (router, _engine) = super::ec2_query::emulator();
    let case = "the refusal table";

    let snapshot_id = snapshot_of(&router, case, 8).await;
    let own_image = register(
        &router,
        case,
        "deregister-image-refusals",
        std::slice::from_ref(&snapshot_id),
    )
    .await;
    // An AMI the account can SEE but does not own: one of the images Amazon publishes, read
    // out of the service's own state so the test cannot invent an id.
    let published = Ec2State::default()
        .images
        .keys()
        .next()
        .cloned()
        .expect("a fresh account must publish at least one image");

    // (case, the id the request names, the expected code).
    let rows: Vec<(&str, &str, &str)> = vec![
        ("no prefix at all", "not-an-image", INVALID_AMI_ID_MALFORMED),
        ("the prefix alone", "ami-", INVALID_AMI_ID_MALFORMED),
        (
            "a suffix of neither published length",
            "ami-0123456",
            INVALID_AMI_ID_MALFORMED,
        ),
        (
            "another resource's prefix",
            "snap-04fd27e9e8a1b2c3d",
            INVALID_AMI_ID_MALFORMED,
        ),
        (
            "a well-formed id the account holds no image under",
            "ami-04fd27e9e8a1b2c3d",
            INVALID_AMI_ID_MALFORMED,
        ),
        ("an image the account does not own", &published, AUTH_FAILURE),
    ];

    for (row, image_id, expected_code) in rows {
        let (status, body) = deregister(&router, image_id, &[]).await;
        assert_eq!(
            status, REFUSAL_STATUS,
            "case {row:?}: expected HTTP {REFUSAL_STATUS} from DeregisterImage of \
             {image_id:?}, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            expected_code,
            "case {row:?}: expected {expected_code} for {image_id:?}, got {body}"
        );
        assert!(
            !error_message(&body).is_empty(),
            "case {row:?}: expected the refusal to carry a message, got {body}"
        );
    }

    // A dry run of the request that WOULD have succeeded: the permission verdict, and nothing
    // deregistered.
    let (status, body) = deregister(&router, &own_image, &[p("DryRun", "true")]).await;
    assert_eq!(
        status, DRY_RUN_STATUS,
        "case {case:?}: expected HTTP {DRY_RUN_STATUS} from a dry-run deregister, got {status} \
         with body {body}"
    );
    assert_eq!(
        error_code(&body),
        DRY_RUN_OPERATION,
        "case {case:?}: expected {DRY_RUN_OPERATION} from a dry-run deregister, got {body}"
    );

    // Every refusal left the account's images and snapshots as they were.
    let images = own_image_ids(&router, case).await;
    assert!(
        images.contains(&own_image),
        "case {case:?}: expected {own_image} to be the account's image still, got {images:?}"
    );
    let held = own_snapshot_ids(&router, case).await;
    assert!(
        held.contains(&snapshot_id),
        "case {case:?}: expected {snapshot_id} to be the account's snapshot still, got {held:?}"
    );
    assert!(
        own_image_ids(&router, case).await.contains(&own_image),
        "case {case:?}: a refused deregister changes nothing, so a second read must report \
         {own_image} too"
    );
}
