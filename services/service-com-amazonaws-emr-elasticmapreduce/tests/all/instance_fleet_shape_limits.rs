// SPDX-License-Identifier: CC0-1.0

//! The SHAPE one instance fleet may be configured with — how many instance types it lists,
//! and how many units the PRIMARY fleet targets — end to end over the wire.
//!
//! Every case drives the real HTTP path — `POST /` with
//! `X-Amz-Target: ElasticMapReduce.<Op>` and an `awsJson1_1` body — through the router's
//! `dispatch`, using the harness `tests/all/job_flow_lifecycle.rs` owns, so these are the
//! same bytes an SDK client sends. Nothing reaches into the service's state struct: an
//! accepted fleet is read back through `ListInstanceFleets`, a refused launch through
//! `ListClusters`, and the instance types a release offers through
//! `ListSupportedInstanceTypes`, so no table below restates a catalog the service already
//! answers from.
//!
//! RULE 1: an instance fleet lists the instance type configurations Amazon EMR fulfills its
//! target capacities from, and how many it may list is a fact of the FLEET — "when you use
//! an allocation strategy, you can include a maximum of 30 instance type configurations for
//! a fleet. … Without an allocation strategy, you may specify a maximum of five instance
//! type configurations for a fleet" (`InstanceTypeConfig` doc, Smithy model). A fleet
//! listing more is refused, naming the fleet and the maximum that fleet is held to, and no
//! fleet is created. The bound holds wherever a fleet's list is SET, so the rows span the
//! launch, the addition of a fleet to a running cluster and the modification of one.
//!
//! RULE 2: the fleet that runs a cluster's PRIMARY nodes targets the units of a primary
//! topology Amazon EMR runs — one node, or the three of a multi-primary cluster — so any
//! other count is refused with the sentence that states both, and no cluster is launched.
//! The rows span the market the units are asked of, the counts either side of each admitted
//! one, and the non-primary fleets the rule does not hold for.

use serde_json::{json, Value};
use service_com_amazonaws_emr_elasticmapreduce::state::{
    MAX_INSTANCE_TYPES_PER_FLEET, MAX_INSTANCE_TYPES_PER_UNALLOCATED_FLEET,
};

use crate::job_flow_lifecycle::{at, Emr, INSTANCE_PROFILE, SERVICE_ROLE};
use crate::refusal_messages::message;
use crate::supported_instance_types::{types, Emr as CatalogEmr};

/// The release every launch below names, so each fleet's instance types resolve against a
/// release this service publishes.
const RELEASE: &str = "emr-6.15.0";

/// The error identity the request-validation channel answers, which `RunJobFlow` is refused
/// under: it models no client error of its own.
const VALIDATION_EXCEPTION: &str = "ValidationException";

/// The error identity `AddInstanceFleet` and `ModifyInstanceFleet` model for a bad request.
const INVALID_REQUEST_EXCEPTION: &str = "InvalidRequestException";

/// The one sentence a primary fleet targeting units no primary topology has is refused
/// with: entirely fixed text, naming neither the capacity asked for nor the fleet.
const PRIMARY_FLEET_SENTENCE: &str =
    "The primary instance fleet can only have a target capacity of 1 or 3. Revise the \
     configuration and resubmit.";

/// The sentence a fleet listing more instance types than it may is refused with: the fleet
/// the request named, and the maximum that fleet is held to.
fn too_many_types_sentence(fleet: &str, maximum: usize) -> String {
    format!(
        "The number of instance types in the instance fleet: {fleet} exceeded the max value \
         ({maximum}). Revise the configuration and resubmit."
    )
}

/// A Spot launch specification naming an allocation strategy, which is one of the two ways
/// a fleet names one (`SpotProvisioningSpecification$AllocationStrategy`).
fn spot_allocation_strategy() -> Value {
    json!({
        "SpotSpecification": {
            "TimeoutDurationMinutes": 10,
            "TimeoutAction": "SWITCH_TO_ON_DEMAND",
            "AllocationStrategy": "capacity-optimized",
        },
    })
}

