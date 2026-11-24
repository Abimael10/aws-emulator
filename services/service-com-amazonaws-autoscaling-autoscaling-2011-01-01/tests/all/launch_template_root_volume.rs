// SPDX-License-Identifier: CC0-1.0

//! The ROOT VOLUME an instance an Auto Scaling group launched boots from, which is the one
//! the launch-template version it was launched from describes.
//!
//! RULE under test: "the launch template contains the parameters to launch an instance"
//! (`LaunchTemplateVersion$LaunchTemplateData`), and the block device mapping a version
//! carries for the AMI's own root device DEFINES the volume that launch creates — every
//! Amazon EBS member the mapping names (its size, type, IOPS, throughput, encryption, key
//! and whether it goes with the instance) reaches the created volume, and the AMI's own
//! mapping answers only the members the version leaves out. A capacity change launches from
//! that version, so the volume a group's instance holds is the volume a launch naming the
//! same version holds; the device the volume is attached at stays the AMI's root device,
//! because a mapping does not move it.
//!
//! The rows span what the rule generalises over: which Amazon EBS members the version
//! declares (one, several, all of a type's performance set), the values they carry (the
//! published size bounds of the type they name, a provisioned-IOPS type beside gp3, a
//! customer managed key, `DeleteOnTermination` both ways), the device the mapping names (the
//! AMI's root device or another one), and where the group names the template (its own
//! `LaunchTemplate` or the launch template of a `MixedInstancesPolicy`). What a version
//! leaves out is never written out here: it is read back from a launch of a version that
//! declares nothing, and the last test pins the whole volume against what `RunInstances`
//! creates from the same version.
//!
//! Both services are mounted on ONE engine and driven over their own wire protocols —
//! awsQuery for Auto Scaling, ec2Query for Amazon EC2 — so every assertion is made where a
//! client makes it.

use std::time::{Duration, Instant};

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_ec2_amazonec2::state as ec2;

use crate::support::{borrowed, ec2_query, emulator, p, query, tag_value, tag_values, REGION};

/// The image every launch template in this suite carries, in the long id form Amazon EC2
/// issues one in — one of the account's published AMIs, built for the architecture
/// [`INSTANCE_TYPE`] runs, since Amazon EC2 carries out no launch whose AMI and instance type
/// are built for architectures that disagree. The root device facts a launch falls back to
/// are read back here rather than written out.
const IMAGE_ID: &str = "ami-00000000000000031";

/// The instance type every launch template in this suite carries.
const INSTANCE_TYPE: &str = "t3.micro";

/// How long a case keeps looking for the volume a launch creates before it gives up —
/// generous on purpose, so a loaded host is never the reason a case fails.
const DEADLINE: Duration = Duration::from_secs(120);

/// How long one look-again cycle pauses before asking again.
const POLL_PAUSE: Duration = Duration::from_millis(25);

/// The zone a group with no subnets places its instances in: the first of the Region's own
/// catalog, never written out here.
fn zone() -> String {
    ec2::availability_zones_in_region(REGION)
        .first()
        .map(|zone| zone.zone_name.clone())
        .expect("the Region has Availability Zones")
}

/// Where the group names the launch template.
#[derive(Debug, Clone, Copy)]
enum Source {
    /// The group's own `LaunchTemplate`.
    Own,
    /// The launch template of its `MixedInstancesPolicy`.
    MixedPolicy,
}

impl Source {
    /// The parameter the template's name is carried under.
    fn member(self) -> &'static str {
        match self {
            Self::Own => "LaunchTemplate.LaunchTemplateName",
            Self::MixedPolicy => "MixedInstancesPolicy.LaunchTemplate.\
                                  LaunchTemplateSpecification.LaunchTemplateName",
        }
    }
}

/// The element the created volume reports one `EbsBlockDevice` member under.
///
/// Tabulated, because a volume states these facts under its OWN member names rather than the
/// mapping's: `Volume$Size`, `Volume$VolumeType`, `Volume$Iops`, `Volume$Throughput`,
/// `Volume$Encrypted` and `Volume$KmsKeyId`, plus the attachment's own
/// `VolumeAttachment$DeleteOnTermination` — each reported under that member's name with a
/// lowercased first letter, which is the ec2Query envelope's spelling of a member.
fn reported_element(member: &str) -> &'static str {
    match member {
        "VolumeSize" => "size",
        "VolumeType" => "volumeType",
        "Iops" => "iops",
        "Throughput" => "throughput",
        "Encrypted" => "encrypted",
        "KmsKeyId" => "kmsKeyId",
        "DeleteOnTermination" => "deleteOnTermination",
        other => panic!("no reported element is known for the Amazon EBS member {other:?}"),
    }
}

