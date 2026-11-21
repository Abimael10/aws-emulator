// SPDX-License-Identifier: CC0-1.0

//! The `AWS/ECS` utilization series a SERVICE publishes, and the dimension set it carries.
//!
//! RULE: Amazon ECS publishes a service's CPU and memory utilization as the `AWS/ECS` metrics
//! `CPUUtilization` and `MemoryUtilization`, in `Percent`, dimensioned on EXACTLY the pair
//! `ClusterName` + `ServiceName` that names the service — for as long as that service holds a
//! running task, and for no service that holds none. Each datapoint MEASURES what the
//! containers of those tasks are doing — the share of the CPU they were given that their
//! programs spent EXECUTING, and the memory they are occupying over what the service's task
//! definition reserves (the ECS developer guide's service utilization divides what the tasks
//! of one service use by what that definition reserves): so none of them is negative or past
//! 100, and a service whose container keeps COMPUTING reports more CPU in use than one whose
//! container only WAITS. The same namespace carries no series on `ClusterName` alone for such
//! a service — a cluster-wide utilization is a measurement of the container instances
//! registered to the cluster, of which a Fargate-only cluster has none — so an alarm over the
//! pair transitions on the published data while its `ClusterName`-only twin stays
//! `INSUFFICIENT_DATA`, and `ListMetrics` over the namespace lists both metric names under
//! that one dimension set.
//!
//! Two crates have to agree for that (ECS meters its services, CloudWatch stores the series,
//! lists it and grades the alarms over it), and the `awsvpc` placements resolve a subnet in a
//! third (Amazon EC2 holds the network), so the suite lives here.
//!
//! The rows differ along what the rule generalises over: HOW MANY tasks the service runs (one,
//! three, and none at all), WHICH capacity runs them (Fargate, and the EC2 launch type on a
//! registered container instance) and WHAT the containers do (a program that computes without
//! pausing, and one that waits), each service in a cluster of its own so a row's series are
//! read on their own. A recording of a single `desiredCount=1` Fargate service whose container
//! spins is one row.
//!
//! Nothing here reaches into the ECS store: the services are created through the ECS front
//! door, and every datapoint, metric listing and alarm state is read back through CloudWatch's
//! own `GetMetricStatistics`, `ListMetrics`, `PutMetricAlarm` and `DescribeAlarms` — the same
//! store a client's `PutMetricData` writes into.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The region every request below is signed for.
const REGION: &str = "us-west-2";

/// The account id every emulator ARN carries, which an instance identity document names.
const ACCOUNT: &str = "000000000000";

/// The namespace and the two metric names this suite reads, and the dimensions they carry —
/// spelled out here rather than imported, so a wrong constant in the implementation cannot
/// make a test pass.
pub(crate) const NAMESPACE: &str = "AWS/ECS";
pub(crate) const CPU_UTILIZATION: &str = "CPUUtilization";
pub(crate) const MEMORY_UTILIZATION: &str = "MemoryUtilization";
pub(crate) const CLUSTER_DIMENSION: &str = "ClusterName";
pub(crate) const SERVICE_DIMENSION: &str = "ServiceName";

/// The `StandardUnit` a percentage is published in.
pub(crate) const PERCENT: &str = "Percent";

/// The alarm states a `StateValue` may report, of the three this suite reads.
pub(crate) const INSUFFICIENT_DATA: &str = "INSUFFICIENT_DATA";
const ALARM: &str = "ALARM";

/// The network the `awsvpc` placements name.
const VPC: &str = "vpc-0123456789abcdef0";
pub(crate) const SUBNET: &str = "subnet-0123456789abcdef0";

/// The image every container names, and the programs a row's container runs. Each of them
/// stays up — so a task that is running is still running while its series is read — and they
/// differ in what they DO: one computes at every step, the other waits.
pub(crate) const IMAGE: &str = "public.ecr.aws/nginx/nginx:latest";
pub(crate) const STAYS_UP: [&str; 3] = ["sh", "-c", "while true; do sleep 1; done"];
const COMPUTES: &str = "i=0; while true; do i=$((i+1)); done";
const WAITS: &str = STAYS_UP[2];