/// An On-Demand launch specification naming an allocation strategy, which is the other
/// (`OnDemandProvisioningSpecification$AllocationStrategy`).
fn on_demand_allocation_strategy() -> Value {
    json!({ "OnDemandSpecification": { "AllocationStrategy": "lowest-price" } })
}

/// One instance fleet of a launch: `name` where the request gives one, the node type it
/// hosts, the units it targets in each market, the instance types it lists, and the launch
/// specification it carries.
fn fleet(
    name: Option<&str>,
    fleet_type: &str,
    on_demand: Option<i64>,
    spot: Option<i64>,
    instance_types: &[String],
    launch_specifications: Option<Value>,
) -> Value {
    let mut config = json!({
        "InstanceFleetType": fleet_type,
        "InstanceTypeConfigs": instance_types
            .iter()
            .map(|instance_type| json!({ "InstanceType": instance_type }))
            .collect::<Vec<Value>>(),
    });
    if let Some(name) = name {
        config["Name"] = json!(name);
    }
    if let Some(on_demand) = on_demand {
        config["TargetOnDemandCapacity"] = json!(on_demand);
    }
    if let Some(spot) = spot {
        config["TargetSpotCapacity"] = json!(spot);
    }
    if let Some(specifications) = launch_specifications {
        config["LaunchSpecifications"] = specifications;
    }
    config
}

/// The primary fleet a row that is not about the primary carries beside its own fleet, so
/// the row's fleet is the only thing being judged.
fn serviceable_primary(instance_types: &[String]) -> Value {
    fleet(
        Some("primary"),
        "MASTER",
        Some(1),
        None,
        &instance_types[..1],
        None,
    )
}

/// A `RunJobFlow` body launching `fleets`.
fn launch(name: &str, fleets: Vec<Value>) -> Value {
    json!({
        "Name": name,
        "ReleaseLabel": RELEASE,
        "JobFlowRole": INSTANCE_PROFILE,
        "ServiceRole": SERVICE_ROLE,
        "Instances": { "KeepJobFlowAliveWhenNoSteps": true, "InstanceFleets": fleets },
    })
}

/// How many clusters `ListClusters` reports, so a refused launch can be shown to have
/// written none.
async fn cluster_count(emr: &Emr) -> usize {
    let payload = emr.ok("ListClusters", json!({})).await;
    at(&payload, &["Clusters"])
        .as_array()
        .map_or(0, Vec::len)
}

/// The fleets `ListInstanceFleets` reports for `cluster_id`, in order.
async fn fleets_of(emr: &Emr, case: &str, cluster_id: &str) -> Vec<Value> {
    let out = emr
        .ok("ListInstanceFleets", json!({ "ClusterId": cluster_id }))
        .await;
    at(&out, &["InstanceFleets"])
        .as_array()
        .cloned()
        .unwrap_or_else(|| {
            panic!("case {case:?}: ListInstanceFleets must answer InstanceFleets, got {out}")
        })
}

/// The instance types one reported fleet says it provisions from, in order.
fn reported_types(reported: &Value) -> Vec<String> {
    at(reported, &["InstanceTypeSpecifications"])
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|specification| specification["InstanceType"].as_str().map(str::to_string))
        .collect()
}

/// Assert `request` is refused with `expected` under `code`, and that nothing was written.
async fn assert_launch_refused(emr: &Emr, case: &str, request: Value, expected: &str) {
    let before = cluster_count(emr).await;
    let (status, payload) = emr.call("RunJobFlow", request).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400, got {status} with {payload}"
    );
    let code = at(&payload, &["__type"]).as_str().unwrap_or_default();
    assert!(
        code.contains(VALIDATION_EXCEPTION),
        "case {case:?}: expected {VALIDATION_EXCEPTION}, got {code:?} in {payload}"
    );
    assert_eq!(
        message(case, &payload),
        expected,
        "case {case:?}: wrong refusal sentence, got {payload}"
    );
    assert_eq!(
        cluster_count(emr).await,
        before,
        "case {case:?}: a refused launch must write no cluster"
    );
}

