// SPDX-License-Identifier: CC0-1.0

//! Adding instance groups to a cluster that is already running, end to end over the wire.
//!
//! Every case drives the real HTTP path — `POST /` with
//! `X-Amz-Target: ElasticMapReduce.<Op>` and an `awsJson1_1` body — through the router's
//! `dispatch`, using the harness `tests/all/job_flow_lifecycle.rs` owns and, for the half
//! that resolves capacity in the service that OWNS instances, the two-service one
//! `tests/all/cluster_instances.rs` owns. Nothing reaches into either service's state
//! struct: an added group is only ever read back through `ListInstanceGroups`, its
//! instances through `ListInstances` and then Amazon EC2's own `DescribeInstances`, which
//! is what makes these tests evidence that the addition was persisted and really
//! provisioned.
//!
//! THE RULE: `AddInstanceGroups` appends one instance group per submitted
//! `InstanceGroupConfig` to the cluster its `JobFlowId` names — "adds one or more instance
//! groups to a running cluster" (its own doc) — each under the id Amazon EMR mints for it,
//! answers those ids with the job flow id and the cluster's ARN, provisions the Amazon EC2
//! instances each added group's `InstanceCount` asks for, and leaves the groups the cluster
//! already carried untouched; a request no running cluster could take — an id that resolves
//! to no cluster, a cluster that is shutting down or gone, a cluster whose instances are
//! instance FLEETS, a second group of a role a cluster carries one of, an instance type the
//! cluster's release does not offer, a configuration past the bounds every persisted
//! `InstanceGroupConfig` is held to — is refused with HTTP 400 and persists nothing.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::cluster_instances::{
    described_in_ec2, ec2_ids, emr as emr_served, emulator, launch as launch_in_subnet,
    list_instances, subnet,
};
use crate::job_flow_lifecycle::{
    assert_minted_id, at, Emr, INSTANCE_PROFILE, SERVICE_ROLE,
};

/// How long a wait for a clock-owed move looks for it before giving up. Generous: it bounds
/// a condition that is already due in well under a second, so a loaded host delays a wait
/// rather than failing it.
const WAIT_DEADLINE: Duration = Duration::from_secs(90);

/// One look inside that deadline.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The release every cluster below runs unless the case is about release gating.
const RELEASE: &str = "emr-6.15.0";

/// A release published before the seventh-generation AMD families, for the release-gated
/// instance type rows.
const OLDER_RELEASE: &str = "emr-5.36.1";

/// The chosen bound this suite takes against the group store
/// (`crate::state::MAX_INSTANCE_GROUPS_PER_CLUSTER`).
const MAX_INSTANCE_GROUPS_PER_CLUSTER: usize = 50;

/// The instances Amazon EMR provisions for ONE instance group
/// (`crate::state::MAX_INSTANCES_PER_GROUP`).
const MAX_INSTANCES_PER_GROUP: i64 = 65_536;

/// What one reported `InstanceGroup` must carry. Every field is what the request asked for,
/// so a row states the request's own values rather than anything the emulator invents.
#[derive(Clone, Copy)]
struct Group {
    role: &'static str,
    instance_type: &'static str,
    count: i64,
    /// "Valid values are ON_DEMAND or SPOT" (`InstanceGroup$Market` doc): a configuration
    /// naming no market provisions On-Demand instances.
    market: &'static str,
    bid_price: Option<&'static str>,
    name: Option<&'static str>,
    classifications: &'static [&'static str],
    /// The device name of each EBS volume the group requested, in configuration order.
    ebs_devices: &'static [&'static str],
}

impl Group {
    const fn of(role: &'static str, instance_type: &'static str, count: i64) -> Self {
        Self {
            role,
            instance_type,
            count,
            market: "ON_DEMAND",
            bid_price: None,
            name: None,
            classifications: &[],
            ebs_devices: &[],
        }
    }
}

/// A `RunJobFlow` body whose `Instances` member is `instances`, against [`RELEASE`].
fn launch(name: &str, instances: Value) -> Value {
    launch_against(name, RELEASE, instances)
}