/// How often a poll re-reads what it is waiting on.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// How long one wait keeps trying before it gives up. Each wait gets this budget of its OWN,
/// never one shared between two: Amazon ECS creates and attaches the interface an `awsvpc` task
/// is allocated one documented step at a time and only then starts it, so a correct run is far
/// inside this and a broken one is reported promptly. Nothing here asserts on elapsed time.
const DEADLINE: Duration = Duration::from_secs(120);

/// How many further looks an assertion of EMPTINESS takes. Together with [`POLL_INTERVAL`] a
/// window far longer than the spacing a series that is catching up publishes at, so an
/// emptiness a row asserts is one that HELD rather than one read too early.
const QUIET_LOOKS: usize = 20;

/// Which capacity a row's service runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Capacity {
    /// `FARGATE`, with an `awsvpc` network configuration and no container instance at all.
    Fargate,
    /// `EC2`, on one container instance registered into the row's cluster.
    Ec2,
}

/// One row: the service to create, where it runs, how many tasks it wants, what its container
/// does, and what it therefore publishes.
struct Row {
    case: &'static str,
    cluster: &'static str,
    service: &'static str,
    capacity: Capacity,
    /// The `desiredCount` the service is created with.
    desired: i64,
    /// The shell program the container runs.
    program: &'static str,
    /// Whether this service publishes a series at all — it holds a running task, so something
    /// is measuring it.
    publishes: bool,
    /// Whether its container is expected to be measured USING the CPU it was given.
    busy: bool,
}

const ROWS: &[Row] = &[
    Row {
        case: "the recorded shape: a Fargate service running one task that computes",
        cluster: "util-fargate-one",
        service: "orders",
        capacity: Capacity::Fargate,
        desired: 1,
        program: COMPUTES,
        publishes: true,
        busy: true,
    },
    Row {
        case: "a Fargate service running three tasks that wait",
        cluster: "util-fargate-three",
        service: "checkout",
        capacity: Capacity::Fargate,
        desired: 3,
        program: WAITS,
        publishes: true,
        busy: false,
    },
    Row {
        case: "an EC2 launch-type service running one task that waits",
        cluster: "util-ec2-one",
        service: "billing",
        capacity: Capacity::Ec2,
        desired: 1,
        program: WAITS,
        publishes: true,
        busy: false,
    },
    Row {
        case: "a Fargate service that wants no task",
        cluster: "util-fargate-none",
        service: "idle",
        capacity: Capacity::Fargate,
        desired: 0,
        program: WAITS,
        publishes: false,
        busy: false,
    },
];

/// One emulator carrying Amazon ECS and CloudWatch — which owns the metric store the series is
/// published into — over ONE shared engine, exactly as the production router builds them, with
/// the network the `awsvpc` placements name held in the account's Amazon EC2 store.
///
/// `pub(crate)`, with the calls and the readers below it, because the sibling suite over the
/// same two stores ([`crate::ecs_service_utilization_setting_independence`]) needs the same
/// pair mounted the same way: one harness for one rule, rather than a second copy of it.
pub(crate) fn emulator() -> MultiServiceRouter {
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

/// One awsJson1_1 call that must succeed, answered with the parsed body.
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
pub(crate) async fn ecs(router: &MultiServiceRouter, operation: &str, body: Value) -> Value {
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
pub(crate) async fn cloudwatch(
    router: &MultiServiceRouter,
    operation: &str,
    body: Value,
) -> Value {
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
            // Wide enough to hold the whole run-up a series opens with.
            "StartTime": now - 1_800.0,
            "EndTime": now + 600.0,
            "Period": 60,
            "Statistics": ["Minimum", "Maximum", "Average", "SampleCount"],
        }),
    )
    .await;
    stats["Datapoints"].as_array().cloned().unwrap_or_default()
}

/// The dimension pair that names one row's service.
fn service_dimensions(row: &Row) -> Value {
    json!([
        { "Name": CLUSTER_DIMENSION, "Value": row.cluster },
        { "Name": SERVICE_DIMENSION, "Value": row.service },
    ])
}

/// The cluster-only dimension set — the one no service-level series carries.
fn cluster_dimensions(row: &Row) -> Value {
    json!([{ "Name": CLUSTER_DIMENSION, "Value": row.cluster }])
}

