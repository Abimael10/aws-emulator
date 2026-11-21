// SPDX-License-Identifier: CC0-1.0

//! When a HyperPod cluster's clock-owed lifecycle transitions become observable.
//!
//! `CreateCluster`, `BatchAddClusterNodes` and `DeleteCluster` each DECIDE their outcome
//! when they accept the call, so the emulator owes the transition from the instant that
//! call answers (`state::transition_owed_at`): the FIRST request after it reports the
//! settled resource. A client driving a cluster with the documented poll-until-settled
//! loop therefore observes every transition on its next probe instead of paying one whole
//! polling interval per transition, and a multi-step workflow converges rather than
//! outliving its own deadline.
//!
//! Every case goes over the wire (`POST /` with `X-Amz-Target: SageMaker.<Op>`,
//! `awsJson1_1`) through the same `MultiServiceRouter` the production router uses, so what
//! these tests exercise is what a real SDK client gets. The harness is the one
//! `inference_deployment_lifecycle` declares.
//!
//! The tests assert RULES over a TABLE of inputs rather than one recorded call:
//!
//! * the first read after a create reports `InService` with every group's declared
//!   instances realised, for clusters differing in group count, declared counts and
//!   orchestrator;
//! * the first read after an addition reports each added node `Running` with its instance
//!   id, for every batch size;
//! * the first read after a delete reports the cluster gone, by either identifier form,
//!   and every operation that addresses it answers the modelled `ResourceNotFound`;
//! * NOTHING a reader does moves a transition whose instant has not passed, however often
//!   it reads;
//! * the whole create -> scale -> delete walk completes with no wait, so the refusals at
//!   its end — a Slurm controller node that may not be removed, and a cluster addressed
//!   after its deletion — are reachable.
//!
//! The seeding helpers this module exports are how the other HyperPod suites reach a
//! cluster or node state no call leaves behind for a later request: they move the instant
//! the clock owes a transition, in the one store every handler reads.
//!
//! Nothing sleeps for a transition and nothing is written to disk.

use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_sagemaker_sagemaker::state as sm;

use crate::inference_deployment_lifecycle::{error_code, message, str_field, Emulator, ACCOUNT};

/// How far out a seeded transition is owed when a case needs the resource to STAY in its
/// transient state: far enough that no reader can settle it mid-test, so "not due yet" is
/// a fact and not a race on a loaded host.
pub(crate) const FAR_FROM_DUE: Duration = Duration::from_secs(3_600);

/// This service's one store, as every handler reaches it.
///
/// `pub(crate)` because this module is the declared seeding seam for the HyperPod
/// suites: one accessor, so no suite opens the store its own way.
pub(crate) fn store(
    engine: &engine::InMemoryEngine,
) -> engine::ServiceState<service_com_amazonaws_sagemaker_sagemaker::ComAmazonawsSagemakerSagemakerState>
{
    sm::service_state(engine).expect("the SageMaker store is on the shared engine")
}

/// Move the instant the clock owes `cluster`'s `Creating` -> `InService` transition to
/// `due_at`.
///
/// `CreateCluster` owes that transition from the instant it answers, so a case about a
/// cluster that has NOT finished provisioning seeds the due instant rather than racing
/// the clock for it; passing an instant in the past makes the transition due again.
///
/// `cluster` is the `ClusterName` the record is stored under, reached directly rather than
/// through `cluster_by_name_or_arn_mut`: that lookup hides a cluster whose removal has
/// fallen due, which is exactly the record [`owe_removal_at`] has to reach.
pub(crate) fn owe_in_service_at(
    engine: &engine::InMemoryEngine,
    cluster: &str,
    due_at: SystemTime,
) {
    let state = store(engine);
    let mut guard = state.lock();
    let held = guard
        .clusters
        .get_mut(cluster)
        .unwrap_or_else(|| panic!("expected a stored cluster {cluster:?}"));
    held.in_service_at = due_at;
}

/// Move the instant the clock owes `cluster`'s teardown to `due_at`, for a case that needs
/// a cluster a later request still finds `Deleting`.
pub(crate) fn owe_removal_at(engine: &engine::InMemoryEngine, cluster: &str, due_at: SystemTime) {
    let state = store(engine);
    let mut guard = state.lock();
    let held = guard
        .clusters
        .get_mut(cluster)
        .unwrap_or_else(|| panic!("expected a stored cluster {cluster:?}"));
    held.removal_due_at = Some(due_at);
}

