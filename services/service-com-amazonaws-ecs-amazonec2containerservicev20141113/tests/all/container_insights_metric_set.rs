// SPDX-License-Identifier: CC0-1.0

//! The WHOLE Container Insights metric set a cluster publishes, and the performance log
//! events it publishes beside it.
//!
//! The rule under test:
//!
//! > A cluster `Cluster$settings` reports CloudWatch Container Insights as ON for publishes
//! > the Container Insights metric set for itself — every metric of the namespace the
//! > collection can measure for its records and for the containers of their tasks,
//! > dimensioned by the cluster, and by the service beside it for a service-level metric —
//! > and writes the same figures as performance log events into the log group Container
//! > Insights keeps for that cluster. A cluster it is OFF for publishes no datapoint and has
//! > no such log group. A figure the collection has no quantity for is published by nothing:
//! > the ephemeral-storage pair exists only for tasks whose placement gave them ephemeral
//! > storage, while a byte COUNTER is a measurement whatever it stands at.
//!
//! The rows span what the rule generalises over: the setting's values (`enabled`, `enhanced`,
//! `disabled`) and the placement of the cluster's tasks — a Fargate placement, which gives a
//! task ephemeral storage, beside an EC2 one, which gives it none.
//!
//! Nothing here reaches into a state struct: the metrics are read back through CloudWatch's
//! OWN `ListMetrics` and `GetMetricStatistics` and the log events through CloudWatch Logs'
//! OWN `DescribeLogGroups` and `FilterLogEvents`, which are the same stores a client's
//! `PutMetricData` and `PutLogEvents` write into.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{
    awsvpc_network_configuration, create_cluster, list, ok, register_capacity, seed_awsvpc_subnets,
    ACCOUNT, SIGNED_REGION,
};

/// The namespace Amazon ECS publishes Container Insights metrics in, spelled out here rather
/// than imported, so a wrong constant in the implementation cannot make a test pass.
const NAMESPACE: &str = "ECS/ContainerInsights";

/// The dimension every Container Insights metric of a cluster carries, the one a service-level
/// metric carries beside it, and the one a task-level metric carries beside it.
const CLUSTER_DIMENSION: &str = "ClusterName";
const SERVICE_DIMENSION: &str = "ServiceName";
const FAMILY_DIMENSION: &str = "TaskDefinitionFamily";

/// The CLUSTER-level metric set, each with the unit it is published in.
///
/// SOURCE: the CloudWatch User Guide's "Amazon ECS Container Insights metrics" table, the
/// rows whose dimension set is `ClusterName` alone. The units are the ones each quantity is
/// measured in: a count of records, a number of CPU units (which the `StandardUnit` type has
/// no name for, so the value it has for a measurement with no unit of its own), the MiB a
/// task definition sizes memory in, bytes for the traffic and storage counters, and the
/// gigabytes a task's ephemeral storage is sized in — a different unit from the memory
/// family, which is the point.
const CLUSTER_METRICS: [(&str, &str); 13] = [
    ("ContainerInstanceCount", "Count"),
    ("CpuReserved", "None"),
    ("CpuUtilized", "None"),
    ("EphemeralStorageReserved", "Gigabytes"),
    ("EphemeralStorageUtilized", "Gigabytes"),
    ("MemoryReserved", "Megabytes"),
    ("MemoryUtilized", "Megabytes"),
    ("NetworkRxBytes", "Bytes"),
    ("NetworkTxBytes", "Bytes"),
    ("ServiceCount", "Count"),
    ("StorageReadBytes", "Bytes"),
    ("StorageWriteBytes", "Bytes"),
    ("TaskCount", "Count"),
];

/// The two of them that state an amount of EPHEMERAL STORAGE, which a task whose placement
/// gave it none has no quantity of — so neither is published for such a cluster.
const EPHEMERAL_METRICS: [&str; 2] = ["EphemeralStorageReserved", "EphemeralStorageUtilized"];

/// SERVICE-level metrics this suite reads back on the cluster-and-service dimension pair: the
/// count only a service has, and one figure of each kind the collection measures.
const SERVICE_METRICS: [&str; 4] = [
    "RunningTaskCount",
    "CpuUtilized",
    "NetworkRxBytes",
    "StorageWriteBytes",
];

