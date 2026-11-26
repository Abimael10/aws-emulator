// SPDX-License-Identifier: CC0-1.0

//! The members an `InstanceGroup` read always carries, end to end over the wire.
//!
//! Every case drives the real HTTP path — `POST /` with
//! `X-Amz-Target: ElasticMapReduce.<Op>` and an `awsJson1_1` body — through the router's
//! `dispatch`, using the harness `tests/all/job_flow_lifecycle.rs` owns, so these are the
//! same bytes an SDK client sends. Nothing reaches into the service's state struct: a group
//! is only ever read back through `ListInstanceGroups`.
//!
//! THE RULES under test:
//!
//! * the `InstanceGroup` members Amazon EMR always reports are reported for a group that
//!   holds nothing of them too — `Configurations` as the empty list, `ConfigurationsVersion`
//!   as the version before any reconfiguration, `ShrinkPolicy` as the empty structure — so a
//!   client iterating or dereferencing one has no absent member to special-case;
//! * the Amazon EBS block devices a group's instances are attached are the volumes its
//!   `EbsConfiguration` requested, and the DEFAULT layout of its instance type when it
//!   requested none: general-purpose SSD volumes, 16 GiB per vCPU, over one volume for a
//!   `large`, two for an `xlarge` and four from `2xlarge` up, and none at all for an instance
//!   type that carries its own instance-store disks. The same rule a fleet's instance type is
//!   served by (`tests/all/instance_fleets.rs`).

use serde_json::{json, Value};

use crate::job_flow_lifecycle::{at, Emr, INSTANCE_PROFILE, SERVICE_ROLE};

/// The release every launch below names, which offers every instance type in the table
/// (`ListSupportedInstanceTypes`).
const RELEASE_LABEL: &str = "emr-6.15.0";

/// One row of the served-members table: the instance type and `EbsConfiguration` the group
/// is launched with, and the `(device, SizeInGB, VolumeType)` its instances must then be
/// reported as holding, in device order.
struct Case {
    label: &'static str,
    instance_type: &'static str,
    /// The `EbsConfiguration` the group's configuration carries, if any.
    ebs: Option<Value>,
    /// The `Configurations` the group's configuration carries, and the classifications it
    /// must then report.
    configurations: Option<Value>,
    classifications: &'static [&'static str],
    devices: &'static [(&'static str, i64, &'static str)],
}

/// The default layout of an `xlarge` (4 vCPUs): two general-purpose SSD volumes of 32 GiB,
/// named from `/dev/sdb` because `/dev/sda` is the root volume.
const DEFAULT_XLARGE: &[(&str, i64, &str)] = &[("/dev/sdb", 32, "gp2"), ("/dev/sdc", 32, "gp2")];