/// Move the instant the clock owes each of `logical_ids` its `Pending` -> `Running`
/// transition to `due_at`, for a case that needs a node that has not finished coming up.
pub(crate) fn owe_nodes_running_at(
    engine: &engine::InMemoryEngine,
    cluster: &str,
    logical_ids: &[String],
    due_at: SystemTime,
) {
    let state = store(engine);
    let mut guard = state.lock();
    let held = guard
        .clusters
        .get_mut(cluster)
        .unwrap_or_else(|| panic!("expected a stored cluster {cluster:?}"));
    for logical_id in logical_ids {
        let node = held
            .nodes
            .get_mut(logical_id)
            .unwrap_or_else(|| panic!("expected node {logical_id:?} of cluster {cluster:?}"));
        node.running_at = due_at;
    }
}

/// One instance group: the members `ClusterInstanceGroupSpecification` marks required
/// (smithy_query shape=ClusterInstanceGroupSpecification). `extra` carries whatever the
/// row adds, e.g. the `SlurmConfig` a Slurm cluster's groups declare.
fn instance_group(name: &str, count: i64, instance_type: &str, extra: Value) -> Value {
    let mut group = json!({
        "InstanceGroupName": name,
        "InstanceCount": count,
        "InstanceType": instance_type,
        "ExecutionRole": format!("arn:aws:iam::{ACCOUNT}:role/hyperpod-{name}"),
    });
    let object = group.as_object_mut().expect("a JSON object");
    for (member, value) in extra.as_object().cloned().unwrap_or_default() {
        object.insert(member, value);
    }
    group
}

/// The `InstanceGroups` of a `DescribeCluster` answer, as `(name, CurrentCount,
/// TargetCount, Status)` rows in the order the reader reported them.
fn group_counts(described: &Value) -> Vec<(String, i64, i64, String)> {
    described
        .get("InstanceGroups")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("expected InstanceGroups in {described}"))
        .iter()
        .map(|group| {
            (
                str_field(group, "InstanceGroupName"),
                group
                    .get("CurrentCount")
                    .and_then(Value::as_i64)
                    .unwrap_or_default(),
                group
                    .get("TargetCount")
                    .and_then(Value::as_i64)
                    .unwrap_or_default(),
                str_field(group, "Status"),
            )
        })
        .collect()
}

/// Every node summary `ListClusterNodes` reports for `cluster`, logical ids included.
async fn node_summaries(emulator: &Emulator, cluster: &str) -> Vec<Value> {
    let listed = emulator
        .ok(
            "ListClusterNodes",
            &json!({ "ClusterName": cluster, "IncludeNodeLogicalIds": true }),
        )
        .await;
    listed
        .get("ClusterNodeSummaries")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| panic!("expected ClusterNodeSummaries in {listed}"))
}

/// The `NodeLogicalId`s `BatchAddClusterNodes` answered, with the status it reported for
/// each: the transient state is what the call that starts the transition answers.
fn added_nodes(added: &Value) -> Vec<(String, String)> {
    added
        .get("Successful")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("expected Successful in {added}"))
        .iter()
        .map(|entry| (str_field(entry, "NodeLogicalId"), str_field(entry, "Status")))
        .collect()
}

/// Whether `candidate` is an EC2 instance id of the modelled `ClusterNodeId` form
/// (`^i-[a-f0-9]{8}(?:[a-f0-9]{9})?$`).
fn is_instance_id(candidate: &str) -> bool {
    let Some(hex) = candidate.strip_prefix("i-") else {
        return false;
    };
    (hex.len() == 8 || hex.len() == 17) && hex.chars().all(|digit| digit.is_ascii_hexdigit())
}

