// SPDX-License-Identifier: CC0-1.0

//! What `ModifyInstanceFleet` leaves on a fleet, member by member, end to end over the
//! wire.
//!
//! THE RULE: every member the submitted `InstanceFleetModifyConfig` carries replaces what
//! the named fleet holds for that member, and a member the configuration OMITS asks for no
//! change and leaves that part of the fleet exactly as it was — so a sequence of
//! modifications accumulates, and `ListInstanceFleets` reports the configuration they left
//! behind. The configuration's members are `InstanceFleetId`, `TargetOnDemandCapacity`,
//! `TargetSpotCapacity`, `ResizeSpecifications`, `InstanceTypeConfigs` and `Context`
//! (`smithy_query(query="shape", shape="InstanceFleetModifyConfig")`), and the rows below
//! span them: each row submits a different subset, and the table carries forward what every
//! earlier row left.
//!
//! The capacities' own table lives in `tests/all/modification_refusal_sentences.rs`; this
//! suite is about the WHOLE configuration — the resize specification, the per-instance-type
//! configurations and the reserved context beside the two targets — and about the fleet
//! beside the one the request names, which no modification touches.
//!
//! Every case drives the real HTTP path through the harness `tests/all/job_flow_lifecycle.rs`
//! owns, and nothing reaches into the service's state struct: a fleet is only ever read back
//! through `ListInstanceFleets`.

use serde_json::{json, Value};

use crate::job_flow_lifecycle::{at, Emr, INSTANCE_PROFILE, SERVICE_ROLE};

/// The Spot resize timeout a fleet Amazon EMR can resize runs under when its own
/// specifications name none.
///
/// The model publishes no default for `SpotResizingSpecification$TimeoutDurationMinutes`,
/// so this number is the emulator's own choice rather than AWS behaviour; what the rows
/// below assert about it is that a modification which does NOT carry
/// `ResizeSpecifications` leaves the fleet reporting whatever it reported before, and that
/// one which carries the member replaces the whole of it.
const DEFAULT_SPOT_RESIZE_TIMEOUT: i64 = 60;

/// The ends of the band the model documents for a resize timeout ("minimum value is 5
/// minutes and maximum value is 10,080 minutes (7 days)",
/// `SpotResizingSpecification$TimeoutDurationMinutes` doc), which the rows name so the
/// values asserted are not one arbitrary number.
const MINIMUM_RESIZE_TIMEOUT: i64 = 5;
const MAXIMUM_RESIZE_TIMEOUT: i64 = 10_080;

/// The observable configuration of one reported fleet: what every member of the
/// modification configuration shows up as on the wire.
#[derive(Debug, PartialEq, Eq)]
struct Reported {
    /// `(TargetOnDemandCapacity, TargetSpotCapacity)`.
    targets: (Option<i64>, Option<i64>),
    /// `ResizeSpecifications.SpotResizeSpecification`'s
    /// `(TimeoutDurationMinutes, AllocationStrategy)`.
    spot_resize: (Option<i64>, Option<String>),
    /// `ResizeSpecifications.OnDemandResizeSpecification`'s
    /// `(TimeoutDurationMinutes, AllocationStrategy)`.
    on_demand_resize: (Option<i64>, Option<String>),
    /// `Context`, the member the model marks reserved.
    context: Option<String>,
    /// `(InstanceType, WeightedCapacity)` per reported instance type specification, in the
    /// order the fleet holds them.
    instance_types: Vec<(String, Option<i64>)>,
}

