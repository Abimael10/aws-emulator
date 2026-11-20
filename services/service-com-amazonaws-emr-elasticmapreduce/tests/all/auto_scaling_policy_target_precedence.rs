// SPDX-License-Identifier: CC0-1.0

//! WHICH of the two ids an automatic-scaling policy call is refused in terms of, across
//! more than one cluster, end to end over the wire.
//!
//! Every case drives the real HTTP path — `POST /` with
//! `X-Amz-Target: ElasticMapReduce.<Op>` and an `awsJson1_1` body — through the harness
//! `tests/all/job_flow_lifecycle.rs` owns, and every policy is read back through
//! `ListInstanceGroups`, so nothing reaches into the service's state struct.
//!
//! THE RULE: `PutAutoScalingPolicy` and `RemoveAutoScalingPolicy` carry a `ClusterId` and
//! an `InstanceGroupId`, and the instance group is the reference resolved FIRST — across
//! the groups this account holds, because an instance group id names one group of one
//! cluster. So an `InstanceGroupId` no cluster of the account carries a group under is
//! refused with the instance-group sentence whatever the `ClusterId` beside it resolves to;
//! a group id that DOES name a group of the account, sent with a `ClusterId` the account
//! holds no cluster for, is refused with the cluster-id sentence; and a group of ANOTHER
//! cluster of the account, sent with a cluster that resolves, is refused with the
//! instance-group sentence again, because that group is no part of the cluster named.
//! Every refusal is HTTP 400 `ValidationException` and persists nothing.
//!
//! `tests/all/auto_scaling_policy.rs` states the same rule for one cluster; the rows here
//! are the ones only a SECOND cluster can state — a group id that resolves somewhere else.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::job_flow_lifecycle::{assert_refused, Emr, INSTANCE_PROFILE, SERVICE_ROLE};

/// Cluster ids no cluster of the account carries: the shape a real id has, a prefix with
/// nothing after it, the empty string (`ClusterId` is `len 0..256`, so the boundary
/// validator passes it on to the handler) and a value that is no cluster id at all.
const UNKNOWN_CLUSTER_IDS: &[&str] = &["j-ZZZZZZZZZZZZZ", "j-", "", "not-a-cluster-id"];

/// Instance group ids no cluster of the account carries a group under, in the same spread.
const UNKNOWN_GROUP_IDS: &[&str] = &["ig-ZZZZZZZZZZZZZ", "ig-", "", "not-a-group-id"];

/// A `RunJobFlow` request launching one instance group of each role.
fn launch(name: &str) -> Value {
    json!({
        "Name": name,
        "ReleaseLabel": "emr-6.15.0",
        "JobFlowRole": INSTANCE_PROFILE,
        "ServiceRole": SERVICE_ROLE,
        "Instances": {
            "KeepJobFlowAliveWhenNoSteps": true,
            "InstanceGroups": [
                { "Name": "primary", "InstanceRole": "MASTER", "InstanceType": "m5.xlarge", "InstanceCount": 1 },
                { "Name": "core", "InstanceRole": "CORE", "InstanceType": "m5.xlarge", "InstanceCount": 2 },
            ],
        },
    })
}

/// A simple-scaling policy the service accepts, so a refusal is about the target rather
/// than about the policy.
fn sound_policy() -> Value {
    json!({
        "Constraints": { "MinCapacity": 1, "MaxCapacity": 10 },
        "Rules": [{
            "Name": "scale-out",
            "Action": {
                "SimpleScalingPolicyConfiguration": {
                    "AdjustmentType": "CHANGE_IN_CAPACITY",
                    "ScalingAdjustment": 1,
                },
            },
            "Trigger": {
                "CloudWatchAlarmDefinition": {
                    "ComparisonOperator": "LESS_THAN",
                    "MetricName": "YARNMemoryAvailablePercentage",
                    "Period": 300,
                    "Threshold": 15.0,
                },
            },
        }],
    })
}

