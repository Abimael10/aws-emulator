// SPDX-License-Identifier: CC0-1.0

//! Which instance-collection vocabulary a cluster is read in, end to end over the wire.
//!
//! Every case drives the real HTTP path — `POST /` with
//! `X-Amz-Target: ElasticMapReduce.<Op>` and an `awsJson1_1` body — through the router's
//! `dispatch`, using the harness `tests/all/job_flow_lifecycle.rs` owns, so these are the
//! same bytes an SDK client sends. Nothing reaches into the service's state struct: which
//! collection a cluster carries is read back through `DescribeCluster`, and the collection
//! itself through the two listings.
//!
//! THE RULE: "the `RunJobFlow` request can contain `InstanceFleets` parameters or
//! `InstanceGroups` parameters, but not both" (`RunJobFlow` doc, Smithy model), so a cluster
//! is described in exactly ONE collection vocabulary — the `InstanceCollectionType` every
//! read of it reports — and the exclusion holds for the whole life of the cluster: the
//! listing of the collection a cluster does NOT carry is refused with the mutual-exclusion
//! sentence, which names the collection the cluster really uses and the listing to call
//! instead, rather than answered with an empty page. The rows span both directions of the
//! exclusion, every form a launch states its capacity in, a paged call, and a cluster that
//! has since been terminated.

use serde_json::{json, Value};

use crate::job_flow_lifecycle::{
    assert_refused, at, await_cluster_state, Emr, INSTANCE_PROFILE, SERVICE_ROLE,
};
use crate::refusal_messages::message;

/// The error identity both listings model for a bad request
/// (`smithy_query(query="operation", operation="ListInstanceGroups,ListInstanceFleets")`).
const INVALID_REQUEST_EXCEPTION: &str = "InvalidRequestException";

/// The release every launch below names.
const RELEASE: &str = "emr-6.15.0";

/// The sentence the listing of the collection a cluster does not carry is refused with:
/// `carried` is the collection the cluster really uses, `operation` the listing called and
/// `counterpart` the one that serves `carried`.
fn exclusion_sentence(carried: &str, operation: &str, counterpart: &str) -> String {
    format!(
        "Instance fleets and instance groups are mutually exclusive. The EMR cluster \
         specified in the request uses instance {carried}. The {operation} operation does \
         not support clusters that use instance {carried}. Use the {counterpart} operation \
         instead."
    )
}

/// A `RunJobFlow` body whose `Instances` member is `instances`.
fn launch(name: &str, instances: Value) -> Value {
    let mut request = json!({
        "Name": name,
        "ReleaseLabel": RELEASE,
        "JobFlowRole": INSTANCE_PROFILE,
        "ServiceRole": SERVICE_ROLE,
        "Instances": instances,
    });
    request["Instances"]["KeepJobFlowAliveWhenNoSteps"] = json!(true);
    request
}

/// One instance fleet of each node type a fleet launch needs.
fn fleet_instances() -> Value {
    json!({
        "InstanceFleets": [
            {
                "Name": "primary",
                "InstanceFleetType": "MASTER",
                "TargetOnDemandCapacity": 1,
                "InstanceTypeConfigs": [{ "InstanceType": "m5.xlarge" }],
            },
            {
                "Name": "core",
                "InstanceFleetType": "CORE",
                "TargetOnDemandCapacity": 2,
                "InstanceTypeConfigs": [{ "InstanceType": "m5.xlarge" }],
            },
        ],
    })
}

