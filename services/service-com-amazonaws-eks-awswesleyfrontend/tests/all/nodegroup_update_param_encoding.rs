// SPDX-License-Identifier: CC0-1.0

//! What the `Update` a managed node group change mints REPORTS of the change it
//! carries: the encoding of each `UpdateParam$value`, and the error list every update
//! answers with.
//!
//! One rule over a TABLE of inputs rather than one recorded request, asserted on the
//! call that STARTS the update and on the `DescribeUpdate` that reads the stored
//! record back:
//!
//! * `UpdateParam$value` is a single `String` carrying the value the request
//!   submitted for the member the `UpdateParamType` names — the JSON TEXT of that
//!   value when the member is a map, a list or a structure, and the scalar's own text
//!   when it is a scalar. A client reads `update.params` to confirm which change an
//!   asynchronous update is applying, so a collection must parse as the collection it
//!   is and a structure must carry every member the request named for it.
//! * `Update$errors` is "any errors associated with a `Failed` update", so an update
//!   nothing has failed reports it as the EMPTY list rather than dropping the member:
//!   a client inspecting it without a null guard branches on a list either way.
//!
//! The rows differ along what the rule generalises over: which member the change
//! carries (scaling, update config, labels, taints, node auto repair, warm pool), how
//! many entries it names, which optional members of a submitted structure it names,
//! and whether the submitted value is a scalar or a collection.
//!
//! Nothing here waits out a span: the clock-owed `CREATING` -> `ACTIVE` move a change
//! needs is POLLED through the shared `wire::read_until`, with its generous deadline,
//! and every node group is created before the first wait so the waits overlap.
//!
//! The wire harness (`Answer`, `emulator`, `call`, `encode_label`, `read_until`) is
//! the shared one in `tests/all/wire.rs`; the cluster and node group builders are
//! `nodegroup_lifecycle`'s, and the `DescribeUpdate` reader is
//! `cluster_plane_reported_members`' — reused rather than written again. That suite
//! asserts the same rule on the label and taint params; the rows here are the member
//! classes it does not reach, and they also pin the exact param SET an update reports.

use crate::cluster_lifecycle::REGION;
use crate::cluster_plane_reported_members::describe_nodegroup_update;
use crate::nodegroup_lifecycle::{
    create_cluster, create_nodegroup_in, minimal_request as nodegroup_request,
};
use crate::wire::{call, emulator, encode_label, read_until, Answer};

/// What one `UpdateParam$value` must carry.
#[derive(Debug)]
enum Expected {
    /// The value PARSES as this JSON document: the form a submitted map, list or
    /// structure takes.
    Json(serde_json::Value),
    /// The value is exactly this text: the form a submitted scalar takes.
    Text(&'static str),
}

/// One row: the case, the change the request carries, and the `UpdateParamType` keys
/// the update must report with the value each must carry.
type Row = (&'static str, serde_json::Value, Vec<(&'static str, Expected)>);