/// Create one launch template whose data carries a block device mapping for `device` with
/// the Amazon EBS members `ebs` names — or no mapping at all when `device` is `None`.
async fn create_launch_template(
    router: &MultiServiceRouter,
    name: &str,
    device: Option<&str>,
    ebs: &[(&str, &str)],
) {
    let mut params = vec![
        p("LaunchTemplateName", name),
        p("LaunchTemplateData.ImageId", IMAGE_ID),
        p("LaunchTemplateData.InstanceType", INSTANCE_TYPE),
    ];
    if let Some(device) = device {
        params.push(p("LaunchTemplateData.BlockDeviceMapping.1.DeviceName", device));
        for (member, value) in ebs {
            params.push(p(
                &format!("LaunchTemplateData.BlockDeviceMapping.1.Ebs.{member}"),
                *value,
            ));
        }
    }
    let (status, body) = ec2_query(router, "CreateLaunchTemplate", &params).await;
    assert_eq!(
        status, 200,
        "CreateLaunchTemplate for {name:?} expected 200, got {status}: {body}"
    );
}

/// Create a group of capacity one from `template` and answer the id of the instance it
/// launched.
async fn launched_instance(
    router: &MultiServiceRouter,
    group: &str,
    template: &str,
    source: Source,
) -> String {
    let params = [
        p("AutoScalingGroupName", group),
        p(source.member(), template),
        p("MinSize", "1"),
        p("MaxSize", "1"),
        p("DesiredCapacity", "1"),
        p("AvailabilityZones.member.1", zone()),
    ];
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "CreateAutoScalingGroup for {group:?} expected 200, got {status}: {body}"
    );
    let (status, described) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeAutoScalingGroups expected 200, got {status}: {described}"
    );
    let instances = tag_values(&described, "InstanceId");
    assert_eq!(
        instances.len(),
        1,
        "the group must have launched its one instance, got {described}"
    );
    instances[0].clone()
}

/// The body `DescribeVolumes` answers for the ONE volume `instance_id` holds, looked for
/// again until it is there or [`DEADLINE`] passes.
async fn root_volume(router: &MultiServiceRouter, case: &str, instance_id: &str) -> String {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (status, body) = ec2_query(
            router,
            "DescribeVolumes",
            &[
                p("Filter.1.Name", "attachment.instance-id"),
                p("Filter.1.Value.1", instance_id),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeVolumes expected 200, got {status}: {body}"
        );
        // One root volume per launch: a second one would mean two volumes answer for the
        // same device. Counted on the ARN, which one volume reports exactly once — its
        // `volumeId` is reported again inside its own attachment.
        let volumes = tag_values(&body, "volumeArn");
        if volumes.len() >= 2 {
            panic!("case {case:?}: the instance must hold exactly one volume, got {body}");
        }
        if volumes.len() == 1 {
            return body;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the group's launch must give its instance a root volume, got {body}"
        );
        tokio::time::sleep(POLL_PAUSE).await;
    }
}

/// The root volume of a group launched from a version that declares NO block device mapping:
/// the device it is attached at, its size and its volume type, as Amazon EC2's own defaults
/// leave them — read back rather than written out here.
async fn ami_root_volume(router: &MultiServiceRouter, index: usize) -> (String, String, String) {
    let case = "a version with no block device mapping at all";
    let template = format!("lt-root-baseline-{index}");
    create_launch_template(router, &template, None, &[]).await;
    let instance_id =
        launched_instance(router, &format!("root-baseline-{index}"), &template, Source::Own).await;
    let body = root_volume(router, case, &instance_id).await;
    let device = tag_value(&body, "device");
    assert!(
        !device.is_empty(),
        "{case}: the volume must report the device it is attached at, got {body}"
    );
    (
        device,
        tag_value(&body, "size"),
        tag_value(&body, "volumeType"),
    )
}

/// One row: the Amazon EBS members the version's root-device mapping declares, and where the
/// group names the template.
struct Row {
    case: &'static str,
    /// `(EbsBlockDevice member, value)` the version declares for the AMI's root device. Every
    /// one of them must reach the created volume.
    ebs: &'static [(&'static str, &'static str)],
    source: Source,
}