/// Every instance type `RELEASE` offers, as a caller finds them: `ListSupportedInstanceTypes`
/// followed to its last page. The tables below take as many as a row needs from this, so
/// nothing here restates the catalog.
async fn offered_types() -> Vec<String> {
    let catalog = CatalogEmr::new();
    let offered = types(&catalog.all_supported(RELEASE).await);
    assert!(
        offered.len() > MAX_INSTANCE_TYPES_PER_FLEET,
        "the release {RELEASE} must offer more than {MAX_INSTANCE_TYPES_PER_FLEET} instance \
         types for these tables to span the bound, got {}",
        offered.len()
    );
    offered
}

// ---------------------------------------------------------------------------
// RULE 1 — how many instance types one fleet lists
// ---------------------------------------------------------------------------

/// One row of the instance-type-count rule.
struct TypeCountCase {
    label: &'static str,
    /// The `Name` the request gives the fleet, or `None` for a fleet it leaves unnamed.
    name: Option<&'static str>,
    fleet_type: &'static str,
    /// How many instance types the fleet lists.
    instance_types: usize,
    /// The launch specification the fleet carries, which decides the bound it is held to.
    launch_specifications: Option<Value>,
    /// How the refusal names the fleet, and the maximum it states, or `None` when the
    /// launch is served.
    refusal: Option<(&'static str, usize)>,
}

fn type_count_cases() -> Vec<TypeCountCase> {
    let unallocated = MAX_INSTANCE_TYPES_PER_UNALLOCATED_FLEET;
    vec![
        TypeCountCase {
            label: "five types and no allocation strategy, the documented maximum",
            name: Some("core"),
            fleet_type: "CORE",
            instance_types: unallocated,
            launch_specifications: None,
            refusal: None,
        },
        TypeCountCase {
            label: "six types and no allocation strategy",
            name: Some("core-wide"),
            fleet_type: "CORE",
            instance_types: unallocated + 1,
            launch_specifications: None,
            refusal: Some(("core-wide", 5)),
        },
        TypeCountCase {
            label: "six types on a fleet the request leaves unnamed",
            name: None,
            fleet_type: "TASK",
            instance_types: unallocated + 1,
            launch_specifications: None,
            // A fleet with no friendly name is named by the node type it hosts.
            refusal: Some(("TaskInstanceFleet", 5)),
        },
        TypeCountCase {
            label: "six types on the primary fleet",
            name: Some("primary"),
            fleet_type: "MASTER",
            instance_types: unallocated + 1,
            launch_specifications: None,
            refusal: Some(("primary", 5)),
        },
        TypeCountCase {
            label: "six types with a Spot allocation strategy",
            name: Some("core"),
            fleet_type: "CORE",
            instance_types: unallocated + 1,
            launch_specifications: Some(spot_allocation_strategy()),
            refusal: None,
        },
        TypeCountCase {
            label: "six types with an On-Demand allocation strategy",
            name: Some("core"),
            fleet_type: "CORE",
            instance_types: unallocated + 1,
            launch_specifications: Some(on_demand_allocation_strategy()),
            refusal: None,
        },
        TypeCountCase {
            label: "thirty types with an allocation strategy, the documented maximum",
            name: Some("core"),
            fleet_type: "CORE",
            instance_types: MAX_INSTANCE_TYPES_PER_FLEET,
            launch_specifications: Some(spot_allocation_strategy()),
            refusal: None,
        },
        TypeCountCase {
            label: "thirty-one types with an allocation strategy",
            name: Some("core-widest"),
            fleet_type: "CORE",
            instance_types: MAX_INSTANCE_TYPES_PER_FLEET + 1,
            launch_specifications: Some(spot_allocation_strategy()),
            refusal: Some(("core-widest", 30)),
        },
        TypeCountCase {
            label: "thirty-one types and no allocation strategy, held to the shorter list",
            name: Some("core-widest"),
            fleet_type: "CORE",
            instance_types: MAX_INSTANCE_TYPES_PER_FLEET + 1,
            launch_specifications: None,
            refusal: Some(("core-widest", 5)),
        },
    ]
}

#[tokio::test]
async fn a_launch_lists_at_most_the_instance_types_a_fleet_may_hold() {
    let offered = offered_types().await;
    let emr = Emr::new();

    for case in type_count_cases() {
        let label = case.label;
        let listed = &offered[..case.instance_types];
        let under_test = fleet(
            case.name,
            case.fleet_type,
            // A primary fleet of one On-Demand unit, every other fleet of two Spot units:
            // the capacity is what the fleet's OWN rules admit, so this row is about the
            // instance types alone.
            if case.fleet_type == "MASTER" { Some(1) } else { None },
            if case.fleet_type == "MASTER" { None } else { Some(2) },
            listed,
            case.launch_specifications.clone(),
        );
        let mut fleets = Vec::new();
        if case.fleet_type != "MASTER" {
            fleets.push(serviceable_primary(&offered));
        }
        fleets.push(under_test);
        let request = launch(label, fleets);

        match case.refusal {
            Some((named, maximum)) => {
                assert_launch_refused(
                    &emr,
                    label,
                    request,
                    &too_many_types_sentence(named, maximum),
                )
                .await;
            }
            None => {
                let cluster_id = emr.run_job_flow(request).await;
                // The accepted fleet provisions from exactly the types it listed. It is the
                // LAST fleet the request carried, and a listing reports them in that order.
                let listing = fleets_of(&emr, label, &cluster_id).await;
                let reported = listing
                    .last()
                    .unwrap_or_else(|| panic!("case {label:?}: the launched fleet must be listed"));
                assert_eq!(
                    reported["InstanceFleetType"].as_str(),
                    Some(case.fleet_type),
                    "case {label:?}: the last listed fleet is the one under test, got {reported}"
                );
                assert_eq!(
                    reported_types(reported),
                    listed.to_vec(),
                    "case {label:?}: the fleet must provision from the types it listed, got \
                     {reported}"
                );
            }
        }
    }
}

/// THE SAME RULE on the two operations that set a fleet's instance types AFTER the launch:
/// a fleet added to a running cluster is held to the bound its own allocation strategy
/// admits, and a modification is judged on the list its merge would leave behind — each
/// refused under the error identity that operation models, leaving the cluster as it was.
#[tokio::test]
async fn adding_or_modifying_a_fleet_holds_it_to_the_same_instance_type_bound() {
    let offered = offered_types().await;
    let emr = Emr::new();
    let cluster_id = emr
        .run_job_flow(launch(
            "after the launch",
            vec![serviceable_primary(&offered)],
        ))
        .await;
    let six = &offered[..MAX_INSTANCE_TYPES_PER_UNALLOCATED_FLEET + 1];

    // AddInstanceFleet: six types and no allocation strategy is refused, naming the added
    // fleet, and the cluster still carries only the fleet it launched with.
    let case = "AddInstanceFleet with six types and no allocation strategy";
    let (status, payload) = emr
        .call(
            "AddInstanceFleet",
            json!({
                "ClusterId": &cluster_id,
                "InstanceFleet": fleet(Some("added"), "TASK", None, Some(2), six, None),
            }),
        )
        .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400, got {status} with {payload}"
    );
    let code = at(&payload, &["__type"]).as_str().unwrap_or_default();
    assert!(
        code.contains(INVALID_REQUEST_EXCEPTION),
        "case {case:?}: expected {INVALID_REQUEST_EXCEPTION}, got {code:?} in {payload}"
    );
    assert_eq!(
        message(case, &payload),
        too_many_types_sentence("added", 5),
        "case {case:?}: wrong refusal sentence, got {payload}"
    );
    assert_eq!(
        fleets_of(&emr, case, &cluster_id).await.len(),
        1,
        "case {case:?}: a refused addition must add no fleet"
    );