/// TASK-level metrics this suite reads back on the cluster-and-family dimension pair: the
/// figures a collection measures for the tasks of one task definition family.
///
/// SOURCE: the same table, the rows whose dimension set is `ClusterName, TaskDefinitionFamily`.
const FAMILY_METRICS: [&str; 6] = [
    "CpuUtilized",
    "CpuReserved",
    "MemoryUtilized",
    "MemoryReserved",
    "NetworkTxBytes",
    "StorageReadBytes",
];

/// The image every container below names, and a program that stays up, so a task that is
/// running is running while the series are read.
const IMAGE: &str = "public.ecr.aws/nginx/nginx:latest";
const STAYS_UP: &str = "while true; do sleep 1; done";

/// How often a poll re-reads, and how long one keeps trying before it gives up. Generous: the
/// series is published as the clock moves the task it measures, this suite runs beside every
/// other test of the workspace, and every poll stops the instant its own condition holds.
const POLL_INTERVAL: Duration = Duration::from_millis(50);
const POLL_BUDGET: Duration = Duration::from_secs(180);

/// How many further looks a row that expects NO series takes. Together with [`POLL_INTERVAL`]
/// a window far longer than the spacing a published series takes, so an emptiness a row
/// asserts is one that HELD rather than one read too early.
const QUIET_LOOKS: usize = 40;

/// One row: the cluster, the `containerInsights` value it is created with, whether its tasks
/// are placed where they are given ephemeral storage, and whether it publishes at all.
struct Row {
    case: &'static str,
    cluster: &'static str,
    family: &'static str,
    /// The setting the create names, and `None` for a cluster that sets nothing.
    setting: Option<&'static str>,
    /// A Fargate placement, which gives the task ephemeral storage, as against an EC2 one.
    fargate: bool,
    /// Whether Container Insights is ON for it.
    publishes: bool,
}

const ROWS: &[Row] = &[
    Row {
        case: "insights enabled, tasks placed on Fargate",
        cluster: "insights-set-fargate",
        family: "insightssetfargate",
        setting: Some("enabled"),
        fargate: true,
        publishes: true,
    },
    Row {
        case: "insights enabled with enhanced observability, tasks placed on EC2",
        cluster: "insights-set-ec2",
        family: "insightssetec2",
        setting: Some("enhanced"),
        fargate: false,
        publishes: true,
    },
    Row {
        case: "insights disabled",
        cluster: "insights-set-off",
        family: "insightssetoff",
        setting: Some("disabled"),
        fargate: true,
        publishes: false,
    },
];

/// An emulator with Amazon ECS, CloudWatch — which owns the metric store the series are
/// published into — and CloudWatch Logs — which owns the log group the performance log events
/// are written to — on ONE shared engine, holding the Amazon VPC subnet an `awsvpc` placement
/// of this suite names.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    seed_awsvpc_subnets(&engine);
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_ecs_amazonec2containerservicev20141113::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_cloudwatch_graniteserviceversion20100801::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(engine),
    );
    router
}

/// One CloudWatch call, the way a client makes it.
async fn cloudwatch(router: &MultiServiceRouter, operation: &str, body: Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("monitoring.{SIGNED_REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header(
            "x-amz-target",
            format!("GraniteServiceVersion20100801.{operation}"),
        )
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDECSTEST/20260101/{SIGNED_REGION}\
                 /monitoring/aws4_request, SignedHeaders=host;x-amz-target, Signature=00"
            ),
        )
        .body(bytes::Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed: Value = if payload.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&payload).unwrap_or(Value::Null)
    };
    assert_eq!(
        status, 200,
        "expected CloudWatch {operation} to answer 200, got {status} with {parsed}"
    );
    parsed
}

/// One CloudWatch Logs call, the way a client makes it.
async fn logs(router: &MultiServiceRouter, operation: &str, body: Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("logs.{SIGNED_REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("Logs_20140328.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDECSTEST/20260101/{SIGNED_REGION}\
                 /logs/aws4_request, SignedHeaders=host;x-amz-target, Signature=00"
            ),
        )
        .body(bytes::Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed: Value = if payload.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&payload).unwrap_or(Value::Null)
    };
    assert_eq!(
        status, 200,
        "expected CloudWatch Logs {operation} to answer 200, got {status} with {parsed}"
    );
    parsed
}