/// The rows, spanning the member classes `UpdateNodegroupConfig` accepts. The param
/// keys are `UpdateParamType` members
/// (`smithy_query(query="shape",shape="UpdateParamType",
/// model_path="services/service-com-amazonaws-eks-awswesleyfrontend/smithy-model/com-amazonaws-eks-awswesleyfrontend.json")`),
/// and the taint effects and repair actions named below are members of `TaintEffect`
/// and `RepairAction` from the same model.
fn rows() -> Vec<Row> {
    vec![
        (
            "the whole scaling triple",
            serde_json::json!({
                "scalingConfig": { "minSize": 2, "maxSize": 5, "desiredSize": 4 },
            }),
            vec![
                ("MinSize", Expected::Text("2")),
                ("MaxSize", Expected::Text("5")),
                ("DesiredSize", Expected::Text("4")),
            ],
        ),
        (
            // "When updating a node group, you can specify any or none of the
            // properties" (`NodegroupScalingConfig`), and the triple the node group
            // runs after the change is what the update reports — the member this
            // request names carries its value, the two it leaves carry the node
            // group's own ([`starting_nodegroup`]).
            "one member of the scaling triple",
            serde_json::json!({ "scalingConfig": { "desiredSize": 5 } }),
            vec![
                ("MinSize", Expected::Text("1")),
                ("MaxSize", Expected::Text("6")),
                ("DesiredSize", Expected::Text("5")),
            ],
        ),
        (
            "an update config given as a count",
            serde_json::json!({ "updateConfig": { "maxUnavailable": 2 } }),
            vec![("MaxUnavailable", Expected::Text("2"))],
        ),
        (
            "an update config given as a percentage, with a strategy",
            serde_json::json!({
                "updateConfig": { "maxUnavailablePercentage": 50, "updateStrategy": "MINIMAL" },
            }),
            vec![
                ("MaxUnavailablePercentage", Expected::Text("50")),
                ("UpdateStrategy", Expected::Text("MINIMAL")),
            ],
        ),
        (
            "three label keys removed",
            serde_json::json!({ "labels": { "removeLabels": ["team", "owner", "tier"] } }),
            vec![(
                "LabelsToRemove",
                Expected::Json(serde_json::json!(["team", "owner", "tier"])),
            )],
        ),
        (
            "one taint added, naming no value",
            serde_json::json!({
                "taints": {
                    "addOrUpdateTaints": [{ "key": "gpu", "effect": "PREFER_NO_SCHEDULE" }],
                },
            }),
            vec![(
                "TaintsToAdd",
                Expected::Json(serde_json::json!([
                    { "effect": "PREFER_NO_SCHEDULE", "key": "gpu" },
                ])),
            )],
        ),
        (
            "taints replaced on both sides of the change",
            serde_json::json!({
                "taints": {
                    "addOrUpdateTaints": [
                        { "key": "batch", "value": "nightly", "effect": "NO_EXECUTE" },
                        { "key": "arch", "value": "arm64", "effect": "NO_SCHEDULE" },
                    ],
                    "removeTaints": [
                        { "key": "dedicated", "value": "gpu", "effect": "NO_SCHEDULE" },
                    ],
                },
            }),
            vec![
                (
                    "TaintsToAdd",
                    Expected::Json(serde_json::json!([
                        { "effect": "NO_EXECUTE", "value": "nightly", "key": "batch" },
                        { "effect": "NO_SCHEDULE", "value": "arm64", "key": "arch" },
                    ])),
                ),
                (
                    "TaintsToRemove",
                    Expected::Json(serde_json::json!([
                        { "effect": "NO_SCHEDULE", "value": "gpu", "key": "dedicated" },
                    ])),
                ),
            ],
        ),
        (
            "node auto repair with a count threshold",
            serde_json::json!({
                "nodeRepairConfig": { "enabled": true, "maxUnhealthyNodeThresholdCount": 3 },
            }),
            vec![(
                "NodeRepairConfig",
                Expected::Json(
                    serde_json::json!({ "enabled": true, "maxUnhealthyNodeThresholdCount": 3 }),
                ),
            )],
        ),
        (
            "node auto repair with percentage thresholds and an action override",
            serde_json::json!({
                "nodeRepairConfig": {
                    "enabled": true,
                    "maxUnhealthyNodeThresholdPercentage": 20,
                    "maxParallelNodesRepairedPercentage": 10,
                    "nodeRepairConfigOverrides": [{
                        "nodeMonitoringCondition": "AcceleratedHardware",
                        "nodeUnhealthyReason": "GpuError",
                        "minRepairWaitTimeMins": 15,
                        "repairAction": "Replace",
                    }],
                },
            }),
            vec![(
                "NodeRepairConfig",
                Expected::Json(serde_json::json!({
                    "enabled": true,
                    "maxUnhealthyNodeThresholdPercentage": 20,
                    "maxParallelNodesRepairedPercentage": 10,
                    "nodeRepairConfigOverrides": [{
                        "nodeMonitoringCondition": "AcceleratedHardware",
                        "nodeUnhealthyReason": "GpuError",
                        "minRepairWaitTimeMins": 15,
                        "repairAction": "Replace",
                    }],
                })),
            )],
        ),
        (
            "node auto repair switched off, naming nothing else",
            serde_json::json!({ "nodeRepairConfig": { "enabled": false } }),
            vec![(
                "NodeRepairConfig",
                Expected::Json(serde_json::json!({ "enabled": false })),
            )],
        ),
        (
            "a warm pool, every member named",
            serde_json::json!({
                "warmPoolConfig": {
                    "enabled": true,
                    "minSize": 1,
                    "maxGroupPreparedCapacity": 8,
                    "poolState": "HIBERNATED",
                    "reuseOnScaleIn": true,
                },
            }),
            vec![
                ("WarmPoolEnabled", Expected::Text("true")),
                ("WarmPoolMinSize", Expected::Text("1")),
                ("WarmPoolMaxGroupPreparedCapacity", Expected::Text("8")),
                ("WarmPoolState", Expected::Text("HIBERNATED")),
                ("WarmPoolReuseOnScaleIn", Expected::Text("true")),
            ],
        ),
    ]
}

