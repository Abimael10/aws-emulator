// SPDX-License-Identifier: CC0-1.0

//! The cluster SHAPES `RunJobFlow` serves, and the ones it refuses, end to end over the
//! wire.
//!
//! Every case drives the real HTTP path — `POST /` with
//! `X-Amz-Target: ElasticMapReduce.RunJobFlow` and an `awsJson1_1` body — through the
//! router's `dispatch`, using the harness `tests/all/job_flow_lifecycle.rs` owns, so these
//! are the same bytes an SDK client sends. An accepted launch is read back through
//! `DescribeCluster`, and a refused one is checked to have written no cluster at all.
//!
//! THE RULES, each one a thing Amazon EMR will not provision:
//!
//! * an instance fleet targets capacity it can provision — "at least one of
//!   `TargetSpotCapacity` and `TargetOnDemandCapacity` should be greater than 0" — and the
//!   fleet hosting the PRIMARY nodes buys that capacity in one market: "for a master
//!   instance fleet, only one of `TargetSpotCapacity` and `TargetOnDemandCapacity` can be
//!   specified" (`InstanceFleetConfig` target-capacity docs, Smithy model);
//! * a placement group places the PRIMARY nodes of a multi-primary cluster, spread across
//!   the hardware: "the only supported instance role is `MASTER`" and "the only supported
//!   placement strategy is `SPREAD` for the `MASTER` instance role"
//!   (`PlacementGroupConfig` member docs), and the configuration is offered only on a
//!   multi-primary cluster — whichever strategy an entry names, the `NONE` that creates no
//!   group included;
//! * a cluster with more than one primary node keeps the file system its primaries
//!   coordinate on CORE nodes, so it has a core instance group;
//! * more than one primary node is a capability of the RELEASE the cluster runs, which the
//!   `ReleaseLabel` names (`RunJobFlow$ReleaseLabel` doc);
//! * a configuration classification is one its parent holds — the CLUSTER at the top of a
//!   tree, which holds the classifications Amazon EMR publishes, and the enclosing
//!   configuration below it, since a nested one is "a list of additional configurations to
//!   apply WITHIN a configuration object" (`Configuration$Configurations` doc);
//! * a launch describes its capacity in ONE of the three forms `JobFlowInstancesConfig`
//!   offers: "the `RunJobFlow` request can contain `InstanceFleets` parameters or
//!   `InstanceGroups` parameters, but not both" (`RunJobFlow` doc), the third being the
//!   uniform instance-count-and-node-types form.
//!
//! Each refusal is HTTP 400 `ValidationException` carrying Amazon EMR's own sentence —
//! recorded behavior, since `RunJobFlow` models only `InternalServerError` — and every
//! sentence is written out here rather than read off the implementation.

use serde_json::{json, Value};

use crate::job_flow_lifecycle::{assert_refused, at, Emr, INSTANCE_PROFILE, SERVICE_ROLE};
use crate::refusal_messages::message;

/// The release every launch below names unless the case is about the release itself: one
/// the catalog offers, later than the release that introduced the multi-primary topology.
const RELEASE: &str = "emr-6.15.0";

/// A `RunJobFlow` body carrying `members` on top of the required ones.
fn launch(name: &str, members: Value) -> Value {
    let mut request = json!({
        "Name": name,
        "ReleaseLabel": RELEASE,
        "JobFlowRole": INSTANCE_PROFILE,
        "ServiceRole": SERVICE_ROLE,
        "Instances": { "KeepJobFlowAliveWhenNoSteps": true },
    });
    if let Some(members) = members.as_object() {
        for (member, value) in members {
            request[member] = value.clone();
        }
    }
    request
}

/// An instance group of `role` holding `count` instances.
fn group(role: &str, count: i64) -> Value {
    json!({
        "Name": role,
        "InstanceRole": role,
        "InstanceType": "m5.xlarge",
        "InstanceCount": count,
    })
}

/// The `Instances` of a launch stating its capacity as `groups`.
fn with_groups(groups: Value) -> Value {
    json!({ "KeepJobFlowAliveWhenNoSteps": true, "InstanceGroups": groups })
}

/// An instance fleet hosting `fleet_type`, with the target capacities the case names — a
/// capacity of `None` is left out of the request altogether, which asks for no units of
/// that market.
fn fleet(fleet_type: &str, on_demand: Option<i64>, spot: Option<i64>) -> Value {
    let mut config = json!({
        "Name": fleet_type,
        "InstanceFleetType": fleet_type,
        "InstanceTypeConfigs": [{ "InstanceType": "m5.xlarge" }],
    });
    if let Some(units) = on_demand {
        config["TargetOnDemandCapacity"] = json!(units);
    }
    if let Some(units) = spot {
        config["TargetSpotCapacity"] = json!(units);
    }
    config
}

