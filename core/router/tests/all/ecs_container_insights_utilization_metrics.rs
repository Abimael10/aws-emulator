// SPDX-License-Identifier: CC0-1.0

//! The Container Insights UTILIZATION a cluster publishes: what its running tasks are
//! measured USING, beside what they reserve.
//!
//! RULE: a cluster `Cluster$settings` turns Container Insights ON for (`ClusterSetting$name`
//! `containerInsights`, whose `enabled`/`enhanced` values "turn on CloudWatch Container
//! Insights ... for a cluster") publishes, into the `ECS/ContainerInsights` namespace, what
//! the containers of its running tasks are USING — `CpuUtilized` in CPU units and
//! `MemoryUtilized` in MiB — beside what those tasks' definitions RESERVE (`CpuReserved`,
//! `MemoryReserved`), dimensioned by the cluster and, for a service's own tasks, by the
//! cluster-and-service pair. Each utilization figure is a measurement of the work those
//! containers really do, so a service whose container keeps COMPUTING reports more CPU in use
//! than one whose container only WAITS, and neither is reported above the reservation its
//! definition states. A cluster the setting is OFF for publishes none of them at all.
//!
//! Two crates have to agree for that (Amazon ECS measures the containers of its tasks and
//! publishes, CloudWatch stores the series and reads it back), and the `awsvpc` placements
//! resolve a subnet in a third (Amazon EC2 holds the network), so the suite lives here.
//!
//! The rows differ along what the rule generalises over: the three values the setting takes
//! (`enabled`, `enhanced`, `disabled`), the work the container does (a program that computes
//! without pausing and one that waits) and the task size the figures are measured against.
//!
//! Nothing here reaches into the ECS store: the clusters and services are created through the
//! ECS front door, and every number is read back through CloudWatch's own
//! `GetMetricStatistics` — the same store a client's `PutMetricData` writes into.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The region every request below is signed for.
const REGION: &str = "us-west-2";

/// The namespace and the four metric names this suite reads, with the dimensions they carry —
/// spelled out here rather than imported, so a wrong constant in the implementation cannot
/// make a test pass. Their source is the CloudWatch User Guide's "Amazon ECS Container
/// Insights metrics" table.
const NAMESPACE: &str = "ECS/ContainerInsights";
const CPU_UTILIZED: &str = "CpuUtilized";
const CPU_RESERVED: &str = "CpuReserved";
const MEMORY_UTILIZED: &str = "MemoryUtilized";
const MEMORY_RESERVED: &str = "MemoryReserved";
const CLUSTER_DIMENSION: &str = "ClusterName";
const SERVICE_DIMENSION: &str = "ServiceName";

/// The network the `awsvpc` placements name.
const VPC: &str = "vpc-0123456789abcdef0";
const SUBNET: &str = "subnet-0123456789abcdef0";

/// The image every container names, and the two programs a row's container runs. Both stay up
/// — so a task that is running is still running while its series is read — and they differ in
/// what they DO: one computes at every step, the other waits.
const IMAGE: &str = "public.ecr.aws/nginx/nginx:latest";
const COMPUTES: &str = "i=0; while true; do i=$((i+1)); done";
const WAITS: &str = "while true; do sleep 1; done";

/// How often a poll re-reads what it is waiting on.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// How long one wait keeps trying before it gives up. Each wait gets this budget of its OWN:
/// Amazon ECS creates and attaches the interface an `awsvpc` task is allocated one documented
/// step at a time and only then starts it, so a correct run is far inside this and a broken
/// one is reported promptly. Nothing here asserts on elapsed time.
const DEADLINE: Duration = Duration::from_secs(120);

/// How many further looks an assertion of EMPTINESS takes. Together with [`POLL_INTERVAL`] a
/// window far longer than the spacing a publishing cluster's series takes, so an emptiness a
/// row asserts is one that HELD rather than one read too early.
const QUIET_LOOKS: usize = 20;