/// RULE: a create owes the cluster its `InService` from the instant it answers, so the
/// FIRST read after it reports the cluster provisioned — every group's declared instances
/// realised in its `CurrentCount` and held as nodes — with no wait of any length.
#[tokio::test]
async fn the_first_read_after_a_create_reports_the_cluster_provisioned() {
    // Rows differ along what the rule generalises over: how many groups the cluster
    // declares, how many instances each asks for (including the modelled lower bound of
    // zero), and which orchestrator it runs.
    let rows: Vec<(&str, Vec<Value>, Value)> = vec![
        (
            "one group of one",
            vec![instance_group("workers", 1, "ml.m5.large", json!({}))],
            json!({}),
        ),
        (
            "one group of none",
            vec![instance_group("idle", 0, "ml.t3.medium", json!({}))],
            json!({}),
        ),
        (
            "three groups of different sizes",
            vec![
                instance_group("alpha", 2, "ml.c5.large", json!({})),
                instance_group("beta", 1, "ml.m5.large", json!({})),
                instance_group("gamma", 3, "ml.g5.xlarge", json!({})),
            ],
            json!({}),
        ),
        (
            "a Slurm cluster with a controller and a compute group",
            vec![
                instance_group(
                    "controller",
                    1,
                    "ml.m5.large",
                    json!({ "SlurmConfig": { "NodeType": "Controller" } }),
                ),
                instance_group(
                    "compute",
                    2,
                    "ml.c5.large",
                    json!({ "SlurmConfig": { "NodeType": "Compute" } }),
                ),
            ],
            json!({ "Orchestrator": { "Slurm": { "SlurmConfigStrategy": "Managed" } } }),
        ),
    ];

    for (index, (case, groups, extra)) in rows.iter().enumerate() {
        let emulator = Emulator::new();
        let cluster = format!("created-{index}");
        let mut body = json!({ "ClusterName": cluster, "InstanceGroups": groups });
        for (member, value) in extra.as_object().cloned().unwrap_or_default() {
            body[member] = value;
        }
        emulator.ok("CreateCluster", &body).await;

        // The very next request, with nothing in between: no sleep, no second probe.
        let described = emulator
            .ok("DescribeCluster", &json!({ "ClusterName": cluster }))
            .await;
        assert_eq!(
            str_field(&described, "ClusterStatus"),
            "InService",
            "case {case:?}: the first read after a create must report the settled cluster, \
             got {described}"
        );
        let expected: Vec<(String, i64, i64, String)> = groups
            .iter()
            .map(|group| {
                let count = group
                    .get("InstanceCount")
                    .and_then(Value::as_i64)
                    .unwrap_or_default();
                (
                    str_field(group, "InstanceGroupName"),
                    count,
                    count,
                    "InService".to_string(),
                )
            })
            .collect();
        assert_eq!(
            group_counts(&described),
            expected,
            "case {case:?}: every group's declared instances must be realised, got \
             {described}"
        );

        // The instances the counts report are nodes the cluster actually holds.
        let summaries = node_summaries(&emulator, &cluster).await;
        let declared: i64 = groups
            .iter()
            .filter_map(|group| group.get("InstanceCount").and_then(Value::as_i64))
            .sum();
        assert_eq!(
            i64::try_from(summaries.len()).unwrap_or_default(),
            declared,
            "case {case:?}: the cluster must hold one node per declared instance, got \
             {summaries:?}"
        );
        for summary in &summaries {
            assert_eq!(
                str_field(&summary["InstanceStatus"], "Status"),
                "Running",
                "case {case:?}: an instance that came up with the cluster is Running, got \
                 {summary}"
            );
            let instance_id = str_field(summary, "InstanceId");
            assert!(
                is_instance_id(&instance_id),
                "case {case:?}: expected an EC2 instance id, got {instance_id:?} in \
                 {summary}"
            );
        }
    }
}

