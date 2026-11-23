// SPDX-License-Identifier: CC0-1.0

//! The volumes a LAUNCH creates BESIDE its root one, driven over the wire the service actually
//! speaks: `ec2Query` — an `application/x-www-form-urlencoded` POST whose body carries
//! `Action=…`, answered with XML (README.md / OPERATIONS.md "Wire protocol").
//!
//! THE RULE: "the block device mapping, which defines the EBS volumes and instance store
//! volumes to attach to the instance at launch"
//! (`RunInstancesRequest$BlockDeviceMappings`) — so every entry naming an Amazon EBS device
//! is a volume the LAUNCH creates, held by the instance at that device from the moment it was
//! launched, with the size, type, IOPS and throughput the entry states. The root device is
//! the one such entry that DEFINES a volume the launch already creates rather than adding
//! another (`tests/all/launch_root_block_device.rs` is that rule); an entry that maps an
//! instance store volume (`BlockDeviceMapping$VirtualName`) names no volume of the account,
//! and one that SUPPRESSES a device (`$NoDevice`) creates nothing.
//!
//! The table spans the class rather than one launch: no mapping at all, one data device, two
//! of them with different types and provisioned members, an instance store mapping, a
//! suppressed device, and a root-device override — and every assertion reads the volumes back
//! through `DescribeVolumes` filtered by the attachment the owning service reports, never
//! against a value written here.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, filters};
use super::wire::{child_text, document, set_items, Param};

/// The Region every call of this suite is signed for.
const REGION: &str = "us-west-2";

/// An AMI id of the right FORM that this account holds no image for: the launch is still
/// served, and its root device is the one the image this crate publishes by default carries.
const UNHELD_AMI: &str = "ami-0123456789abcdef0";

/// How long a poll loop keeps looking for the volumes a launch creates before it gives up — a
/// generous multiple of the service's own span, so a loaded host is never the reason a case
/// fails.
const POLL_DEADLINE: Duration = Duration::from_secs(120);

/// How long a poll loop waits before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

fn p(name: &str, value: &str) -> Param {
    (name.to_string(), value.to_string())
}

