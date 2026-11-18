// SPDX-License-Identifier: CC0-1.0

//! The Amazon ECS cluster an AWS Batch compute environment is backed by, read from BOTH sides
//! on one engine: the environment's own reader and the service that HOLDS the cluster.
//!
//! The RULE under test: **a compute environment AWS Batch orchestrates with Amazon ECS is
//! backed by ONE cluster of that service, provisioned when the environment is created, and
//! `ComputeEnvironmentDetail$ecsClusterArn` ("the Amazon Resource Name (ARN) of the underlying
//! Amazon ECS cluster that the compute environment uses") reports that cluster's ARN — the same
//! one through every reader, and for the environment's whole life — so a client that hands the
//! ARN back to Amazon ECS finds the cluster there.** It is the documented handle a caller
//! resolves a Batch workload through, so what it names has to EXIST where that service holds
//! it: `ecs:DescribeClusters` answers for it with an empty `failures` list and `ecs:ListClusters`
//! lists it. Being provisioned at the create is what makes it stable: a READ of the environment
//! reports the cluster, it never provisions a second one, distinct environments never share one,
//! and an update to the environment leaves it exactly where it was. An environment orchestrated
//! with Amazon EKS is backed by no cluster of this service, so the member is absent for it.
//!
//! Sources: `smithy_query(query="doc",shape="ComputeEnvironmentDetail",member="ecsClusterArn")`
//! for the member under test, and
//! `smithy_query(query="operation",operation="DescribeClusters,ListClusters")` for the Amazon
//! ECS readers the ARN is resolved through (`clusters`/`failures`, `clusterArns`).
//!
//! The rows differ along what the rule generalises over: the environment's type and the compute
//! type behind it (`MANAGED` over Fargate, `MANAGED` over EC2, `UNMANAGED`), the orchestration
//! that decides whether there is a cluster at all (Amazon ECS against Amazon EKS), the READER
//! the ARN is read through (the environment named, the environment named by its ARN, the
//! unfiltered describe), the COUNT of environments one emulator holds at once, and what happens
//! to the environment afterwards (read again, and updated).
//!
//! What is NOT asserted here: the half of the chain that belongs to the owning crate's own
//! suite — `services/service-com-amazonaws-batch-awsbatchv20160810/tests/all/batch_ecs_orchestration.rs`
//! spans the `containerOrchestrationType` each environment reports and the deletion that takes
//! the cluster with it.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for, and so the Region of every ARN minted for them.
const REGION: &str = "us-east-1";

/// The account this emulator mints its ARNs under.
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// The `CEStatus` a compute environment comes to rest in. Source:
/// `smithy_query(query="shape",shape="CEStatus")`.
const VALID: &str = "VALID";

/// The `ClusterField`-independent `Cluster$status` of a cluster Amazon ECS holds and may be
/// used. Source: `smithy_query(query="shape",shape="Cluster")` ("the status of the cluster").
const ACTIVE: &str = "ACTIVE";

/// The prefix AWS Batch names the cluster of a compute environment with, spelled out here
/// rather than imported from the service crate, so a wrong constant in the implementation
/// cannot make this suite pass: `AWSBatch-<compute environment name>-<the environment's id>`.
const BATCH_CLUSTER_PREFIX: &str = "AWSBatch-";

/// How long a wait below gives the clock: far above the spans Batch gives a compute
/// environment's moves, and every wait looks again for the CONDITION rather than sleeping a
/// fixed stretch, so a loaded machine delays a row instead of breaking it.
const DEADLINE: Duration = Duration::from_secs(120);

/// How long one look waits before the next.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// AWS Batch and Amazon ECS over ONE shared engine, the way `core/router/src/lib.rs` mounts
/// every service: the cluster backing a compute environment is Amazon ECS's resource, so the
/// ARN the environment reports is answered for by that service's own readers. Each service
/// claims only requests scoped to its own signing name, so no call below is routed any
/// differently for the other's presence.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_batch_awsbatchv20160810::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_ecs_amazonec2containerservicev20141113::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 credential scope naming `service`, which is what routes the request. The key id and
/// signature are not credentials: the router above runs with request authentication off, so
/// nothing is verified.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

/// The status and the parsed body of one response.
fn answered(response: http::Response<Bytes>) -> (u16, Value) {
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&payload)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&payload).into_owned()))
    };
    (status, parsed)
}