/// RULE: an addition owes each node it adds its `Running` from the instant it answers —
/// reporting the node `Pending` in its OWN answer — so the FIRST read after it reports the
/// node running on a minted instance id, with no wait of any length.
#[tokio::test]
async fn the_first_read_after_an_addition_reports_the_nodes_running() {
    // Rows differ along the batch size, which is the axis the rule generalises over
    // (`IncrementTargetCountBy` is modelled `@range 1..50`).
    for increment in [1_i64, 2, 5] {
        let emulator = Emulator::new();
        let cluster = format!("scaled-{increment}");
        emulator
            .ok(
                "CreateCluster",
                &json!({
                    "ClusterName": cluster,
                    "InstanceGroups": [instance_group("pool", 0, "ml.c5.large", json!({}))],
                }),
            )
            .await;

        let added = emulator
            .ok(
                "BatchAddClusterNodes",
                &json!({
                    "ClusterName": cluster,
                    "NodesToAdd": [{
                        "InstanceGroupName": "pool",
                        "IncrementTargetCountBy": increment,
                    }],
                }),
            )
            .await;
        let answered = added_nodes(&added);
        assert_eq!(
            i64::try_from(answered.len()).unwrap_or_default(),
            increment,
            "case increment {increment}: one entry per added node, got {added}"
        );
        for (logical_id, status) in &answered {
            assert_eq!(
                status, "Pending",
                "case increment {increment}: the call that starts the transition answers \
                 the transient status for node {logical_id:?}, got {added}"
            );
        }

        // The very next request reports every one of them running, with its instance id.
        for (logical_id, _) in &answered {
            let described = emulator
                .ok(
                    "DescribeClusterNode",
                    &json!({ "ClusterName": cluster, "NodeLogicalId": logical_id }),
                )
                .await;
            let details = described
                .get("NodeDetails")
                .cloned()
                .unwrap_or_else(|| panic!("expected NodeDetails in {described}"));
            assert_eq!(
                str_field(&details["InstanceStatus"], "Status"),
                "Running",
                "case increment {increment}: the first read after the add must report node \
                 {logical_id:?} running, got {described}"
            );
            let instance_id = str_field(&details, "InstanceId");
            assert!(
                is_instance_id(&instance_id),
                "case increment {increment}: node {logical_id:?} must carry a minted \
                 instance id, got {instance_id:?} in {described}"
            );
        }
    }
}

/// RULE: a delete owes the cluster's removal from the instant it answers, so from the
/// FIRST request after it every operation that addresses the cluster answers the modelled
/// `ResourceNotFound` — the reader, the node operations and the delete itself — and the
/// name is free for another cluster.
#[tokio::test]
async fn the_first_request_after_a_delete_reports_the_cluster_gone() {
    // Rows differ along the identifier form the request names the cluster by, which is
    // what `ClusterNameOrArn` generalises over, and along the cluster's shape.
    for by_arn in [false, true] {
        for (case, declared) in [("an empty group", 0_i64), ("a populated group", 3)] {
            let emulator = Emulator::new();
            let cluster = format!("torn-down-{}-{declared}", usize::from(by_arn));
            let created = emulator
                .ok(
                    "CreateCluster",
                    &json!({
                        "ClusterName": cluster,
                        "InstanceGroups": [
                            instance_group("pool", declared, "ml.c5.large", json!({})),
                        ],
                    }),
                )
                .await;
            let arn = str_field(&created, "ClusterArn");
            let identifier = if by_arn { arn.clone() } else { cluster.clone() };

            emulator
                .ok("DeleteCluster", &json!({ "ClusterName": identifier }))
                .await;

            // The very next request: the cluster is gone for every operation that
            // addresses it, whichever identifier form names it.
            for target in [
                "DescribeCluster",
                "DeleteCluster",
                "ListClusterNodes",
                "BatchAddClusterNodes",
            ] {
                let mut body = json!({ "ClusterName": identifier });
                if target == "BatchAddClusterNodes" {
                    body["NodesToAdd"] = json!([{
                        "InstanceGroupName": "pool",
                        "IncrementTargetCountBy": 1,
                    }]);
                }
                let (status, refused) = emulator.call(target, &body).await;
                assert_eq!(
                    status, 400,
                    "case {case:?} by_arn={by_arn} {target}: expected HTTP 400, got {status} \
                     with {refused}"
                );
                assert_eq!(
                    error_code(&refused),
                    "ResourceNotFound",
                    "case {case:?} by_arn={by_arn} {target}: expected ResourceNotFound, got \
                     {refused}"
                );
                assert_eq!(
                    message(&refused),
                    sm::cluster_not_found(&identifier),
                    "case {case:?} by_arn={by_arn} {target}: refusal message, got {refused}"
                );
            }

            // And the name it held is free: a create under it mints a DIFFERENT cluster.
            let recreated = emulator
                .ok(
                    "CreateCluster",
                    &json!({
                        "ClusterName": cluster,
                        "InstanceGroups": [instance_group("pool", 1, "ml.m5.large", json!({}))],
                    }),
                )
                .await;
            let fresh = str_field(&recreated, "ClusterArn");
            assert_ne!(
                fresh, arn,
                "case {case:?} by_arn={by_arn}: the name must be free for a NEW cluster, got \
                 {recreated}"
            );
        }
    }
}