/// [`launch`], against the release `release`.
fn launch_against(name: &str, release: &str, instances: Value) -> Value {
    json!({
        "Name": name,
        "ReleaseLabel": release,
        "JobFlowRole": INSTANCE_PROFILE,
        "ServiceRole": SERVICE_ROLE,
        "Instances": instances,
    })
}

/// One `TASK` group of `count` instances of `instance_type`, as an `InstanceGroups` list.
fn task_groups(instance_type: &str, count: i64) -> Value {
    json!([{
        "InstanceRole": "TASK",
        "InstanceType": instance_type,
        "InstanceCount": count,
    }])
}

/// The sentence a refusal carries, whichever casing of the member the envelope uses.
fn sentence_of(payload: &Value) -> Option<&str> {
    at(payload, &["message"])
        .as_str()
        .or_else(|| at(payload, &["Message"]).as_str())
}

/// The groups `ListInstanceGroups` reports for `cluster_id`, every page of them: following
/// the `Marker` to its end is what collects the whole listing.
async fn list_all_groups(emr: &Emr, case: &str, cluster_id: &str) -> Vec<Value> {
    let mut all: Vec<Value> = Vec::new();
    let mut marker: Option<String> = None;
    loop {
        let mut request = json!({ "ClusterId": cluster_id });
        if let Some(token) = marker.as_ref() {
            request["Marker"] = json!(token);
        }
        let out = emr.ok("ListInstanceGroups", request).await;
        let page = out["InstanceGroups"].as_array().cloned().unwrap_or_else(|| {
            panic!("case {case:?}: ListInstanceGroups must answer InstanceGroups, got {out}")
        });
        all.extend(page);
        match out["Marker"].as_str() {
            Some(token) => marker = Some(token.to_string()),
            None => break,
        }
    }
    all
}