// ---------------------------------------------------------------------------
// RULE: each `UpdateParam$value` carries the value the request submitted for the
// member its type names — the JSON text of a map, list or structure, the text of a
// scalar — and every update reports `errors` as the list it is.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_nodegroup_update_reports_the_submitted_value_of_every_param_it_carries() {
    let router = emulator();
    let cluster = "update-param-encoding";
    create_cluster(&router, cluster).await;

    let rows = rows();

    // Every node group is created BEFORE the first wait, so the clock-owed
    // `CREATING` -> `ACTIVE` move of all of them overlaps instead of being waited for
    // once per row. One node group per row, so each change is made against the same
    // starting labels, taints and scaling triple.
    let names: Vec<String> = (0..rows.len()).map(|index| format!("ng-{index}")).collect();
    for (case, name) in rows.iter().map(|row| row.0).zip(&names) {
        let created = create_nodegroup_in(&router, REGION, cluster, &starting_nodegroup(name)).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: the node group must be created, got {created:?} ({})",
            created.message()
        );
    }

    for ((case, change, expected), name) in rows.iter().zip(&names) {
        let nodegroup_uri = format!(
            "/clusters/{}/node-groups/{}",
            encode_label(cluster),
            encode_label(name)
        );
        read_until(
            case,
            "the node group to reach ACTIVE so a configuration change is taken",
            || call(&router, REGION, "GET", &nodegroup_uri, ""),
            |answer| answer.body["nodegroup"]["status"] == serde_json::json!("ACTIVE"),
        )
        .await;

        let started = call(
            &router,
            REGION,
            "POST",
            &format!("{nodegroup_uri}/update-config"),
            &change.to_string(),
        )
        .await;
        assert_eq!(
            started.status, 200,
            "case {case:?}: the update must be served, got {started:?} ({})",
            started.message()
        );

        let update_id = started.body["update"]["id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            !update_id.is_empty(),
            "case {case:?}: the update must be reported with an id, got {}",
            started.body["update"]
        );
        let read = describe_nodegroup_update(&router, cluster, name, &update_id).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: DescribeUpdate must serve the update it minted, got {read:?} ({})",
            read.message()
        );

        // The update reports a param for every member the change named, and for no
        // member it did not name.
        let expected_types: Vec<&str> = expected.iter().map(|(key, _)| *key).collect();
        for (source, answer) in [
            ("UpdateNodegroupConfig", &started),
            ("DescribeUpdate", &read),
        ] {
            assert_eq!(
                param_types(answer),
                sorted(&expected_types),
                "case {case:?}: {source} must report exactly the params {expected_types:?}, \
                 got {}",
                answer.body["update"]["params"]
            );
            for (param_type, value) in expected {
                assert_param(case, source, answer, param_type, value);
            }
            // The member every update reports as the list it is.
            assert_eq!(
                answer.body["update"]["errors"],
                serde_json::json!([]),
                "case {case:?}: {source} must report an empty error list, got {}",
                answer.body["update"]
            );
        }
    }
}

/// The node group each row starts from: the labels, taints and scaling triple a
/// change of any member can be made against.
fn starting_nodegroup(name: &str) -> serde_json::Value {
    let mut body = nodegroup_request(name);
    body["labels"] = serde_json::json!({ "tier": "spot", "team": "platform", "owner": "sre" });
    body["taints"] = serde_json::json!([
        { "key": "dedicated", "value": "gpu", "effect": "NO_SCHEDULE" },
    ]);
    body["scalingConfig"] = serde_json::json!({ "minSize": 1, "maxSize": 6, "desiredSize": 3 });
    body
}

/// The `UpdateParamType` keys an answer's update reports, sorted so the comparison is
/// about WHICH params are reported rather than the order they are listed in.
fn param_types(answer: &Answer) -> Vec<String> {
    let mut types: Vec<String> = answer.body["update"]["params"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|param| param["type"].as_str().unwrap_or_default().to_string())
        .collect();
    types.sort();
    types
}

/// The same list, built from the keys a row expects.
fn sorted(types: &[&str]) -> Vec<String> {
    let mut sorted: Vec<String> = types.iter().map(|key| (*key).to_string()).collect();
    sorted.sort();
    sorted
}

/// Assert that `answer`'s update carries `param_type` with the value `expected`
/// describes.
///
/// It reads the RAW text of the member rather than reusing
/// `cluster_plane_reported_members`'s parsing reader, because what this rule is about is
/// that text: a scalar param's value is not a JSON document, so parsing first would hide
/// the only form half these rows assert.
fn assert_param(
    case: &str,
    source: &str,
    answer: &Answer,
    param_type: &str,
    expected: &Expected,
) {
    let raw = answer.body["update"]["params"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .find(|param| param["type"] == serde_json::json!(param_type))
        .and_then(|param| param["value"].as_str().map(str::to_owned));
    let raw = raw.unwrap_or_else(|| {
        panic!(
            "case {case:?}: {source} reports no {param_type} value, got {}",
            answer.body["update"]["params"]
        )
    });
    match expected {
        Expected::Json(document) => {
            let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap_or_else(|error| {
                panic!(
                    "case {case:?}: {source}'s {param_type} value must be the JSON text of the \
                     submitted value, got {raw:?} which does not parse ({error})"
                )
            });
            assert_eq!(
                &parsed, document,
                "case {case:?}: {source}'s {param_type} value must carry {document}, got \
                 {parsed} (raw {raw:?})"
            );
        }
        Expected::Text(text) => assert_eq!(
            raw, *text,
            "case {case:?}: {source}'s {param_type} value must be {text:?}, got {raw:?}"
        ),
    }
}