    // The same six types WITH an allocation strategy are added, and the fleet provisions
    // from all of them.
    let case = "AddInstanceFleet with six types and an allocation strategy";
    emr.ok(
        "AddInstanceFleet",
        json!({
            "ClusterId": &cluster_id,
            "InstanceFleet": fleet(
                Some("allocated"),
                "TASK",
                None,
                Some(2),
                six,
                Some(spot_allocation_strategy()),
            ),
        }),
    )
    .await;
    let added = fleets_of(&emr, case, &cluster_id)
        .await
        .into_iter()
        .find(|reported| reported["Name"].as_str() == Some("allocated"))
        .unwrap_or_else(|| panic!("case {case:?}: the added fleet must be listed"));
    assert_eq!(
        reported_types(&added),
        six.to_vec(),
        "case {case:?}: the added fleet must provision from the types it listed, got {added}"
    );

    // ModifyInstanceFleet: the PRIMARY fleet names no allocation strategy, so a merge that
    // would leave it listing six types is refused and it keeps the one it launched with.
    let case = "ModifyInstanceFleet past the shorter list";
    let primary = fleets_of(&emr, case, &cluster_id)
        .await
        .into_iter()
        .find(|reported| reported["Name"].as_str() == Some("primary"))
        .unwrap_or_else(|| panic!("case {case:?}: the primary fleet must be listed"));
    let primary_id = primary["Id"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: a listed fleet carries an Id"))
        .to_string();
    let (status, payload) = emr
        .call(
            "ModifyInstanceFleet",
            json!({
                "ClusterId": &cluster_id,
                "InstanceFleet": {
                    "InstanceFleetId": &primary_id,
                    "InstanceTypeConfigs": six
                        .iter()
                        .map(|instance_type| json!({ "InstanceType": instance_type }))
                        .collect::<Vec<Value>>(),
                },
            }),
        )
        .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400, got {status} with {payload}"
    );
    assert_eq!(
        message(case, &payload),
        too_many_types_sentence("primary", 5),
        "case {case:?}: wrong refusal sentence, got {payload}"
    );
    let primary = fleets_of(&emr, case, &cluster_id)
        .await
        .into_iter()
        .find(|reported| reported["Name"].as_str() == Some("primary"))
        .unwrap_or_else(|| panic!("case {case:?}: the primary fleet must be listed"));
    assert_eq!(
        reported_types(&primary),
        offered[..1].to_vec(),
        "case {case:?}: a refused merge must leave the fleet's types alone, got {primary}"
    );