/// Epoch seconds, for the window a statistics read asks about.
fn epoch_seconds() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

/// Every datapoint CloudWatch holds for one series of `dimensions`.
async fn datapoints(router: &MultiServiceRouter, dimensions: Value, metric: &str) -> Vec<Value> {
    let now = epoch_seconds();
    let stats = cloudwatch(
        router,
        "GetMetricStatistics",
        json!({
            "Namespace": NAMESPACE,
            "MetricName": metric,
            "Dimensions": dimensions,
            "StartTime": now - 1200.0,
            "EndTime": now + 1200.0,
            "Period": 60,
            "Statistics": ["Maximum", "Sum", "SampleCount"],
        }),
    )
    .await;
    stats["Datapoints"].as_array().cloned().unwrap_or_default()
}

/// The dimension list a CLUSTER-level series carries, and the pair a SERVICE-level one does.
fn cluster_dimensions(cluster: &str) -> Value {
    json!([{ "Name": CLUSTER_DIMENSION, "Value": cluster }])
}

fn service_dimensions(cluster: &str, service: &str) -> Value {
    json!([
        { "Name": CLUSTER_DIMENSION, "Value": cluster },
        { "Name": SERVICE_DIMENSION, "Value": service },
    ])
}

/// And the pair a TASK-level one carries: the cluster and the task definition family.
fn family_dimensions(cluster: &str, family: &str) -> Value {
    json!([
        { "Name": CLUSTER_DIMENSION, "Value": cluster },
        { "Name": FAMILY_DIMENSION, "Value": family },
    ])
}

/// The metric names CloudWatch lists for one dimension set of the namespace.
async fn listed_metrics(router: &MultiServiceRouter, dimensions: Value) -> Vec<String> {
    let listed = cloudwatch(
        router,
        "ListMetrics",
        json!({ "Namespace": NAMESPACE, "Dimensions": dimensions }),
    )
    .await;
    listed["Metrics"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|metric| metric["MetricName"].as_str().map(str::to_string))
        .collect()
}

/// Create the cluster one row asks for, with the capacity an EC2 placement needs, and a
/// service of `family` that keeps one task running in it.
///
/// A service rather than a standalone task: "the service scheduler ... replaces tasks that
/// fail or stop" (`CreateService`), so the task it places keeps running for as long as the row
/// reads.
async fn set_up(router: &MultiServiceRouter, row: &Row) {
    let mut request = json!({ "clusterName": row.cluster });
    if let Some(value) = row.setting {
        request["settings"] = json!([{ "name": "containerInsights", "value": value }]);
    }
    ok(router, "CreateCluster", request, row.case).await;
    let mut definition = json!({
        "family": row.family,
        "containerDefinitions": [{
            "name": "app",
            "image": IMAGE,
            // A container that reserves CPU and memory, so the cluster has quantities of each
            // to report its tasks' usage against (`ContainerDefinition$cpu`/`$memory`).
            "cpu": 128,
            "memory": 128,
            "essential": true,
            "command": ["sh", "-c", STAYS_UP],
        }],
    });
    let mut service = json!({
        "cluster": row.cluster,
        "serviceName": row.family,
        "taskDefinition": row.family,
        "desiredCount": 1,
    });
    if row.fargate {
        // A Fargate placement gives the task its ephemeral storage and its own elastic
        // network interface, which is why such a revision declares a task size and the
        // `awsvpc` network mode.
        definition["requiresCompatibilities"] = json!(["FARGATE"]);
        definition["networkMode"] = json!("awsvpc");
        definition["cpu"] = json!("256");
        definition["memory"] = json!("512");
        definition["executionRoleArn"] = json!(format!("arn:aws:iam::{ACCOUNT}:role/insights"));
        service["launchType"] = json!("FARGATE");
        service["networkConfiguration"] = awsvpc_network_configuration();
    } else {
        register_capacity(router, row.cluster, row.case).await;
        service["launchType"] = json!("EC2");
    }
    ok(router, "RegisterTaskDefinition", definition, row.case).await;
    ok(router, "CreateService", service, row.case).await;
}