/// The ids `ListInstanceGroups` reports for `cluster_id`, in creation order.
async fn listed_group_ids(emr: &Emr, case: &str, cluster_id: &str) -> Vec<String> {
    list_all_groups(emr, case, cluster_id)
        .await
        .iter()
        .map(|group| group["Id"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// The ids every cluster in `cluster_ids` reports its groups under — the snapshot a refusal
/// must leave exactly as it was.
///
/// A cluster carries instance fleets or instance groups and never both, so the group
/// listing is asked only of a cluster that reports carrying groups: on a fleet cluster it is
/// refused for that collection (`tests/all/instance_collection_exclusion.rs`), and that
/// refusal is itself the evidence the cluster holds no group for an addition to land in.
async fn group_ids_of_each(
    emr: &Emr,
    case: &str,
    cluster_ids: &[String],
) -> BTreeMap<String, Vec<String>> {
    let mut held = BTreeMap::new();
    for cluster_id in cluster_ids {
        let described = emr.describe_cluster(cluster_id).await;
        if at(&described, &["InstanceCollectionType"]).as_str() == Some("INSTANCE_FLEET") {
            let (status, payload) = emr
                .call("ListInstanceGroups", json!({ "ClusterId": cluster_id }))
                .await;
            assert_eq!(
                status, 400,
                "case {case:?}: a fleet cluster carries no instance group, so the group \
                 listing must be refused, got {status} with {payload}"
            );
            held.insert(cluster_id.clone(), Vec::new());
            continue;
        }
        held.insert(
            cluster_id.clone(),
            listed_group_ids(emr, case, cluster_id).await,
        );
    }
    held
}

/// The cluster's groups once every one of them is `RUNNING`, looked for until the deadline
/// passes rather than waited out — the clock owes a new group the move out of
/// `PROVISIONING`, and what a case asserts is the condition, never how long it took.
async fn groups_running(emr: &Emr, case: &str, cluster_id: &str) -> Vec<Value> {
    let deadline = Instant::now() + WAIT_DEADLINE;
    loop {
        let listed = list_all_groups(emr, case, cluster_id).await;
        let settled = !listed.is_empty()
            && listed
                .iter()
                .all(|group| at(group, &["Status", "State"]).as_str() == Some("RUNNING"));
        if settled {
            return listed;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: every group must reach RUNNING, got {listed:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Assert one reported group is the projection of what the request configured.
fn assert_group(case: &str, expected: &Group, got: &Value) {
    assert_minted_id(case, "ig-", got["Id"].as_str().unwrap_or_default());
    assert_eq!(
        got["InstanceGroupType"].as_str(),
        Some(expected.role),
        "case {case:?}: wrong InstanceGroupType, got {got}"
    );
    assert_eq!(
        got["InstanceType"].as_str(),
        Some(expected.instance_type),
        "case {case:?}: wrong InstanceType, got {got}"
    );
    assert_eq!(
        got["RequestedInstanceCount"].as_i64(),
        Some(expected.count),
        "case {case:?}: wrong RequestedInstanceCount, got {got}"
    );
    assert_eq!(
        got["Market"].as_str(),
        Some(expected.market),
        "case {case:?}: wrong Market, got {got}"
    );
    assert_eq!(
        got["BidPrice"].as_str(),
        expected.bid_price,
        "case {case:?}: wrong BidPrice, got {got}"
    );
    assert_eq!(
        got["Name"].as_str(),
        expected.name,
        "case {case:?}: wrong Name, got {got}"
    );
    let classifications: Vec<&str> = got["Configurations"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .map(|entry| entry["Classification"].as_str().unwrap_or_default())
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        classifications, expected.classifications,
        "case {case:?}: wrong Configurations, got {got}"
    );
    let devices: Vec<&str> = got["EbsBlockDevices"]
        .as_array()
        .map(|devices| {
            devices
                .iter()
                .map(|device| device["Device"].as_str().unwrap_or_default())
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        devices, expected.ebs_devices,
        "case {case:?}: wrong EbsBlockDevices, got {got}"
    );
}

/// One row of the addition table: the cluster to launch, the `AddInstanceGroups` CALLS to
/// make against it in order, and every group the cluster must then be reported as carrying —
/// the ones it launched with first, then the added ones in the order they were submitted.
struct Row {
    label: &'static str,
    instances: Value,
    additions: Vec<Value>,
    expected: Vec<Group>,
    /// How many of `expected` came from the additions, so the answered ids are matched
    /// against the tail of the listing.
    added: usize,
}

/// THE RULE's addition half, over what it generalises over: how many groups ONE call adds,
/// how many CALLS add them, the role each one fills (a task group, and a core group on a
/// cluster launched without one), the form the cluster was launched in (the uniform
/// node-type form and a configured `InstanceGroups` list), the market a group buys in, and
/// the optional members a configuration carries (a name, a bid price, configurations, EBS
/// volumes).
#[tokio::test]
async fn add_instance_groups_appends_every_submitted_group_and_a_listing_reads_them_back() {
    let rows = vec![
        Row {
            label: "one task group on a uniform-form cluster",
            instances: json!({
                "InstanceCount": 2,
                "MasterInstanceType": "m5.xlarge",
                "SlaveInstanceType": "m5.xlarge",
                "KeepJobFlowAliveWhenNoSteps": true,
            }),
            additions: vec![task_groups("c5.xlarge", 2)],
            expected: vec![
                Group::of("MASTER", "m5.xlarge", 1),
                Group::of("CORE", "m5.xlarge", 1),
                Group::of("TASK", "c5.xlarge", 2),
            ],
            added: 1,
        },
        Row {
            label: "two groups in one call, on a cluster launched with no core group",
            instances: json!({
                "InstanceCount": 1,
                "MasterInstanceType": "m5.xlarge",
                "KeepJobFlowAliveWhenNoSteps": true,
            }),
            additions: vec![json!([
                {
                    "Name": "storage",
                    "InstanceRole": "CORE",
                    "InstanceType": "r5.xlarge",
                    "InstanceCount": 2,
                    "Market": "SPOT",
                    "BidPrice": "0.10",
                },
                { "InstanceRole": "TASK", "InstanceType": "m5.large", "InstanceCount": 1 },
            ])],
            expected: vec![
                Group::of("MASTER", "m5.xlarge", 1),
                Group {
                    name: Some("storage"),
                    market: "SPOT",
                    bid_price: Some("0.10"),
                    ..Group::of("CORE", "r5.xlarge", 2)
                },
                Group::of("TASK", "m5.large", 1),
            ],
            added: 2,
        },
        Row {
            label: "two calls, each adding one group to a configured cluster",
            instances: json!({
                "KeepJobFlowAliveWhenNoSteps": true,
                "InstanceGroups": [
                    { "Name": "primary", "InstanceRole": "MASTER", "InstanceType": "m5.xlarge",
                      "InstanceCount": 1 },
                    { "InstanceRole": "CORE", "InstanceType": "m5.xlarge", "InstanceCount": 1 },
                ],
            }),
            additions: vec![
                json!([{
                    "Name": "first-wave",
                    "InstanceRole": "TASK",
                    "InstanceType": "c5.xlarge",
                    "InstanceCount": 1,
                    "Configurations": [{
                        "Classification": "spark-defaults",
                        "Properties": { "spark.executor.memory": "2g" },
                    }],
                }]),
                json!([{
                    "InstanceRole": "TASK",
                    "InstanceType": "c5.2xlarge",
                    "InstanceCount": 3,
                    "EbsConfiguration": {
                        "EbsOptimized": true,
                        "EbsBlockDeviceConfigs": [{
                            "VolumeSpecification": { "VolumeType": "gp3", "SizeInGB": 100 },
                            "VolumesPerInstance": 2,
                        }],
                    },
                }]),
            ],
            expected: vec![
                Group {
                    name: Some("primary"),
                    ..Group::of("MASTER", "m5.xlarge", 1)
                },
                Group::of("CORE", "m5.xlarge", 1),
                Group {
                    name: Some("first-wave"),
                    classifications: &["spark-defaults"],
                    ..Group::of("TASK", "c5.xlarge", 1)
                },
                Group {
                    // `/dev/sda` is the root volume, so the requested volumes start at
                    // `/dev/sdb`.
                    ebs_devices: &["/dev/sdb", "/dev/sdc"],
                    ..Group::of("TASK", "c5.2xlarge", 3)
                },
            ],
            added: 2,
        },
    ];

    let emr = Emr::new();
    for row in &rows {
        let label = row.label;
        let cluster_id = emr.run_job_flow(launch(label, row.instances.clone())).await;
        let cluster_arn = emr.describe_cluster(&cluster_id).await["ClusterArn"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let launched_ids = listed_group_ids(&emr, label, &cluster_id).await;

        // Every call's answer: the job flow it added to, that cluster's ARN, and one minted
        // id per submitted configuration, in submission order.
        let mut answered: Vec<String> = Vec::new();
        for (index, addition) in row.additions.iter().enumerate() {
            let case = format!("{label}, call {index}");
            let submitted = addition.as_array().map(Vec::len).unwrap_or_default();
            let out = emr
                .ok(
                    "AddInstanceGroups",
                    json!({ "JobFlowId": &cluster_id, "InstanceGroups": addition }),
                )
                .await;
            assert_eq!(
                out["JobFlowId"].as_str(),
                Some(cluster_id.as_str()),
                "case {case:?}: the answer names the job flow the groups were added to, \
                 got {out}"
            );
            assert_eq!(
                out["ClusterArn"].as_str(),
                Some(cluster_arn.as_str()),
                "case {case:?}: the answer carries the cluster's own ARN, got {out}"
            );
            let ids: Vec<String> = out["InstanceGroupIds"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|id| id.as_str().unwrap_or_default().to_string())
                .collect();
            assert_eq!(
                ids.len(),
                submitted,
                "case {case:?}: one id per submitted group, got {out}"
            );
            for id in &ids {
                assert_minted_id(&case, "ig-", id);
            }
            answered.extend(ids);
        }

        // The cluster now carries what it launched with, then the added groups in the order
        // they were submitted, and reports each of them as the projection of its
        // configuration once the clock has brought it up.
        let listed = groups_running(&emr, label, &cluster_id).await;
        assert_eq!(
            listed.len(),
            row.expected.len(),
            "case {label:?}: wrong number of groups, got {listed:?}"
        );
        for (expected, got) in row.expected.iter().zip(&listed) {
            assert_group(label, expected, got);
            assert_eq!(
                got["RunningInstanceCount"].as_i64(),
                Some(expected.count),
                "case {label:?}: a group that is up runs what it requested, got {got}"
            );
        }
        let ids: Vec<String> = listed
            .iter()
            .map(|group| group["Id"].as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(
            &ids[..ids.len() - row.added],
            &launched_ids[..],
            "case {label:?}: the groups the cluster launched with are left as they were"
        );
        assert_eq!(
            &ids[ids.len() - row.added..],
            &answered[..],
            "case {label:?}: the added groups are the ids the calls answered, in order"
        );
        let distinct: BTreeSet<&String> = ids.iter().collect();
        assert_eq!(
            distinct.len(),
            ids.len(),
            "case {label:?}: no group id is minted twice, got {ids:?}"
        );
    }
}

/// THE RULE's refusal half: a request no running cluster could take is refused with HTTP
/// 400 and leaves every cluster carrying exactly the groups it already had.
///
/// The rows span what the rule generalises over: the job flow id (a shape no cluster id is
/// drawn from, and well-shaped ids that resolve to nothing), the collection type the
/// cluster's capacity is stated in, the role the addition names against the roles the
/// cluster already carries, the instance type against the release the cluster runs, and
/// each bound a persisted `InstanceGroupConfig` is held to.
#[tokio::test]
async fn add_instance_groups_refuses_every_request_no_running_cluster_could_take() {
    let emr = Emr::new();
    // A cluster that DOES exist, with a master and a core group, so the refusals below are
    // about the request and not about an empty store.
    let cluster_id = emr
        .run_job_flow(launch(
            "live",
            json!({
                "InstanceCount": 2,
                "MasterInstanceType": "m5.xlarge",
                "SlaveInstanceType": "m5.xlarge",
                "KeepJobFlowAliveWhenNoSteps": true,
            }),
        ))
        .await;
    // A cluster whose capacity is stated as instance FLEETS, which carries no group at all.
    let fleet_cluster_id = emr
        .run_job_flow(launch(
            "fleets",
            json!({
                "KeepJobFlowAliveWhenNoSteps": true,
                "InstanceFleets": [{
                    "InstanceFleetType": "MASTER",
                    "TargetOnDemandCapacity": 1,
                    "InstanceTypeConfigs": [{ "InstanceType": "m5.xlarge" }],
                }],
            }),
        ))
        .await;
    // A cluster on an earlier release, carrying a master group and no core group.
    let older_cluster_id = emr
        .run_job_flow(launch_against(
            "older",
            OLDER_RELEASE,
            json!({
                "InstanceCount": 1,
                "MasterInstanceType": "m5.xlarge",
                "KeepJobFlowAliveWhenNoSteps": true,
            }),
        ))
        .await;
    let clusters = vec![
        cluster_id.clone(),
        fleet_cluster_id.clone(),
        older_cluster_id.clone(),
    ];

    let many_groups: Vec<Value> = (0..=MAX_INSTANCE_GROUPS_PER_CLUSTER)
        .map(|_| json!({ "InstanceRole": "TASK", "InstanceType": "m5.xlarge", "InstanceCount": 1 }))
        .collect();

    // (case, job flow id, submitted groups, the sentence Amazon EMR answers).
    let rows: Vec<(String, String, Value, String)> = vec![
        (
            "a job flow id no cluster carries".to_string(),
            "j-ZZZZZZZZZZZZZ".to_string(),
            task_groups("m5.xlarge", 1),
            "Specified job flow ID not valid.".to_string(),
        ),
        (
            "a job flow id that is only the prefix".to_string(),
            "j-".to_string(),
            task_groups("m5.xlarge", 1),
            "Specified job flow ID not valid.".to_string(),
        ),
        (
            "an empty job flow id".to_string(),
            String::new(),
            task_groups("m5.xlarge", 1),
            "Specified job flow ID not valid.".to_string(),
        ),
        (
            "a job flow id that is no cluster id at all".to_string(),
            "arn:aws:elasticmapreduce:us-east-1:123456789012:cluster/j-1".to_string(),
            task_groups("m5.xlarge", 1),
            "clusterId 'arn:aws:elasticmapreduce:us-east-1:123456789012:cluster/j-1' is not \
             valid."
                .to_string(),
        ),
        (
            "a request naming no instance group to add".to_string(),
            cluster_id.clone(),
            json!([]),
            "Instance groups cannot be null or empty.".to_string(),
        ),
        (
            "a cluster whose capacity is instance fleets".to_string(),
            fleet_cluster_id.clone(),
            task_groups("m5.xlarge", 1),
            format!(
                "Cluster '{fleet_cluster_id}' uses instance fleets, so an instance group \
                 cannot be added to it."
            ),
        ),
        (
            "a second primary instance group".to_string(),
            cluster_id.clone(),
            json!([{ "InstanceRole": "MASTER", "InstanceType": "m5.xlarge", "InstanceCount": 1 }]),
            "A cluster carries one MASTER instance group, which this cluster already holds."
                .to_string(),
        ),
        (
            "a second core instance group".to_string(),
            cluster_id.clone(),
            json!([{ "InstanceRole": "CORE", "InstanceType": "m5.xlarge", "InstanceCount": 1 }]),
            "A cluster carries one CORE instance group, which this cluster already holds."
                .to_string(),
        ),
        (
            // One call naming two core groups is refused by the same reading: what is
            // bounded is what the cluster would be left carrying, not what it already has.
            "two core instance groups in one call".to_string(),
            older_cluster_id.clone(),
            json!([
                { "InstanceRole": "CORE", "InstanceType": "m5.xlarge", "InstanceCount": 1 },
                { "InstanceRole": "CORE", "InstanceType": "m5.xlarge", "InstanceCount": 1 },
            ]),
            "A cluster carries one CORE instance group, which this cluster already holds."
                .to_string(),
        ),
        (
            // The seventh-generation AMD families arrived with the 7.x series, so the 5.x
            // cluster above cannot be provisioned from one, while the type itself IS an
            // instance type of this service — which is the sentence that names the release.
            "a real instance type the cluster's release does not offer".to_string(),
            older_cluster_id.clone(),
            task_groups("c7a.8xlarge", 1),
            format!("Instance type c7a.8xlarge not supported for release {OLDER_RELEASE}."),
        ),
        (
            // No family ladder carries an `m5.3xlarge`, so the value names no instance type
            // at all and earns the sentence that names no release.
            "a value that names no instance type at all".to_string(),
            cluster_id.clone(),
            task_groups("m5.3xlarge", 1),
            "Instance type 'm5.3xlarge' is not supported.".to_string(),
        ),
        (
            "an instance count one past what a group is provisioned".to_string(),
            cluster_id.clone(),
            task_groups("m5.xlarge", MAX_INSTANCES_PER_GROUP + 1),
            format!(
                "InstanceCount {} is above {MAX_INSTANCES_PER_GROUP}, which is the maximum \
                 number of instances provisioned per instance group",
                MAX_INSTANCES_PER_GROUP + 1
            ),
        ),
        (
            "more groups in one call than a cluster carries".to_string(),
            cluster_id.clone(),
            json!(many_groups),
            format!(
                "InstanceGroups names {} instance groups; a cluster carries at most \
                 {MAX_INSTANCE_GROUPS_PER_CLUSTER}",
                many_groups.len()
            ),
        ),
        (
            "a configuration nested under a parent that does not hold it".to_string(),
            cluster_id.clone(),
            json!([{
                "InstanceRole": "TASK",
                "InstanceType": "m5.xlarge",
                "InstanceCount": 1,
                "Configurations": [{
                    "Classification": "spark-defaults",
                    "Configurations": [{ "Classification": "hive-site" }],
                }],
            }]),
            "Classification 'hive-site' is not valid for parent classification \
             'spark-defaults'."
                .to_string(),
        ),
    ];

    for (case, job_flow_id, groups, sentence) in &rows {
        let before = group_ids_of_each(&emr, case, &clusters).await;
        let (status, payload) = emr
            .call(
                "AddInstanceGroups",
                json!({ "JobFlowId": job_flow_id, "InstanceGroups": groups }),
            )
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {payload}"
        );
        // The operation models no client error of its own, so the request-validation
        // envelope is what carries every refusal of it.
        let code = at(&payload, &["__type"]).as_str().unwrap_or_default();
        assert!(
            code.contains("ValidationException"),
            "case {case:?}: expected ValidationException, got {code:?} in {payload}"
        );
        assert_eq!(
            sentence_of(&payload),
            Some(sentence.as_str()),
            "case {case:?}: wrong sentence, got {payload}"
        );
        assert!(
            payload.get("InstanceGroupIds").is_none(),
            "case {case:?}: a refused addition hands back no group id, got {payload}"
        );
        assert_eq!(
            group_ids_of_each(&emr, case, &clusters).await,
            before,
            "case {case:?}: a refused addition persists no group"
        );
    }
}

/// THE RULE's bound half: the ceiling ITSELF is an addition Amazon EMR takes, and the group
/// one past it is refused for what the cluster already carries — so the refusal is about
/// the value past the bound, not about the shape of the request.
#[tokio::test]
async fn an_addition_filling_the_group_ceiling_is_served_and_the_next_one_refused() {
    let emr = Emr::new();
    let cluster_id = emr
        .run_job_flow(launch(
            "at-the-bound",
            json!({
                "InstanceCount": 1,
                "MasterInstanceType": "m5.xlarge",
                "KeepJobFlowAliveWhenNoSteps": true,
            }),
        ))
        .await;
    let held = listed_group_ids(&emr, "at the bound", &cluster_id).await.len();
    let room = MAX_INSTANCE_GROUPS_PER_CLUSTER - held;
    let fills: Vec<Value> = (0..room)
        .map(|_| json!({ "InstanceRole": "TASK", "InstanceType": "m5.xlarge", "InstanceCount": 1 }))
        .collect();
    let out = emr
        .ok(
            "AddInstanceGroups",
            json!({ "JobFlowId": &cluster_id, "InstanceGroups": fills }),
        )
        .await;
    assert_eq!(
        out["InstanceGroupIds"].as_array().map(Vec::len),
        Some(room),
        "case \"at the bound\": the addition that fills the cluster is accepted, got {out}"
    );
    assert_eq!(
        listed_group_ids(&emr, "at the bound", &cluster_id).await.len(),
        MAX_INSTANCE_GROUPS_PER_CLUSTER,
        "case \"at the bound\": every group of the accepted addition is persisted"
    );

    let (status, payload) = emr
        .call(
            "AddInstanceGroups",
            json!({
                "JobFlowId": &cluster_id,
                "InstanceGroups": [{
                    "InstanceRole": "TASK", "InstanceType": "m5.xlarge", "InstanceCount": 1,
                }],
            }),
        )
        .await;
    assert_eq!(
        status, 400,
        "case \"past the bound\": expected HTTP 400, got {status} with {payload}"
    );
    assert_eq!(
        sentence_of(&payload),
        Some(
            format!(
                "Cluster '{cluster_id}' already carries {MAX_INSTANCE_GROUPS_PER_CLUSTER} \
                 instance groups; a cluster carries at most \
                 {MAX_INSTANCE_GROUPS_PER_CLUSTER}."
            )
            .as_str()
        ),
        "case \"past the bound\": wrong sentence, got {payload}"
    );
    assert_eq!(
        listed_group_ids(&emr, "past the bound", &cluster_id).await.len(),
        MAX_INSTANCE_GROUPS_PER_CLUSTER,
        "case \"past the bound\": a refused addition persists no group"
    );
}

/// THE RULE's state half: a cluster that is shutting down or gone takes no addition — its
/// capacity is being released, so there is nothing to add to — and the sentence the legacy
/// job-flow surface states that with names neither the id nor the state.
#[tokio::test]
async fn add_instance_groups_refuses_a_cluster_that_is_shutting_down_or_gone() {
    let emr = Emr::new();
    let cluster_id = emr
        .run_job_flow(launch(
            "doomed",
            json!({
                "InstanceCount": 1,
                "MasterInstanceType": "m5.xlarge",
                "KeepJobFlowAliveWhenNoSteps": true,
            }),
        ))
        .await;
    let before = listed_group_ids(&emr, "shutting down", &cluster_id).await;
    emr.ok("TerminateJobFlows", json!({ "JobFlowIds": [&cluster_id] }))
        .await;

    // Every state the shutdown passes through refuses the addition: the one the call put the
    // cluster in, and the terminal state the clock then owes it — looked for until it is
    // reported rather than waited out.
    let deadline = Instant::now() + WAIT_DEADLINE;
    let mut states: BTreeSet<String> = BTreeSet::new();
    loop {
        let state = at(
            &emr.describe_cluster(&cluster_id).await,
            &["Status", "State"],
        )
        .as_str()
        .unwrap_or_default()
        .to_string();
        let case = format!("a cluster in state {state}");
        let (status, payload) = emr
            .call(
                "AddInstanceGroups",
                json!({
                    "JobFlowId": &cluster_id,
                    "InstanceGroups": [{
                        "InstanceRole": "TASK", "InstanceType": "m5.xlarge", "InstanceCount": 1,
                    }],
                }),
            )
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {payload}"
        );
        assert_eq!(
            sentence_of(&payload),
            Some("A job flow that is shutting down, terminated, or finished may not be modified."),
            "case {case:?}: wrong sentence, got {payload}"
        );
        assert_eq!(
            listed_group_ids(&emr, &case, &cluster_id).await,
            before,
            "case {case:?}: a refused addition persists no group"
        );
        states.insert(state.clone());
        if state != "TERMINATING" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the cluster must reach a terminal state, it is still {state}, having been in \
             {states:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// THE RULE's capacity half, in the service that OWNS instances: the instances an added
/// group's `InstanceCount` asks for are really launched in Amazon EC2 — so `ListInstances`
/// reports one record per instance for the new group and Amazon EC2 itself holds each of
/// them, of the instance type the group was configured with and in the cluster's subnet.
#[tokio::test]
async fn an_added_instance_group_runs_the_amazon_ec2_instances_it_asks_for() {
    let case = "a task group added to a running cluster";
    let emulator = emulator();
    let subnet_id = subnet(&emulator).await;
    let cluster_id = launch_in_subnet(
        &emulator,
        case,
        &subnet_id,
        json!({
            "InstanceGroups": [
                { "InstanceRole": "MASTER", "InstanceType": "m5.xlarge", "InstanceCount": 1 },
            ],
        }),
    )
    .await;

    let added = 2;
    let out = emr_served(
        &emulator.router,
        "AddInstanceGroups",
        json!({
            "JobFlowId": &cluster_id,
            "InstanceGroups": [{
                "InstanceRole": "TASK",
                "InstanceType": "c5.xlarge",
                "InstanceCount": added,
            }],
        }),
    )
    .await;
    let group_id = out["InstanceGroupIds"][0]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the addition answers a group id, got {out}"))
        .to_string();

    // The instances of the ADDED group only, so what is asserted is the capacity this call
    // asked for rather than the cluster's whole listing.
    let deadline = Instant::now() + WAIT_DEADLINE;
    let instances = loop {
        let listed = list_instances(
            &emulator,
            case,
            &cluster_id,
            json!({ "InstanceGroupId": &group_id }),
        )
        .await;
        let settled = listed.len() == added as usize
            && listed
                .iter()
                .all(|instance| at(instance, &["Status", "State"]).as_str() == Some("RUNNING"));
        if settled {
            break listed;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the added group must run {added} instances, got {listed:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    };

    let ids = ec2_ids(&instances);
    let held = described_in_ec2(&emulator, &ids).await;
    for id in &ids {
        let instance = held.get(id).unwrap_or_else(|| {
            panic!("case {case:?}: Amazon EC2 must hold {id:?}, it holds {:?}", held.keys())
        });
        assert_eq!(
            instance.instance_type.as_deref(),
            Some("c5.xlarge"),
            "case {case:?}: {id:?} must be the type the added group configured"
        );
        assert_eq!(
            instance.subnet_id.as_deref(),
            Some(subnet_id.as_str()),
            "case {case:?}: {id:?} must run in the cluster's subnet"
        );
        assert_eq!(
            instance.state, "running",
            "case {case:?}: {id:?} must be running in Amazon EC2"
        );
    }
}