    // A merge INSIDE the bound is applied, so the refusal above is about the count.
    let case = "ModifyInstanceFleet inside the shorter list";
    emr.ok(
        "ModifyInstanceFleet",
        json!({
            "ClusterId": &cluster_id,
            "InstanceFleet": {
                "InstanceFleetId": &primary_id,
                "InstanceTypeConfigs": [{ "InstanceType": &offered[1] }],
            },
        }),
    )
    .await;
    let primary = fleets_of(&emr, case, &cluster_id)
        .await
        .into_iter()
        .find(|reported| reported["Name"].as_str() == Some("primary"))
        .unwrap_or_else(|| panic!("case {case:?}: the primary fleet must be listed"));
    assert_eq!(
        reported_types(&primary),
        vec![offered[0].clone(), offered[1].clone()],
        "case {case:?}: the merged type must be on the fleet, got {primary}"
    );
}

// ---------------------------------------------------------------------------
// RULE 2 — what the primary fleet may target
// ---------------------------------------------------------------------------

/// One row of the primary-fleet capacity rule.
struct PrimaryCapacityCase {
    label: &'static str,
    fleet_type: &'static str,
    on_demand: Option<i64>,
    spot: Option<i64>,
    /// The sentence the launch is refused with, or `None` when it is served.
    refusal: Option<&'static str>,
}