/// One row: the cluster and the service it holds, the setting the cluster is created with, the
/// program its container runs, the size its revision reserves, and what it therefore
/// publishes.
struct Row {
    case: &'static str,
    cluster: &'static str,
    /// The `containerInsights` value the cluster is created with.
    setting: &'static str,
    service: &'static str,
    /// The shell program the container runs.
    program: &'static str,
    /// `TaskDefinition$cpu` in CPU units and `$memory` in MiB — what the row's figures are
    /// measured against.
    cpu: i64,
    memory: i64,
    /// Whether the cluster publishes into the namespace at all.
    publishes: bool,
    /// Whether its container is expected to be measured USING the CPU it reserved.
    busy: bool,
}

const ROWS: &[Row] = &[
    Row {
        case: "an insights-enabled cluster whose container computes",
        cluster: "insights-utilization-enabled",
        setting: "enabled",
        service: "orders",
        program: COMPUTES,
        cpu: 256,
        memory: 512,
        publishes: true,
        busy: true,
    },
    Row {
        case: "an enhanced-observability cluster whose container waits",
        cluster: "insights-utilization-enhanced",
        setting: "enhanced",
        service: "checkout",
        program: WAITS,
        cpu: 1024,
        memory: 2048,
        publishes: true,
        busy: false,
    },
    Row {
        case: "a cluster Container Insights is disabled for, whose container computes",
        cluster: "insights-utilization-disabled",
        setting: "disabled",
        service: "billing",
        program: COMPUTES,
        cpu: 512,
        memory: 1024,
        publishes: false,
        busy: false,
    },
];

/// One emulator carrying Amazon ECS and CloudWatch — which owns the metric store the series is
/// published into — over ONE shared engine, exactly as the production router builds them, with
/// the network the `awsvpc` placements name held in the account's Amazon EC2 store.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    crate::ec2_network::register_network(&engine, VPC, &[SUBNET], &format!("{REGION}a"));
    // A task placed in this subnet reaches the registry its containers' image comes from, so
    // no task of these services is stopped before it starts.
    crate::ec2_network::register_internet_route(&engine, VPC);
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_ecs_amazonec2containerservicev20141113::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_cloudwatch_graniteserviceversion20100801::EmulatedService::new(
            engine,
        ),
    );
    router
}

/// One awsJson call that must succeed, answered with the parsed body.
async fn rpc(
    router: &MultiServiceRouter,
    host: &str,
    signing_name: &str,
    target_prefix: &str,
    content_type: &str,
    operation: &str,
    body: Value,
) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{host}.{REGION}.amazonaws.com"))
        .header("content-type", content_type)
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}\
                 /{signing_name}/aws4_request, SignedHeaders=host;x-amz-target, Signature=00"
            ),
        )
        .body(Bytes::from(
            serde_json::to_vec(&body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed: Value = if payload.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&payload)
            .unwrap_or_else(|_| json!({ "__unparsed": String::from_utf8_lossy(&payload) }))
    };
    assert_eq!(
        status, 200,
        "expected {operation} to answer 200, got {status} with {parsed}"
    );
    parsed
}

/// One ECS call, the way a client makes it.
async fn ecs(router: &MultiServiceRouter, operation: &str, body: Value) -> Value {
    rpc(
        router,
        "ecs",
        "ecs",
        "AmazonEC2ContainerServiceV20141113",
        "application/x-amz-json-1.1",
        operation,
        body,
    )
    .await
}

/// One CloudWatch call, the way a client makes it. CloudWatch's `sigv4Name` is `monitoring`.
async fn cloudwatch(router: &MultiServiceRouter, operation: &str, body: Value) -> Value {
    rpc(
        router,
        "monitoring",
        "monitoring",
        "GraniteServiceVersion20100801",
        "application/x-amz-json-1.0",
        operation,
        body,
    )
    .await
}

/// Epoch seconds, for the window a statistics read asks about.
fn epoch_seconds() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