/// One call that must be served, as its body.
async fn ok(router: &MultiServiceRouter, case: &str, action: &str, params: &[Param]) -> String {
    let (status, body) = call(router, REGION, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// The `BlockDeviceMapping.<index>` parameters naming `device` with the Amazon EBS members
/// `members` carries.
fn ebs_mapping(index: usize, device: &str, members: &[(&str, &str)]) -> Vec<Param> {
    let mut params = vec![p(&format!("BlockDeviceMapping.{index}.DeviceName"), device)];
    for (member, value) in members {
        params.push(p(
            &format!("BlockDeviceMapping.{index}.Ebs.{member}"),
            value,
        ));
    }
    params
}

/// The `BlockDeviceMapping.<index>` parameters naming `device` with one member of the mapping
/// itself — `VirtualName` or `NoDevice`, neither of which is an Amazon EBS device.
fn plain_mapping(index: usize, device: &str, member: &str, value: &str) -> Vec<Param> {
    vec![
        p(&format!("BlockDeviceMapping.{index}.DeviceName"), device),
        p(&format!("BlockDeviceMapping.{index}.{member}"), value),
    ]
}

/// One instance launched from [`UNHELD_AMI`] with `mappings` beside it, answering its id.
async fn launched(router: &MultiServiceRouter, case: &str, mappings: &[Param]) -> String {
    let mut params = vec![
        p("ImageId", UNHELD_AMI),
        p("MinCount", "1"),
        p("MaxCount", "1"),
    ];
    params.extend_from_slice(mappings);
    let body = ok(router, case, "RunInstances", &params).await;
    element(&body, "instanceId")
        .unwrap_or_else(|| panic!("case {case:?}: the launch must report its instance: {body}"))
}

/// The volumes Amazon EC2 holds attached to `instance_id`, as
/// `device -> (volume id, size, volume type)` — read through the service's own attachment
/// filter, and looked for again until there are `expected` of them or the deadline passes.
async fn volumes_of(
    router: &MultiServiceRouter,
    case: &str,
    instance_id: &str,
    expected: usize,
) -> BTreeMap<String, (String, String, String)> {
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let body = ok(
            router,
            case,
            "DescribeVolumes",
            &filters(&[("attachment.instance-id", &[instance_id])]),
        )
        .await;
        let parsed = document(case, &body);
        let mut held: BTreeMap<String, (String, String, String)> = BTreeMap::new();
        for volume in parsed
            .descendants()
            .filter(|node| node.is_element() && child_text(*node, "volumeId").is_some())
        {
            let volume_id = child_text(volume, "volumeId").unwrap_or_default();
            let size = child_text(volume, "size").unwrap_or_default();
            let volume_type = child_text(volume, "volumeType").unwrap_or_default();
            for attachment in set_items(volume, "attachmentSet") {
                if child_text(attachment, "instanceId").as_deref() != Some(instance_id) {
                    continue;
                }
                let device = child_text(attachment, "device").unwrap_or_default();
                held.insert(device, (volume_id.clone(), size.clone(), volume_type.clone()));
            }
        }
        if held.len() == expected || Instant::now() >= deadline {
            assert_eq!(
                held.len(),
                expected,
                "case {case:?}: expected {expected} volume(s) attached to {instance_id}, \
                 got {held:?} from {body}"
            );
            return held;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// THE RULE: every `BlockDeviceMappings` entry naming an Amazon EBS device is a volume the
/// launch creates, attached at that device with what the entry states; an instance store
/// mapping and a suppressed device create none.
#[tokio::test]
async fn a_launch_creates_the_ebs_volumes_its_block_device_mapping_names() {
    struct Case {
        label: &'static str,
        mappings: Vec<Param>,
        /// The devices the launch creates volumes at BESIDE the root one, with the `(size,
        /// type)` each must hold.
        expected: Vec<(&'static str, &'static str, &'static str)>,
    }
    let cases = vec![
        Case {
            label: "no mapping at all",
            mappings: Vec::new(),
            expected: Vec::new(),
        },
        Case {
            label: "one data device",
            mappings: ebs_mapping(1, "/dev/sdb", &[("VolumeSize", "100"), ("VolumeType", "gp2")]),
            expected: vec![("/dev/sdb", "100", "gp2")],
        },
        Case {
            label: "two data devices of different types",
            mappings: {
                let mut params = ebs_mapping(
                    1,
                    "/dev/sdb",
                    &[
                        ("VolumeSize", "50"),
                        ("VolumeType", "gp3"),
                        ("Iops", "4000"),
                        ("Throughput", "250"),
                    ],
                );
                params.extend(ebs_mapping(
                    2,
                    "/dev/sdc",
                    &[("VolumeSize", "500"), ("VolumeType", "st1")],
                ));
                params
            },
            expected: vec![("/dev/sdb", "50", "gp3"), ("/dev/sdc", "500", "st1")],
        },
        Case {
            label: "an instance store mapping, which is no volume of the account",
            mappings: plain_mapping(1, "/dev/sdb", "VirtualName", "ephemeral0"),
            expected: Vec::new(),
        },
        Case {
            label: "a suppressed device, which creates nothing",
            mappings: plain_mapping(1, "/dev/sdb", "NoDevice", ""),
            expected: Vec::new(),
        },
    ];

    let (router, _engine) = super::ec2_query::emulator();
    for case in &cases {
        let instance_id = launched(&router, case.label, &case.mappings).await;
        // The root volume every EBS-backed launch creates, plus one per named Amazon EBS
        // device: a mapping that names no such device adds nothing to the root one.
        let held = volumes_of(&router, case.label, &instance_id, 1 + case.expected.len()).await;
        for (device, size, volume_type) in &case.expected {
            let (volume_id, held_size, held_type) = held.get(*device).unwrap_or_else(|| {
                panic!(
                    "case {:?}: the launch must create a volume at {device}, got {held:?}",
                    case.label
                )
            });
            assert!(
                volume_id.starts_with("vol-"),
                "case {:?}: the volume at {device} must be one Amazon EC2 minted, got {volume_id}",
                case.label
            );
            assert_eq!(
                (held_size.as_str(), held_type.as_str()),
                (*size, *volume_type),
                "case {:?}: the volume at {device} must be the requested {size} GiB \
                 {volume_type}, got {held:?}",
                case.label
            );
        }
    }
}

/// THE RULE, for the ROOT device: an entry for it DEFINES the volume the launch already
/// creates rather than adding a second one at that device — so a launch that overrides its
/// size still holds exactly one volume, at the size it asked for.
#[tokio::test]
async fn a_root_device_entry_defines_the_root_volume_rather_than_adding_one() {
    let case = "a root device override beside a data device";
    let (router, _engine) = super::ec2_query::emulator();

    // The root device the image publishes, read off a launch rather than written here.
    let probe = launched(&router, case, &[]).await;
    let probe_volumes = volumes_of(&router, case, &probe, 1).await;
    let root_device = probe_volumes
        .keys()
        .next()
        .expect("the probe launch holds its root volume")
        .clone();

    let mut mappings = ebs_mapping(1, &root_device, &[("VolumeSize", "64")]);
    mappings.extend(ebs_mapping(
        2,
        "/dev/sdb",
        &[("VolumeSize", "30"), ("VolumeType", "gp2")],
    ));
    let instance_id = launched(&router, case, &mappings).await;
    let held = volumes_of(&router, case, &instance_id, 2).await;
    let (_, root_size, _) = held.get(&root_device).unwrap_or_else(|| {
        panic!("case {case:?}: the instance must hold a volume at {root_device}, got {held:?}")
    });
    assert_eq!(
        root_size, "64",
        "case {case:?}: the root device entry must define the root volume, got {held:?}"
    );
    let (_, data_size, data_type) = held.get("/dev/sdb").unwrap_or_else(|| {
        panic!("case {case:?}: the data device must be created too, got {held:?}")
    });
    assert_eq!(
        (data_size.as_str(), data_type.as_str()),
        ("30", "gp2"),
        "case {case:?}: the data device must be the requested 30 GiB gp2, got {held:?}"
    );
}