fn primary_capacity_cases() -> Vec<PrimaryCapacityCase> {
    vec![
        PrimaryCapacityCase {
            label: "one On-Demand unit, a single-primary cluster",
            fleet_type: "MASTER",
            on_demand: Some(1),
            spot: None,
            refusal: None,
        },
        PrimaryCapacityCase {
            label: "three On-Demand units, a multi-primary cluster",
            fleet_type: "MASTER",
            on_demand: Some(3),
            spot: None,
            refusal: None,
        },
        PrimaryCapacityCase {
            label: "one Spot unit",
            fleet_type: "MASTER",
            on_demand: None,
            spot: Some(1),
            refusal: None,
        },
        PrimaryCapacityCase {
            label: "three Spot units",
            fleet_type: "MASTER",
            on_demand: Some(0),
            spot: Some(3),
            refusal: None,
        },
        PrimaryCapacityCase {
            label: "two On-Demand units, between the two counts",
            fleet_type: "MASTER",
            on_demand: Some(2),
            spot: None,
            refusal: Some(PRIMARY_FLEET_SENTENCE),
        },
        PrimaryCapacityCase {
            label: "two Spot units, the same count of the other market",
            fleet_type: "MASTER",
            on_demand: None,
            spot: Some(2),
            refusal: Some(PRIMARY_FLEET_SENTENCE),
        },
        PrimaryCapacityCase {
            label: "four On-Demand units, one past the multi-primary count",
            fleet_type: "MASTER",
            on_demand: Some(4),
            spot: None,
            refusal: Some(PRIMARY_FLEET_SENTENCE),
        },
        PrimaryCapacityCase {
            label: "a far larger primary capacity",
            fleet_type: "MASTER",
            on_demand: None,
            spot: Some(97),
            refusal: Some(PRIMARY_FLEET_SENTENCE),
        },
        PrimaryCapacityCase {
            label: "two units on a CORE fleet, which is not the primary",
            fleet_type: "CORE",
            on_demand: Some(2),
            spot: None,
            refusal: None,
        },
        PrimaryCapacityCase {
            label: "ninety-seven units on a TASK fleet, which is not the primary",
            fleet_type: "TASK",
            on_demand: None,
            spot: Some(97),
            refusal: None,
        },
    ]
}

#[tokio::test]
async fn the_primary_instance_fleet_targets_only_a_primary_topology_emr_runs() {
    let offered = offered_types().await;
    let emr = Emr::new();

    for case in primary_capacity_cases() {
        let label = case.label;
        let mut fleets = Vec::new();
        if case.fleet_type != "MASTER" {
            fleets.push(serviceable_primary(&offered));
        }
        fleets.push(fleet(
            Some(case.fleet_type),
            case.fleet_type,
            case.on_demand,
            case.spot,
            &offered[..1],
            None,
        ));
        let request = launch(label, fleets);

        match case.refusal {
            Some(expected) => assert_launch_refused(&emr, label, request, expected).await,
            None => {
                let cluster_id = emr.run_job_flow(request).await;
                let reported = fleets_of(&emr, label, &cluster_id)
                    .await
                    .into_iter()
                    .find(|reported| reported["Name"].as_str() == Some(case.fleet_type))
                    .unwrap_or_else(|| panic!("case {label:?}: the launched fleet must be listed"));
                assert_eq!(
                    (
                        reported["TargetOnDemandCapacity"].as_i64(),
                        reported["TargetSpotCapacity"].as_i64(),
                    ),
                    (
                        Some(case.on_demand.unwrap_or(0)),
                        Some(case.spot.unwrap_or(0)),
                    ),
                    "case {label:?}: the fleet must target what it asked for, got {reported}"
                );
            }
        }
    }
}
