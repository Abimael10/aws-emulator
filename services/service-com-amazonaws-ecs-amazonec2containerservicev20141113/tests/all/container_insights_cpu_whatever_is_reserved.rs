// SPDX-License-Identifier: CC0-1.0

//! The CPU a Container Insights cluster reports its tasks USING, whatever their revisions
//! reserve.
//!
//! The rule under test:
//!
//! > A cluster `Cluster$settings` reports CloudWatch Container Insights as ON for publishes the
//! > RESOURCE-UTILISATION series of a service with running tasks beside the COUNT series of the
//! > same service, on the same `ClusterName`-and-`ServiceName` pair: what those tasks'
//! > containers are measured using is published whatever form their revision states a
//! > reservation in — and whether or not it states one at all, because a usage is a measurement
//! > of the containers rather than a reading of the revision — while a RESERVATION is read off
//! > that revision and is published only where it states one.
//!
//! The rows span what the rule generalises over: the FORM the revision states its CPU
//! reservation in — for the whole task (`TaskDefinition$cpu`, "the number of CPU units used by
//! the task"), per container (`ContainerDefinition$cpu`, "the number of `cpu` units the Amazon
//! ECS container agent reserves for the container"), and a revision that states neither, which
//! is a revision Amazon ECS registers and places all the same.
//!
//! Nothing here reaches into a state struct: the clusters, the revisions and the services are
//! created through this service's own API, and every figure is read back through CloudWatch's
//! OWN `GetMetricStatistics` — the same store a client's `PutMetricData` writes into.

use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{cloudwatch, emulator_with_cloudwatch, ok, poll_until, register_capacity};

/// The namespace Amazon ECS publishes Container Insights metrics in, the two dimensions a
/// service-level series carries and the four metric names this suite reads — spelled out here
/// rather than imported, so a wrong constant in the implementation cannot make a test pass.
/// Their source is the CloudWatch User Guide's "Amazon ECS Container Insights metrics" table,
/// whose service-level rows are dimensioned by `ClusterName` and `ServiceName`.
const NAMESPACE: &str = "ECS/ContainerInsights";
const CLUSTER_DIMENSION: &str = "ClusterName";
const SERVICE_DIMENSION: &str = "ServiceName";
const CPU_UTILIZED: &str = "CpuUtilized";
const CPU_RESERVED: &str = "CpuReserved";
const MEMORY_UTILIZED: &str = "MemoryUtilized";
const RUNNING_TASK_COUNT: &str = "RunningTaskCount";

/// The image every container below names, and the program each one runs: a loop that COMPUTES
/// at every step and never pauses, so the container is using the CPU it was given for as long
/// as the row reads its series.
const IMAGE: &str = "public.ecr.aws/nginx/nginx:latest";
const COMPUTES: &str = "i=0; while true; do i=$((i+1)); done";

/// How many CPU units one vCPU is.
///
/// SOURCE: `TaskDefinition$cpu` — "1024 CPU units is the equivalent of 1 vCPU". A container's
/// program is performed on one performer, which is one vCPU of capacity, so this is the ceiling
/// the usage of a task that reserves nothing is measured against.
const CPU_UNITS_PER_VCPU: f64 = 1024.0;

/// One row: the cluster, the service it holds and the revision that service runs — stated as
/// the members the revision carries, which is the axis the rows span.
struct Row {
    case: &'static str,
    cluster: &'static str,
    /// The service's name, which is also the family of the revision it runs.
    service: &'static str,
    /// `TaskDefinition$cpu` and `$memory`, the reservation stated for the WHOLE task, and
    /// `None` for a revision that states neither there.
    task_cpu: Option<&'static str>,
    task_memory: Option<i64>,
    /// `ContainerDefinition$cpu` and `$memory`, the reservation stated PER CONTAINER, under the
    /// same rule.
    container_cpu: Option<i64>,
    container_memory: Option<i64>,
    /// The CPU units the revision reserves in total — what `CpuReserved` reports, and the
    /// ceiling the usage is measured against — and `None` for a revision that reserves none, for
    /// which no reservation is published at all.
    reserved_cpu: Option<f64>,
}