/// Every datapoint CloudWatch holds for `metric` on exactly `dimensions`.
async fn datapoints(router: &MultiServiceRouter, metric: &str, dimensions: Value) -> Vec<Value> {
    let now = epoch_seconds();
    let stats = cloudwatch(
        router,
        "GetMetricStatistics",
        json!({
            "Namespace": NAMESPACE,
            "MetricName": metric,
            "Dimensions": dimensions,
            "StartTime": now - 1_800.0,
            "EndTime": now + 600.0,
            "Period": 60,
            "Statistics": ["Minimum", "Maximum", "Average", "SampleCount"],
        }),
    )
    .await;
    stats["Datapoints"].as_array().cloned().unwrap_or_default()
}

/// The dimension pair that names one row's service, and the cluster-only set its cluster's own
/// figures carry.
fn service_dimensions(row: &Row) -> Value {
    json!([
        { "Name": CLUSTER_DIMENSION, "Value": row.cluster },
        { "Name": SERVICE_DIMENSION, "Value": row.service },
    ])
}

fn cluster_dimensions(row: &Row) -> Value {
    json!([{ "Name": CLUSTER_DIMENSION, "Value": row.cluster }])
}

/// The largest `statistic` any datapoint reports, and `None` for a series that holds none.
fn highest(points: &[Value], statistic: &str) -> Option<f64> {
    points
        .iter()
        .filter_map(|point| point[statistic].as_f64())
        .fold(None, |held: Option<f64>, value| {
            Some(held.map_or(value, |held| held.max(value)))
        })
}

/// Create one row's cluster with the setting it asks for, the Fargate revision its service
/// runs, and the service itself.
async fn create_service(router: &MultiServiceRouter, row: &Row) {
    ecs(
        router,
        "CreateCluster",
        json!({
            "clusterName": row.cluster,
            "settings": [{ "name": "containerInsights", "value": row.setting }],
        }),
    )
    .await;
    ecs(
        router,
        "RegisterTaskDefinition",
        json!({
            "family": row.service,
            "networkMode": "awsvpc",
            "requiresCompatibilities": ["FARGATE"],
            "cpu": row.cpu.to_string(),
            "memory": row.memory.to_string(),
            "containerDefinitions": [{
                "name": "app",
                "image": IMAGE,
                "essential": true,
                "command": ["sh", "-c", row.program],
            }],
        }),
    )
    .await;
    ecs(
        router,
        "CreateService",
        json!({
            "cluster": row.cluster,
            "serviceName": row.service,
            "taskDefinition": row.service,
            "desiredCount": 1,
            "launchType": "FARGATE",
            "networkConfiguration": {
                "awsvpcConfiguration": {
                    "subnets": [SUBNET],
                    "securityGroups": [],
                    "assignPublicIp": "ENABLED",
                }
            },
        }),
    )
    .await;
}

