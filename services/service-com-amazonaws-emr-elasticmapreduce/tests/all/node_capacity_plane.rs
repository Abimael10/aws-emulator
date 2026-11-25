// SPDX-License-Identifier: CC0-1.0

//! The NODE CAPACITY plane of an EMR cluster, end to end over the wire: the Amazon EBS
//! volumes every node of a collection holds, the instances a moved target provisions or
//! releases, and the capacity a managed scaling policy adds for submitted work.
//!
//! Every case drives the real HTTP path through the router's `dispatch` and then resolves
//! what EMR reports in the service that OWNS instances and volumes, so these tests are
//! evidence that each answer is computed from persisted state AND that what it names really
//! exists in Amazon EC2. The two services are mounted on one engine through the harness
//! `cluster_instances` builds, exactly as the assembled router mounts them.
//!
//! THE RULES under test:
//!
//! * a collection's `EbsConfiguration` asks the LAUNCH for so many Amazon EBS volumes per
//!   instance, so every node of it holds one volume per requested device and `ListInstances`
//!   reports each as an `Instance$EbsVolumes` entry naming the device and the volume Amazon
//!   EC2 minted — with the size and type the `VolumeSpecification` asked for, and the
//!   cluster's `EbsRootVolumeSize` on the node's root device;
//! * the instances a collection holds follow its TARGET: whatever moves that target, Amazon
//!   EMR provisions instances up to it and releases the ones past it, and `ListInstances`
//!   reports exactly the ones Amazon EC2 holds;
//! * a cluster carrying a managed scaling policy and holding submitted work has core and task
//!   capacity added up to `ComputeLimits$MaximumCapacityUnits` — in the policy's own unit type
//!   and no further than `$MaximumCoreCapacityUnits` for the core part — and its primary
//!   collection is never scaled.

use std::collections::BTreeMap;
use std::time::Instant;

use serde_json::{json, Value};

use crate::cluster_instances::{
    described_in_ec2, ec2_ids, emr, emulator, list_instances, subnet, Emulator, POLL_INTERVAL,
    WAIT_DEADLINE,
};

/// The release every launch here names, and the roles it runs as — the same facts the
/// harness suite launches its clusters with.
const RELEASE_LABEL: &str = "emr-7.9.0";
const INSTANCE_PROFILE: &str = "EMR_EC2_DefaultRole";
const SERVICE_ROLE: &str = "EMR_DefaultRole";

/// The instance type every collection below is provisioned as, and the vCPU cores one
/// instance of it carries — `<family>.xlarge` is 4 vCPUs on every published Amazon EC2 size
/// ladder (`ListSupportedInstanceTypes`, `SupportedInstanceType$VCpu`).
const INSTANCE_TYPE: &str = "m5.xlarge";
const INSTANCE_VCPU: i64 = 4;

/// A cluster launched with `instances` in `subnet_id`, carrying the top-level members
/// `extra` names beside them, answering its id.
async fn launch_cluster(
    emulator: &Emulator,
    name: &str,
    subnet_id: &str,
    instances: Value,
    extra: Value,
) -> String {
    let mut instances = instances;
    instances["Ec2SubnetId"] = json!(subnet_id);
    // The cluster stays up with no step still to run, so every case reads a live cluster.
    instances["KeepJobFlowAliveWhenNoSteps"] = json!(true);
    let mut request = json!({
        "Name": name,
        "ReleaseLabel": RELEASE_LABEL,
        "JobFlowRole": INSTANCE_PROFILE,
        "ServiceRole": SERVICE_ROLE,
        "Instances": instances,
    });
    for (member, value) in extra.as_object().cloned().unwrap_or_default() {
        request[member] = value;
    }
    emr(&emulator.router, "RunJobFlow", request).await["JobFlowId"]
        .as_str()
        .expect("RunJobFlow answers a JobFlowId")
        .to_string()
}

/// One instance group configuration: `role` provisioned as `count` instances of
/// [`INSTANCE_TYPE`], with the `EbsConfiguration` `ebs` names.
fn group(role: &str, count: i64, ebs: Option<Value>) -> Value {
    let mut config = json!({
        "Name": format!("{role}-group"),
        "InstanceRole": role,
        "InstanceType": INSTANCE_TYPE,
        "InstanceCount": count,
    });
    if let Some(ebs) = ebs {
        config["EbsConfiguration"] = ebs;
    }
    config
}

/// An `EbsConfiguration` asking for `volumes` volumes per instance of `volume_type` at
/// `size_in_gb` GiB each.
fn ebs_configuration(volume_type: &str, size_in_gb: i64, volumes: i64) -> Value {
    json!({
        "EbsBlockDeviceConfigs": [{
            "VolumeSpecification": { "VolumeType": volume_type, "SizeInGB": size_in_gb },
            "VolumesPerInstance": volumes,
        }],
        "EbsOptimized": true,
    })
}