const ROWS: &[Row] = &[
    Row {
        case: "a revision that reserves its CPU for the whole task",
        cluster: "insights-cpu-task-size",
        service: "insightscputask",
        task_cpu: Some("256"),
        task_memory: Some(512),
        container_cpu: None,
        container_memory: None,
        reserved_cpu: Some(256.0),
    },
    Row {
        case: "a revision that reserves its CPU per container",
        cluster: "insights-cpu-container",
        service: "insightscpucontainer",
        task_cpu: None,
        task_memory: None,
        container_cpu: Some(128),
        container_memory: Some(128),
        reserved_cpu: Some(128.0),
    },
    Row {
        case: "a revision that reserves no CPU anywhere",
        cluster: "insights-cpu-unreserved",
        service: "insightscpuunreserved",
        task_cpu: None,
        task_memory: None,
        container_cpu: None,
        container_memory: Some(128),
        reserved_cpu: None,
    },
];

/// An emulator with Amazon ECS and CloudWatch — which owns the metric store these series are
/// published into — on ONE shared engine ([`emulator_with_cloudwatch`]).
fn emulator() -> MultiServiceRouter {
    emulator_with_cloudwatch()
}

/// Epoch seconds, for the window a statistics read asks about.
fn epoch_seconds() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

/// The dimension pair one row's service-level series carries.
fn service_dimensions(row: &Row) -> Value {
    json!([
        { "Name": CLUSTER_DIMENSION, "Value": row.cluster },
        { "Name": SERVICE_DIMENSION, "Value": row.service },
    ])
}

/// What `GetMetricStatistics` answers for `metric` on exactly `dimensions`.
async fn statistics(router: &MultiServiceRouter, metric: &str, dimensions: &Value) -> Value {
    let now = epoch_seconds();
    cloudwatch(
        router,
        "GetMetricStatistics",
        json!({
            "Namespace": NAMESPACE,
            "MetricName": metric,
            "Dimensions": dimensions,
            "StartTime": now - 1_200.0,
            "EndTime": now + 1_200.0,
            "Period": 60,
            "Statistics": ["Average", "Maximum", "SampleCount"],
        }),
    )
    .await
}