/// The names of the log groups CloudWatch Logs holds under the Container Insights prefix.
async fn insights_log_groups(router: &MultiServiceRouter) -> Vec<String> {
    let body = logs(
        router,
        "DescribeLogGroups",
        json!({ "logGroupNamePrefix": "/aws/ecs/containerinsights/" }),
    )
    .await;
    body["logGroups"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|group| group["logGroupName"].as_str().map(str::to_string))
        .collect()
}

/// The messages of the log events one group holds, each with the stream it was read from,
/// parsed as the documents they are.
async fn log_documents(router: &MultiServiceRouter, log_group: &str) -> Vec<(String, Value)> {
    let body = logs(
        router,
        "FilterLogEvents",
        json!({ "logGroupName": log_group }),
    )
    .await;
    body["events"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|event| {
            let stream = event["logStreamName"].as_str()?.to_string();
            let document = serde_json::from_str::<Value>(event["message"].as_str()?).ok()?;
            Some((stream, document))
        })
        .collect()
}

/// The performance log group of one cluster, and the stream its own collection travels — by
/// the naming Container Insights keeps them under, spelled out here rather than imported.
fn performance_group(cluster: &str) -> String {
    format!("/aws/ecs/containerinsights/{cluster}/performance")
}

fn cluster_telemetry_stream(cluster: &str) -> String {
    format!("ClusterTelemetry-{cluster}")
}

/// What the stream of one TASK's own collection is named: a task Fargate runs is named after
/// the task, and a task on a container instance travels the stream that instance's agent
/// writes, which is named after the instance.
const FARGATE_TELEMETRY_PREFIX: &str = "FargateTelemetry-";

/// The id of the container instance the task of one row runs on, as `DescribeTasks` reports
/// the instance's ARN: the last segment of that ARN, which is how the instance is named.
async fn container_instance_of(router: &MultiServiceRouter, row: &Row) -> String {
    let listed = ok(
        router,
        "ListTasks",
        json!({ "cluster": row.cluster, "desiredStatus": "RUNNING" }),
        row.case,
    )
    .await;
    let arns = list(&listed, "taskArns", row.case);
    let described = ok(
        router,
        "DescribeTasks",
        json!({ "cluster": row.cluster, "tasks": arns }),
        row.case,
    )
    .await;
    let tasks = list(&described, "tasks", row.case);
    let instance = tasks
        .iter()
        .find_map(|task| task["containerInstanceArn"].as_str())
        .unwrap_or_else(|| {
            panic!(
                "case {:?}: expected a running task on a container instance, got {described}",
                row.case
            )
        });
    instance
        .rsplit('/')
        .next()
        .unwrap_or(instance)
        .to_string()
}