/// THE RULES above, over what they generalise over: the instance size the default layout is
/// read off (`large`, `xlarge`, `2xlarge`, `4xlarge` — one volume, two, four, and four of a
/// larger capacity), the workload class of the family (general purpose and memory
/// optimised), an instance type carrying its own instance-store disks (which is attached no
/// Amazon EBS volume at all), and a group that declared its own volumes (which gets exactly
/// those, and no default beside them). Each row also reads back the three members a group
/// that holds nothing of them still reports.
#[tokio::test]
async fn an_instance_group_read_carries_every_member_amazon_emr_always_reports() {
    let cases = vec![
        Case {
            label: "a large EBS-only type",
            instance_type: "m5.large",
            ebs: None,
            configurations: None,
            classifications: &[],
            // 2 vCPUs: 32 GiB over one volume.
            devices: &[("/dev/sdb", 32, "gp2")],
        },
        Case {
            label: "an xlarge EBS-only type, with configurations",
            instance_type: "m5.xlarge",
            ebs: None,
            configurations: Some(json!([{
                "Classification": "spark-defaults",
                "Properties": { "spark.executor.memory": "2g" },
            }])),
            classifications: &["spark-defaults"],
            devices: DEFAULT_XLARGE,
        },
        Case {
            label: "a 2xlarge EBS-only type",
            instance_type: "m5.2xlarge",
            ebs: None,
            configurations: None,
            classifications: &[],
            // 8 vCPUs: 128 GiB over four volumes.
            devices: &[
                ("/dev/sdb", 32, "gp2"),
                ("/dev/sdc", 32, "gp2"),
                ("/dev/sdd", 32, "gp2"),
                ("/dev/sde", 32, "gp2"),
            ],
        },
        Case {
            label: "a 4xlarge EBS-only type",
            instance_type: "m5.4xlarge",
            ebs: None,
            configurations: None,
            classifications: &[],
            // 16 vCPUs: 256 GiB over the same four volumes, so each one is larger.
            devices: &[
                ("/dev/sdb", 64, "gp2"),
                ("/dev/sdc", 64, "gp2"),
                ("/dev/sdd", 64, "gp2"),
                ("/dev/sde", 64, "gp2"),
            ],
        },
        Case {
            label: "a memory-optimised xlarge type",
            instance_type: "r5.xlarge",
            ebs: None,
            configurations: None,
            classifications: &[],
            devices: DEFAULT_XLARGE,
        },
        Case {
            label: "an instance type carrying its own disks",
            instance_type: "i3.xlarge",
            ebs: None,
            configurations: None,
            classifications: &[],
            // The type's storage is its instance-store disks, so Amazon EMR attaches no
            // Amazon EBS volume — and the member is still reported, as the empty list.
            devices: &[],
        },
        Case {
            label: "a group that declared its own volumes",
            instance_type: "m5.xlarge",
            ebs: Some(json!({
                "EbsBlockDeviceConfigs": [
                    {
                        "VolumeSpecification": { "VolumeType": "gp3", "SizeInGB": 100 },
                        "VolumesPerInstance": 2,
                    },
                    { "VolumeSpecification": { "VolumeType": "st1", "SizeInGB": 500 } },
                ],
            })),
            configurations: None,
            classifications: &[],
            // Exactly what was requested, in configuration order, and no default beside it.
            devices: &[
                ("/dev/sdb", 100, "gp3"),
                ("/dev/sdc", 100, "gp3"),
                ("/dev/sdd", 500, "st1"),
            ],
        },
    ];

    let emr = Emr::new();
    for case in &cases {
        let label = case.label;
        let mut group = json!({
            "Name": label,
            "InstanceRole": "MASTER",
            "InstanceType": case.instance_type,
            "InstanceCount": 1,
        });
        if let Some(ebs) = case.ebs.clone() {
            group["EbsConfiguration"] = ebs;
        }
        if let Some(configurations) = case.configurations.clone() {
            group["Configurations"] = configurations;
        }
        let cluster_id = emr
            .run_job_flow(json!({
                "Name": label,
                "ReleaseLabel": RELEASE_LABEL,
                "JobFlowRole": INSTANCE_PROFILE,
                "ServiceRole": SERVICE_ROLE,
                "Instances": {
                    "KeepJobFlowAliveWhenNoSteps": true,
                    "InstanceGroups": [group],
                },
            }))
            .await;

        let listed = emr
            .ok("ListInstanceGroups", json!({ "ClusterId": &cluster_id }))
            .await;
        let groups = listed["InstanceGroups"].as_array().cloned().unwrap_or_else(|| {
            panic!("case {label:?}: ListInstanceGroups must answer InstanceGroups, got {listed}")
        });
        let got = groups.first().unwrap_or_else(|| {
            panic!("case {label:?}: the launched group must be reported, got {listed}")
        });

        // The Amazon EBS layout: one entry per volume an instance of the group holds, with
        // the size and type that volume was asked for.
        let devices: Vec<(&str, i64, &str)> = got["EbsBlockDevices"]
            .as_array()
            .unwrap_or_else(|| {
                panic!("case {label:?}: EbsBlockDevices must be reported as a list, got {got}")
            })
            .iter()
            .map(|device| {
                (
                    device["Device"].as_str().unwrap_or_default(),
                    at(device, &["VolumeSpecification", "SizeInGB"])
                        .as_i64()
                        .unwrap_or_default(),
                    at(device, &["VolumeSpecification", "VolumeType"])
                        .as_str()
                        .unwrap_or_default(),
                )
            })
            .collect();
        assert_eq!(
            devices, case.devices,
            "case {label:?}: wrong EbsBlockDevices, got {got}"
        );

        // The specification the group requests, which is reported whether or not it holds
        // one.
        let classifications: Vec<&str> = got["Configurations"]
            .as_array()
            .unwrap_or_else(|| {
                panic!("case {label:?}: Configurations must be reported as a list, got {got}")
            })
            .iter()
            .map(|entry| entry["Classification"].as_str().unwrap_or_default())
            .collect();
        assert_eq!(
            classifications, case.classifications,
            "case {label:?}: wrong Configurations, got {got}"
        );
        assert_eq!(
            got["ConfigurationsVersion"].as_i64(),
            Some(0),
            "case {label:?}: a group nothing has reconfigured requests version 0, got {got}"
        );
        assert_eq!(
            got["ShrinkPolicy"].as_object().map(|policy| policy.len()),
            Some(0),
            "case {label:?}: a group nothing has resized reports the empty shrink policy, got {got}"
        );
    }
}

