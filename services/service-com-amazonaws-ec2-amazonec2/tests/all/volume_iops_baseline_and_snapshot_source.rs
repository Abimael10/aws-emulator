// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the IOPS a volume HAS and for the snapshot a volume is RESTORED
//! FROM, driven over the wire the service actually speaks — `ec2Query`, through the crate's
//! shared [`super::ec2_query`] harness.
//!
//! The rules under test are:
//!
//! 1. EVERY VOLUME REPORTS THE IOPS ITS TYPE AND SIZE GIVE IT, not only the types whose
//!    `Iops` a request provisions: "for gp3, io1, and io2 volumes, this represents the
//!    number of IOPS that are provisioned for the volume. For gp2 volumes, this represents
//!    the baseline performance of the volume" (`Volume$Iops`). A `gp2` volume's baseline is
//!    the Amazon EBS User Guide's General Purpose SSD figure — 3 IOPS per GiB, with a
//!    minimum of 100 IOPS and a maximum of 16,000 IOPS — so it follows the volume's SIZE
//!    wherever that size comes from: the request, the snapshot it restores, the block device
//!    mapping a launch materialises, or the size a modification leaves it at. A type with
//!    neither provisioned nor baseline IOPS reports none.
//! 2. A VOLUME RESTORED FROM A SNAPSHOT IS RESTORED FROM A SNAPSHOT THE ACCOUNT HOLDS: the
//!    volume reports that snapshot, takes its size when the request names none ("if you
//!    specify a snapshot, the default is the snapshot size, and you can specify a volume
//!    size that is equal to or larger than the snapshot size",
//!    `CreateVolumeRequest$Size`) and is encrypted when the snapshot is; a `SnapshotId` that
//!    names no snapshot the account holds is refused `InvalidSnapshot.NotFound` with
//!    `CreateVolume`'s own sentence, and a value that is no snapshot id at all is refused as
//!    malformed first.
//!
//! Each table row is a distinct input along what the rules generalise over — the volume
//! type, the size at each bound of the baseline rule and one past it, where the size came
//! from, and which half of the snapshot refusal applies — so no row is the specification.

use ec2::state::{
    CREATE_VOLUME_SNAPSHOT_NOT_FOUND_MESSAGE, INVALID_SNAPSHOT_ID_MALFORMED,
    INVALID_SNAPSHOT_ID_NOT_FOUND,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, error_code, error_message, filters, indexed};
use super::volume_lifecycle::{REGION, p, zone_name};
use super::wire::{child_text, document};

/// One request's form parameters, as the ec2Query harness takes them.
type Params = Vec<(String, String)>;

fn emulator() -> MultiServiceRouter {
    super::ec2_query::emulator().0
}