/// One AWS Batch call: a `restJson1` service, so `POST <uri>` with a JSON body, which must
/// answer 200.
async fn batch(router: &MultiServiceRouter, case: &str, uri: &str, body: &Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://batch.{REGION}.amazonaws.com{uri}"))
        .header("host", format!("batch.{REGION}.amazonaws.com"))
        .header("authorization", authorization("batch"))
        .header("content-type", "application/json")
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let (status, parsed) = answered(router.dispatch(request).await);
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Batch {uri}, got {status} with {parsed}"
    );
    parsed
}

/// One Amazon ECS call (`awsJson1_1`, `AmazonEC2ContainerServiceV20141113.<Operation>`), which
/// must answer 200.
async fn ecs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("ecs.{REGION}.amazonaws.com"))
        .header("authorization", authorization("ecs"))
        .header("content-type", "application/x-amz-json-1.1")
        .header(
            "x-amz-target",
            format!("AmazonEC2ContainerServiceV20141113.{operation}"),
        )
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let (status, parsed) = answered(router.dispatch(request).await);
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Amazon ECS {operation}, got {status} with {parsed}"
    );
    parsed
}

/// A string member of a body at `pointer`, or a panic naming the case and what came back.
fn member(case: &str, body: &Value, pointer: &str) -> String {
    body.pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected {pointer} in {body}"))
        .to_string()
}