/// RULE: reading is not what moves a transition on. A transition whose instant has not
/// passed stays unmaterialised however many times — and through whichever reader — a
/// caller looks, so no read changes what a later caller sees.
#[tokio::test]
async fn a_transition_whose_instant_has_not_passed_survives_every_read() {
    let emulator = Emulator::new();
    let cluster = "not-due-yet";
    emulator
        .ok(
            "CreateCluster",
            &json!({
                "ClusterName": cluster,
                "InstanceGroups": [instance_group("pool", 2, "ml.c5.large", json!({}))],
            }),
        )
        .await;
    owe_in_service_at(
        &emulator.engine,
        cluster,
        SystemTime::now() + FAR_FROM_DUE,
    );

    // Reading it, through every reader of a cluster, leaves the cluster provisioning and
    // its groups unrealised.
    for read in 0..4 {
        let described = emulator
            .ok("DescribeCluster", &json!({ "ClusterName": cluster }))
            .await;
        assert_eq!(
            str_field(&described, "ClusterStatus"),
            "Creating",
            "read {read}: a cluster whose InService is not due stays Creating, got \
             {described}"
        );
        assert_eq!(
            group_counts(&described),
            vec![("pool".to_string(), 0, 2, "Creating".to_string())],
            "read {read}: nothing is realised before the transition is due, got {described}"
        );
        let listed = emulator
            .ok("ListClusters", &json!({ "NameContains": cluster }))
            .await;
        let summary = listed
            .get("ClusterSummaries")
            .and_then(Value::as_array)
            .and_then(|summaries| summaries.first())
            .cloned()
            .unwrap_or_else(|| panic!("expected the cluster in {listed}"));
        assert_eq!(
            str_field(&summary, "ClusterStatus"),
            "Creating",
            "read {read}: the listing must report the same status as the reader, got \
             {listed}"
        );
        assert!(
            node_summaries(&emulator, cluster).await.is_empty(),
            "read {read}: a cluster that has not provisioned holds no node yet"
        );
    }

    // Once the instant has passed, the first read materialises the transition.
    owe_in_service_at(&emulator.engine, cluster, SystemTime::now());
    let described = emulator
        .ok("DescribeCluster", &json!({ "ClusterName": cluster }))
        .await;
    assert_eq!(
        str_field(&described, "ClusterStatus"),
        "InService",
        "a cluster whose InService has fallen due is reported settled, got {described}"
    );
    assert_eq!(
        node_summaries(&emulator, cluster).await.len(),
        2,
        "the instances the group declared come up with the cluster"
    );
}