/// The groups `ListInstanceGroups` reports for `cluster_id`, keyed by role.
async fn groups_by_role(emr: &Emr, case: &str, cluster_id: &str) -> BTreeMap<String, Value> {
    let listed = emr
        .ok("ListInstanceGroups", json!({ "ClusterId": cluster_id }))
        .await;
    let groups = listed["InstanceGroups"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected InstanceGroups, got {listed}"));
    groups
        .into_iter()
        .map(|group| {
            let role = group["InstanceGroupType"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: a group must carry its type"))
                .to_string();
            (role, group)
        })
        .collect()
}

/// The id of the core instance group of `cluster_id`.
async fn core_group_of(emr: &Emr, case: &str, cluster_id: &str) -> String {
    groups_by_role(emr, case, cluster_id).await["CORE"]["Id"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the core group must carry an Id"))
        .to_string()
}

/// The ids of the groups of `cluster_id` that report an `AutoScalingPolicy`.
async fn groups_with_a_policy(emr: &Emr, case: &str, cluster_id: &str) -> Vec<String> {
    groups_by_role(emr, case, cluster_id)
        .await
        .into_values()
        .filter(|group| !group["AutoScalingPolicy"].is_null())
        .map(|group| group["Id"].as_str().unwrap_or_default().to_string())
        .collect()
}

#[tokio::test]
async fn an_unresolvable_policy_target_names_the_reference_that_did_not_resolve() {
    let emr = Emr::new();
    let first = emr.run_job_flow(launch("precedence-first")).await;
    let second = emr.run_job_flow(launch("precedence-second")).await;
    let first_core = core_group_of(&emr, "precedence-first", &first).await;
    let second_core = core_group_of(&emr, "precedence-second", &second).await;
    assert_ne!(
        first_core, second_core,
        "the two clusters must carry distinct core groups, got {first_core:?} twice"
    );

    let cluster_sentence =
        |cluster_id: &str| format!("Cluster id '{cluster_id}' is not valid.");
    let group_sentence =
        |group_id: &str| format!("Instance group id '{group_id}' is not valid.");

    // `(the case, the ClusterId, the InstanceGroupId, the sentence the refusal must be)`.
    let mut rows: Vec<(String, String, String, String)> = Vec::new();

    // A cluster id that resolves nothing, beside a group id that resolves to a group of
    // EITHER cluster: the group is the reference that resolved, so the cluster is named.
    for unknown_cluster in UNKNOWN_CLUSTER_IDS {
        for (owner, group) in [("the first", &first_core), ("the second", &second_core)] {
            rows.push((
                format!("cluster {unknown_cluster:?} with {owner} cluster's core group"),
                (*unknown_cluster).to_string(),
                group.clone(),
                cluster_sentence(unknown_cluster),
            ));
        }
    }

    // A group of ANOTHER cluster of this account, named under a cluster that resolves: the
    // group resolves somewhere, but not inside the cluster the request scoped it to.
    rows.push((
        "the first cluster with the second cluster's core group".to_string(),
        first.clone(),
        second_core.clone(),
        group_sentence(&second_core),
    ));
    rows.push((
        "the second cluster with the first cluster's core group".to_string(),
        second.clone(),
        first_core.clone(),
        group_sentence(&first_core),
    ));

    // A group id no cluster of the account carries, under a cluster that resolves and
    // under one that does not: the group is named either way.
    for unknown_group in UNKNOWN_GROUP_IDS {
        for (owner, cluster) in [
            ("the first cluster", first.clone()),
            ("the second cluster", second.clone()),
            ("a cluster the account holds none of", UNKNOWN_CLUSTER_IDS[0].to_string()),
        ] {
            rows.push((
                format!("{owner} with group {unknown_group:?}"),
                cluster,
                (*unknown_group).to_string(),
                group_sentence(unknown_group),
            ));
        }
    }

    for (label, cluster, group, expected) in &rows {
        let requests: Vec<(&str, Value)> = vec![
            (
                "PutAutoScalingPolicy",
                json!({
                    "ClusterId": cluster,
                    "InstanceGroupId": group,
                    "AutoScalingPolicy": sound_policy(),
                }),
            ),
            (
                "RemoveAutoScalingPolicy",
                json!({ "ClusterId": cluster, "InstanceGroupId": group }),
            ),
        ];
        for (operation, request) in requests {
            let case = format!("{operation} on {label}");
            let (status, payload) = emr.call(operation, request).await;
            assert_refused(&case, status, &payload, "ValidationException");
            let said = crate::input_refusals::error_message(&payload);
            assert_eq!(
                said, *expected,
                "case {case:?}: expected {expected:?}, got {payload}"
            );
        }
    }

    // Nothing above persisted a policy on either cluster.
    for (label, cluster) in [("the first", &first), ("the second", &second)] {
        let carrying = groups_with_a_policy(&emr, label, cluster).await;
        assert!(
            carrying.is_empty(),
            "case {label:?}: a refused call must persist nothing, and groups {carrying:?} \
             report a policy"
        );
    }

    // The control: each cluster's OWN core group is served by both operations, so every
    // refusal above was about the pair of ids and not about the operation.
    for (label, cluster, group) in [
        ("the first cluster", &first, &first_core),
        ("the second cluster", &second, &second_core),
    ] {
        let attached = emr
            .ok(
                "PutAutoScalingPolicy",
                json!({
                    "ClusterId": cluster,
                    "InstanceGroupId": group,
                    "AutoScalingPolicy": sound_policy(),
                }),
            )
            .await;
        assert_eq!(
            attached["InstanceGroupId"].as_str(),
            Some(group.as_str()),
            "case {label:?}: the attach must answer the group it was given, got {attached}"
        );
        assert_eq!(
            groups_with_a_policy(&emr, label, cluster).await,
            vec![group.clone()],
            "case {label:?}: only the group named must carry the policy"
        );

        emr.ok(
            "RemoveAutoScalingPolicy",
            json!({ "ClusterId": cluster, "InstanceGroupId": group }),
        )
        .await;
        let carrying = groups_with_a_policy(&emr, label, cluster).await;
        assert!(
            carrying.is_empty(),
            "case {label:?}: the removal must take the policy off, and groups {carrying:?} \
             still report one"
        );
    }
}