/// One row of the exclusion rule: a launch, the collection the cluster it mints carries, and
/// how many entries the listing that serves that collection must report.
struct Case {
    label: &'static str,
    instances: Value,
    collection_type: &'static str,
    /// `(the listing that serves the cluster, how many entries it reports)`.
    served: (&'static str, usize),
    /// The listing that does NOT serve the cluster, and the noun the refusal names.
    refused: (&'static str, &'static str),
}

fn cases() -> Vec<Case> {
    vec![
        Case {
            label: "a launch stating its capacity as instance fleets",
            instances: fleet_instances(),
            collection_type: "INSTANCE_FLEET",
            served: ("ListInstanceFleets", 2),
            refused: ("ListInstanceGroups", "fleets"),
        },
        Case {
            label: "a launch stating its capacity as instance groups",
            instances: json!({
                "InstanceGroups": [
                    { "Name": "primary", "InstanceRole": "MASTER", "InstanceType": "m5.xlarge", "InstanceCount": 1 },
                    { "Name": "core", "InstanceRole": "CORE", "InstanceType": "m5.xlarge", "InstanceCount": 2 },
                    { "Name": "workers", "InstanceRole": "TASK", "InstanceType": "c5.xlarge", "InstanceCount": 1 },
                ],
            }),
            collection_type: "INSTANCE_GROUP",
            served: ("ListInstanceGroups", 3),
            refused: ("ListInstanceFleets", "groups"),
        },
        Case {
            label: "a launch stating its capacity the uniform way",
            instances: json!({
                "InstanceCount": 2,
                "MasterInstanceType": "m5.xlarge",
                "SlaveInstanceType": "m5.xlarge",
            }),
            // The uniform form states the capacity instance GROUPS state, so the cluster
            // carries the groups it implies.
            collection_type: "INSTANCE_GROUP",
            served: ("ListInstanceGroups", 2),
            refused: ("ListInstanceFleets", "groups"),
        },
    ]
}

/// The listing's own output member, which is the one it reports its collection in.
fn listed_member(operation: &str) -> &'static str {
    if operation == "ListInstanceFleets" {
        return "InstanceFleets";
    }
    "InstanceGroups"
}

/// The listing that serves the vocabulary `operation` does not.
fn counterpart(operation: &str) -> &'static str {
    if operation == "ListInstanceFleets" {
        return "ListInstanceGroups";
    }
    "ListInstanceFleets"
}

/// Assert `operation` is refused on `cluster_id` with the exclusion sentence for `carried`.
async fn assert_excluded(
    emr: &Emr,
    case: &str,
    operation: &str,
    carried: &str,
    cluster_id: &str,
    extra: Value,
) {
    let mut body = json!({ "ClusterId": cluster_id });
    if let Some(members) = extra.as_object() {
        for (member, value) in members {
            body[member] = value.clone();
        }
    }
    let (status, payload) = emr.call(operation, body).await;
    assert_refused(case, status, &payload, INVALID_REQUEST_EXCEPTION);
    assert_eq!(
        message(case, &payload),
        exclusion_sentence(carried, operation, counterpart(operation)),
        "case {case:?}: wrong exclusion sentence, got {payload}"
    );
}

#[tokio::test]
async fn the_listing_of_the_collection_a_cluster_does_not_carry_is_refused() {
    let emr = Emr::new();

    for case in cases() {
        let label = case.label;
        let cluster_id = emr.run_job_flow(launch(label, case.instances.clone())).await;

        // What the cluster itself says it carries, which is what the rule turns on.
        let described = emr.describe_cluster(&cluster_id).await;
        assert_eq!(
            at(&described, &["InstanceCollectionType"]).as_str(),
            Some(case.collection_type),
            "case {label:?}: the cluster must report the collection it carries, got {described}"
        );

        // The listing that SERVES that collection reports every entry of it — the positive
        // control, so the refusal below is about the vocabulary and not about the cluster.
        let (serving, expected) = case.served;
        let served = emr
            .ok(serving, json!({ "ClusterId": &cluster_id }))
            .await;
        assert_eq!(
            at(&served, &[listed_member(serving)]).as_array().map(Vec::len),
            Some(expected),
            "case {label:?}: {serving} must report {expected} entries, got {served}"
        );

        // The listing of the OTHER vocabulary is refused for the collection the cluster
        // really carries, with no empty page anywhere in the answer.
        let (refused, carried) = case.refused;
        assert_excluded(&emr, label, refused, carried, &cluster_id, json!({})).await;

        // A paged call is refused the same way: the cluster's vocabulary is settled before
        // the token is read, so a caller cannot page past the exclusion.
        assert_excluded(
            &emr,
            &format!("{label}, paged"),
            refused,
            carried,
            &cluster_id,
            json!({ "Marker": "ig-ZZZZZZZZZZZZZ" }),
        )
        .await;

        // And it still is once the cluster has been taken down: a terminated cluster keeps
        // the collection it was launched with.
        emr.ok(
            "TerminateJobFlows",
            json!({ "JobFlowIds": [cluster_id.clone()] }),
        )
        .await;
        await_cluster_state(&emr, &cluster_id, label, "TERMINATED").await;
        assert_excluded(
            &emr,
            &format!("{label}, terminated"),
            refused,
            carried,
            &cluster_id,
            json!({}),
        )
        .await;
    }
}