/// Wait until `DescribeServices` reports the row's service holding its task `RUNNING` — the
/// state its utilization is measured in.
async fn converged(router: &MultiServiceRouter, row: &Row) {
    let case = row.case;
    let deadline = Instant::now() + DEADLINE;
    loop {
        let described = ecs(
            router,
            "DescribeServices",
            json!({ "cluster": row.cluster, "services": [row.service] }),
        )
        .await;
        let service = described["services"][0].clone();
        if service["runningCount"].as_i64() == Some(1) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the service to hold a running task, got {service}"
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Wait until some datapoint of `metric` on `dimensions` reports a POSITIVE average, and
/// answer it.
///
/// What a MEASUREMENT of a container that is doing work looks like: the figure is read off the
/// run this emulator performs for that container, so it is positive once that run has
/// performed any of its program.
async fn measured_positive(
    router: &MultiServiceRouter,
    row: &Row,
    metric: &str,
    dimensions: Value,
) -> f64 {
    let case = row.case;
    let deadline = Instant::now() + DEADLINE;
    loop {
        let points = datapoints(router, metric, dimensions.clone()).await;
        if let Some(average) = highest(&points, "Average").filter(|value| *value > 0.0) {
            return average;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected a positive {metric} average on {dimensions}, got {points:?}"
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Wait until `metric` on `dimensions` reports exactly `expected` — the reservation the row's
/// revision was registered with.
async fn reported_reservation(
    router: &MultiServiceRouter,
    row: &Row,
    metric: &str,
    dimensions: Value,
    expected: f64,
) {
    let case = row.case;
    let deadline = Instant::now() + DEADLINE;
    loop {
        let points = datapoints(router, metric, dimensions.clone()).await;
        if highest(&points, "Maximum") == Some(expected) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {metric} to report {expected} on {dimensions}, got \
             {points:?}"
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}

#[tokio::test]
async fn an_insights_cluster_publishes_what_its_tasks_use_and_what_they_reserve() {
    let router = emulator();
    // Every row's service is created up front, so the clock-owed placements of all of them
    // fall due at once rather than one row's wait after another's.
    for row in ROWS {
        create_service(&router, row).await;
    }

    let mut computing: Option<f64> = None;
    let mut waiting: Option<f64> = None;
    for row in ROWS {
        let case = row.case;
        converged(&router, row).await;
        if !row.publishes {
            // A cluster the setting is OFF for publishes nothing in this namespace, however
            // busy its tasks are — asserted over a WINDOW, because a datapoint the clock owed
            // would arrive a moment later.
            for _ in 0..QUIET_LOOKS {
                ::tokio::time::sleep(POLL_INTERVAL).await;
                for metric in [CPU_UTILIZED, CPU_RESERVED, MEMORY_UTILIZED, MEMORY_RESERVED] {
                    for dimensions in [service_dimensions(row), cluster_dimensions(row)] {
                        let points = datapoints(&router, metric, dimensions.clone()).await;
                        assert!(
                            points.is_empty(),
                            "case {case:?}: expected no {metric} datapoint on {dimensions}, \
                             got {points:?}"
                        );
                    }
                }
            }
            continue;
        }

        // What the service's own tasks are USING, on the dimension pair a per-service alarm
        // and dashboard select: a measurement, so positive once the container has performed
        // any of its program, and never above what the revision reserves.
        let cpu = measured_positive(&router, row, CPU_UTILIZED, service_dimensions(row)).await;
        let memory =
            measured_positive(&router, row, MEMORY_UTILIZED, service_dimensions(row)).await;
        let reserved_cpu = row.cpu as f64;
        let reserved_memory = row.memory as f64;
        assert!(
            cpu <= reserved_cpu,
            "case {case:?}: expected {CPU_UTILIZED} at most the reserved {reserved_cpu}, got \
             {cpu}"
        );
        assert!(
            memory <= reserved_memory,
            "case {case:?}: expected {MEMORY_UTILIZED} at most the reserved \
             {reserved_memory}, got {memory}"
        );
        if row.busy {
            computing = Some(computing.map_or(cpu, |held: f64| held.max(cpu)));
        } else {
            waiting = Some(waiting.map_or(cpu, |held: f64| held.max(cpu)));
        }

        // ...beside what those tasks RESERVE, which is what the revision was registered with
        // — reported for the service and for the cluster the one task runs in.
        for dimensions in [service_dimensions(row), cluster_dimensions(row)] {
            reported_reservation(&router, row, CPU_RESERVED, dimensions.clone(), reserved_cpu)
                .await;
            reported_reservation(
                &router,
                row,
                MEMORY_RESERVED,
                dimensions,
                reserved_memory,
            )
            .await;
        }

        // The cluster's own figures are measured off the same tasks, so the cluster holding
        // this one service is using something too.
        measured_positive(&router, row, CPU_UTILIZED, cluster_dimensions(row)).await;
    }

    // The figures are a MEASUREMENT of what each container does: the one that computes at
    // every step is using more CPU than the one that sits in a wait.
    let computing = computing.expect("a row whose container computes");
    let waiting = waiting.expect("a row whose container waits");
    assert!(
        computing > waiting,
        "expected the computing container's {CPU_UTILIZED} ({computing}) to exceed the \
         waiting one's ({waiting})"
    );
}