/// Every Amazon EBS member a version's root-device mapping names reaches the volume the
/// group's launch creates.
#[tokio::test]
async fn every_ebs_member_of_the_versions_root_mapping_reaches_the_volume() {
    // The sizes are the published bounds of the type each row names ("Valid sizes: gp2: 1 -
    // 16,384 GiB gp3: 1 - 65,536 GiB io1: 4 - 16,384 GiB", `CreateVolumeRequest$Size`), and
    // the IOPS and throughput figures sit inside the published ranges of their own types
    // ("gp3: 3,000 (default) - 80,000 IOPS io1: 100 - 64,000 IOPS",
    // `CreateVolumeRequest$Iops`; "125 - 1000 MiB/s", `CreateVolumeRequest$Throughput`).
    let rows = [
        Row {
            case: "a size of its own and nothing else",
            ebs: &[("VolumeSize", "100")],
            source: Source::Own,
        },
        Row {
            case: "a size and a type of its own",
            ebs: &[("VolumeSize", "30"), ("VolumeType", "gp2")],
            source: Source::Own,
        },
        Row {
            case: "the published minimum size of the type it names",
            ebs: &[("VolumeSize", "1"), ("VolumeType", "gp3")],
            source: Source::Own,
        },
        Row {
            case: "the published maximum size of the type it names, through a mixed \
                   instances policy",
            ebs: &[("VolumeSize", "65536"), ("VolumeType", "gp3")],
            source: Source::MixedPolicy,
        },
        Row {
            case: "every performance member of a gp3 root volume, kept past termination",
            ebs: &[
                ("VolumeSize", "200"),
                ("VolumeType", "gp3"),
                ("Iops", "4000"),
                ("Throughput", "250"),
                ("DeleteOnTermination", "false"),
            ],
            source: Source::Own,
        },
        Row {
            case: "a provisioned-IOPS root volume, through a mixed instances policy",
            ebs: &[
                ("VolumeSize", "20"),
                ("VolumeType", "io1"),
                ("Iops", "600"),
                ("DeleteOnTermination", "true"),
            ],
            source: Source::MixedPolicy,
        },
        Row {
            case: "an encrypted root volume under a named key",
            ebs: &[
                ("VolumeSize", "64"),
                ("Encrypted", "true"),
                ("KmsKeyId", "alias/aws/ebs"),
            ],
            source: Source::Own,
        },
    ];

    let (router, _engine) = emulator();
    let (root_device, _size, _volume_type) = ami_root_volume(&router, 0).await;

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let template = format!("lt-root-{index}");
        create_launch_template(&router, &template, Some(&root_device), row.ebs).await;
        let instance_id =
            launched_instance(&router, &format!("root-{index}"), &template, row.source).await;
        let body = root_volume(&router, case, &instance_id).await;

        for (member, value) in row.ebs {
            let element = reported_element(member);
            assert_eq!(
                tag_value(&body, element),
                *value,
                "case {case:?}: the volume must report the {member} the version declares \
                 ({value}) as {element}, got {body}"
            );
        }
        // A mapping defines the root volume; it never moves the root device.
        assert_eq!(
            tag_value(&body, "device"),
            root_device,
            "case {case:?}: the volume must be attached at the AMI's root device \
             ({root_device}), got {body}"
        );
    }
}

/// One row of the fallback table: the device the version's mapping names and the Amazon EBS
/// members it declares, where neither the size nor the type is among them.
struct FallbackRow {
    case: &'static str,
    /// `None` for a version carrying no mapping at all; `Some(true)` for one naming the
    /// AMI's own root device, `Some(false)` for one naming another device.
    device: Option<bool>,
    ebs: &'static [(&'static str, &'static str)],
    source: Source,
}