/// RULE: a cluster Container Insights is on for publishes the whole metric set it can measure,
/// and the performance log events beside it; one it is off for publishes neither.
#[tokio::test]
async fn container_insights_publishes_the_whole_metric_set_and_the_performance_log() {
    let router = emulator();
    for row in ROWS {
        set_up(&router, row).await;
    }

    for row in ROWS {
        let case = row.case;
        // What this cluster's placement lets the collection measure: every metric of the set,
        // less the ephemeral-storage pair for tasks whose placement gave them none.
        let expected: Vec<(&str, &str)> = CLUSTER_METRICS
            .iter()
            .copied()
            .filter(|(metric, _)| row.fargate || !EPHEMERAL_METRICS.contains(metric))
            .collect();
        if !row.publishes {
            // A cluster the setting is OFF for publishes nothing at all — asserted over a
            // WINDOW, because a datapoint the clock owes would arrive a moment later.
            for _ in 0..QUIET_LOOKS {
                tokio::time::sleep(POLL_INTERVAL).await;
                let listed = listed_metrics(&router, cluster_dimensions(row.cluster)).await;
                assert!(
                    listed.is_empty(),
                    "case {case:?}: expected no series for the cluster, got {listed:?}"
                );
                let groups = insights_log_groups(&router).await;
                assert!(
                    !groups.contains(&performance_group(row.cluster)),
                    "case {case:?}: expected no performance log group, got {groups:?}"
                );
            }
            continue;
        }
        // Every metric the collection can measure for this cluster arrives, which is what a
        // dashboard over the namespace selects.
        let deadline = Instant::now() + POLL_BUDGET;
        let listed = loop {
            let listed = listed_metrics(&router, cluster_dimensions(row.cluster)).await;
            if expected
                .iter()
                .all(|(metric, _)| listed.iter().any(|held| held == metric))
            {
                break listed;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: expected the whole metric set, got {listed:?}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        };
        // ...and a metric the collection has no quantity for is published by nothing.
        if !row.fargate {
            for metric in EPHEMERAL_METRICS {
                assert!(
                    !listed.iter().any(|held| held == metric),
                    "case {case:?}: expected no {metric} for a placement given no ephemeral \
                     storage, got {listed:?}"
                );
            }
        }
        // Each of them holds a datapoint, in the unit that quantity is measured in.
        for (metric, unit) in &expected {
            let points = datapoints(&router, cluster_dimensions(row.cluster), metric).await;
            let first = points.first().unwrap_or_else(|| {
                panic!("case {case:?}: expected a {metric} datapoint, got {points:?}")
            });
            assert_eq!(
                first["Unit"],
                json!(unit),
                "case {case:?}: expected {metric} in {unit}, got {first}"
            );
        }
        // The service-level series carry the service dimension beside the cluster's, which is
        // what a per-service alarm selects; the task-level ones carry the task definition
        // family beside it, which is what a dashboard over one workload selects.
        for (what, dimensions, wanted) in [
            (
                "the service's own series",
                service_dimensions(row.cluster, row.family),
                SERVICE_METRICS.to_vec(),
            ),
            (
                "the task definition family's own series",
                family_dimensions(row.cluster, row.family),
                FAMILY_METRICS.to_vec(),
            ),
        ] {
            let deadline = Instant::now() + POLL_BUDGET;
            loop {
                let listed = listed_metrics(&router, dimensions.clone()).await;
                if wanted
                    .iter()
                    .all(|metric| listed.iter().any(|held| held == metric))
                {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "case {case:?}: expected {what}, got {listed:?}"
                );
                tokio::time::sleep(POLL_INTERVAL).await;
            }
        }
        // And the collection's figures are in the cluster's performance log group, as the
        // documents a consumer of that group reads: a CLUSTER document carrying every figure
        // the series above carry and a SERVICE document, both in the cluster's own telemetry
        // stream, and a TASK document for the task of the service in the stream of that task.
        //
        // The document waited for is one taken while the cluster's task was RUNNING — the
        // earliest collections were taken before it was, and a collection reports no quantity
        // of CPU for a cluster holding no running task to have reserved any.
        let log_group = performance_group(row.cluster);
        let cluster_stream = cluster_telemetry_stream(row.cluster);
        let log_deadline = Instant::now() + POLL_BUDGET;
        let (cluster_document, task_stream) = loop {
            let documents = log_documents(&router, &log_group).await;
            let complete = documents.iter().find(|(stream, document)| {
                *stream == cluster_stream
                    && document["Type"] == json!("Cluster")
                    && expected
                        .iter()
                        .all(|(metric, _)| document.get(metric).is_some_and(Value::is_number))
            });
            let service_documents = documents
                .iter()
                .any(|(stream, document)| {
                    *stream == cluster_stream && document["Type"] == json!("Service")
                });
            let task_document = documents.iter().find(|(_, document)| {
                document["Type"] == json!("Task")
                    && document[FAMILY_DIMENSION] == json!(row.family)
                    && document["TaskId"].is_string()
            });
            match (complete, task_document) {
                (Some((_, cluster_document)), Some((task_stream, _))) if service_documents => {
                    break (cluster_document.clone(), task_stream.clone());
                }
                _ => {}
            }
            assert!(
                Instant::now() < log_deadline,
                "case {case:?}: expected performance log events in {log_group} carrying every \
                 figure, got {documents:?}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        };
        assert_eq!(
            cluster_document[CLUSTER_DIMENSION],
            json!(row.cluster),
            "case {case:?}: expected the cluster's own performance document, got \
             {cluster_document}"
        );
        // A document DECLARES the figures it carries: the namespace they are published in, the
        // dimension sets they are published under and the unit each is measured in.
        let directive = cluster_document["CloudWatchMetrics"]
            .as_array()
            .and_then(|blocks| blocks.first().cloned())
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected the document to declare its figures, got \
                     {cluster_document}"
                )
            });
        assert_eq!(
            directive["Namespace"],
            json!(NAMESPACE),
            "case {case:?}: expected the declaration to name the namespace, got {directive}"
        );
        assert_eq!(
            directive["Dimensions"],
            json!([[CLUSTER_DIMENSION]]),
            "case {case:?}: expected the cluster's own dimension set, got {directive}"
        );
        for (metric, unit) in &expected {
            assert!(
                directive["Metrics"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|declared| {
                        declared["Name"] == json!(metric) && declared["Unit"] == json!(unit)
                    }),
                "case {case:?}: expected {metric} declared in {unit}, got {directive}"
            );
        }
        // One task's telemetry travels the stream of that task: named after the task for a task
        // Fargate runs, which holds no container instance of its own.
        if row.fargate {
            assert!(
                task_stream.starts_with(FARGATE_TELEMETRY_PREFIX),
                "case {case:?}: expected the task's telemetry in a \
                 {FARGATE_TELEMETRY_PREFIX}* stream, got {task_stream:?}"
            );
        } else {
            // A task placed on a container INSTANCE travels the stream that instance's agent
            // writes, which is named after the instance.
            let instance = container_instance_of(&router, row).await;
            assert_eq!(
                task_stream, instance,
                "case {case:?}: expected the task's telemetry in the stream of the container \
                 instance it runs on, got {task_stream:?}"
            );
        }
    }
}