/// One call that must be served, as its body.
async fn ok(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(String, String)],
) -> String {
    let (status, body) = call(router, REGION, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// Create one volume in the Region's first zone with `extra` beside the zone, as the status
/// and body of the call.
async fn create_volume(router: &MultiServiceRouter, extra: Params) -> (u16, String) {
    let mut params = vec![p("AvailabilityZone", zone_name(0))];
    params.extend(extra);
    call(router, REGION, "CreateVolume", &params).await
}

/// The `iops` `DescribeVolumes` reports for one volume, `None` when the member is absent.
async fn described_iops(router: &MultiServiceRouter, case: &str, volume_id: &str) -> Option<String> {
    let body = ok(
        router,
        case,
        "DescribeVolumes",
        &indexed("VolumeId", &[volume_id]),
    )
    .await;
    element(&body, "iops")
}

/// The baseline IOPS the Amazon EBS User Guide's General Purpose SSD rule gives a `gp2`
/// volume of `size` GiB: 3 IOPS per GiB, with a minimum of 100 and a maximum of 16,000.
///
/// Written out here — the test's own statement of the published rule — for the cases whose
/// size is not the row's own choice but the catalog's or the snapshot's. The table rows below
/// name their expected figures literally instead, so the rule is pinned rather than mirrored.
fn gp2_baseline(size: i64) -> i64 {
    (size * 3).clamp(100, 16_000)
}

/// RULE 1: the IOPS a volume reports follow its TYPE and its SIZE, whether a request
/// provisioned them or not.
#[tokio::test]
async fn a_volume_reports_the_iops_its_type_and_size_give_it() {
    struct Row {
        case: &'static str,
        /// The `VolumeType` the request names, or `None` for a request that names none —
        /// "Default: gp2" (`CreateVolumeRequest$VolumeType`).
        volume_type: Option<&'static str>,
        size: i64,
        /// Whatever else the row's type needs, such as the IOPS io1 and io2 require.
        extra: Params,
        /// The `iops` the create and the describe must both report.
        iops: Option<&'static str>,
    }

    let rows = vec![
        Row {
            case: "gp2 at the smallest size it comes in, under the baseline floor",
            volume_type: Some("gp2"),
            size: 1,
            extra: Vec::new(),
            iops: Some("100"),
        },
        Row {
            case: "gp2 at the largest size the floor still binds at",
            volume_type: Some("gp2"),
            size: 33,
            extra: Vec::new(),
            iops: Some("100"),
        },
        Row {
            case: "gp2 one GiB past the floor",
            volume_type: Some("gp2"),
            size: 34,
            extra: Vec::new(),
            iops: Some("102"),
        },
        Row {
            case: "gp2 inside the linear part of the rule",
            volume_type: Some("gp2"),
            size: 500,
            extra: Vec::new(),
            iops: Some("1500"),
        },
        Row {
            case: "gp2 one GiB below the ceiling",
            volume_type: Some("gp2"),
            size: 5_333,
            extra: Vec::new(),
            iops: Some("15999"),
        },
        Row {
            case: "gp2 at the size the ceiling starts binding at",
            volume_type: Some("gp2"),
            size: 5_334,
            extra: Vec::new(),
            iops: Some("16000"),
        },
        Row {
            case: "gp2 at the largest size it comes in, above the ceiling",
            volume_type: Some("gp2"),
            size: 16_384,
            extra: Vec::new(),
            iops: Some("16000"),
        },
        Row {
            case: "a request naming no type at all takes gp2 and its baseline",
            volume_type: None,
            size: 8,
            extra: Vec::new(),
            iops: Some("100"),
        },
        Row {
            case: "gp3 takes the default its member documents, whatever its size",
            volume_type: Some("gp3"),
            size: 500,
            extra: Vec::new(),
            iops: Some("3000"),
        },
        Row {
            case: "io1 provisions the IOPS the request names",
            volume_type: Some("io1"),
            size: 4,
            extra: vec![p("Iops", "200")],
            iops: Some("200"),
        },
        Row {
            case: "io2 provisions the IOPS the request names",
            volume_type: Some("io2"),
            size: 4,
            extra: vec![p("Iops", "300")],
            iops: Some("300"),
        },
        Row {
            case: "st1 has no IOPS to report",
            volume_type: Some("st1"),
            size: 125,
            extra: Vec::new(),
            iops: None,
        },
        Row {
            case: "sc1 has no IOPS to report",
            volume_type: Some("sc1"),
            size: 125,
            extra: Vec::new(),
            iops: None,
        },
        Row {
            case: "standard has no IOPS to report",
            volume_type: Some("standard"),
            size: 1,
            extra: Vec::new(),
            iops: None,
        },
    ];

    for row in rows {
        let router = emulator();
        let mut params = vec![p("Size", row.size.to_string())];
        if let Some(volume_type) = row.volume_type {
            params.push(p("VolumeType", volume_type));
        }
        params.extend(row.extra);
        let (status, body) = create_volume(&router, params).await;
        assert_eq!(
            status, 200,
            "case {:?}: expected HTTP 200 from CreateVolume, got {status} with body {body}",
            row.case
        );
        let reported = element(&body, "iops");
        assert_eq!(
            reported.as_deref(),
            row.iops,
            "case {:?}: expected CreateVolume to report iops={:?}, got {reported:?}",
            row.case,
            row.iops
        );
        // The figure is a fact of the STORED volume, not of the answer alone.
        let volume_id = element(&body, "volumeId").unwrap_or_default();
        let stored = described_iops(&router, row.case, &volume_id).await;
        assert_eq!(
            stored.as_deref(),
            row.iops,
            "case {:?}: expected DescribeVolumes to report iops={:?}, got {stored:?}",
            row.case,
            row.iops
        );
    }
}

/// RULE 1: a `gp2` volume's baseline follows the size a MODIFICATION leaves it at, and a
/// type change moves it onto the rule of the type it lands on.
#[tokio::test]
async fn the_baseline_follows_the_configuration_a_modification_leaves() {
    struct Row {
        case: &'static str,
        /// The volume the row starts from: its type and size.
        from: (&'static str, i64),
        /// The IOPS it reports before the modification.
        before: Option<&'static str>,
        /// The modification's own parameters.
        modify: Params,
        /// The IOPS it reports after the modification.
        after: Option<&'static str>,
    }

    let rows = vec![
        Row {
            case: "a gp2 volume grown in place takes the baseline of its new size",
            from: ("gp2", 8),
            before: Some("100"),
            modify: vec![p("Size", "200")],
            after: Some("600"),
        },
        Row {
            case: "a gp2 volume grown past the ceiling stops at it",
            from: ("gp2", 100),
            before: Some("300"),
            modify: vec![p("Size", "9000")],
            after: Some("16000"),
        },
        Row {
            case: "a gp3 volume converted to gp2 takes the gp2 baseline of its size",
            from: ("gp3", 400),
            before: Some("3000"),
            modify: vec![p("VolumeType", "gp2")],
            after: Some("1200"),
        },
        Row {
            case: "a gp2 volume converted to st1 has no IOPS left to report",
            from: ("gp2", 200),
            before: Some("600"),
            modify: vec![p("VolumeType", "st1"), p("Size", "500")],
            after: None,
        },
    ];

    for row in rows {
        let router = emulator();
        let (volume_type, size) = row.from;
        let (status, body) = create_volume(
            &router,
            vec![p("VolumeType", volume_type), p("Size", size.to_string())],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: expected HTTP 200 seeding the volume, got {status} with body {body}",
            row.case
        );
        let volume_id = element(&body, "volumeId").unwrap_or_default();
        let before = described_iops(&router, row.case, &volume_id).await;
        assert_eq!(
            before.as_deref(),
            row.before,
            "case {:?}: expected iops={:?} before the modification, got {before:?}",
            row.case,
            row.before
        );

        let mut params = vec![p("VolumeId", volume_id.clone())];
        params.extend(row.modify);
        let modified = ok(&router, row.case, "ModifyVolume", &params).await;
        assert_eq!(
            element(&modified, "targetIops").as_deref(),
            row.after,
            "case {:?}: expected the modification to target iops={:?}, got {modified}",
            row.case,
            row.after
        );
        let after = described_iops(&router, row.case, &volume_id).await;
        assert_eq!(
            after.as_deref(),
            row.after,
            "case {:?}: expected iops={:?} after the modification, got {after:?}",
            row.case,
            row.after
        );
    }
}

/// RULE 1: the volume a LAUNCH materialises from a block device mapping reports the IOPS its
/// type and size give it, the size being the one the mapping asked for.
#[tokio::test]
async fn a_launched_root_volume_reports_the_baseline_of_the_size_its_mapping_asked_for() {
    // One size per part of the baseline rule: one under the floor, one inside the linear
    // range, and the size the catalog's own image publishes (read back, never written here).
    for size in [10_i64, 400] {
        let case = format!("a root mapping of {size} GiB");
        let router = emulator();
        let (image_id, root_device) = published_ebs_image(&router, &case).await;
        let launched = ok(
            &router,
            &case,
            "RunInstances",
            &[
                p("ImageId", image_id.clone()),
                p("MinCount", "1"),
                p("MaxCount", "1"),
                p("BlockDeviceMapping.1.DeviceName", root_device.clone()),
                p("BlockDeviceMapping.1.Ebs.VolumeType", "gp2"),
                p("BlockDeviceMapping.1.Ebs.VolumeSize", size.to_string()),
            ],
        )
        .await;
        let instance_id = element(&launched, "instanceId").unwrap_or_default();
        let volume = instance_volume(&router, &case, &instance_id).await;
        assert_eq!(
            volume.volume_type.as_deref(),
            Some("gp2"),
            "case {case:?}: the root volume must be the gp2 the mapping asked for, got {volume:?}"
        );
        assert_eq!(
            volume.size,
            Some(size),
            "case {case:?}: the root volume must be the size the mapping asked for, got {volume:?}"
        );
        assert_eq!(
            volume.iops,
            Some(gp2_baseline(size)),
            "case {case:?}: expected the gp2 baseline of {size} GiB, got {volume:?}"
        );
    }
}

/// RULE 1 and RULE 2: a volume created FROM A SNAPSHOT reports that snapshot, takes its size
/// when the request names none, and reports the baseline of whichever size it ends up with.
#[tokio::test]
async fn a_volume_restored_from_a_snapshot_reports_the_snapshot_and_its_baseline() {
    // (case, the Size the request names, the size the volume must have, its iops)
    let rows: [(&str, Option<i64>, i64, &str); 3] = [
        ("no size at all takes the snapshot's", None, 60, "180"),
        ("a size equal to the snapshot's", Some(60), 60, "180"),
        ("a size larger than the snapshot's", Some(1_000), 1_000, "3000"),
    ];
    for (case, requested_size, size, iops) in rows {
        let router = emulator();
        let (_source_id, snapshot_id) = snapshot_of_a_volume(&router, case, 60).await;
        let mut params = vec![p("SnapshotId", snapshot_id.clone())];
        if let Some(requested_size) = requested_size {
            params.push(p("Size", requested_size.to_string()));
        }
        let (status, body) = create_volume(&router, params).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 restoring the snapshot, got {status} with \
             body {body}"
        );
        assert_eq!(
            element(&body, "snapshotId").as_deref(),
            Some(snapshot_id.as_str()),
            "case {case:?}: the volume must report the snapshot it restores, got {body}"
        );
        assert_eq!(
            element(&body, "size").as_deref(),
            Some(size.to_string().as_str()),
            "case {case:?}: expected size={size}, got {body}"
        );
        assert_eq!(
            element(&body, "iops").as_deref(),
            Some(iops),
            "case {case:?}: expected iops={iops}, got {body}"
        );
        // Stored, not merely answered: the describe reports the same volume, and the
        // `snapshot-id` filter finds it by the snapshot it restores.
        let volume_id = element(&body, "volumeId").unwrap_or_default();
        let described = ok(
            &router,
            case,
            "DescribeVolumes",
            &filters(&[("snapshot-id", &[snapshot_id.as_str()])]),
        )
        .await;
        assert_eq!(
            element(&described, "volumeId").as_deref(),
            Some(volume_id.as_str()),
            "case {case:?}: the snapshot-id filter must find the restored volume, got {described}"
        );
        assert_eq!(
            element(&described, "iops").as_deref(),
            Some(iops),
            "case {case:?}: expected the stored iops={iops}, got {described}"
        );
    }
}

/// RULE 2: a `SnapshotId` the account holds no snapshot for is refused with `CreateVolume`'s
/// own classification, and nothing is created.
#[tokio::test]
async fn a_snapshot_the_account_does_not_hold_is_refused_with_create_volume_s_own_sentence() {
    // (case, how the row names its snapshot, the code and the message it must be refused
    // with). A `deleted` row names the snapshot the row itself created and then deleted.
    enum Named {
        /// The snapshot this row created and deleted before the restore.
        Deleted,
        /// A literal value the request carries.
        Value(&'static str),
    }
    let rows: [(&str, Named, &str, &str); 4] = [
        (
            "a snapshot the account deleted",
            Named::Deleted,
            INVALID_SNAPSHOT_ID_NOT_FOUND,
            CREATE_VOLUME_SNAPSHOT_NOT_FOUND_MESSAGE,
        ),
        (
            "a well-formed long id the account never held",
            Named::Value("snap-0123456789abcdef0"),
            INVALID_SNAPSHOT_ID_NOT_FOUND,
            CREATE_VOLUME_SNAPSHOT_NOT_FOUND_MESSAGE,
        ),
        (
            "a well-formed short id the account never held",
            Named::Value("snap-1a2b3c4d"),
            INVALID_SNAPSHOT_ID_NOT_FOUND,
            CREATE_VOLUME_SNAPSHOT_NOT_FOUND_MESSAGE,
        ),
        (
            "a value that is no snapshot id at all",
            Named::Value("not-a-snapshot"),
            INVALID_SNAPSHOT_ID_MALFORMED,
            "Value (not-a-snapshot) for snapshot ID is invalid.",
        ),
    ];

    for (case, named, code, message) in rows {
        let router = emulator();
        let snapshot_id = match named {
            Named::Deleted => {
                let (_volume_id, snapshot_id) = snapshot_of_a_volume(&router, case, 8).await;
                ok(
                    &router,
                    case,
                    "DeleteSnapshot",
                    &[p("SnapshotId", snapshot_id.clone())],
                )
                .await;
                snapshot_id
            }
            Named::Value(value) => value.to_string(),
        };
        let volumes_before = volume_ids(&router, case).await;
        let (status, body) = create_volume(&router, vec![p("SnapshotId", snapshot_id)]).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            code,
            "case {case:?}: expected the code {code:?}, got {body}"
        );
        assert_eq!(
            error_message(&body),
            message,
            "case {case:?}: expected the message {message:?}, got {body}"
        );
        assert_eq!(
            volume_ids(&router, case).await,
            volumes_before,
            "case {case:?}: a refused CreateVolume must store nothing"
        );
    }
}

/// RULE 2: a volume smaller than the snapshot it would restore is refused, and nothing is
/// created — "you can specify a volume size that is equal to or larger than the snapshot
/// size" (`CreateVolumeRequest$Size`).
#[tokio::test]
async fn a_volume_smaller_than_its_snapshot_is_refused() {
    let case = "a 4 GiB volume from a 60 GiB snapshot";
    let router = emulator();
    let (_volume_id, snapshot_id) = snapshot_of_a_volume(&router, case, 60).await;
    let volumes_before = volume_ids(&router, case).await;
    let (status, body) = create_volume(
        &router,
        vec![p("SnapshotId", snapshot_id.clone()), p("Size", "4")],
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400, got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body),
        "InvalidParameterValue",
        "case {case:?}: expected InvalidParameterValue, got {body}"
    );
    assert_eq!(
        error_message(&body),
        format!("Volume of size 4GB is smaller than snapshot '{snapshot_id}', expect size >= 60GB"),
        "case {case:?}: expected the smaller-than-the-snapshot sentence, got {body}"
    );
    assert_eq!(
        volume_ids(&router, case).await,
        volumes_before,
        "case {case:?}: a refused CreateVolume must store nothing"
    );
}

/// Every volume id the account holds, in the order the describe reported them.
async fn volume_ids(router: &MultiServiceRouter, case: &str) -> Vec<String> {
    let body = ok(router, case, "DescribeVolumes", &[]).await;
    super::ec2_query::elements(&body, "volumeId")
}

/// A `gp2` volume of `size` GiB and a snapshot of it, as `(volume id, snapshot id)`.
async fn snapshot_of_a_volume(
    router: &MultiServiceRouter,
    case: &str,
    size: i64,
) -> (String, String) {
    let (status, body) = create_volume(
        router,
        vec![p("VolumeType", "gp2"), p("Size", size.to_string())],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 seeding the source volume, got {status} with \
         body {body}"
    );
    let volume_id = element(&body, "volumeId").unwrap_or_default();
    let snapshot = ok(
        router,
        case,
        "CreateSnapshot",
        &[p("VolumeId", volume_id.clone())],
    )
    .await;
    let snapshot_id = element(&snapshot, "snapshotId").unwrap_or_else(|| {
        panic!("case {case:?}: CreateSnapshot answered no snapshotId: {snapshot}")
    });
    (volume_id, snapshot_id)
}

/// An EBS-backed image this account can already see, with the device it holds its root
/// volume at: `(ImageId, RootDeviceName)`, read out of the catalog the service publishes
/// rather than written here.
async fn published_ebs_image(router: &MultiServiceRouter, case: &str) -> (String, String) {
    let body = ok(
        router,
        case,
        "DescribeImages",
        &filters(&[("root-device-type", &["ebs"])]),
    )
    .await;
    let parsed = document(case, &body);
    parsed
        .descendants()
        .filter(|node| node.is_element() && child_text(*node, "imageId").is_some())
        .find_map(|node| {
            let image_id = child_text(node, "imageId")?;
            let root_device = child_text(node, "rootDeviceName")?;
            (!root_device.is_empty()).then_some((image_id, root_device))
        })
        .unwrap_or_else(|| {
            panic!("case {case:?}: the catalog must publish an EBS-backed image: {body}")
        })
}

/// The one volume attached to `instance_id`, as the members this suite asserts on.
#[derive(Debug)]
struct ReportedVolume {
    volume_type: Option<String>,
    size: Option<i64>,
    iops: Option<i64>,
}

/// The volume `DescribeVolumes` reports for the instance `instance_id`, narrowed by the
/// `attachment.instance-id` filter so the row read back is the volume the launch created.
async fn instance_volume(
    router: &MultiServiceRouter,
    case: &str,
    instance_id: &str,
) -> ReportedVolume {
    let body = ok(
        router,
        case,
        "DescribeVolumes",
        &filters(&[("attachment.instance-id", &[instance_id])]),
    )
    .await;
    let number = |name: &str| element(&body, name).and_then(|value| value.parse::<i64>().ok());
    assert!(
        element(&body, "volumeId").is_some(),
        "case {case:?}: the launch must have created a volume for {instance_id}: {body}"
    );
    ReportedVolume {
        volume_type: element(&body, "volumeType"),
        size: number("size"),
        iops: number("iops"),
    }
}