/// The list a `DescribeComputeEnvironments` body carries.
fn environments(body: &Value) -> Vec<Value> {
    body["computeEnvironments"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// The one record `DescribeComputeEnvironments` reports for `reference` — a compute
/// environment's name or its ARN, both of which the operation's `computeEnvironments` accepts.
async fn record_for(router: &MultiServiceRouter, case: &str, reference: &str) -> Value {
    let body = batch(
        router,
        case,
        "/v1/describecomputeenvironments",
        &json!({"computeEnvironments": [reference]}),
    )
    .await;
    let listed = environments(&body);
    assert_eq!(
        listed.len(),
        1,
        "case {case:?}: expected exactly one record for {reference}, got {body}"
    );
    listed[0].clone()
}

/// The record the UNFILTERED `DescribeComputeEnvironments` carries for `name`.
async fn record_in_the_unfiltered_describe(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
) -> Value {
    let body = batch(router, case, "/v1/describecomputeenvironments", &json!({})).await;
    environments(&body)
        .into_iter()
        .find(|record| record["computeEnvironmentName"].as_str() == Some(name))
        .unwrap_or_else(|| {
            panic!("case {case:?}: the unfiltered describe must carry {name}, got {body}")
        })
}

/// Look again until `name` reports `VALID`, then answer its record.
async fn until_valid(router: &MultiServiceRouter, case: &str, name: &str) -> Value {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let record = record_for(router, case, name).await;
        if record["status"].as_str() == Some(VALID) {
            return record;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the compute environment never reached {VALID}, last {record}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The ARNs of the clusters Amazon ECS LISTS that AWS Batch provisioned for a compute
/// environment, which is how many clusters that service really holds for them.
async fn batch_clusters(router: &MultiServiceRouter, case: &str) -> BTreeSet<String> {
    let listed = ecs(router, case, "ListClusters", &json!({})).await;
    listed["clusterArns"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(Value::as_str)
        .filter(|arn| {
            arn.rsplit('/')
                .next()
                .is_some_and(|name| name.starts_with(BATCH_CLUSTER_PREFIX))
        })
        .map(str::to_string)
        .collect()
}

/// The compute resources a `MANAGED` Fargate environment is created with: the two required
/// members of `ComputeResource` (`type`, `maxvCpus`) plus the networking Fargate capacity needs.
fn fargate_compute_resources() -> Value {
    json!({
        "type": "FARGATE",
        "maxvCpus": 8,
        "subnets": ["subnet-0b1c2d3e4f5a6b7c8"],
        "securityGroupIds": ["sg-0b1c2d3e4f5a6b7c8"],
    })
}

/// The compute resources a `MANAGED` EC2 environment is created with.
fn ec2_compute_resources() -> Value {
    json!({
        "type": "EC2",
        "maxvCpus": 8,
        "minvCpus": 0,
        "subnets": ["subnet-0b1c2d3e4f5a6b7c8"],
        "instanceRole": format!("arn:aws:iam::{ACCOUNT}:instance-profile/ecsInstanceRole"),
    })
}

/// One row of the compute environment table: what the create asks for, and whether a cluster of
/// Amazon ECS has to back what it creates.
struct Row {
    case: &'static str,
    /// The `CreateComputeEnvironment` body, minus the name this suite gives it.
    request: fn() -> Value,
    /// Whether Amazon ECS is the orchestration, and so whether a cluster of that service backs
    /// the environment and its ARN is reported.
    backed_by_a_cluster: bool,
}

/// The table both tests below create their environments from.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "managed over Fargate capacity",
            request: || json!({"type": "MANAGED", "computeResources": fargate_compute_resources()}),
            backed_by_a_cluster: true,
        },
        Row {
            case: "managed over EC2 capacity",
            request: || json!({"type": "MANAGED", "computeResources": ec2_compute_resources()}),
            backed_by_a_cluster: true,
        },
        Row {
            // The case `DescribeComputeEnvironments`' own documentation names: "if you're using
            // an unmanaged compute environment, you can use the DescribeComputeEnvironment
            // operation to determine the ecsClusterArn that you launch your Amazon ECS
            // container instances into".
            case: "unmanaged",
            request: || json!({"type": "UNMANAGED"}),
            backed_by_a_cluster: true,
        },
        Row {
            // The boundary of the rule: an environment whose jobs run as pods of an Amazon EKS
            // cluster has no cluster of Amazon ECS, and its `eksConfiguration` names the one it
            // does have.
            case: "managed and orchestrated with Amazon EKS",
            request: || {
                json!({
                    "type": "MANAGED",
                    "computeResources": ec2_compute_resources(),
                    "eksConfiguration": {
                        "eksClusterArn": format!("arn:aws:eks:{REGION}:{ACCOUNT}:cluster/analytics"),
                        "kubernetesNamespace": "batch-jobs",
                    },
                })
            },
            backed_by_a_cluster: false,
        },
    ]
}

/// Create every row's environment on `router`, answering the name each was created under.
async fn create_each(router: &MultiServiceRouter, rows: &[Row], prefix: &str) -> Vec<String> {
    let mut names = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let name = format!("{prefix}-{index}");
        let mut request = (row.request)();
        request["computeEnvironmentName"] = json!(name);
        batch(router, row.case, "/v1/createcomputeenvironment", &request).await;
        names.push(name);
    }
    names
}

/// RULE: every reader of a compute environment AWS Batch orchestrates with Amazon ECS reports
/// the SAME backing cluster, and that cluster is one Amazon ECS holds — it answers for the ARN
/// with no failure and lists it; an environment orchestrated with Amazon EKS reports none
/// through any reader.
#[tokio::test]
async fn every_reader_reports_the_one_cluster_amazon_ecs_holds_for_a_compute_environment() {
    let rows = rows();
    let router = router();
    // Every environment is created before any is waited on, so the spans the clock owes them
    // run together rather than one after another.
    let names = create_each(&router, &rows, "ce-backing").await;

    for (row, name) in rows.iter().zip(&names) {
        let case = row.case;
        let by_name = until_valid(&router, case, name).await;
        let environment_arn = member(case, &by_name, "/computeEnvironmentArn");
        let reported = by_name["ecsClusterArn"].as_str().map(str::to_string);

        // The SAME cluster through the other two readers: the environment named by its ARN, and
        // the unfiltered describe a client that names nothing gets.
        let by_arn = record_for(&router, case, &environment_arn).await;
        let unfiltered = record_in_the_unfiltered_describe(&router, case, name).await;
        for (reader, record) in [("named by its ARN", &by_arn), ("unfiltered", &unfiltered)] {
            assert_eq!(
                record["ecsClusterArn"].as_str().map(str::to_string),
                reported,
                "case {case:?}: the {reader} reader must report the cluster the reader by name \
                 reports ({reported:?}), got {record}"
            );
        }

        let Some(cluster_arn) = reported else {
            assert!(
                !row.backed_by_a_cluster,
                "case {case:?}: an environment Amazon ECS orchestrates must report its backing \
                 cluster, got {by_name}"
            );
            continue;
        };
        assert!(
            row.backed_by_a_cluster,
            "case {case:?}: an environment Amazon ECS does not orchestrate has no cluster of \
             that service, got {cluster_arn}"
        );

        // An Amazon ECS cluster ARN is `arn:aws:ecs:<region>:<account>:cluster/<name>`, and AWS
        // Batch names the cluster of an environment after the environment and its own id.
        let prefix = format!("arn:aws:ecs:{REGION}:{ACCOUNT}:cluster/");
        let cluster_name = cluster_arn
            .strip_prefix(&prefix)
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected a cluster ARN under {prefix}, got {cluster_arn}")
            })
            .to_string();
        let uuid = member(case, &by_name, "/uuid");
        assert_eq!(
            cluster_name,
            format!("{BATCH_CLUSTER_PREFIX}{name}-{uuid}"),
            "case {case:?}: expected the cluster AWS Batch names for this environment, got \
             {cluster_arn}"
        );

        // It EXISTS where Amazon ECS holds it: that service answers for the ARN, with no
        // failure — which is what makes the reported handle usable at all.
        let described = ecs(
            &router,
            case,
            "DescribeClusters",
            &json!({"clusters": [&cluster_arn]}),
        )
        .await;
        let failures = described["failures"].as_array().cloned().unwrap_or_default();
        assert!(
            failures.is_empty(),
            "case {case:?}: Amazon ECS must resolve the cluster the environment reports, got \
             failures {failures:?}"
        );
        let held = described["clusters"].as_array().cloned().unwrap_or_default();
        assert_eq!(
            held.len(),
            1,
            "case {case:?}: expected exactly one cluster for {cluster_arn}, got {described}"
        );
        assert_eq!(
            held[0]["clusterArn"].as_str(),
            Some(cluster_arn.as_str()),
            "case {case:?}: expected the cluster to carry the ARN the environment reports, got \
             {described}"
        );
        assert_eq!(
            held[0]["clusterName"].as_str(),
            Some(cluster_name.as_str()),
            "case {case:?}: expected the cluster to carry its own name, got {described}"
        );
        assert_eq!(
            held[0]["status"].as_str(),
            Some(ACTIVE),
            "case {case:?}: expected an {ACTIVE} cluster, got {described}"
        );

        // And a client that does not hold the ARN finds it the other way, in the listing of the
        // clusters that service holds.
        let listed = batch_clusters(&router, case).await;
        assert!(
            listed.contains(&cluster_arn),
            "case {case:?}: Amazon ECS must list the cluster backing the environment, listed \
             {listed:?}"
        );
    }
}