/// The payload the container of the storage row writes and reads back: a word long enough
/// that the bytes it moves cannot be confused with a figure of zero.
const STORAGE_PAYLOAD_BYTES: usize = 512;

/// RULE (the same one, on the traffic and storage half of the set): the byte figures are
/// MEASUREMENTS of what the containers of the cluster's tasks really moved, so a container
/// whose program writes a file and reads it back makes the cluster's storage series report at
/// least the bytes it moved.
#[tokio::test]
async fn the_storage_series_reports_the_bytes_a_containers_program_really_moved() {
    let router = emulator();
    let case = "a container that writes a file and reads it back";
    let cluster = "insights-storage";
    let family = "insightsstorage";
    create_cluster(&router, cluster, case).await;
    ok(
        &router,
        "UpdateClusterSettings",
        json!({
            "cluster": cluster,
            "settings": [{ "name": "containerInsights", "value": "enabled" }],
        }),
        case,
    )
    .await;
    register_capacity(&router, cluster, case).await;
    let payload = "x".repeat(STORAGE_PAYLOAD_BYTES);
    ok(
        &router,
        "RegisterTaskDefinition",
        json!({
            "family": family,
            "containerDefinitions": [{
                "name": "app",
                "image": IMAGE,
                "memory": 128,
                "essential": true,
                "command": [
                    "sh",
                    "-c",
                    format!("echo {payload} > /tmp/payload; cat /tmp/payload; {STAYS_UP}"),
                ],
            }],
        }),
        case,
    )
    .await;
    ok(
        &router,
        "CreateService",
        json!({
            "cluster": cluster,
            "serviceName": family,
            "taskDefinition": family,
            "desiredCount": 1,
            "launchType": "EC2",
        }),
        case,
    )
    .await;

    // The bytes the series reports are the bytes that program moved, so each of the two
    // figures reaches the size of the payload it wrote and read back.
    for metric in ["StorageWriteBytes", "StorageReadBytes"] {
        let deadline = Instant::now() + POLL_BUDGET;
        loop {
            let points = datapoints(&router, cluster_dimensions(cluster), metric).await;
            let moved: f64 = points
                .iter()
                .filter_map(|point| point["Sum"].as_f64())
                .sum();
            if moved >= STORAGE_PAYLOAD_BYTES as f64 {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: expected {metric} to reach {STORAGE_PAYLOAD_BYTES} bytes, \
                 got {moved} from {points:?}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
}
