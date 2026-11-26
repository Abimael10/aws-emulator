// SPDX-License-Identifier: CC0-1.0

//! WHICH member an instance-group-scoped operation names when the instance group it is
//! told to act on resolves to nothing, end to end over the wire.
//!
//! Every case drives the real HTTP path — `POST /` with
//! `X-Amz-Target: ElasticMapReduce.<Op>` and an `awsJson1_1` body — through the harness
//! `tests/all/job_flow_lifecycle.rs` owns.
//!
//! THE RULE: an operation scoped to one instance group of a cluster resolves the
//! `InstanceGroupId` member the request names, and when that reference resolves no group
//! inside a cluster that DID resolve it is refused — HTTP 400 `ValidationException` —
//! with the instance-group sentence interpolating the value of THAT member. The cluster
//! reference is the scope the lookup happens in, so its value is never what the sentence
//! names, and the sentence does not depend on which operation stated it: the attach, the
//! detach and the capacity change share one lookup and therefore one wording.
//!
//! `tests/all/auto_scaling_policy_target_precedence.rs` states the PRECEDENCE half of
//! this rule — which of the two references is resolved first — for the automatic-scaling
//! pair across two clusters. The rows here are the ones only a THIRD operation sharing
//! the lookup can state, and they span the shapes a group reference arrives in, including
//! one that is itself a well-formed cluster id and one that is the request's own cluster
//! id, where naming the wrong member would be invisible to a test that only ever sent an
//! `ig-` shaped value.

use serde_json::{json, Value};

use crate::input_refusals::error_message;
use crate::job_flow_lifecycle::{assert_refused, Emr, INSTANCE_PROFILE, SERVICE_ROLE};

/// The `InstanceGroupId` member's constraint is `@length 0..256`
/// (`smithy_query(query="shape",shape="com.amazonaws.emr#InstanceGroupId")`), so a value
/// this long reaches the handler's lookup rather than the boundary validator. One past
/// the bound is the boundary validator's class, and `tests/all/malformed_input_refusals.rs`
/// owns it.
fn longest_group_id() -> String {
    format!("ig-{}", "Z".repeat(253))
}

/// A `RunJobFlow` request launching a primary and a core instance group.
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

/// A simple-scaling policy the service accepts, so a refusal is about the group reference
/// rather than about the policy.
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

/// The requests the operations that share the instance-group lookup are driven with, each
/// carrying `cluster_id` as its cluster reference and `group_id` as its group reference.
fn scoped_requests(cluster_id: &str, group_id: &str) -> Vec<(&'static str, Value)> {
    vec![
        (
            "PutAutoScalingPolicy",
            json!({
                "ClusterId": cluster_id,
                "InstanceGroupId": group_id,
                "AutoScalingPolicy": sound_policy(),
            }),
        ),
        (
            "RemoveAutoScalingPolicy",
            json!({ "ClusterId": cluster_id, "InstanceGroupId": group_id }),
        ),
        (
            "ModifyInstanceGroups",
            json!({
                "ClusterId": cluster_id,
                "InstanceGroups": [{ "InstanceGroupId": group_id, "InstanceCount": 3 }],
            }),
        ),
    ]
}

#[tokio::test]
async fn an_unresolved_instance_group_is_refused_naming_that_member_not_the_cluster() {
    let emr = Emr::new();
    let cluster_id = emr.run_job_flow(launch("unresolved-group-scope")).await;

    // `(the case label, the value the request's InstanceGroupId carries)`. The rows differ
    // along the FORM of the reference — a well-formed id nothing answers to, a prefix with
    // nothing after it, the empty string, a value drawn from no id alphabet, the longest
    // value the member admits, a value shaped like a CLUSTER id, and the request's own
    // cluster id — because the rule is about which member the sentence reads, not about
    // what the value looks like.
    let longest = longest_group_id();
    let group_references: Vec<(String, String)> = vec![
        (
            "a well-formed group id nothing answers to".to_string(),
            "ig-2QD7HLKX9B4VN".to_string(),
        ),
        ("the group id prefix alone".to_string(), "ig-".to_string()),
        ("the empty group id".to_string(), String::new()),
        (
            "a value from no id alphabet".to_string(),
            "no such group".to_string(),
        ),
        (
            "the longest group id the member admits".to_string(),
            longest,
        ),
        (
            "a value shaped like a cluster id".to_string(),
            "j-ZZZZZZZZZZZZZ".to_string(),
        ),
        (
            "the request's own cluster id".to_string(),
            cluster_id.clone(),
        ),
    ];

    for (label, group_id) in &group_references {
        let expected = format!("Instance group id '{group_id}' is not valid.");
        // Every operation sharing the lookup must answer the SAME sentence, so the
        // wording is collected per row and compared across the operations.
        let mut said_by: Vec<(&'static str, String)> = Vec::new();

        for (operation, request) in scoped_requests(&cluster_id, group_id) {
            let case = format!("{operation} on a cluster that resolves with {label}");
            let (status, payload) = emr.call(operation, request).await;
            assert_refused(&case, status, &payload, "ValidationException");
            let said = error_message(&payload);
            assert_eq!(
                said, expected,
                "case {case:?}: expected {expected:?}, got {payload}"
            );
            assert!(
                !said.contains("Cluster id"),
                "case {case:?}: the cluster reference resolved, so the sentence must not \
                 name the cluster member, got {said:?}"
            );
            // The one row whose group reference IS the cluster id cannot distinguish the
            // two values, so it is exempt from this check and carried by the sentence
            // comparison above instead.
            if group_id != &cluster_id {
                assert!(
                    !said.contains(cluster_id.as_str()),
                    "case {case:?}: expected the group reference {group_id:?} in the \
                     sentence and not the cluster id {cluster_id:?}, got {said:?}"
                );
            }
            said_by.push((operation, said));
        }

        let (first_op, first_said) = said_by
            .first()
            .cloned()
            .expect("every row drives at least one operation");
        for (operation, said) in &said_by {
            assert_eq!(
                said, &first_said,
                "case {label:?}: {operation} and {first_op} share one instance-group \
                 lookup, so they must word the refusal identically, got {said:?} and \
                 {first_said:?}"
            );
        }
    }
}

#[tokio::test]
async fn an_unresolved_instance_group_names_that_member_with_no_cluster_scope_sent() {
    let emr = Emr::new();
    // A cluster the account DOES hold, so the store is not empty when the lookup fails.
    emr.run_job_flow(launch("unresolved-group-no-scope")).await;

    // "The ID of the cluster to which the instance group belongs"
    // (`smithy_query(query="operation",operation="ModifyInstanceGroups")`: `ClusterId` is
    // not required) — an instance group id already names the one cluster that carries it,
    // so a request that omits the cluster reference has only the group reference to name.
    for (label, group_id) in [
        ("a well-formed group id nothing answers to", "ig-7XK2PMQW4D8ZT"),
        ("the group id prefix alone", "ig-"),
        ("a value from no id alphabet", "no such group"),
    ] {
        let case = format!("ModifyInstanceGroups with no cluster scope and {label}");
        let (status, payload) = emr
            .call(
                "ModifyInstanceGroups",
                json!({
                    "InstanceGroups": [{ "InstanceGroupId": group_id, "InstanceCount": 3 }],
                }),
            )
            .await;
        assert_refused(&case, status, &payload, "ValidationException");
        let expected = format!("Instance group id '{group_id}' is not valid.");
        let said = error_message(&payload);
        assert_eq!(
            said, expected,
            "case {case:?}: expected {expected:?}, got {payload}"
        );
    }
}