/// An EC2 instance identity document, as the agent reads it from the instance metadata
/// service: a registration presents it with a base64 signature beside it.
fn identity_document(instance_id: &str) -> String {
    json!({
        "accountId": ACCOUNT,
        "instanceId": instance_id,
        "region": REGION,
        "instanceType": "m5.large",
        "availabilityZone": format!("{REGION}a"),
    })
    .to_string()
}

/// Create one row's cluster, the revision its service runs and the service itself.
///
/// The cluster pins `containerInsights` to `disabled`, so what is published here is the BASE
/// `AWS/ECS` namespace and not the Container Insights one.
async fn create_service(router: &MultiServiceRouter, row: &Row) {
    ecs(
        router,
        "CreateCluster",
        json!({
            "clusterName": row.cluster,
            "settings": [{ "name": "containerInsights", "value": "disabled" }],
        }),
    )
    .await;
    let mut definition = json!({
        "family": row.service,
        "containerDefinitions": [{
            "name": "app",
            "image": IMAGE,
            "memory": 128,
            "essential": true,
            "command": ["sh", "-c", row.program],
        }],
    });
    let mut create = json!({
        "cluster": row.cluster,
        "serviceName": row.service,
        "taskDefinition": row.service,
        "desiredCount": row.desired,
    });
    match row.capacity {
        Capacity::Fargate => {
            definition["networkMode"] = json!("awsvpc");
            definition["requiresCompatibilities"] = json!(["FARGATE"]);
            definition["cpu"] = json!("256");
            definition["memory"] = json!("512");
            create["launchType"] = json!("FARGATE");
            create["networkConfiguration"] = json!({
                "awsvpcConfiguration": {
                    "subnets": [SUBNET],
                    "securityGroups": [],
                    "assignPublicIp": "ENABLED",
                }
            });
        }
        Capacity::Ec2 => {
            // "The EC2 launch type runs your tasks on Amazon EC2 instances registered to your
            // cluster", so this row's cluster is given one.
            ecs(
                router,
                "RegisterContainerInstance",
                json!({
                    "cluster": row.cluster,
                    "instanceIdentityDocument": identity_document("i-0c0ffee0c0ffee0c0"),
                    "instanceIdentityDocumentSignature": "ZXhhbXBsZQ".repeat(35),
                }),
            )
            .await;
            create["launchType"] = json!("EC2");
        }
    }
    ecs(router, "RegisterTaskDefinition", definition).await;
    ecs(router, "CreateService", create).await;
}