/// The datapoints one such answer carries.
fn points(answered: &Value) -> Vec<Value> {
    answered["Datapoints"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// The largest `statistic` any of them reports, and `None` for a series that holds none.
fn highest(answered: &Value, statistic: &str) -> Option<f64> {
    points(answered)
        .iter()
        .filter_map(|point| point[statistic].as_f64())
        .fold(None, |held: Option<f64>, value| {
            Some(held.map_or(value, |held| held.max(value)))
        })
}

/// Create one row's cluster with Container Insights on, the capacity its task is placed on, the
/// revision it describes and a service that keeps one task of it running.
///
/// A service rather than a standalone task: "the service scheduler ... replaces tasks that fail
/// or stop" (`CreateService`), so the task it places keeps running while the row reads.
async fn set_up(router: &MultiServiceRouter, row: &Row) {
    ok(
        router,
        "CreateCluster",
        json!({
            "clusterName": row.cluster,
            "settings": [{ "name": "containerInsights", "value": "enabled" }],
        }),
        row.case,
    )
    .await;
    register_capacity(router, row.cluster, row.case).await;
    let mut container = json!({
        "name": "app",
        "image": IMAGE,
        "essential": true,
        "command": ["sh", "-c", COMPUTES],
    });
    if let Some(cpu) = row.container_cpu {
        container["cpu"] = json!(cpu);
    }
    if let Some(memory) = row.container_memory {
        container["memory"] = json!(memory);
    }
    let mut definition = json!({
        "family": row.service,
        "containerDefinitions": [container],
    });
    if let Some(cpu) = row.task_cpu {
        definition["cpu"] = json!(cpu);
    }
    if let Some(memory) = row.task_memory {
        definition["memory"] = json!(memory.to_string());
    }
    ok(router, "RegisterTaskDefinition", definition, row.case).await;
    ok(
        router,
        "CreateService",
        json!({
            "cluster": row.cluster,
            "serviceName": row.service,
            "taskDefinition": row.service,
            "desiredCount": 1,
            "launchType": "EC2",
        }),
        row.case,
    )
    .await;
}

/// RULE: the utilisation series of a service with running tasks arrives beside its count series,
/// whatever its revision reserves.
#[tokio::test]
async fn container_insights_reports_the_cpu_its_tasks_use_whatever_they_reserve() {
    let router = emulator();
    // Every row is set up first, so the clock-owed placements of all of them fall due together
    // rather than one row's wait after another's.
    for row in ROWS {
        set_up(&router, row).await;
    }

    for row in ROWS {
        let case = row.case;
        let dimensions = service_dimensions(row);
        // The service holds its task RUNNING, which is the state its utilisation is measured
        // in and the count its series carries.
        poll_until(
            case,
            "the service to hold a running task",
            || {
                ok(
                    &router,
                    "DescribeServices",
                    json!({ "cluster": row.cluster, "services": [row.service] }),
                    case,
                )
            },
            |described| described["services"][0]["runningCount"] == json!(1),
        )
        .await;

        // The COUNT series of that service arrives on the dimension pair a per-service alarm
        // and dashboard select...
        let counted = poll_until(
            case,
            &format!("a {RUNNING_TASK_COUNT} datapoint of at least one task"),
            || statistics(&router, RUNNING_TASK_COUNT, &dimensions),
            |answered| highest(answered, "Maximum").is_some_and(|count| count >= 1.0),
        )
        .await;
        let running = highest(&counted, "Maximum").unwrap_or_default();

        // ...and so does what those tasks' containers are measured USING, on the very same
        // pair: a program that computes at every step is using the CPU it was given, whatever
        // its revision reserves.
        let used = poll_until(
            case,
            &format!("a positive {CPU_UTILIZED} average beside the count of {running} task(s)"),
            || statistics(&router, CPU_UTILIZED, &dimensions),
            |answered| highest(answered, "Average").is_some_and(|value| value > 0.0),
        )
        .await;
        let cpu = highest(&used, "Average").unwrap_or_default();
        // A usage is never more than the capacity those tasks were given: what the revision
        // reserves where it states a reservation, and the one performer per container this
        // emulator performs their programs on — one vCPU — where it states none.
        let ceiling = row.reserved_cpu.unwrap_or(CPU_UNITS_PER_VCPU);
        assert!(
            cpu <= ceiling,
            "case {case:?}: expected {CPU_UTILIZED} at most the {ceiling} CPU units those \
             tasks were given, got {cpu}"
        );
        // The memory those same containers occupy is measured the same way, and arrives in the
        // same collection.
        poll_until(
            case,
            &format!("a positive {MEMORY_UTILIZED} average"),
            || statistics(&router, MEMORY_UTILIZED, &dimensions),
            |answered| highest(answered, "Average").is_some_and(|value| value > 0.0),
        )
        .await;

        // A RESERVATION, as against a usage, is read off the revision: reported as the units it
        // states, and reported by nothing for a revision that states none.
        match row.reserved_cpu {
            Some(reserved) => {
                poll_until(
                    case,
                    &format!("{CPU_RESERVED} to report the reserved {reserved}"),
                    || statistics(&router, CPU_RESERVED, &dimensions),
                    |answered| highest(answered, "Maximum") == Some(reserved),
                )
                .await;
            }
            None => {
                // Read AFTER the usage above arrived, so this is a series the same collection
                // left empty rather than one read before any collection ran.
                let answered = statistics(&router, CPU_RESERVED, &dimensions).await;
                let reserved = points(&answered);
                assert!(
                    reserved.is_empty(),
                    "case {case:?}: expected no {CPU_RESERVED} datapoint for a revision that \
                     reserves none, got {reserved:?}"
                );
            }
        }
    }
}