/// Read one reported fleet into the configuration it carries.
fn reported(fleet: &Value) -> Reported {
    let resize = |half: &str| {
        (
            at(fleet, &["ResizeSpecifications", half, "TimeoutDurationMinutes"]).as_i64(),
            at(fleet, &["ResizeSpecifications", half, "AllocationStrategy"])
                .as_str()
                .map(str::to_string),
        )
    };
    Reported {
        targets: (
            fleet["TargetOnDemandCapacity"].as_i64(),
            fleet["TargetSpotCapacity"].as_i64(),
        ),
        spot_resize: resize("SpotResizeSpecification"),
        on_demand_resize: resize("OnDemandResizeSpecification"),
        context: fleet["Context"].as_str().map(str::to_string),
        instance_types: fleet["InstanceTypeSpecifications"]
            .as_array()
            .map(|entries| {
                entries
                    .iter()
                    .map(|entry| {
                        (
                            entry["InstanceType"]
                                .as_str()
                                .unwrap_or_default()
                                .to_string(),
                            entry["WeightedCapacity"].as_i64(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default(),
    }
}

/// The expectation one row states, written the way a row reads: the two targets, the two
/// halves of the resize specification, the context, and the instance types.
fn expect(
    targets: (i64, i64),
    spot_resize: (i64, Option<&str>),
    on_demand_resize: (Option<i64>, Option<&str>),
    context: Option<&str>,
    instance_types: &[(&str, i64)],
) -> Reported {
    Reported {
        targets: (Some(targets.0), Some(targets.1)),
        spot_resize: (Some(spot_resize.0), spot_resize.1.map(str::to_string)),
        on_demand_resize: (on_demand_resize.0, on_demand_resize.1.map(str::to_string)),
        context: context.map(str::to_string),
        instance_types: instance_types
            .iter()
            .map(|(instance_type, weight)| ((*instance_type).to_string(), Some(*weight)))
            .collect(),
    }
}

/// A `RunJobFlow` launching a primary fleet and a CORE fleet, the CORE one carrying one
/// instance type and neither a resize specification nor a context — so every member a
/// modification below sets is one the launch left unset or set to a different value.
fn fleet_launch(name: &str) -> Value {
    json!({
        "Name": name,
        "ReleaseLabel": "emr-6.15.0",
        "JobFlowRole": INSTANCE_PROFILE,
        "ServiceRole": SERVICE_ROLE,
        "Instances": {
            "KeepJobFlowAliveWhenNoSteps": true,
            "InstanceFleets": [
                {
                    "Name": "primary",
                    "InstanceFleetType": "MASTER",
                    "TargetOnDemandCapacity": 1,
                    "InstanceTypeConfigs": [{ "InstanceType": "m5.xlarge", "WeightedCapacity": 1 }],
                },
                {
                    "Name": "core",
                    "InstanceFleetType": "CORE",
                    "TargetOnDemandCapacity": 2,
                    "TargetSpotCapacity": 1,
                    "InstanceTypeConfigs": [{ "InstanceType": "m5.xlarge", "WeightedCapacity": 1 }],
                },
            ],
        },
    })
}

/// The fleets `ListInstanceFleets` reports for `cluster_id`, by the name they were launched
/// with.
async fn fleets_by_name(emr: &Emr, case: &str, cluster_id: &str) -> Vec<(String, Value)> {
    let out = emr
        .ok("ListInstanceFleets", json!({ "ClusterId": cluster_id }))
        .await;
    out["InstanceFleets"]
        .as_array()
        .unwrap_or_else(|| {
            panic!("case {case:?}: ListInstanceFleets must answer InstanceFleets, got {out}")
        })
        .iter()
        .map(|fleet| {
            (
                fleet["Name"].as_str().unwrap_or_default().to_string(),
                fleet.clone(),
            )
        })
        .collect()
}

/// The fleet named `name`, as `ListInstanceFleets` reports it.
async fn fleet(emr: &Emr, case: &str, cluster_id: &str, name: &str) -> Value {
    fleets_by_name(emr, case, cluster_id)
        .await
        .into_iter()
        .find(|(reported, _)| reported == name)
        .map(|(_, fleet)| fleet)
        .unwrap_or_else(|| panic!("case {case:?}: the {name:?} fleet must be listed"))
}

/// THE RULE: every member the submitted `InstanceFleetModifyConfig` carries replaces what
/// the named fleet holds for it, and a member it omits leaves that part of the fleet
/// exactly as it was — so a sequence of modifications accumulates and `ListInstanceFleets`
/// reports what they left. The rows span the subsets of the configuration's members a
/// caller sends: each target alone, both at once, each half of the resize specification,
/// both halves, the reserved context, the per-instance-type configurations, and a
/// configuration carrying nothing but the fleet id.
#[tokio::test]
async fn a_modification_replaces_the_members_it_carries_and_keeps_the_rest() {
    let emr = Emr::new();
    let cluster_id = emr.run_job_flow(fleet_launch("fleet merge")).await;

    let core = fleet(&emr, "launched", &cluster_id, "core").await;
    let core_id = core["Id"]
        .as_str()
        .unwrap_or_else(|| panic!("case \"launched\": a listed fleet carries an Id, got {core}"))
        .to_string();

    // The fleet as the launch left it: the targets it asked for, the Spot resize timeout
    // the service supplies a fleet it can resize, and nothing else.
    let launched = expect(
        (2, 1),
        (DEFAULT_SPOT_RESIZE_TIMEOUT, None),
        (None, None),
        None,
        &[("m5.xlarge", 1)],
    );
    assert_eq!(
        reported(&core),
        launched,
        "case \"launched\": the fleet must report what the launch configured, got {core}"
    );

    // `(case, the modification beside the fleet id, what the fleet must then report)`.
    // Each row starts from what the row before it left, so a member a row omits is
    // asserted to have survived every later call as well.
    let rows: Vec<(&str, Value, Reported)> = vec![
        (
            "nothing but the fleet id",
            json!({}),
            expect(
                (2, 1),
                (DEFAULT_SPOT_RESIZE_TIMEOUT, None),
                (None, None),
                None,
                &[("m5.xlarge", 1)],
            ),
        ),
        (
            "one target alone",
            json!({ "TargetOnDemandCapacity": 7 }),
            expect(
                (7, 1),
                (DEFAULT_SPOT_RESIZE_TIMEOUT, None),
                (None, None),
                None,
                &[("m5.xlarge", 1)],
            ),
        ),
        (
            "the other target alone",
            json!({ "TargetSpotCapacity": 4 }),
            expect(
                (7, 4),
                (DEFAULT_SPOT_RESIZE_TIMEOUT, None),
                (None, None),
                None,
                &[("m5.xlarge", 1)],
            ),
        ),
        (
            // `SpotProvisioningAllocationStrategy`, one value the model declares
            // (`smithy_query(query="shape", shape="SpotProvisioningAllocationStrategy")`),
            // with the documented minimum of the resize timeout band.
            "a Spot resize specification, no target",
            json!({
                "ResizeSpecifications": {
                    "SpotResizeSpecification": {
                        "TimeoutDurationMinutes": MINIMUM_RESIZE_TIMEOUT,
                        "AllocationStrategy": "capacity-optimized",
                    },
                },
            }),
            expect(
                (7, 4),
                (MINIMUM_RESIZE_TIMEOUT, Some("capacity-optimized")),
                (None, None),
                None,
                &[("m5.xlarge", 1)],
            ),
        ),
        (
            "the reserved context alone",
            json!({ "Context": "merge-case-context" }),
            expect(
                (7, 4),
                (MINIMUM_RESIZE_TIMEOUT, Some("capacity-optimized")),
                (None, None),
                Some("merge-case-context"),
                &[("m5.xlarge", 1)],
            ),
        ),
        (
            // `ResizeSpecifications` is ONE member of the configuration, so a submitted
            // value replaces the whole of it: the Spot half this one does not carry is not
            // retained from the row above, and the fleet falls back to the Spot resize
            // timeout the service supplies. `OnDemandProvisioningAllocationStrategy`,
            // the other end of the timeout band.
            "an On-Demand resize specification replaces the whole member",
            json!({
                "ResizeSpecifications": {
                    "OnDemandResizeSpecification": {
                        "TimeoutDurationMinutes": MAXIMUM_RESIZE_TIMEOUT,
                        "AllocationStrategy": "prioritized",
                    },
                },
            }),
            expect(
                (7, 4),
                (DEFAULT_SPOT_RESIZE_TIMEOUT, None),
                (Some(MAXIMUM_RESIZE_TIMEOUT), Some("prioritized")),
                Some("merge-case-context"),
                &[("m5.xlarge", 1)],
            ),
        ),
        (
            // Both halves in one call, and both are reported.
            "both halves of the resize specification at once",
            json!({
                "ResizeSpecifications": {
                    "SpotResizeSpecification": { "TimeoutDurationMinutes": 30 },
                    "OnDemandResizeSpecification": { "TimeoutDurationMinutes": 45 },
                },
            }),
            expect(
                (7, 4),
                (30, None),
                (Some(45), None),
                Some("merge-case-context"),
                &[("m5.xlarge", 1)],
            ),
        ),
        (
            // One instance type the fleet already holds a configuration for, which the
            // submitted one replaces, and one it does not, which is appended.
            "an instance type replaced and another added",
            json!({
                "InstanceTypeConfigs": [
                    { "InstanceType": "m5.xlarge", "WeightedCapacity": 2 },
                    { "InstanceType": "r5.2xlarge", "WeightedCapacity": 8 },
                ],
            }),
            expect(
                (7, 4),
                (30, None),
                (Some(45), None),
                Some("merge-case-context"),
                &[("m5.xlarge", 2), ("r5.2xlarge", 8)],
            ),
        ),
        (
            // The documented floor of both targets, asked for after every other member was
            // set: the capacities move and nothing else does.
            "both targets down to the documented floor",
            json!({ "TargetOnDemandCapacity": 0, "TargetSpotCapacity": 0 }),
            expect(
                (0, 0),
                (30, None),
                (Some(45), None),
                Some("merge-case-context"),
                &[("m5.xlarge", 2), ("r5.2xlarge", 8)],
            ),
        ),
        (
            "nothing but the fleet id, once everything is set",
            json!({}),
            expect(
                (0, 0),
                (30, None),
                (Some(45), None),
                Some("merge-case-context"),
                &[("m5.xlarge", 2), ("r5.2xlarge", 8)],
            ),
        ),
    ];

    // What the primary fleet reported before any modification: no row may change it.
    let primary_before = reported(&fleet(&emr, "launched", &cluster_id, "primary").await);

    for (case, modification, expected) in rows {
        let mut submitted = modification.clone();
        submitted["InstanceFleetId"] = json!(&core_id);
        let out = emr
            .ok(
                "ModifyInstanceFleet",
                json!({ "ClusterId": &cluster_id, "InstanceFleet": submitted }),
            )
            .await;
        assert!(
            out.is_null() || out.as_object().is_some_and(serde_json::Map::is_empty),
            "case {case:?}: ModifyInstanceFleet answers a body with no members, got {out}"
        );

        let listed = fleet(&emr, case, &cluster_id, "core").await;
        assert_eq!(
            reported(&listed),
            expected,
            "case {case:?}: the fleet must report the merge of {modification}, got {listed}"
        );

        let primary = fleet(&emr, case, &cluster_id, "primary").await;
        assert_eq!(
            reported(&primary),
            primary_before,
            "case {case:?}: the fleet the request did not name must be left alone, got \
             {primary}"
        );
    }
}

/// THE SAME RULE, read across the fleets a cluster carries: a modification reaches the
/// fleet whose `InstanceFleetId` the configuration names and no other, so resizing each
/// resizable fleet in turn leaves every fleet carrying exactly the configuration the calls
/// that named IT left. The rows span the node types a resizable fleet hosts (`CORE`,
/// `TASK`), the fleet that arrived with the launch and the one added afterwards, and the
/// primary fleet as a control nothing names.
#[tokio::test]
async fn a_modification_reaches_only_the_fleet_the_request_names() {
    let emr = Emr::new();
    let cluster_id = emr.run_job_flow(fleet_launch("fleet addressing")).await;

    // A TASK fleet beside the launched ones, so the cluster carries three fleets of
    // different node types and a resize has to pick one of them by id.
    emr.ok(
        "AddInstanceFleet",
        json!({
            "ClusterId": &cluster_id,
            "InstanceFleet": {
                "Name": "workers",
                "InstanceFleetType": "TASK",
                "TargetSpotCapacity": 3,
                "InstanceTypeConfigs": [{ "InstanceType": "c5.xlarge", "WeightedCapacity": 1 }],
            },
        }),
    )
    .await;

    // Every fleet, with the id it carries and the configuration it reports before any
    // resize — which is what each one must still report until a call names it.
    let fleets = fleets_by_name(&emr, "added", &cluster_id).await;
    assert_eq!(
        fleets.len(),
        3,
        "case \"added\": the cluster must carry three fleets, got {fleets:?}"
    );
    let mut expected: Vec<(String, String, Reported)> = fleets
        .iter()
        .map(|(name, fleet)| {
            (
                name.clone(),
                fleet["Id"]
                    .as_str()
                    .unwrap_or_else(|| panic!("case \"added\": a listed fleet carries an Id"))
                    .to_string(),
                reported(fleet),
            )
        })
        .collect();

    // Each resizable fleet in turn, named by ITS id. The primary fleet is never named: a
    // resize policy governs the fleets Amazon EMR scales, and the primary nodes are not
    // scaled after the launch.
    for named in ["core", "workers"] {
        let case = format!("resize of the {named:?} fleet");
        let units = 6;
        let context = format!("resized-{named}");
        let id = expected
            .iter()
            .find(|(name, _, _)| name == named)
            .map(|(_, id, _)| id.clone())
            .unwrap_or_else(|| panic!("case {case:?}: the {named:?} fleet must be listed"));
        emr.ok(
            "ModifyInstanceFleet",
            json!({
                "ClusterId": &cluster_id,
                "InstanceFleet": {
                    "InstanceFleetId": &id,
                    "TargetOnDemandCapacity": units,
                    "Context": &context,
                },
            }),
        )
        .await;

        // Only the named fleet's expectation moves, and only in the two members the call
        // carried.
        for (name, _, fleet) in expected.iter_mut() {
            if name == named {
                fleet.targets.0 = Some(units);
                fleet.context = Some(context.clone());
            }
        }

        for (name, _, want) in &expected {
            let now = fleet(&emr, &case, &cluster_id, name).await;
            assert_eq!(
                &reported(&now),
                want,
                "case {case:?}: the {name:?} fleet must report what the calls naming it \
                 left, got {now}"
            );
        }
    }
}