/// Wait until `DescribeServices` reports the row's service holding every task it wants in the
/// `RUNNING` state — the state its utilization is measured in.
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
        let running = service["runningCount"].as_i64().unwrap_or_default();
        if running == row.desired {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected runningCount {}, got {running} in {service}",
            row.desired
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Wait until the row's service has published at least one datapoint of `metric`, and answer
/// every datapoint the series then holds.
async fn published(router: &MultiServiceRouter, row: &Row, metric: &str) -> Vec<Value> {
    let case = row.case;
    let deadline = Instant::now() + DEADLINE;
    loop {
        let points = datapoints(router, metric, service_dimensions(row)).await;
        if !points.is_empty() {
            return points;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {metric} datapoints on {}, got none",
            service_dimensions(row)
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The largest `Average` any datapoint of the row's `metric` reports, and `None` for a series
/// that holds none.
fn highest_average(points: &[Value]) -> Option<f64> {
    points
        .iter()
        .filter_map(|point| point["Average"].as_f64())
        .fold(None, |held: Option<f64>, value| {
            Some(held.map_or(value, |held| held.max(value)))
        })
}

/// Wait until some datapoint of the row's `metric` reports a POSITIVE average, and answer it.
///
/// What a MEASUREMENT of a container that is doing work looks like: the figure is read off the
/// run this emulator performs for that container, so it is positive once that run has
/// performed any of its program.
async fn measured_positive(router: &MultiServiceRouter, row: &Row, metric: &str) -> f64 {
    let case = row.case;
    let deadline = Instant::now() + DEADLINE;
    loop {
        let points = datapoints(router, metric, service_dimensions(row)).await;
        if let Some(highest) = highest_average(&points).filter(|value| *value > 0.0) {
            return highest;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected a positive {metric} average on {}, got {points:?}",
            service_dimensions(row)
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Put one alarm over `metric` on `dimensions`, named `name`.
///
/// The condition is one every percentage satisfies — a utilization is at most 100 — so what
/// decides whether the alarm leaves `INSUFFICIENT_DATA` is whether the dimension set it names
/// receives data at all, which is the contrast this suite is about.
async fn put_alarm(router: &MultiServiceRouter, name: &str, metric: &str, dimensions: Value) {
    cloudwatch(
        router,
        "PutMetricAlarm",
        json!({
            "AlarmName": name,
            "Namespace": NAMESPACE,
            "MetricName": metric,
            "Dimensions": dimensions,
            "Statistic": "Average",
            "Period": 60,
            "EvaluationPeriods": 1,
            "Threshold": 100.0,
            "ComparisonOperator": "LessThanOrEqualToThreshold",
        }),
    )
    .await;
}

/// The `StateValue` CloudWatch reports for the alarm `name`.
pub(crate) async fn alarm_state(router: &MultiServiceRouter, name: &str) -> String {
    let described = cloudwatch(router, "DescribeAlarms", json!({ "AlarmNames": [name] })).await;
    described["MetricAlarms"][0]["StateValue"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

#[tokio::test]
async fn a_running_service_publishes_percent_utilization_on_its_cluster_and_service_pair() {
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
            // A service measured by nothing publishes nothing — asserted over a WINDOW,
            // because a datapoint the clock owed would arrive a moment later.
            for _ in 0..QUIET_LOOKS {
                ::tokio::time::sleep(POLL_INTERVAL).await;
                for metric in [CPU_UTILIZATION, MEMORY_UTILIZATION] {
                    let points = datapoints(&router, metric, service_dimensions(row)).await;
                    assert!(
                        points.is_empty(),
                        "case {case:?}: expected no {metric} datapoint, got {points:?}"
                    );
                }
            }
            continue;
        }

        for metric in [CPU_UTILIZATION, MEMORY_UTILIZATION] {
            let points = published(&router, row, metric).await;
            for point in &points {
                assert_eq!(
                    point["Unit"].as_str(),
                    Some(PERCENT),
                    "case {case:?}: expected {metric} in {PERCENT}, got {point}"
                );
                let minimum = point["Minimum"].as_f64().unwrap_or_else(|| {
                    panic!("case {case:?}: expected a Minimum for {metric}, got {point}")
                });
                let maximum = point["Maximum"].as_f64().unwrap_or_else(|| {
                    panic!("case {case:?}: expected a Maximum for {metric}, got {point}")
                });
                // A utilization is a percentage of what the task definition reserves, so no
                // datapoint of either metric is negative or past 100.
                assert!(
                    (0.0..=100.0).contains(&minimum) && (0.0..=100.0).contains(&maximum),
                    "case {case:?}: expected {metric} within 0..100, got {point}"
                );
                assert!(
                    minimum <= maximum,
                    "case {case:?}: expected {metric} Minimum at most Maximum, got {point}"
                );
                assert!(
                    point["SampleCount"].as_f64().unwrap_or_default() >= 1.0,
                    "case {case:?}: expected {metric} to report its observations, got {point}"
                );
            }
        }

        // The memory a running container's session and its own layer hold is memory it is
        // using, so every metered service reports some of its reserved memory in use.
        measured_positive(&router, row, MEMORY_UTILIZATION).await;
        // ...and the CPU is the work its containers really perform: the one that computes is
        // measured using the CPU it was given, while the ones that wait have their waiting
        // charged to nothing.
        if row.busy {
            let highest = measured_positive(&router, row, CPU_UTILIZATION).await;
            computing = Some(computing.map_or(highest, |held: f64| held.max(highest)));
        } else {
            let points = datapoints(&router, CPU_UTILIZATION, service_dimensions(row)).await;
            let highest = highest_average(&points).unwrap_or_default();
            waiting = Some(waiting.map_or(highest, |held: f64| held.max(highest)));
        }

        // The namespace carries no series on the CLUSTER alone for this service: a
        // cluster-wide utilization is a measurement of registered container instances, which
        // is not what a service's series is of.
        for _ in 0..QUIET_LOOKS {
            ::tokio::time::sleep(POLL_INTERVAL).await;
            for metric in [CPU_UTILIZATION, MEMORY_UTILIZATION] {
                let points = datapoints(&router, metric, cluster_dimensions(row)).await;
                assert!(
                    points.is_empty(),
                    "case {case:?}: expected no {metric} datapoint on {CLUSTER_DIMENSION} \
                     alone, got {points:?}"
                );
            }
        }

        // ...and that is exactly what a client selecting the cluster's metrics finds: both
        // metric names, each under the one dimension set naming this service.
        let listed = cloudwatch(
            &router,
            "ListMetrics",
            json!({
                "Namespace": NAMESPACE,
                "Dimensions": [{ "Name": CLUSTER_DIMENSION, "Value": row.cluster }],
            }),
        )
        .await;
        let metrics: Vec<Value> = listed["Metrics"].as_array().cloned().unwrap_or_default();
        for metric in [CPU_UTILIZATION, MEMORY_UTILIZATION] {
            let listed_metric = metrics
                .iter()
                .find(|held| held["MetricName"].as_str() == Some(metric))
                .unwrap_or_else(|| panic!("case {case:?}: expected {metric} among {metrics:?}"));
            let dimensions: Vec<(String, String)> = listed_metric["Dimensions"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|dimension| {
                    (
                        dimension["Name"].as_str().unwrap_or_default().to_string(),
                        dimension["Value"].as_str().unwrap_or_default().to_string(),
                    )
                })
                .collect();
            assert_eq!(
                dimensions,
                vec![
                    (CLUSTER_DIMENSION.to_string(), row.cluster.to_string()),
                    (SERVICE_DIMENSION.to_string(), row.service.to_string()),
                ],
                "case {case:?}: expected {metric} dimensioned by the cluster and the service, \
                 got {listed_metric}"
            );
        }
    }

    // The percentages are a MEASUREMENT of what each service's containers do: the service
    // whose container computes at every step is using more of the CPU it was given than the
    // ones whose containers sit in a wait.
    let computing = computing.expect("a row whose container computes");
    let waiting = waiting.expect("a row whose container waits");
    assert!(
        computing > waiting,
        "expected the computing service's {CPU_UTILIZATION} ({computing}) to exceed the \
         waiting services' ({waiting})"
    );
}

#[tokio::test]
async fn an_alarm_on_the_service_pair_transitions_while_its_cluster_only_twin_does_not() {
    let router = emulator();
    // One row's service: the contrast here is between two alarms over ONE service, and the
    // test above already spans how many tasks a service runs and which capacity runs them.
    let row = &ROWS[0];
    let case = row.case;
    create_service(&router, row).await;
    converged(&router, row).await;
    published(&router, row, CPU_UTILIZATION).await;

    put_alarm(
        &router,
        "utilization-per-service",
        CPU_UTILIZATION,
        service_dimensions(row),
    )
    .await;
    put_alarm(
        &router,
        "utilization-per-cluster",
        CPU_UTILIZATION,
        cluster_dimensions(row),
    )
    .await;

    // "PutMetricAlarm ... the alarm is then evaluated and its state is set appropriately": the
    // alarm over the series that receives data leaves INSUFFICIENT_DATA for the state its
    // datapoints imply, and the one over the dimension set nothing publishes cannot.
    let deadline = Instant::now() + DEADLINE;
    loop {
        let state = alarm_state(&router, "utilization-per-service").await;
        if state == ALARM {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the alarm on the service pair to reach {ALARM}, got \
             {state:?}"
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
    for _ in 0..QUIET_LOOKS {
        ::tokio::time::sleep(POLL_INTERVAL).await;
        let state = alarm_state(&router, "utilization-per-cluster").await;
        assert_eq!(
            state, INSUFFICIENT_DATA,
            "case {case:?}: expected the {CLUSTER_DIMENSION}-only twin to stay \
             {INSUFFICIENT_DATA}, got {state:?}"
        );
    }
}