/// A member the version's mapping leaves out is the AMI's own, and a mapping for another
/// device leaves the root volume entirely alone.
#[tokio::test]
async fn a_member_the_version_leaves_out_falls_back_to_the_ami() {
    let rows = [
        FallbackRow {
            case: "a version with no block device mapping at all",
            device: None,
            ebs: &[],
            source: Source::Own,
        },
        FallbackRow {
            case: "a mapping for another device, leaving the root device alone",
            device: Some(false),
            ebs: &[("VolumeSize", "500"), ("VolumeType", "gp2")],
            source: Source::Own,
        },
        FallbackRow {
            case: "a root mapping that names only encryption",
            device: Some(true),
            ebs: &[("Encrypted", "true")],
            source: Source::Own,
        },
        FallbackRow {
            case: "a root mapping that names only termination, through a mixed instances \
                   policy",
            device: Some(true),
            ebs: &[("DeleteOnTermination", "false")],
            source: Source::MixedPolicy,
        },
    ];

    let (router, _engine) = emulator();
    let (root_device, ami_size, ami_volume_type) = ami_root_volume(&router, 1).await;
    // A device that is not the AMI's root one, whatever that root device is named.
    let other_device = format!("{root_device}-other");

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let template = format!("lt-fallback-{index}");
        let device = match row.device {
            None => None,
            Some(true) => Some(root_device.as_str()),
            Some(false) => Some(other_device.as_str()),
        };
        create_launch_template(&router, &template, device, row.ebs).await;
        let instance_id =
            launched_instance(&router, &format!("fallback-{index}"), &template, row.source).await;
        let body = root_volume(&router, case, &instance_id).await;

        assert_eq!(
            (
                tag_value(&body, "size"),
                tag_value(&body, "volumeType"),
                tag_value(&body, "device"),
            ),
            (
                ami_size.clone(),
                ami_volume_type.clone(),
                root_device.clone()
            ),
            "case {case:?}: a size and a type the version leaves out must be the AMI's own \
             ({ami_size}, {ami_volume_type}) at its own root device ({root_device}), got {body}"
        );
        // What the row DOES declare still reaches the volume.
        for (member, value) in row.ebs {
            if row.device == Some(false) {
                continue;
            }
            let element = reported_element(member);
            assert_eq!(
                tag_value(&body, element),
                *value,
                "case {case:?}: the volume must report the {member} the version declares \
                 ({value}) as {element}, got {body}"
            );
        }
    }
}

/// The members of the volume the two launch paths must agree on, as the elements the
/// envelope reports them under. The volume's id, its ARN, its creation time and its state
/// are per-launch or clock-owed, so they are not among them.
const COMPARED: &[&str] = &[
    "size",
    "volumeType",
    "iops",
    "throughput",
    "encrypted",
    "kmsKeyId",
    "snapshotId",
    "device",
    "deleteOnTermination",
];

/// A capacity change launches FROM the version, so the root volume a group's instance holds
/// is the one `RunInstances` creates from that same version — including every member neither
/// side wrote out, which is Amazon EC2's to decide.
#[tokio::test]
async fn a_group_instance_and_a_launch_from_the_same_version_agree() {
    let (router, _engine) = emulator();
    let (root_device, _size, _volume_type) = ami_root_volume(&router, 2).await;

    let rows: [(&str, &[(&str, &str)]); 3] = [
        ("a version that declares no mapping", &[]),
        ("a version that declares a size alone", &[("VolumeSize", "75")]),
        (
            "a version that declares the whole gp3 set",
            &[
                ("VolumeSize", "150"),
                ("VolumeType", "gp3"),
                ("Iops", "5000"),
                ("Throughput", "300"),
                ("Encrypted", "true"),
                ("DeleteOnTermination", "false"),
            ],
        ),
    ];

    for (index, (case, ebs)) in rows.iter().enumerate() {
        let template = format!("lt-parity-{index}");
        let device = if ebs.is_empty() {
            None
        } else {
            Some(root_device.as_str())
        };
        create_launch_template(&router, &template, device, ebs).await;

        let by_group =
            launched_instance(&router, &format!("parity-{index}"), &template, Source::Own).await;
        let (status, body) = ec2_query(
            &router,
            "RunInstances",
            &[
                p("LaunchTemplate.LaunchTemplateName", template.as_str()),
                p("MinCount", "1"),
                p("MaxCount", "1"),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: RunInstances expected 200, got {status}: {body}"
        );
        let by_run_instances = tag_value(&body, "instanceId");
        assert!(
            !by_run_instances.is_empty(),
            "case {case:?}: RunInstances must report its instance, got {body}"
        );

        let group_volume = root_volume(&router, case, &by_group).await;
        let launch_volume = root_volume(&router, case, &by_run_instances).await;
        let members = |volume: &str| -> Vec<String> {
            COMPARED
                .iter()
                .map(|member| format!("{member}={}", tag_value(volume, member)))
                .collect()
        };
        assert_eq!(
            members(&group_volume),
            members(&launch_volume),
            "case {case:?}: the group's instance must hold the root volume a launch from the \
             same version holds, got {group_volume} against {launch_volume}"
        );
    }
}