/// A queue of `steps` steps for a cluster to hold: the work a managed scaling activity reads
/// as the demand on its core and task capacity. Each one's failure is the step's own
/// (`ActionOnFailure: CONTINUE`), so the cluster stays up whatever its queue comes to, and
/// there are several of them so the queue still holds work while a case reads the capacity
/// it asks for.
fn submitted_work(steps: usize) -> Value {
    Value::Array(
        (0..steps)
            .map(|index| {
                json!({
                    "Name": format!("submitted-work-{index}"),
                    "ActionOnFailure": "CONTINUE",
                    "HadoopJarStep": {
                        "Jar": "command-runner.jar",
                        "Args": ["spark-submit", "s3://emr-capacity/work.py"],
                    },
                })
            })
            .collect(),
    )
}

/// How many steps a case queues for that reason.
const QUEUED_WORK: usize = 8;

/// The instance groups of `cluster_id`, as `(role, (group id, requested count))`.
async fn groups_by_role(emulator: &Emulator, cluster_id: &str) -> BTreeMap<String, (String, i64)> {
    emr(
        &emulator.router,
        "ListInstanceGroups",
        json!({ "ClusterId": cluster_id }),
    )
    .await["InstanceGroups"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|reported| {
            (
                reported["InstanceGroupType"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                (
                    reported["Id"].as_str().unwrap_or_default().to_string(),
                    reported["RequestedInstanceCount"].as_i64().unwrap_or(-1),
                ),
            )
        })
        .collect()
}

/// The instances of `cluster_id` that have not been terminated, under `filters`.
async fn live_instances(
    emulator: &Emulator,
    case: &str,
    cluster_id: &str,
    filters: Value,
) -> Vec<Value> {
    list_instances(emulator, case, cluster_id, filters)
        .await
        .into_iter()
        .filter(|instance| instance["Status"]["State"].as_str() != Some("TERMINATED"))
        .collect()
}

/// The live instances of `cluster_id` once there are `expected` of them under `filters`,
/// looked for until the deadline passes rather than waited out.
async fn live_instances_settling_at(
    emulator: &Emulator,
    case: &str,
    cluster_id: &str,
    filters: Value,
    expected: usize,
) -> Vec<Value> {
    let deadline = Instant::now() + WAIT_DEADLINE;
    loop {
        let live = live_instances(emulator, case, cluster_id, filters.clone()).await;
        if live.len() == expected {
            return live;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {expected} live instances, got {}: {live:?}",
            live.len()
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The volumes Amazon EC2 holds attached to `ec2_instance_id`, as
/// `device -> (volume id, size, type)` — resolved through the owning service's own filter,
/// "attachment.instance-id - The ID of the instance the volume is attached to"
/// (`DescribeVolumes$Filters`, Amazon EC2 model).
async fn volumes_of(
    emulator: &Emulator,
    ec2_instance_id: &str,
) -> BTreeMap<String, (String, i32, String)> {
    let answer = emulator
        .ec2
        .describe_volumes()
        .filters(
            ::aws_sdk_ec2::types::Filter::builder()
                .name("attachment.instance-id")
                .values(ec2_instance_id)
                .build(),
        )
        .send()
        .await
        .expect("EC2 must describe the volumes of an instance it holds");
    let mut held = BTreeMap::new();
    for volume in answer.volumes() {
        let Some(volume_id) = volume.volume_id() else {
            continue;
        };
        for attachment in volume.attachments() {
            if attachment.instance_id() != Some(ec2_instance_id) {
                continue;
            }
            held.insert(
                attachment.device().unwrap_or_default().to_string(),
                (
                    volume_id.to_string(),
                    volume.size().unwrap_or_default(),
                    volume
                        .volume_type()
                        .map(|kind| kind.as_str().to_string())
                        .unwrap_or_default(),
                ),
            );
        }
    }
    held
}

// ---------------------------------------------------------------------------
// The Amazon EBS volumes a node holds
// ---------------------------------------------------------------------------

/// THE RULE: a collection's `EbsConfiguration` asks the launch for so many Amazon EBS volumes
/// per instance, so every node of it holds one volume per requested device — reported as an
/// `Instance$EbsVolumes` entry naming the device and the volume Amazon EC2 minted, with the
/// size and type the `VolumeSpecification` asked for — and a collection that configures none
/// reports none.
///
/// The table spans the request shape: how many volumes per instance (one, two, three), the
/// volume type and size asked for (`gp2`, `gp3`, `st1` — three of the types
/// `VolumeSpecification$VolumeType` publishes), several block device configurations in one
/// request, the role of the collection that asks, and a collection that asks for nothing.
#[tokio::test]
async fn an_ebs_configuration_provisions_the_volumes_every_node_of_the_collection_holds() {
    struct Case {
        label: &'static str,
        role: &'static str,
        ebs: Option<Value>,
        /// The devices and the `(size, type)` each one must hold, in device order.
        expected: Vec<(&'static str, i32, &'static str)>,
    }
    let cases = vec![
        Case {
            label: "two gp2 volumes on the primary",
            role: "MASTER",
            ebs: Some(ebs_configuration("gp2", 47, 2)),
            expected: vec![("/dev/sdb", 47, "gp2"), ("/dev/sdc", 47, "gp2")],
        },
        Case {
            label: "one st1 volume on a core node",
            role: "CORE",
            ebs: Some(ebs_configuration("st1", 500, 1)),
            expected: vec![("/dev/sdb", 500, "st1")],
        },
        Case {
            label: "three gp3 volumes on a task node",
            role: "TASK",
            ebs: Some(ebs_configuration("gp3", 120, 3)),
            expected: vec![
                ("/dev/sdb", 120, "gp3"),
                ("/dev/sdc", 120, "gp3"),
                ("/dev/sdd", 120, "gp3"),
            ],
        },
        Case {
            label: "two block device configurations in one request",
            role: "CORE",
            ebs: Some(json!({
                "EbsBlockDeviceConfigs": [
                    {
                        "VolumeSpecification": { "VolumeType": "gp3", "SizeInGB": 32 },
                        "VolumesPerInstance": 1,
                    },
                    {
                        "VolumeSpecification": { "VolumeType": "gp2", "SizeInGB": 64 },
                        "VolumesPerInstance": 2,
                    },
                ],
            })),
            expected: vec![
                ("/dev/sdb", 32, "gp3"),
                ("/dev/sdc", 64, "gp2"),
                ("/dev/sdd", 64, "gp2"),
            ],
        },
        Case {
            label: "a collection that configures no Amazon EBS volume",
            role: "CORE",
            ebs: None,
            expected: Vec::new(),
        },
    ];

    let emulator = emulator();
    let subnet_id = subnet(&emulator).await;
    for case in &cases {
        // Every cluster carries a primary; a case about another role carries that collection
        // beside it, so the group under test is the one the listing is filtered to.
        let mut groups = vec![group("MASTER", 1, None)];
        if case.role != "MASTER" {
            groups.push(group(case.role, 1, case.ebs.clone()));
        } else {
            groups[0] = group("MASTER", 1, case.ebs.clone());
        }
        let cluster_id = launch_cluster(
            &emulator,
            case.label,
            &subnet_id,
            json!({ "InstanceGroups": groups }),
            json!({}),
        )
        .await;

        let live = live_instances_settling_at(
            &emulator,
            case.label,
            &cluster_id,
            json!({ "InstanceGroupTypes": [case.role] }),
            1,
        )
        .await;
        let node = &live[0];
        let reported: Vec<(String, String)> = node["EbsVolumes"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|volume| {
                (
                    volume["Device"].as_str().unwrap_or_default().to_string(),
                    volume["VolumeId"].as_str().unwrap_or_default().to_string(),
                )
            })
            .collect();
        assert_eq!(
            reported.len(),
            case.expected.len(),
            "case {:?}: expected {} EbsVolumes entries, got {node}",
            case.label,
            case.expected.len()
        );
        if case.expected.is_empty() {
            // A collection that asks for no volume leaves the member absent, the way its
            // group's own `EbsBlockDevices` is left absent.
            assert!(
                node["EbsVolumes"].is_null(),
                "case {:?}: a node whose collection configured no volume reports none, got {node}",
                case.label
            );
            continue;
        }

        // What EMR named has to EXIST in the service that owns volumes, at the device EMR
        // reported it at and with the size and type the configuration asked for.
        let ec2_instance_id = node["Ec2InstanceId"].as_str().unwrap_or_default().to_string();
        let held = volumes_of(&emulator, &ec2_instance_id).await;
        for (index, (device, size, volume_type)) in case.expected.iter().enumerate() {
            let (reported_device, reported_id) = &reported[index];
            assert_eq!(
                reported_device, device,
                "case {:?}: entry {index} must name device {device}, got {reported:?}",
                case.label
            );
            let (held_id, held_size, held_type) = held.get(*device).unwrap_or_else(|| {
                panic!(
                    "case {:?}: EC2 must hold a volume at {device} of {ec2_instance_id}, got {held:?}",
                    case.label
                )
            });
            assert_eq!(
                held_id, reported_id,
                "case {:?}: the volume at {device} must be the one EMR reported, got {held:?}",
                case.label
            );
            assert_eq!(
                (*held_size, held_type.as_str()),
                (*size, *volume_type),
                "case {:?}: the volume at {device} must be the requested {size} GiB {volume_type}, got {held:?}",
                case.label
            );
        }
    }
}

/// THE RULE: "the size, in GiB, of the Amazon EBS root device volume of the Linux AMI that is
/// used for each Amazon EC2 instance" (`RunJobFlow$EbsRootVolumeSize` doc) is what each node's
/// ROOT device really holds — beside, and not instead of, the volumes its collection's
/// `EbsConfiguration` asks for. The table spans two requested sizes and a launch that asks for
/// none, which leaves the image's own root device.
#[tokio::test]
async fn the_requested_root_volume_size_is_what_every_node_boots() {
    /// The device an Amazon Linux AMI holds its root volume at (Amazon EC2 User Guide,
    /// "Device names on Linux instances"), which is where a cluster's root volume is.
    const ROOT_DEVICE: &str = "/dev/xvda";

    let cases: [(&str, Option<i64>); 3] = [
        ("a 53 GiB root volume", Some(53)),
        ("a 200 GiB root volume", Some(200)),
        ("no root volume size at all", None),
    ];

    let emulator = emulator();
    let subnet_id = subnet(&emulator).await;
    for (label, requested) in cases {
        let mut extra = json!({});
        if let Some(size) = requested {
            extra["EbsRootVolumeSize"] = json!(size);
        }
        let cluster_id = launch_cluster(
            &emulator,
            label,
            &subnet_id,
            json!({ "InstanceGroups": [group("MASTER", 1, Some(ebs_configuration("gp2", 47, 2)))] }),
            extra,
        )
        .await;
        let live = live_instances_settling_at(&emulator, label, &cluster_id, json!({}), 1).await;
        let ec2_instance_id = live[0]["Ec2InstanceId"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let held = volumes_of(&emulator, &ec2_instance_id).await;
        let (_, root_size, _) = held.get(ROOT_DEVICE).unwrap_or_else(|| {
            panic!("case {label:?}: EC2 must hold a root volume at {ROOT_DEVICE}, got {held:?}")
        });
        match requested {
            Some(size) => assert_eq!(
                i64::from(*root_size),
                size,
                "case {label:?}: the root volume must be the requested {size} GiB, got {held:?}"
            ),
            // The launch asked for nothing, so the node boots the image's own root device —
            // whatever size that is, it is not one this request chose.
            None => assert!(
                *root_size > 0,
                "case {label:?}: the node must still boot a root volume, got {held:?}"
            ),
        }
        // The storage the collection asked for is there either way.
        assert_eq!(
            live[0]["EbsVolumes"].as_array().map(Vec::len),
            Some(2),
            "case {label:?}: the requested storage volumes are reported beside the root one, got {}",
            live[0]
        );
    }
}

// ---------------------------------------------------------------------------
// The instances a moved target asks for
// ---------------------------------------------------------------------------

/// THE RULE: the instances a collection holds follow its TARGET — a raised
/// `RequestedInstanceCount` provisions the additional Amazon EC2 instances, so
/// `ListInstances` reports that many non-terminated instances for the group and every one of
/// them exists in the service that owns instances, and a lowered one releases the instances
/// past it, which EMR then reports `TERMINATED` and Amazon EC2 has really terminated.
///
/// The table spans the direction and the size of the move: a growth by one, a growth by
/// several, a shrink, and a modification that asks for the count the group already holds.
#[tokio::test]
async fn a_moved_instance_count_provisions_or_releases_the_instances_it_asks_for() {
    struct Case {
        label: &'static str,
        launched: i64,
        target: i64,
    }
    let cases = [
        Case {
            label: "one more instance",
            launched: 1,
            target: 2,
        },
        Case {
            label: "three more instances",
            launched: 1,
            target: 4,
        },
        Case {
            label: "two instances fewer",
            launched: 3,
            target: 1,
        },
        Case {
            label: "the count the group already holds",
            launched: 2,
            target: 2,
        },
    ];

    let emulator = emulator();
    let subnet_id = subnet(&emulator).await;
    for case in &cases {
        let cluster_id = launch_cluster(
            &emulator,
            case.label,
            &subnet_id,
            json!({
                "InstanceGroups": [
                    group("MASTER", 1, None),
                    group("CORE", case.launched, None),
                ],
            }),
            json!({}),
        )
        .await;
        let core = groups_by_role(&emulator, &cluster_id).await["CORE"].0.clone();
        let filters = json!({ "InstanceGroupId": &core });
        let before = live_instances_settling_at(
            &emulator,
            case.label,
            &cluster_id,
            filters.clone(),
            usize::try_from(case.launched).unwrap_or_default(),
        )
        .await;

        emr(
            &emulator.router,
            "ModifyInstanceGroups",
            json!({
                "ClusterId": &cluster_id,
                "InstanceGroups": [{ "InstanceGroupId": &core, "InstanceCount": case.target }],
            }),
        )
        .await;

        // The capacity the new target asks for, reported by EMR …
        let after = live_instances_settling_at(
            &emulator,
            case.label,
            &cluster_id,
            filters.clone(),
            usize::try_from(case.target).unwrap_or_default(),
        )
        .await;
        assert_eq!(
            groups_by_role(&emulator, &cluster_id).await["CORE"].1,
            case.target,
            "case {:?}: the group must request {} instances",
            case.label,
            case.target
        );
        // … and really held by the service that owns instances.
        let live_ids = ec2_ids(&after);
        let held = described_in_ec2(&emulator, &live_ids).await;
        for id in &live_ids {
            let instance = held.get(id).unwrap_or_else(|| {
                panic!(
                    "case {:?}: EC2 must hold the instance {id} EMR reports, got {:?}",
                    case.label,
                    held.keys().collect::<Vec<_>>()
                )
            });
            assert!(
                instance.state == "pending" || instance.state == "running",
                "case {:?}: instance {id} must be up in EC2, got {:?}",
                case.label,
                instance.state
            );
        }

        // Whatever the group released is reported TERMINATED by EMR and terminated in EC2.
        let released: Vec<String> = ec2_ids(&before)
            .into_iter()
            .filter(|id| !live_ids.contains(id))
            .collect();
        assert_eq!(
            released.len(),
            usize::try_from((case.launched - case.target).max(0)).unwrap_or_default(),
            "case {:?}: the instances past the target are the released ones, got {released:?}",
            case.label
        );
        if !released.is_empty() {
            let listed = list_instances(&emulator, case.label, &cluster_id, filters).await;
            for id in &released {
                let reported = listed
                    .iter()
                    .find(|instance| instance["Ec2InstanceId"].as_str() == Some(id.as_str()))
                    .unwrap_or_else(|| {
                        panic!("case {:?}: {id} must still be listed, got {listed:?}", case.label)
                    });
                assert_eq!(
                    reported["Status"]["State"].as_str(),
                    Some("TERMINATED"),
                    "case {:?}: the released instance must report TERMINATED, got {reported}",
                    case.label
                );
            }
            let held = described_in_ec2(&emulator, &released).await;
            for id in &released {
                let state = held.get(id).map(|instance| instance.state.clone());
                assert!(
                    state.as_deref() == Some("shutting-down") || state.as_deref() == Some("terminated"),
                    "case {:?}: EC2 must have terminated {id}, got {state:?}",
                    case.label
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The capacity a managed scaling policy adds
// ---------------------------------------------------------------------------

/// THE RULE: a cluster carrying a managed scaling policy and holding submitted work has
/// Amazon EMR add core and task capacity up to "the upper boundary of Amazon EC2 units"
/// (`ComputeLimits$MaximumCapacityUnits` doc) in the policy's own unit type, no further than
/// "the upper boundary of Amazon EC2 units for core node type in a cluster"
/// (`$MaximumCoreCapacityUnits`) for the core part — and never scales the primary, which
/// "cannot be scaled after initial configuration" (`ComputeLimits` doc). A ceiling the cluster
/// already holds adds nothing.
///
/// The table spans both unit types an instance-group cluster states its limits in, a ceiling
/// equal to what is provisioned, and a core boundary that holds the core collection where it
/// is.
#[tokio::test]
async fn managed_scaling_adds_core_and_task_capacity_up_to_the_policys_ceiling() {
    struct Case {
        label: &'static str,
        unit_type: &'static str,
        /// The ceiling the policy publishes, in its own units.
        maximum: i64,
        /// `MaximumCoreCapacityUnits`, where the policy publishes one.
        maximum_core: Option<i64>,
        /// The core and task instances the cluster must end up asking for.
        expected_core: i64,
        expected_task: i64,
    }
    // One primary, one core and one task instance are provisioned, so the cluster starts at
    // two scalable instances — eight vCPU cores of them.
    let cases = [
        Case {
            label: "a ceiling of five instances",
            unit_type: "Instances",
            maximum: 5,
            maximum_core: None,
            // The core collection is filled first, and nothing bounds it here.
            expected_core: 4,
            expected_task: 1,
        },
        Case {
            label: "a ceiling of five instances with the core part held at one",
            unit_type: "Instances",
            maximum: 5,
            maximum_core: Some(1),
            expected_core: 1,
            expected_task: 4,
        },
        Case {
            label: "a ceiling of sixteen vCPU cores",
            unit_type: "VCPU",
            maximum: 4 * INSTANCE_VCPU,
            maximum_core: Some(2 * INSTANCE_VCPU),
            expected_core: 2,
            expected_task: 2,
        },
        Case {
            label: "a ceiling the cluster already holds",
            unit_type: "Instances",
            maximum: 2,
            maximum_core: None,
            expected_core: 1,
            expected_task: 1,
        },
    ];

    let emulator = emulator();
    let subnet_id = subnet(&emulator).await;
    for case in &cases {
        let mut limits = json!({
            "UnitType": case.unit_type,
            "MinimumCapacityUnits": 1,
            "MaximumCapacityUnits": case.maximum,
        });
        if let Some(maximum_core) = case.maximum_core {
            limits["MaximumCoreCapacityUnits"] = json!(maximum_core);
        }
        let cluster_id = launch_cluster(
            &emulator,
            case.label,
            &subnet_id,
            json!({
                "InstanceGroups": [
                    group("MASTER", 1, None),
                    group("CORE", 1, None),
                    group("TASK", 1, None),
                ],
            }),
            json!({
                "ManagedScalingPolicy": { "ComputeLimits": limits },
                "Steps": submitted_work(QUEUED_WORK),
            }),
        )
        .await;

        let expected_live = usize::try_from(1 + case.expected_core + case.expected_task)
            .unwrap_or_default();
        let live =
            live_instances_settling_at(&emulator, case.label, &cluster_id, json!({}), expected_live)
                .await;
        let groups = groups_by_role(&emulator, &cluster_id).await;
        assert_eq!(
            groups["CORE"].1, case.expected_core,
            "case {:?}: the core collection must request {} instances, got {groups:?}",
            case.label, case.expected_core
        );
        assert_eq!(
            groups["TASK"].1, case.expected_task,
            "case {:?}: the task collection must request {} instances, got {groups:?}",
            case.label, case.expected_task
        );
        // "The master node cannot be scaled after initial configuration."
        assert_eq!(
            groups["MASTER"].1, 1,
            "case {:?}: the primary collection is never scaled, got {groups:?}",
            case.label
        );
        // And every instance the activity provisioned exists in the service that owns them.
        let live_ids = ec2_ids(&live);
        let held = described_in_ec2(&emulator, &live_ids).await;
        for id in &live_ids {
            assert!(
                held.contains_key(id),
                "case {:?}: EC2 must hold the instance {id} EMR reports, got {:?}",
                case.label,
                held.keys().collect::<Vec<_>>()
            );
        }
    }
}

/// THE RULE, in the other direction: raising a policy's ceiling is what lets the activity add
/// capacity, so a cluster clamped at what it is provisioned with grows only once the ceiling
/// is raised — and a cluster carrying no submitted work is left where it is.
#[tokio::test]
async fn a_raised_ceiling_is_what_lets_the_activity_grow_the_cluster() {
    let emulator = emulator();
    let subnet_id = subnet(&emulator).await;
    let groups = json!({
        "InstanceGroups": [
            group("MASTER", 1, None),
            group("CORE", 1, None),
            group("TASK", 1, None),
        ],
    });
    let clamped = json!({
        "ComputeLimits": {
            "UnitType": "Instances",
            "MinimumCapacityUnits": 1,
            "MaximumCapacityUnits": 2,
        },
    });

    // A cluster with no work submitted to it is left exactly as it is, whatever headroom its
    // policy publishes.
    let idle = launch_cluster(
        &emulator,
        "no submitted work",
        &subnet_id,
        groups.clone(),
        json!({
            "ManagedScalingPolicy": {
                "ComputeLimits": {
                    "UnitType": "Instances",
                    "MinimumCapacityUnits": 1,
                    "MaximumCapacityUnits": 8,
                },
            },
        }),
    )
    .await;
    // The cluster reaching WAITING is several evaluation spans after its launch, so an
    // activity that was going to add capacity has had its chance by then.
    await_cluster_state(&emulator, "no submitted work", &idle, "WAITING").await;
    let live = live_instances(&emulator, "no submitted work", &idle, json!({})).await;
    assert_eq!(
        live.len(),
        3,
        "case {:?}: a cluster with no submitted work keeps the capacity it was launched with, got {live:?}",
        "no submitted work"
    );

    let case = "clamped then raised";
    let cluster_id = launch_cluster(
        &emulator,
        case,
        &subnet_id,
        groups,
        json!({
            "ManagedScalingPolicy": clamped,
            "Steps": submitted_work(QUEUED_WORK),
        }),
    )
    .await;
    // A cluster that is up with work still in its queue reports `RUNNING` — "the cluster
    // transitions to the `WAITING` state … after the steps have completed" (`RunJobFlow`
    // doc) — and that is the state its capacity is read in, while the queue still holds the
    // demand an activity reads.
    await_cluster_state(&emulator, case, &cluster_id, "RUNNING").await;
    let live = live_instances(&emulator, case, &cluster_id, json!({})).await;
    assert_eq!(
        live.len(),
        3,
        "case {case:?}: a ceiling equal to the provisioned capacity adds nothing, got {live:?}"
    );

    emr(
        &emulator.router,
        "PutManagedScalingPolicy",
        json!({
            "ClusterId": &cluster_id,
            "ManagedScalingPolicy": {
                "ComputeLimits": {
                    "UnitType": "Instances",
                    "MinimumCapacityUnits": 1,
                    "MaximumCapacityUnits": 5,
                },
            },
        }),
    )
    .await;
    // Six nodes: the primary, and the five core and task instances the raised ceiling admits.
    live_instances_settling_at(&emulator, case, &cluster_id, json!({}), 6).await;
}

/// THE RULE, for a cluster whose capacity is stated as instance FLEETS: a fleet states its
/// capacity in units — "the target capacity of On-Demand units for the instance fleet, which
/// determines how many On-Demand Instances to provision"
/// (`InstanceFleet$TargetOnDemandCapacity` doc) — so an activity measured in
/// `InstanceFleetUnits` raises that target toward the ceiling, and the instances those units
/// ask for are really provisioned.
#[tokio::test]
async fn managed_scaling_adds_the_fleet_units_its_ceiling_admits() {
    let case = "a fleet ceiling of five units";
    let emulator = emulator();
    let subnet_id = subnet(&emulator).await;
    let cluster_id = launch_cluster(
        &emulator,
        case,
        &subnet_id,
        json!({
            "InstanceFleets": [
                {
                    "Name": "primary",
                    "InstanceFleetType": "MASTER",
                    "TargetOnDemandCapacity": 1,
                    "InstanceTypeConfigs": [{ "InstanceType": INSTANCE_TYPE }],
                },
                {
                    "Name": "core",
                    "InstanceFleetType": "CORE",
                    "TargetOnDemandCapacity": 1,
                    "InstanceTypeConfigs": [{ "InstanceType": INSTANCE_TYPE }],
                },
            ],
        }),
        json!({
            "ManagedScalingPolicy": {
                "ComputeLimits": {
                    "UnitType": "InstanceFleetUnits",
                    "MinimumCapacityUnits": 1,
                    "MaximumCapacityUnits": 5,
                },
            },
            "Steps": submitted_work(QUEUED_WORK),
        }),
    )
    .await;

    // The primary fleet's one unit is not scaled, and the core fleet is taken to the five
    // units the ceiling admits: six nodes in all.
    let live = live_instances_settling_at(&emulator, case, &cluster_id, json!({}), 6).await;
    let fleets = emr(
        &emulator.router,
        "ListInstanceFleets",
        json!({ "ClusterId": &cluster_id }),
    )
    .await;
    let reported: BTreeMap<String, i64> = fleets["InstanceFleets"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|fleet| {
            (
                fleet["InstanceFleetType"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                fleet["TargetOnDemandCapacity"].as_i64().unwrap_or(-1),
            )
        })
        .collect();
    assert_eq!(
        reported.get("CORE"),
        Some(&5),
        "case {case:?}: the core fleet must target the five units the ceiling admits, got {reported:?}"
    );
    assert_eq!(
        reported.get("MASTER"),
        Some(&1),
        "case {case:?}: the primary fleet is never scaled, got {reported:?}"
    );
    let live_ids = ec2_ids(&live);
    let held = described_in_ec2(&emulator, &live_ids).await;
    for id in &live_ids {
        assert!(
            held.contains_key(id),
            "case {case:?}: EC2 must hold the instance {id} EMR reports, got {:?}",
            held.keys().collect::<Vec<_>>()
        );
    }

    // And the same rule the other way: a fleet target lowered — here to NO units at all —
    // releases the instances that market was holding, down to the primary fleet's one node.
    let core = fleets["InstanceFleets"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .find(|fleet| fleet["InstanceFleetType"].as_str() == Some("CORE"))
        .and_then(|fleet| fleet["Id"].as_str().map(str::to_string))
        .expect("the cluster carries a core fleet");
    // The policy goes first, so no activity provisions the units back.
    emr(
        &emulator.router,
        "RemoveManagedScalingPolicy",
        json!({ "ClusterId": &cluster_id }),
    )
    .await;
    emr(
        &emulator.router,
        "ModifyInstanceFleet",
        json!({
            "ClusterId": &cluster_id,
            "InstanceFleet": { "InstanceFleetId": &core, "TargetOnDemandCapacity": 0 },
        }),
    )
    .await;
    let left = live_instances_settling_at(&emulator, case, &cluster_id, json!({}), 1).await;
    assert_eq!(
        left[0]["InstanceFleetId"].as_str().map(str::to_string),
        fleets["InstanceFleets"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .find(|fleet| fleet["InstanceFleetType"].as_str() == Some("MASTER"))
            .and_then(|fleet| fleet["Id"].as_str().map(str::to_string)),
        "case {case:?}: the node left must be the primary fleet's, got {left:?}"
    );
}

/// THE RULE for the instances a modification names: "the Amazon EC2 InstanceIds to terminate.
/// After you terminate the instances, the instance group will not return to its original
/// requested size" (`InstanceGroupModifyConfig$EC2InstanceIdsToTerminate` doc) — so a named
/// instance of the group is released, the group then asks for what it still holds, and an id
/// that names no live instance of it is refused with the whole modification.
#[tokio::test]
async fn a_named_instance_is_the_one_the_group_releases() {
    let case = "an instance named for termination";
    let emulator = emulator();
    let subnet_id = subnet(&emulator).await;
    let cluster_id = launch_cluster(
        &emulator,
        case,
        &subnet_id,
        json!({
            "InstanceGroups": [group("MASTER", 1, None), group("CORE", 3, None)],
        }),
        json!({}),
    )
    .await;
    let core = groups_by_role(&emulator, &cluster_id).await["CORE"].0.clone();
    let filters = json!({ "InstanceGroupId": &core });
    let before = live_instances_settling_at(&emulator, case, &cluster_id, filters.clone(), 3).await;
    let named = before[1]["Ec2InstanceId"]
        .as_str()
        .expect("the listing names the Amazon EC2 instance")
        .to_string();

    // An id that names no instance of the group is refused, and nothing changes.
    let (status, _) = crate::cluster_instances::emr_call(
        &emulator.router,
        "ModifyInstanceGroups",
        json!({
            "ClusterId": &cluster_id,
            "InstanceGroups": [{
                "InstanceGroupId": &core,
                "EC2InstanceIdsToTerminate": ["i-0123456789abcdef0"],
            }],
        }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: an id naming no instance of the group must be refused"
    );
    assert_eq!(
        live_instances(&emulator, case, &cluster_id, filters.clone())
            .await
            .len(),
        3,
        "case {case:?}: the refused modification releases nothing"
    );

    emr(
        &emulator.router,
        "ModifyInstanceGroups",
        json!({
            "ClusterId": &cluster_id,
            "InstanceGroups": [{
                "InstanceGroupId": &core,
                "EC2InstanceIdsToTerminate": [&named],
            }],
        }),
    )
    .await;
    let after = live_instances_settling_at(&emulator, case, &cluster_id, filters, 2).await;
    assert!(
        !ec2_ids(&after).contains(&named),
        "case {case:?}: the named instance must be the released one, got {after:?}"
    );
    // "… the instance group will not return to its original requested size": the group asks
    // for what it holds, so nothing provisions a replacement.
    assert_eq!(
        groups_by_role(&emulator, &cluster_id).await["CORE"].1,
        2,
        "case {case:?}: the group must request the two instances it still holds"
    );
    let held = described_in_ec2(&emulator, std::slice::from_ref(&named)).await;
    let state = held.get(&named).map(|instance| instance.state.clone());
    assert!(
        state.as_deref() == Some("shutting-down") || state.as_deref() == Some("terminated"),
        "case {case:?}: EC2 must have terminated {named}, got {state:?}"
    );
}

/// The cluster `cluster_id` once `DescribeCluster` reports it in `state`, looked for until the
/// deadline passes rather than waited out.
async fn await_cluster_state(emulator: &Emulator, case: &str, cluster_id: &str, state: &str) {
    let deadline = Instant::now() + WAIT_DEADLINE;
    loop {
        let described = emr(
            &emulator.router,
            "DescribeCluster",
            json!({ "ClusterId": cluster_id }),
        )
        .await;
        let reported = described["Cluster"]["Status"]["State"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if reported == state {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the cluster in {state}, got {reported}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}