/// THE RULES' other side: the three members are the group's own state, not constants. A
/// reconfiguration moves the version the group requests and fills the specification it
/// reports, and a resize that submits a shrink policy fills that — read back through the
/// same listing, which is what shows the empty values above are what the group holds rather
/// than what the serializer always writes.
#[tokio::test]
async fn a_reconfigured_and_resized_group_reports_what_it_was_given() {
    const DECOMMISSION_TIMEOUT: i64 = 600;

    let emr = Emr::new();
    let cluster_id = emr
        .run_job_flow(json!({
            "Name": "served members after a modification",
            "ReleaseLabel": RELEASE_LABEL,
            "JobFlowRole": INSTANCE_PROFILE,
            "ServiceRole": SERVICE_ROLE,
            "Instances": {
                "KeepJobFlowAliveWhenNoSteps": true,
                "InstanceGroups": [
                    { "InstanceRole": "MASTER", "InstanceType": "m5.xlarge", "InstanceCount": 1 },
                    { "InstanceRole": "CORE", "InstanceType": "m5.xlarge", "InstanceCount": 2 },
                ],
            },
        }))
        .await;

    let listed = emr
        .ok("ListInstanceGroups", json!({ "ClusterId": &cluster_id }))
        .await;
    let core_id = listed["InstanceGroups"]
        .as_array()
        .and_then(|groups| {
            groups
                .iter()
                .find(|group| group["InstanceGroupType"].as_str() == Some("CORE"))
        })
        .and_then(|group| group["Id"].as_str())
        .unwrap_or_else(|| panic!("the launched core group must be reported, got {listed}"))
        .to_string();

    emr.ok(
        "ModifyInstanceGroups",
        json!({
            "ClusterId": &cluster_id,
            "InstanceGroups": [{
                "InstanceGroupId": &core_id,
                "InstanceCount": 1,
                "Configurations": [{
                    "Classification": "yarn-site",
                    "Properties": { "yarn.nodemanager.vmem-check-enabled": "false" },
                }],
                "ShrinkPolicy": { "DecommissionTimeout": DECOMMISSION_TIMEOUT },
            }],
        }),
    )
    .await;

    let listed = emr
        .ok("ListInstanceGroups", json!({ "ClusterId": &cluster_id }))
        .await;
    let got = listed["InstanceGroups"]
        .as_array()
        .and_then(|groups| {
            groups
                .iter()
                .find(|group| group["Id"].as_str() == Some(core_id.as_str()))
        })
        .unwrap_or_else(|| panic!("the modified group must be reported, got {listed}"))
        .clone();

    assert_eq!(
        got["ConfigurationsVersion"].as_i64(),
        Some(1),
        "case \"modified group\": the first reconfiguration is version 1, got {got}"
    );
    let classifications: Vec<&str> = got["Configurations"]
        .as_array()
        .unwrap_or_else(|| panic!("case \"modified group\": Configurations must be a list, got {got}"))
        .iter()
        .map(|entry| entry["Classification"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(
        classifications,
        vec!["yarn-site"],
        "case \"modified group\": the submitted specification must be reported, got {got}"
    );
    assert_eq!(
        at(&got, &["ShrinkPolicy", "DecommissionTimeout"]).as_i64(),
        Some(DECOMMISSION_TIMEOUT),
        "case \"modified group\": the submitted shrink policy must be reported, got {got}"
    );
}