/// RULE: because no transition of the walk costs a caller a probe, the whole lifecycle —
/// create, scale a group, refuse what may not be removed, remove what may, tear the
/// cluster down — is driveable end to end without waiting, so the refusals at its end are
/// reachable: a Slurm controller node is not removable, and a cluster addressed after its
/// deletion is `ResourceNotFound`.
#[tokio::test]
async fn the_whole_cluster_lifecycle_is_driveable_without_waiting() {
    let emulator = Emulator::new();
    let cluster = "slurm-lifecycle";
    emulator
        .ok(
            "CreateCluster",
            &json!({
                "ClusterName": cluster,
                "Orchestrator": { "Slurm": { "SlurmConfigStrategy": "Managed" } },
                "InstanceGroups": [
                    instance_group(
                        "controller",
                        1,
                        "ml.m5.large",
                        json!({ "SlurmConfig": { "NodeType": "Controller" } }),
                    ),
                    instance_group(
                        "compute",
                        0,
                        "ml.c5.large",
                        json!({ "SlurmConfig": { "NodeType": "Compute" } }),
                    ),
                ],
            }),
        )
        .await;

    // Provisioned on the first probe.
    let described = emulator
        .ok("DescribeCluster", &json!({ "ClusterName": cluster }))
        .await;
    assert_eq!(
        str_field(&described, "ClusterStatus"),
        "InService",
        "the cluster must be provisioned on the first probe, got {described}"
    );

    // Scale the compute group, and read the worker back running on the first probe.
    let added = emulator
        .ok(
            "BatchAddClusterNodes",
            &json!({
                "ClusterName": cluster,
                "NodesToAdd": [{
                    "InstanceGroupName": "compute",
                    "IncrementTargetCountBy": 1,
                }],
            }),
        )
        .await;
    let (worker, pending) = added_nodes(&added)
        .first()
        .cloned()
        .unwrap_or_else(|| panic!("expected an added node in {added}"));
    assert_eq!(
        pending, "Pending",
        "the add answers the transient status, got {added}"
    );
    let described = emulator
        .ok(
            "DescribeClusterNode",
            &json!({ "ClusterName": cluster, "NodeLogicalId": worker }),
        )
        .await;
    let details = described
        .get("NodeDetails")
        .cloned()
        .unwrap_or_else(|| panic!("expected NodeDetails in {described}"));
    assert_eq!(
        str_field(&details["InstanceStatus"], "Status"),
        "Running",
        "the worker must be running on the first probe, got {described}"
    );
    let worker_instance = str_field(&details, "InstanceId");
    assert!(
        is_instance_id(&worker_instance),
        "the worker must carry a minted instance id, got {described}"
    );

    // The controller node of a Slurm cluster is not removable: the refusal the end of the
    // walk reaches.
    let controller = node_summaries(&emulator, cluster)
        .await
        .into_iter()
        .find(|summary| str_field(summary, "InstanceGroupName") == "controller")
        .map(|summary| str_field(&summary, "NodeLogicalId"))
        .unwrap_or_else(|| panic!("expected a controller node of {cluster:?}"));
    let (status, refused) = emulator
        .call(
            "BatchDeleteClusterNodes",
            &json!({ "ClusterName": cluster, "NodeLogicalIds": [controller] }),
        )
        .await;
    assert_eq!(
        status, 400,
        "a controller node must not be removable, got {status} with {refused}"
    );
    assert_eq!(
        error_code(&refused),
        "ValidationException",
        "controller-node refusal code, got {refused}"
    );
    assert_eq!(
        message(&refused),
        sm::ALL_NODES_FAILED_VALIDATION,
        "controller-node refusal message, got {refused}"
    );

    // The worker is removable, and is gone from the inventory on the first probe.
    let removed = emulator
        .ok(
            "BatchDeleteClusterNodes",
            &json!({ "ClusterName": cluster, "NodeLogicalIds": [worker.clone()] }),
        )
        .await;
    assert_eq!(
        removed.get("SuccessfulNodeLogicalIds"),
        Some(&json!([worker.clone()])),
        "the worker must be removed, got {removed}"
    );
    let held: Vec<String> = node_summaries(&emulator, cluster)
        .await
        .iter()
        .map(|summary| str_field(summary, "NodeLogicalId"))
        .collect();
    assert!(
        !held.contains(&worker),
        "the removed worker must be gone from the inventory, got {held:?}"
    );

    // Tear the cluster down: gone on the first probe, and every operation addressing it
    // answers the modelled refusal with the sentence this service words it in.
    emulator
        .ok("DeleteCluster", &json!({ "ClusterName": cluster }))
        .await;
    for (target, body) in [
        ("DescribeCluster", json!({ "ClusterName": cluster })),
        (
            "BatchAddClusterNodes",
            json!({
                "ClusterName": cluster,
                "NodesToAdd": [{
                    "InstanceGroupName": "compute",
                    "IncrementTargetCountBy": 1,
                }],
            }),
        ),
    ] {
        let (status, refused) = emulator.call(target, &body).await;
        assert_eq!(
            status, 400,
            "case {target}: a deleted cluster must be gone, got {status} with {refused}"
        );
        assert_eq!(
            error_code(&refused),
            "ResourceNotFound",
            "case {target}: expected ResourceNotFound, got {refused}"
        );
        assert_eq!(
            message(&refused),
            sm::cluster_not_found(cluster),
            "case {target}: refusal message, got {refused}"
        );
    }
}