/// RULE: the backing cluster is provisioned ONCE, when the compute environment is created — so
/// each ECS-orchestrated environment has its own, reading an environment provisions no second
/// one, and an update to the environment leaves the cluster it already reports in place.
#[tokio::test]
async fn the_backing_cluster_is_provisioned_once_per_compute_environment() {
    let rows = rows();
    let router = router();
    let names = create_each(&router, &rows, "ce-once").await;

    // What each environment reports once it is settled, and what Amazon ECS therefore has to
    // hold: one cluster per environment this service orchestrates with it.
    let mut reported: Vec<(String, Option<String>)> = Vec::new();
    for (row, name) in rows.iter().zip(&names) {
        let record = until_valid(&router, row.case, name).await;
        reported.push((
            name.clone(),
            record["ecsClusterArn"].as_str().map(str::to_string),
        ));
    }
    let clusters: BTreeSet<String> = reported
        .iter()
        .filter_map(|(_, cluster)| cluster.clone())
        .collect();
    let backed = rows.iter().filter(|row| row.backed_by_a_cluster).count();
    assert_eq!(
        clusters.len(),
        backed,
        "case per-environment clusters: expected {backed} distinct clusters, one for each \
         environment Amazon ECS orchestrates, got {clusters:?}"
    );
    assert_eq!(
        batch_clusters(&router, "per-environment clusters").await,
        clusters,
        "case per-environment clusters: Amazon ECS must hold exactly the clusters the \
         environments report, got {clusters:?}"
    );

    // READING an environment reports the cluster it already has; it neither changes the ARN nor
    // provisions another cluster, however often a client looks.
    for round in 0..3 {
        let case = format!("read again, round {round}");
        for (name, cluster) in &reported {
            let again = record_for(&router, &case, name).await;
            assert_eq!(
                again["ecsClusterArn"].as_str().map(str::to_string),
                *cluster,
                "case {case:?}: {name} must still report {cluster:?}, got {again}"
            );
        }
        assert_eq!(
            batch_clusters(&router, &case).await,
            clusters,
            "case {case:?}: reading an environment must provision no further cluster, held \
             {clusters:?} before"
        );
    }

    // An UPDATE to the environment is not a re-provisioning either: the cluster the environment
    // already reports is the one it reports afterwards.
    let (updated, before) = reported
        .first()
        .cloned()
        .expect("the table creates at least one environment");
    let case = "updated";
    batch(
        &router,
        case,
        "/v1/updatecomputeenvironment",
        &json!({"computeEnvironment": updated, "state": "DISABLED"}),
    )
    .await;
    let after = until_valid(&router, case, &updated).await;
    assert_eq!(
        after["ecsClusterArn"].as_str().map(str::to_string),
        before,
        "case {case:?}: an updated environment keeps the cluster it was created with \
         ({before:?}), got {after}"
    );
    assert_eq!(
        batch_clusters(&router, case).await,
        clusters,
        "case {case:?}: updating an environment must provision no further cluster, held \
         {clusters:?} before"
    );
}