/// The cluster ids `ListClusters` reports.
async fn listed_clusters(emr: &Emr) -> Vec<String> {
    let payload = emr.ok("ListClusters", json!({})).await;
    at(&payload, &["Clusters"])
        .as_array()
        .map(|clusters| {
            clusters
                .iter()
                .filter_map(|cluster| cluster["Id"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Send one launch and assert it is refused with `expected`, having written no cluster.
async fn assert_launch_refused(emr: &Emr, case: &str, request: Value, expected: &str) {
    let before = listed_clusters(emr).await;
    let (status, payload) = emr.call("RunJobFlow", request).await;
    assert_refused(case, status, &payload, "ValidationException");
    let said = message(case, &payload);
    assert_eq!(
        said, expected,
        "case {case:?}: expected {expected:?}, got {said:?} in {payload}"
    );
    assert!(
        payload.get("JobFlowId").is_none(),
        "case {case:?}: a refused launch hands back no cluster id, got {payload}"
    );
    assert_eq!(
        listed_clusters(emr).await,
        before,
        "case {case:?}: a refused launch must write no cluster"
    );
}

/// Send one launch, assert it is served, and answer the cluster it created as
/// `DescribeCluster` reports it.
async fn assert_launch_served(emr: &Emr, case: &str, request: Value) -> Value {
    let (status, payload) = emr.call("RunJobFlow", request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the launch to be served, got {status} with {payload}"
    );
    let cluster_id = payload["JobFlowId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: a served launch answers a JobFlowId, got {payload}"))
        .to_string();
    emr.describe_cluster(&cluster_id).await
}

/// The sentence a fleet that targets no units of either market is refused with: fixed text
/// but the fleet's name, which is the node type it hosts.
fn empty_fleet_sentence(fleet_name: &str) -> String {
    format!(
        "The instance fleet ({fleet_name}) should have a value of one or greater for \
         either targetOnDemandCapacity or targetSpotCapacity."
    )
}

/// The sentence a primary fleet asking for both markets is refused with: entirely fixed.
const MASTER_FLEET_BOTH_MARKETS_SENTENCE: &str =
    "Configure the master instance fleet using either targetOnDemandCapacity or \
     targetSpotCapacity, but not both.";

/// The sentence a launch describing its capacity in more than one of the three forms is
/// refused with: entirely fixed, and it names the three forms themselves.
const SINGLE_CAPACITY_FORM_SENTENCE: &str =
    "Please configure instances using one and only one of the following: instance groups; \
     instance fleets; instance count, master and slave instance type.";

/// One row of the capacity-form rule: the `Instances` member of the launch, and whether the
/// launch is refused for describing its capacity more than one way.
struct CapacityFormCase {
    label: &'static str,
    instances: Value,
    refused: bool,
}

fn capacity_form_cases() -> Vec<CapacityFormCase> {
    vec![
        // Two forms at once: every pair of the three.
        CapacityFormCase {
            label: "instance groups beside instance fleets",
            instances: json!({
                "KeepJobFlowAliveWhenNoSteps": true,
                "InstanceGroups": [group("MASTER", 1)],
                "InstanceFleets": [fleet("MASTER", Some(1), None)],
            }),
            refused: true,
        },
        CapacityFormCase {
            label: "instance groups beside the uniform master instance type",
            instances: json!({
                "KeepJobFlowAliveWhenNoSteps": true,
                "InstanceGroups": [group("MASTER", 1)],
                "InstanceCount": 1,
                "MasterInstanceType": "m5.xlarge",
            }),
            refused: true,
        },
        CapacityFormCase {
            label: "instance groups beside the uniform slave instance type",
            instances: json!({
                "KeepJobFlowAliveWhenNoSteps": true,
                "InstanceGroups": [group("MASTER", 1), group("CORE", 1)],
                "SlaveInstanceType": "m5.xlarge",
            }),
            refused: true,
        },
        CapacityFormCase {
            label: "instance fleets beside the uniform master instance type",
            instances: json!({
                "KeepJobFlowAliveWhenNoSteps": true,
                "InstanceFleets": [fleet("MASTER", Some(1), None)],
                "InstanceCount": 1,
                "MasterInstanceType": "m5.xlarge",
            }),
            refused: true,
        },
        // One form alone: each of the three.
        CapacityFormCase {
            label: "instance groups alone",
            instances: with_groups(json!([group("MASTER", 1)])),
            refused: false,
        },
        CapacityFormCase {
            label: "instance fleets alone",
            instances: json!({
                "KeepJobFlowAliveWhenNoSteps": true,
                "InstanceFleets": [fleet("MASTER", Some(1), None)],
            }),
            refused: false,
        },
        CapacityFormCase {
            label: "the uniform form alone",
            instances: json!({
                "KeepJobFlowAliveWhenNoSteps": true,
                "InstanceCount": 2,
                "MasterInstanceType": "m5.xlarge",
                "SlaveInstanceType": "m5.xlarge",
            }),
            refused: false,
        },
        // A collection the request carried EMPTY describes no capacity in that
        // vocabulary, so it is not a second form.
        CapacityFormCase {
            label: "an empty instance-group list beside the uniform form",
            instances: json!({
                "KeepJobFlowAliveWhenNoSteps": true,
                "InstanceGroups": [],
                "InstanceCount": 1,
                "MasterInstanceType": "m5.xlarge",
            }),
            refused: false,
        },
        CapacityFormCase {
            label: "an empty instance-fleet list beside instance groups",
            instances: json!({
                "KeepJobFlowAliveWhenNoSteps": true,
                "InstanceFleets": [],
                "InstanceGroups": [group("MASTER", 1)],
            }),
            refused: false,
        },
        // `InstanceCount` is the number of instances OF the uniform form: on its own it
        // names no node type to provision, so it is not a form of its own. (An EMPTY
        // instance type is no row of this table: the member's modelled minimum length
        // refuses that at the boundary, before any handler sees it.)
        CapacityFormCase {
            label: "an instance count beside instance groups",
            instances: json!({
                "KeepJobFlowAliveWhenNoSteps": true,
                "InstanceGroups": [group("MASTER", 1)],
                "InstanceCount": 1,
            }),
            refused: false,
        },
    ]
}

/// THE RULE: a launch describes its capacity in ONE of the three forms
/// `JobFlowInstancesConfig` offers — "the `RunJobFlow` request can contain `InstanceFleets`
/// parameters or `InstanceGroups` parameters, but not both" (`RunJobFlow` doc), and the
/// uniform instance-count-and-node-types form is the third way of describing the same
/// capacity — so a request that mixes two of them is refused, with Amazon EMR's own
/// sentence naming all three.
#[tokio::test]
async fn a_launch_describes_its_capacity_in_one_and_only_one_form() {
    let emr = Emr::new();
    for case in capacity_form_cases() {
        let label = case.label;
        let request = launch(label, json!({ "Instances": case.instances.clone() }));
        if case.refused {
            assert_launch_refused(&emr, label, request, SINGLE_CAPACITY_FORM_SENTENCE).await;
        } else {
            let cluster = assert_launch_served(&emr, label, request).await;
            assert_eq!(
                cluster["Name"].as_str(),
                Some(label),
                "case {label:?}: the launched cluster must be the one asked for, got {cluster}"
            );
        }
    }
}

/// One row of the fleet-capacity rule.
struct FleetCase {
    label: &'static str,
    /// The fleet the row is about: its node type and the capacities it targets.
    fleet_type: &'static str,
    on_demand: Option<i64>,
    spot: Option<i64>,
    /// The sentence the launch is refused with, or `None` when it is served.
    refusal: Option<String>,
}

fn fleet_cases() -> Vec<FleetCase> {
    vec![
        FleetCase {
            label: "a core fleet with both targets zero",
            fleet_type: "CORE",
            on_demand: Some(0),
            spot: Some(0),
            refusal: Some(empty_fleet_sentence("CoreInstanceFleet")),
        },
        FleetCase {
            label: "a task fleet naming neither target",
            fleet_type: "TASK",
            on_demand: None,
            spot: None,
            refusal: Some(empty_fleet_sentence("TaskInstanceFleet")),
        },
        FleetCase {
            label: "a primary fleet with an explicit zero of one market and none of the other",
            fleet_type: "MASTER",
            on_demand: Some(0),
            spot: None,
            refusal: Some(empty_fleet_sentence("MasterInstanceFleet")),
        },
        FleetCase {
            label: "a primary fleet of one unit in each market",
            fleet_type: "MASTER",
            on_demand: Some(1),
            spot: Some(1),
            refusal: Some(MASTER_FLEET_BOTH_MARKETS_SENTENCE.to_string()),
        },
        FleetCase {
            label: "a primary fleet of three On-Demand units and one Spot unit",
            fleet_type: "MASTER",
            on_demand: Some(3),
            spot: Some(1),
            refusal: Some(MASTER_FLEET_BOTH_MARKETS_SENTENCE.to_string()),
        },
        FleetCase {
            label: "a primary fleet of one On-Demand unit",
            fleet_type: "MASTER",
            on_demand: Some(1),
            spot: Some(0),
            refusal: None,
        },
        FleetCase {
            label: "a primary fleet of three Spot units",
            fleet_type: "MASTER",
            on_demand: None,
            spot: Some(3),
            refusal: None,
        },
        FleetCase {
            label: "a core fleet buying units in both markets, which is not the primary",
            fleet_type: "CORE",
            on_demand: Some(2),
            spot: Some(5),
            refusal: None,
        },
        FleetCase {
            label: "a task fleet of Spot units alone",
            fleet_type: "TASK",
            on_demand: None,
            spot: Some(4),
            refusal: None,
        },
    ]
}

/// THE RULE for an instance fleet's target capacity, over the markets, the node types and
/// the two forms of "no units" (an explicit zero and an absent member).
#[tokio::test]
async fn an_instance_fleet_is_launched_only_with_capacity_it_can_provision() {
    let emr = Emr::new();

    for case in fleet_cases() {
        let label = case.label;
        // A row about a non-primary fleet carries a serviceable primary fleet beside it,
        // so the row's own fleet is the only thing being judged.
        let mut fleets = Vec::new();
        if case.fleet_type != "MASTER" {
            fleets.push(fleet("MASTER", Some(1), None));
        }
        fleets.push(fleet(case.fleet_type, case.on_demand, case.spot));
        let request = launch(
            label,
            json!({
                "Instances": {
                    "KeepJobFlowAliveWhenNoSteps": true,
                    "InstanceFleets": fleets,
                },
            }),
        );

        match case.refusal.as_deref() {
            Some(expected) => assert_launch_refused(&emr, label, request, expected).await,
            None => {
                // The accepted fleet is on the cluster the launch created, with the
                // capacities it asked for.
                let cluster = assert_launch_served(&emr, label, request).await;
                let cluster_id = cluster["Id"].as_str().unwrap_or_default().to_string();
                let listed = emr
                    .ok("ListInstanceFleets", json!({ "ClusterId": cluster_id }))
                    .await;
                let reported = at(&listed, &["InstanceFleets"])
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .find(|reported| reported["Name"].as_str() == Some(case.fleet_type))
                    .unwrap_or_else(|| {
                        panic!("case {label:?}: the launched fleet must be listed, got {listed}")
                    });
                assert_eq!(
                    (
                        reported["TargetOnDemandCapacity"].as_i64(),
                        reported["TargetSpotCapacity"].as_i64(),
                    ),
                    (
                        Some(case.on_demand.unwrap_or(0)),
                        Some(case.spot.unwrap_or(0)),
                    ),
                    "case {label:?}: the fleet reports the capacity it was given, got {reported}"
                );
            }
        }
    }
}

/// The sentence a placement group for a role Amazon EMR does not place is refused with.
const PLACEMENT_GROUP_ROLE_SENTENCE: &str =
    "Placement group supports only the following instance role: [MASTER]";

/// The sentence a primary placement group of an unsupported strategy is refused with.
const PLACEMENT_GROUP_STRATEGY_SENTENCE: &str =
    "Placement group for master node supports only the following placement strategy: \
     [SPREAD, NONE]";

/// The sentence a launch that configures ONE instance role with several placement group
/// entries is refused with. Its one variable part is the role configured more than once,
/// and only the primary role is placed at all ([`PLACEMENT_GROUP_ROLE_SENTENCE`] answers
/// every other), so that is the role this sentence can name.
const PLACEMENT_GROUP_SINGLE_STRATEGY_SENTENCE: &str =
    "Placement group supports single placement strategy for an instance role. Found \
     multiple strategy for the following instance role: MASTER";

/// The sentence a primary placement group on a cluster that is not multi-primary is
/// refused with.
const PLACEMENT_GROUP_MULTI_MASTER_SENTENCE: &str =
    "Placement group support for master node is available only for multi-master cluster \
     and not for single-master cluster.";

/// One row of the placement-group rule: the entries the launch carries, how many primary
/// nodes the cluster it describes has, and the sentence it earns.
struct PlacementCase {
    label: &'static str,
    configs: Value,
    primaries: i64,
    refusal: Option<&'static str>,
}

fn placement_cases() -> Vec<PlacementCase> {
    vec![
        // `com.amazonaws.emr#PlacementGroupStrategy` declares SPREAD, PARTITION, CLUSTER
        // and NONE (`smithy_query(query="shape", shape="PlacementGroupStrategy")`); the
        // two that are neither SPREAD nor NONE place no cluster's primaries.
        PlacementCase {
            label: "the PARTITION strategy on a multi-primary cluster",
            configs: json!([{ "InstanceRole": "MASTER", "PlacementStrategy": "PARTITION" }]),
            primaries: 3,
            refusal: Some(PLACEMENT_GROUP_STRATEGY_SENTENCE),
        },
        PlacementCase {
            label: "the CLUSTER strategy on a multi-primary cluster",
            configs: json!([{ "InstanceRole": "MASTER", "PlacementStrategy": "CLUSTER" }]),
            primaries: 3,
            refusal: Some(PLACEMENT_GROUP_STRATEGY_SENTENCE),
        },
        // `com.amazonaws.emr#InstanceRoleType` declares MASTER, CORE and TASK; only the
        // primary role is placed, whichever strategy the entry names.
        PlacementCase {
            label: "the TASK role",
            configs: json!([{ "InstanceRole": "TASK", "PlacementStrategy": "SPREAD" }]),
            primaries: 3,
            refusal: Some(PLACEMENT_GROUP_ROLE_SENTENCE),
        },
        PlacementCase {
            label: "the CORE role",
            configs: json!([{ "InstanceRole": "CORE", "PlacementStrategy": "SPREAD" }]),
            primaries: 3,
            refusal: Some(PLACEMENT_GROUP_ROLE_SENTENCE),
        },
        PlacementCase {
            label: "the TASK role with a strategy that is also unsupported",
            configs: json!([{ "InstanceRole": "TASK", "PlacementStrategy": "PARTITION" }]),
            primaries: 3,
            refusal: Some(PLACEMENT_GROUP_ROLE_SENTENCE),
        },
        PlacementCase {
            label: "a primary placement group on a single-primary cluster",
            configs: json!([{ "InstanceRole": "MASTER", "PlacementStrategy": "SPREAD" }]),
            primaries: 1,
            refusal: Some(PLACEMENT_GROUP_MULTI_MASTER_SENTENCE),
        },
        PlacementCase {
            label: "a primary placement group on a two-primary cluster",
            configs: json!([{ "InstanceRole": "MASTER", "PlacementStrategy": "SPREAD" }]),
            primaries: 2,
            refusal: Some(PLACEMENT_GROUP_MULTI_MASTER_SENTENCE),
        },
        PlacementCase {
            label: "a primary placement group on a four-primary cluster",
            configs: json!([{ "InstanceRole": "MASTER", "PlacementStrategy": "SPREAD" }]),
            primaries: 4,
            refusal: Some(PLACEMENT_GROUP_MULTI_MASTER_SENTENCE),
        },
        PlacementCase {
            label: "an entry naming no strategy, which still asks for a group",
            configs: json!([{ "InstanceRole": "MASTER" }]),
            primaries: 1,
            refusal: Some(PLACEMENT_GROUP_MULTI_MASTER_SENTENCE),
        },
        // One instance role is placed under ONE strategy, so a second entry for a role the
        // request already configured asks for a second placement of it — whichever
        // strategies the two entries carry, and whether or not they agree.
        PlacementCase {
            label: "the primary role configured with two different strategies",
            configs: json!([
                { "InstanceRole": "MASTER", "PlacementStrategy": "SPREAD" },
                { "InstanceRole": "MASTER", "PlacementStrategy": "NONE" },
            ]),
            primaries: 3,
            refusal: Some(PLACEMENT_GROUP_SINGLE_STRATEGY_SENTENCE),
        },
        PlacementCase {
            label: "the primary role configured twice with the same strategy",
            configs: json!([
                { "InstanceRole": "MASTER", "PlacementStrategy": "SPREAD" },
                { "InstanceRole": "MASTER", "PlacementStrategy": "SPREAD" },
            ]),
            primaries: 3,
            refusal: Some(PLACEMENT_GROUP_SINGLE_STRATEGY_SENTENCE),
        },
        PlacementCase {
            label: "the primary role configured twice, once with no strategy at all",
            configs: json!([
                { "InstanceRole": "MASTER" },
                { "InstanceRole": "MASTER", "PlacementStrategy": "SPREAD" },
            ]),
            primaries: 3,
            refusal: Some(PLACEMENT_GROUP_SINGLE_STRATEGY_SENTENCE),
        },
        // A role configured twice is reported before the STRATEGIES those entries carry:
        // the request has asked for two placements of one role, which is what a caller
        // fixes, and no single strategy of theirs is the one to report on.
        PlacementCase {
            label: "the primary role configured twice, one entry with an unplaced strategy",
            configs: json!([
                { "InstanceRole": "MASTER", "PlacementStrategy": "SPREAD" },
                { "InstanceRole": "MASTER", "PlacementStrategy": "CLUSTER" },
            ]),
            primaries: 3,
            refusal: Some(PLACEMENT_GROUP_SINGLE_STRATEGY_SENTENCE),
        },
        // And AFTER which role may be placed at all: a role Amazon EMR places none of is
        // reported as that, however many entries configure it.
        PlacementCase {
            label: "a role that is not placed at all, configured twice",
            configs: json!([
                { "InstanceRole": "CORE", "PlacementStrategy": "SPREAD" },
                { "InstanceRole": "CORE", "PlacementStrategy": "NONE" },
            ]),
            primaries: 3,
            refusal: Some(PLACEMENT_GROUP_ROLE_SENTENCE),
        },
        PlacementCase {
            label: "the SPREAD strategy on a multi-primary cluster",
            configs: json!([{ "InstanceRole": "MASTER", "PlacementStrategy": "SPREAD" }]),
            primaries: 3,
            refusal: None,
        },
        PlacementCase {
            label: "the NONE strategy, which asks for no placement group, on one primary",
            configs: json!([{ "InstanceRole": "MASTER", "PlacementStrategy": "NONE" }]),
            primaries: 1,
            refusal: Some(PLACEMENT_GROUP_MULTI_MASTER_SENTENCE),
        },
        PlacementCase {
            label: "the NONE strategy on a two-primary cluster",
            configs: json!([{ "InstanceRole": "MASTER", "PlacementStrategy": "NONE" }]),
            primaries: 2,
            refusal: Some(PLACEMENT_GROUP_MULTI_MASTER_SENTENCE),
        },
        PlacementCase {
            label: "the NONE strategy on a multi-primary cluster, which creates no group",
            configs: json!([{ "InstanceRole": "MASTER", "PlacementStrategy": "NONE" }]),
            primaries: 3,
            refusal: None,
        },
        PlacementCase {
            label: "no placement group configs at all on one primary",
            configs: json!([]),
            primaries: 1,
            refusal: None,
        },
    ]
}

/// THE RULE for a placement group: which role may be placed, which strategy places it, and
/// which clusters have primaries to spread.
#[tokio::test]
async fn a_placement_group_is_launched_only_for_the_primaries_of_a_multi_primary_cluster() {
    let emr = Emr::new();

    for case in placement_cases() {
        let label = case.label;
        // A cluster with several primaries keeps its file system on core nodes, so every
        // row carries a core group beside its primary group.
        let request = launch(
            label,
            json!({
                "PlacementGroupConfigs": case.configs.clone(),
                "Instances": with_groups(json!([
                    group("MASTER", case.primaries),
                    group("CORE", 2),
                ])),
            }),
        );

        match case.refusal {
            Some(expected) => assert_launch_refused(&emr, label, request, expected).await,
            None => {
                // An accepted launch creates exactly the groups the entries ask for: an
                // entry asking for the NONE strategy asks for none.
                let cluster = assert_launch_served(&emr, label, request).await;
                let expected: Vec<(&str, Option<&str>)> = case
                    .configs
                    .as_array()
                    .map(|configs| {
                        configs
                            .iter()
                            .filter(|config| {
                                config["PlacementStrategy"].as_str() != Some("NONE")
                            })
                            .map(|config| {
                                (
                                    config["InstanceRole"].as_str().unwrap_or_default(),
                                    config["PlacementStrategy"].as_str(),
                                )
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let reported: Vec<(&str, Option<&str>)> = cluster["PlacementGroups"]
                    .as_array()
                    .map(|groups| {
                        groups
                            .iter()
                            .map(|placement| {
                                (
                                    placement["InstanceRole"].as_str().unwrap_or_default(),
                                    placement["PlacementStrategy"].as_str(),
                                )
                            })
                            .collect()
                    })
                    .unwrap_or_else(|| {
                        panic!("case {label:?}: PlacementGroups must be reported, got {cluster}")
                    });
                assert_eq!(
                    reported, expected,
                    "case {label:?}: wrong PlacementGroups, got {cluster}"
                );
            }
        }
    }
}

/// The sentence a highly available cluster with no core instance group is refused with.
const HIGHLY_AVAILABLE_CORE_GROUP_SENTENCE: &str =
    "A HA based cluster must have core instance group. Please specify a core instance \
     group and resubmit.";

/// One row of the highly-available rule: the `Instances` the launch carries and the
/// sentence it earns.
struct CoreGroupCase {
    label: &'static str,
    instances: Value,
    refusal: Option<&'static str>,
}

fn core_group_cases() -> Vec<CoreGroupCase> {
    vec![
        CoreGroupCase {
            label: "three primaries and no core group",
            instances: with_groups(json!([group("MASTER", 3)])),
            refusal: Some(HIGHLY_AVAILABLE_CORE_GROUP_SENTENCE),
        },
        CoreGroupCase {
            label: "two primaries and no core group",
            instances: with_groups(json!([group("MASTER", 2)])),
            refusal: Some(HIGHLY_AVAILABLE_CORE_GROUP_SENTENCE),
        },
        CoreGroupCase {
            label: "three primaries and a task group, which holds no file system",
            instances: with_groups(json!([group("MASTER", 3), group("TASK", 2)])),
            refusal: Some(HIGHLY_AVAILABLE_CORE_GROUP_SENTENCE),
        },
        CoreGroupCase {
            label: "three primaries and a core group",
            instances: with_groups(json!([group("MASTER", 3), group("CORE", 1)])),
            refusal: None,
        },
        CoreGroupCase {
            label: "three primaries, a core group and a task group",
            instances: with_groups(json!([
                group("MASTER", 3),
                group("CORE", 1),
                group("TASK", 2),
            ])),
            refusal: None,
        },
        CoreGroupCase {
            label: "one primary and no core group, which is not highly available",
            instances: with_groups(json!([group("MASTER", 1)])),
            refusal: None,
        },
        CoreGroupCase {
            label: "the uniform form, whose primary group is one instance",
            instances: json!({
                "KeepJobFlowAliveWhenNoSteps": true,
                "InstanceCount": 3,
                "MasterInstanceType": "m5.xlarge",
                "SlaveInstanceType": "m5.xlarge",
            }),
            refusal: None,
        },
        CoreGroupCase {
            label: "a primary instance fleet of three units, which carries no groups",
            instances: json!({
                "KeepJobFlowAliveWhenNoSteps": true,
                "InstanceFleets": [fleet("MASTER", Some(3), None)],
            }),
            refusal: None,
        },
    ]
}

/// THE RULE for a highly available cluster: it has a core instance group.
#[tokio::test]
async fn a_highly_available_cluster_is_launched_only_with_a_core_instance_group() {
    let emr = Emr::new();

    for case in core_group_cases() {
        let label = case.label;
        let request = launch(label, json!({ "Instances": case.instances.clone() }));
        match case.refusal {
            Some(expected) => assert_launch_refused(&emr, label, request, expected).await,
            None => {
                let cluster = assert_launch_served(&emr, label, request).await;
                assert_eq!(
                    cluster["Name"].as_str(),
                    Some(label),
                    "case {label:?}: the launched cluster must be the one asked for, got {cluster}"
                );
            }
        }
    }
}

/// The sentence a multi-primary cluster on a release that provisions one primary is refused
/// with.
const SINGLE_PRIMARY_SENTENCE: &str = "A master instance group must specify a single instance.";

/// THE RULE for the release: more than one primary node needs a release that provisions
/// them, and the bound is `emr-5.23.0` — the release the model's own `PlacementGroupConfig`
/// members date the multi-primary topology from.
///
/// The rows span the published bound (the release before it, the bound itself, releases
/// well below and well above it), the primary count (one, which no release refuses, and
/// three), and a launch that names no release label at all.
#[tokio::test]
async fn several_primaries_need_a_release_that_provisions_them() {
    let emr = Emr::new();

    // Releases BELOW the bound refuse the multi-primary shape.
    for release in ["emr-5.22.0", "emr-5.20.0", "emr-5.0.0", "emr-4.9.5"] {
        let label = format!("three primaries on {release}");
        assert_launch_refused(
            &emr,
            &label,
            launch(
                &label,
                json!({
                    "ReleaseLabel": release,
                    "Instances": with_groups(json!([group("MASTER", 3), group("CORE", 1)])),
                }),
            ),
            SINGLE_PRIMARY_SENTENCE,
        )
        .await;

        // A single primary is the shape every release provisions, so this rule says
        // nothing about it: whatever answer such a launch gets, it is not this sentence.
        let one = format!("one primary on {release}");
        let (_, payload) = emr
            .call(
                "RunJobFlow",
                launch(
                    &one,
                    json!({
                        "ReleaseLabel": release,
                        "Instances": with_groups(json!([group("MASTER", 1)])),
                    }),
                ),
            )
            .await;
        assert_ne!(
            at(&payload, &["message"]).as_str(),
            Some(SINGLE_PRIMARY_SENTENCE),
            "case {one:?}: a single primary is never refused for the release, got {payload}"
        );
    }

    // AT the bound and above it the rule does not refuse the shape.
    for release in ["emr-5.23.0", "emr-5.30.0", RELEASE, "emr-7.5.0"] {
        let label = format!("three primaries on {release}");
        let (_, payload) = emr
            .call(
                "RunJobFlow",
                launch(
                    &label,
                    json!({
                        "ReleaseLabel": release,
                        "Instances": with_groups(json!([group("MASTER", 3), group("CORE", 1)])),
                    }),
                ),
            )
            .await;
        assert_ne!(
            at(&payload, &["message"]).as_str(),
            Some(SINGLE_PRIMARY_SENTENCE),
            "case {label:?}: this release provisions several primaries, got {payload}"
        );
    }

    // A launch that names no release label names no release to read, so the shape stands.
    let label = "three primaries and no release label";
    let cluster = assert_launch_served(
        &emr,
        label,
        json!({
            "Name": label,
            "JobFlowRole": INSTANCE_PROFILE,
            "ServiceRole": SERVICE_ROLE,
            "Instances": with_groups(json!([group("MASTER", 3), group("CORE", 1)])),
        }),
    )
    .await;
    assert!(
        cluster["ReleaseLabel"].as_str().is_none(),
        "case {label:?}: such a cluster reports no release label, got {cluster}"
    );
}

/// A configuration of `classification` nesting `children`.
fn configuration(classification: &str, children: Value) -> Value {
    let mut config = json!({
        "Classification": classification,
        "Properties": { "a": "b" },
    });
    if children.as_array().is_some_and(|children| !children.is_empty()) {
        config["Configurations"] = children;
    }
    config
}

/// One row of the nesting rule: a configuration tree, and the `(child, parent)` pair it is
/// refused for when it is refused.
struct NestingCase {
    label: &'static str,
    configurations: Value,
    refusal: Option<(&'static str, &'static str)>,
}

fn nesting_cases() -> Vec<NestingCase> {
    vec![
        NestingCase {
            label: "the export section under a property file",
            configurations: json!([configuration(
                "spark-defaults",
                json!([configuration("export", json!([]))]),
            )]),
            refusal: Some(("export", "spark-defaults")),
        },
        NestingCase {
            label: "the export section under another property file",
            configurations: json!([configuration(
                "core-site",
                json!([configuration("export", json!([]))]),
            )]),
            refusal: Some(("export", "core-site")),
        },
        NestingCase {
            label: "a property file under a property file",
            configurations: json!([configuration(
                "hive-site",
                json!([configuration("spark-defaults", json!([]))]),
            )]),
            refusal: Some(("spark-defaults", "hive-site")),
        },
        NestingCase {
            label: "an environment file under an environment file",
            configurations: json!([configuration(
                "yarn-env",
                json!([configuration("hadoop-env", json!([]))]),
            )]),
            refusal: Some(("hadoop-env", "yarn-env")),
        },
        NestingCase {
            label: "another application's file under an application",
            configurations: json!([configuration(
                "spark",
                json!([configuration("hive-site", json!([]))]),
            )]),
            refusal: Some(("hive-site", "spark")),
        },
        NestingCase {
            label: "a mis-nesting one level down, under a nesting that is valid",
            configurations: json!([configuration(
                "spark",
                json!([configuration(
                    "spark-env",
                    json!([configuration("core-site", json!([]))]),
                )]),
            )]),
            refusal: Some(("core-site", "spark-env")),
        },
        NestingCase {
            label: "the export section under Spark's environment file",
            configurations: json!([configuration(
                "spark-env",
                json!([configuration("export", json!([]))]),
            )]),
            refusal: None,
        },
        NestingCase {
            label: "the export section under Hadoop's environment file",
            configurations: json!([configuration(
                "hadoop-env",
                json!([configuration("export", json!([]))]),
            )]),
            refusal: None,
        },
        NestingCase {
            label: "an application's own file under that application",
            configurations: json!([configuration(
                "spark",
                json!([configuration("spark-hive-site", json!([]))]),
            )]),
            refusal: None,
        },
        NestingCase {
            label: "an application, its environment file and the section it exports",
            configurations: json!([configuration(
                "hadoop",
                json!([configuration(
                    "hadoop-env",
                    json!([configuration("export", json!([]))]),
                )]),
            )]),
            refusal: None,
        },
        NestingCase {
            label: "a flat tree, which nests nothing",
            configurations: json!([
                configuration("spark-defaults", json!([])),
                configuration("core-site", json!([])),
            ]),
            refusal: None,
        },
        // The TOP of a tree is judged too: its parent is the cluster, which holds exactly
        // the classifications Amazon EMR publishes, and a value that names no configuration
        // file of an application is applied by nothing. The refusal states that parent as
        // `null`, the way every sentence of this service renders a value the request did
        // not carry.
        NestingCase {
            label: "a misspelled classification at the top of the tree",
            configurations: json!([configuration("spark-default", json!([]))]),
            refusal: Some(("spark-default", "null")),
        },
        NestingCase {
            label: "another misspelling of the same file",
            configurations: json!([configuration("spark-defaultz", json!([]))]),
            refusal: Some(("spark-defaultz", "null")),
        },
        NestingCase {
            label: "a classification of no application at all",
            configurations: json!([configuration("nonsuch-site", json!([]))]),
            refusal: Some(("nonsuch-site", "null")),
        },
        NestingCase {
            label: "a published file the launch may name, beside a misspelling of another",
            configurations: json!([
                configuration("hive-site", json!([])),
                configuration("yarn-sites", json!([])),
            ]),
            refusal: Some(("yarn-sites", "null")),
        },
        // And the published files of other applications are applied: two the issues above
        // never named, read off the Release Guide's classification list.
        NestingCase {
            label: "the table formats' own configuration files",
            configurations: json!([
                configuration("hudi-defaults", json!([])),
                configuration("iceberg-defaults", json!([])),
            ]),
            refusal: None,
        },
        NestingCase {
            label: "an application's name, under which a cluster-wide property is set",
            configurations: json!([configuration("hbase", json!([]))]),
            refusal: None,
        },
        NestingCase {
            label: "no configurations at all",
            configurations: json!([]),
            refusal: None,
        },
    ]
}

/// THE RULE for a configuration: a classification is applied only where its parent holds
/// it — the cluster at the top of a tree, the enclosing configuration below it — wherever
/// the launch states the tree.
#[tokio::test]
async fn a_classification_is_launched_only_under_a_parent_that_holds_it() {
    let emr = Emr::new();

    for case in nesting_cases() {
        let label = case.label;
        let request = launch(
            label,
            json!({
                "Configurations": case.configurations.clone(),
                "Instances": with_groups(json!([group("MASTER", 1)])),
            }),
        );

        match case.refusal {
            Some((child, parent)) => {
                let expected =
                    format!("Classification '{child}' is not valid for parent classification '{parent}'.");
                assert_launch_refused(&emr, label, request, &expected).await;
            }
            None => {
                // An accepted tree is on the cluster the launch created, nesting and all.
                let cluster = assert_launch_served(&emr, label, request).await;
                let reported = classifications(&cluster["Configurations"]);
                assert_eq!(
                    reported,
                    classifications(&case.configurations),
                    "case {label:?}: the served tree must be the one supplied, got {cluster}"
                );
            }
        }
    }
}

/// Every classification of a configuration tree, depth first, as `parent>child` paths — so
/// a comparison of two trees compares their NESTING and not only their names.
fn classifications(configurations: &Value) -> Vec<String> {
    fn walk(configurations: &Value, prefix: &str, into: &mut Vec<String>) {
        for configuration in configurations.as_array().into_iter().flatten() {
            let classification = configuration["Classification"]
                .as_str()
                .unwrap_or_default();
            let path = if prefix.is_empty() {
                classification.to_string()
            } else {
                format!("{prefix}>{classification}")
            };
            into.push(path.clone());
            walk(&configuration["Configurations"], &path, into);
        }
    }
    let mut paths = Vec::new();
    walk(configurations, "", &mut paths);
    paths
}

/// THE SAME NESTING RULE in the other two places a launch states a configuration tree: an
/// instance group's own `Configurations`, and those of one instance type of a fleet.
#[tokio::test]
async fn the_nesting_rule_holds_wherever_the_launch_states_a_configuration() {
    let emr = Emr::new();
    let mis_nested = json!([configuration(
        "spark-defaults",
        json!([configuration("export", json!([]))]),
    )]);
    let expected =
        "Classification 'export' is not valid for parent classification 'spark-defaults'.";

    // On an instance group of the launch.
    let label = "a mis-nested configuration on an instance group";
    let mut primary = group("MASTER", 1);
    primary["Configurations"] = mis_nested.clone();
    assert_launch_refused(
        &emr,
        label,
        launch(label, json!({ "Instances": with_groups(json!([primary])) })),
        expected,
    )
    .await;

    // On one instance type of an instance fleet of the launch.
    let label = "a mis-nested configuration on a fleet's instance type";
    let mut fleet_config = fleet("MASTER", Some(1), None);
    fleet_config["InstanceTypeConfigs"] = json!([{
        "InstanceType": "m5.xlarge",
        "Configurations": mis_nested.clone(),
    }]);
    assert_launch_refused(
        &emr,
        label,
        launch(
            label,
            json!({
                "Instances": {
                    "KeepJobFlowAliveWhenNoSteps": true,
                    "InstanceFleets": [fleet_config],
                },
            }),
        ),
        expected,
    )
    .await;

    // And a launch whose trees are all valid in those same positions is served.
    let label = "valid configurations in every position";
    let valid = json!([configuration(
        "spark-env",
        json!([configuration("export", json!([]))]),
    )]);
    let mut primary = group("MASTER", 1);
    primary["Configurations"] = valid.clone();
    let cluster = assert_launch_served(
        &emr,
        label,
        launch(
            label,
            json!({
                "Configurations": valid.clone(),
                "Instances": with_groups(json!([primary])),
            }),
        ),
    )
    .await;
    assert_eq!(
        classifications(&cluster["Configurations"]),
        classifications(&valid),
        "case {label:?}: the served tree must be the one supplied, got {cluster}"
    );
}
