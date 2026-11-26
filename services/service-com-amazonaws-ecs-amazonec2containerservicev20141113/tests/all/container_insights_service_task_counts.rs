// SPDX-License-Identifier: CC0-1.0

//! The PER-SERVICE Container Insights task counts of a cluster, and the convergence they
//! report.
//!
//! The rule under test:
//!
//! > A service "runs and maintains your desired number of tasks from a specified task
//! > definition" (`CreateService`), so `Service$runningCount` ("The number of tasks in the
//! > cluster that are in the RUNNING state") reaches the `Service$desiredCount` the caller
//! > asked for with `Service$pendingCount` back to zero — whatever the count, and whatever
//! > program the revision's container runs, as long as that program stays up. A cluster whose
//! > `containerInsights` setting is on publishes those very counts into the Container Insights
//! > namespace as `RunningTaskCount` and `DesiredTaskCount` at one-minute resolution with the
//! > unit `Count`, dimensioned by `ClusterName` and `ServiceName` for a service-level series —
//! > and each service's series is scoped to that service's OWN tasks, so a datapoint of one
//! > service never carries another service's count.
//!
//! The rows span what the rule generalises over: HOW MANY tasks each service of the one
//! cluster wants (none, one, two, three) — several services of one cluster at once, which is
//! what makes the per-service scoping observable at all.
//!
//! Nothing here reaches into a state struct: the network is built through Amazon EC2's own
//! API, the cluster, the revision and the services through Amazon ECS's, the counts are read
//! back through `DescribeServices` and `ListTasks`, and the series through CloudWatch's own
//! `GetMetricStatistics`.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{cloudwatch, count, ec2, ec2_element, list, ok, string, SIGNED_REGION};

/// The namespace Amazon ECS publishes Container Insights metrics in, the two counts this
/// suite reads and the dimensions they carry — spelled out here rather than imported, so a
/// wrong constant in the implementation cannot make a test pass.
///
/// SOURCE: the CloudWatch User Guide's "Amazon ECS Container Insights metrics" table, the
/// rows whose dimension set is `ClusterName, ServiceName`.
const NAMESPACE: &str = "ECS/ContainerInsights";
const RUNNING_TASK_COUNT: &str = "RunningTaskCount";
const DESIRED_TASK_COUNT: &str = "DesiredTaskCount";
const CLUSTER_DIMENSION: &str = "ClusterName";
const SERVICE_DIMENSION: &str = "ServiceName";

/// The `StandardUnit` a count of tasks is published in, and the resolution those series are
/// published at, in seconds.
const COUNT_UNIT: &str = "Count";
const PERIOD_SECONDS: i64 = 60;

/// The cluster every row's service runs in, and the task-definition family they all run.
const CLUSTER: &str = "insights-service-counts";
const FAMILY: &str = "insights-service-counts";

/// The image every container names, and the program it runs: a wait with nothing else to do,
/// which is how a container of a maintained service stays up.
const IMAGE: &str = "public.ecr.aws/docker/library/busybox:latest";
const STAYS_UP: [&str; 3] = ["sh", "-c", "sleep 3600"];

/// How often a poll re-reads, and how long one keeps trying before it gives up. Generous: the
/// tasks come up on the clock, the series is published as that clock moves them, and every
/// poll stops the instant its own condition holds.
const POLL_INTERVAL: Duration = Duration::from_millis(50);
const POLL_BUDGET: Duration = Duration::from_secs(240);

/// How long a converged service is WATCHED, re-read inside the loop that spends it: the
/// containers of its tasks were given nothing but a wait far longer than this window, so a
/// service whose tasks stay up holds the very same tasks at the end of it however slowly the
/// host got there.
const WATCH_WINDOW: Duration = Duration::from_secs(2);

/// One row: a service of the one cluster, and the count it wants.
#[derive(Debug)]
struct Row {
    case: &'static str,
    service: &'static str,
    desired: i64,
}

const ROWS: &[Row] = &[
    Row {
        case: "a service that wants no task at all",
        service: "counts-none",
        desired: 0,
    },
    Row {
        case: "a service that wants one task",
        service: "counts-one",
        desired: 1,
    },
    Row {
        case: "a service that wants two tasks",
        service: "counts-two",
        desired: 2,
    },
    Row {
        case: "a service that wants three tasks",
        service: "counts-three",
        desired: 3,
    },
];

/// An emulator carrying Amazon ECS, Amazon EC2 — whose records the `awsvpc` placement and the
/// path to the image registry are resolved against — Amazon ECR Public, which SERVES the pull
/// of the gallery image every container names, and CloudWatch, which owns the metric store the
/// series is published into, over ONE shared engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_ecs_amazonec2containerservicev20141113::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_ec2_amazonec2::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_ecrpublic_spencerfrontendservice::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_cloudwatch_graniteserviceversion20100801::EmulatedService::new(
            engine,
        ),
    );
    router
}

/// The subnet and security group every row's service places into, built through Amazon EC2's
/// own API: a VPC with an internet gateway and a default route to it, so a placement given a
/// public address reaches the registry its image is held in.
async fn network(router: &MultiServiceRouter) -> (String, String) {
    let case = "the network every row's service places into";
    let vpc = ec2_element(
        &ec2(router, "CreateVpc", &[("CidrBlock", "10.95.0.0/16")], case).await,
        "vpcId",
        case,
    );
    let gateway = ec2_element(
        &ec2(router, "CreateInternetGateway", &[], case).await,
        "internetGatewayId",
        case,
    );
    ec2(
        router,
        "AttachInternetGateway",
        &[("InternetGatewayId", &gateway), ("VpcId", &vpc)],
        case,
    )
    .await;
    let subnet = ec2_element(
        &ec2(
            router,
            "CreateSubnet",
            &[
                ("VpcId", &vpc),
                ("CidrBlock", "10.95.1.0/24"),
                ("AvailabilityZone", &format!("{SIGNED_REGION}a")),
            ],
            case,
        )
        .await,
        "subnetId",
        case,
    );
    // The default route goes on the VPC's MAIN route table, which is the table a subnet with
    // no association of its own gets its routes from — the shape a client that builds a public
    // subnet in a fresh VPC ends up with.
    let table = ec2_element(
        &ec2(
            router,
            "DescribeRouteTables",
            &[
                ("Filter.1.Name", "vpc-id"),
                ("Filter.1.Value.1", &vpc),
                ("Filter.2.Name", "association.main"),
                ("Filter.2.Value.1", "true"),
            ],
            case,
        )
        .await,
        "routeTableId",
        case,
    );
    ec2(
        router,
        "CreateRoute",
        &[
            ("RouteTableId", &table),
            ("DestinationCidrBlock", "0.0.0.0/0"),
            ("GatewayId", &gateway),
        ],
        case,
    )
    .await;
    let group = ec2_element(
        &ec2(
            router,
            "CreateSecurityGroup",
            &[
                ("VpcId", &vpc),
                ("GroupName", "insights-service-counts"),
                ("GroupDescription", "no+ingress"),
            ],
            case,
        )
        .await,
        "groupId",
        case,
    );
    (subnet, group)
}

/// Epoch seconds, for the window a statistics read asks about.
fn epoch_seconds() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

/// Every datapoint CloudWatch holds for one SERVICE's `metric`, read the way a client reads
/// it: the cluster-and-service dimension pair, at the one-minute period those series are
/// published at.
async fn service_datapoints(
    router: &MultiServiceRouter,
    service: &str,
    metric: &str,
) -> Vec<Value> {
    let now = epoch_seconds();
    let stats = cloudwatch(
        router,
        "GetMetricStatistics",
        json!({
            "Namespace": NAMESPACE,
            "MetricName": metric,
            "Dimensions": [
                { "Name": CLUSTER_DIMENSION, "Value": CLUSTER },
                { "Name": SERVICE_DIMENSION, "Value": service },
            ],
            "StartTime": now - 1800.0,
            "EndTime": now + 60.0,
            "Period": PERIOD_SECONDS,
            "Statistics": ["Maximum", "Minimum"],
        }),
    )
    .await;
    stats["Datapoints"].as_array().cloned().unwrap_or_default()
}

/// Wait until one service's `metric` series holds a datapoint whose `Maximum` is `wanted`,
/// and answer every datapoint of that series as it then stands.
async fn series_reaching(
    router: &MultiServiceRouter,
    row: &Row,
    metric: &str,
    wanted: f64,
) -> Vec<Value> {
    let case = row.case;
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let points = service_datapoints(router, row.service, metric).await;
        let reached = points
            .iter()
            .any(|point| point["Maximum"].as_f64() == Some(wanted));
        if reached || Instant::now() >= deadline {
            assert!(
                reached,
                "case {case:?}: expected a {metric} datapoint reading {wanted} for \
                 {}, got {points:?}",
                row.service
            );
            return points;
        }
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The service as `DescribeServices` reports it.
async fn describe(router: &MultiServiceRouter, row: &Row) -> Value {
    let body = ok(
        router,
        "DescribeServices",
        json!({ "cluster": CLUSTER, "services": [row.service] }),
        row.case,
    )
    .await;
    assert!(
        list(&body, "failures", row.case).is_empty(),
        "case {:?}: DescribeServices must report no failure, got {body}",
        row.case
    );
    list(&body, "services", row.case)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {:?}: expected the service back, got {body}", row.case))
}

/// Wait until one row's service holds exactly the tasks it wants, and answer it as it is then
/// reported.
async fn converged(router: &MultiServiceRouter, row: &Row) -> Value {
    let case = row.case;
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let service = describe(router, row).await;
        let running = service["runningCount"].as_i64();
        let pending = service["pendingCount"].as_i64();
        let converged = running == Some(row.desired) && pending == Some(0);
        if converged || Instant::now() >= deadline {
            assert!(
                converged,
                "case {case:?}: expected runningCount {} with pendingCount 0, got \
                 runningCount {running:?}/pendingCount {pending:?} in {service}",
                row.desired
            );
            return service;
        }
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The ARNs of the tasks of one row's service that are wanted RUNNING, sorted, with every
/// one of them asserted to BE running — the tasks that service itself ran, whose count is the
/// ceiling its own series may report.
async fn running_tasks(router: &MultiServiceRouter, row: &Row) -> Vec<String> {
    let case = row.case;
    let listed = ok(
        router,
        "ListTasks",
        json!({
            "cluster": CLUSTER,
            "serviceName": row.service,
            "desiredStatus": "RUNNING",
        }),
        case,
    )
    .await;
    let arns: Vec<Value> = list(&listed, "taskArns", case);
    if arns.is_empty() {
        return Vec::new();
    }
    let described = ok(
        router,
        "DescribeTasks",
        json!({ "cluster": CLUSTER, "tasks": arns }),
        case,
    )
    .await;
    let tasks = list(&described, "tasks", case);
    let mut held = Vec::new();
    for task in &tasks {
        assert_eq!(
            task["lastStatus"].as_str(),
            Some("RUNNING"),
            "case {case:?}: a task of a converged service is running, got {task}"
        );
        assert_eq!(
            task["launchType"].as_str(),
            Some("FARGATE"),
            "case {case:?}: the task runs the launch type the service was created with, got \
             {task}"
        );
        held.push(string(task, "taskArn", case));
    }
    held.sort();
    held
}

/// RULE: every service of a Container-Insights cluster converges on its own `desiredCount`,
/// and the namespace carries that count per service.
#[tokio::test]
async fn each_services_insights_task_counts_follow_its_own_desired_count() {
    let router = emulator();
    let (subnet, group) = network(&router).await;
    ok(
        &router,
        "CreateCluster",
        json!({
            "clusterName": CLUSTER,
            "settings": [{ "name": "containerInsights", "value": "enabled" }],
        }),
        "the cluster every row's service runs in",
    )
    .await;
    // The setting the cluster was created with is the one it reports, which is what a client
    // confirms before it watches the namespace.
    let described = ok(
        &router,
        "DescribeClusters",
        json!({ "clusters": [CLUSTER], "include": ["SETTINGS"] }),
        "the cluster's own settings",
    )
    .await;
    let settings: BTreeMap<String, String> =
        list(&described["clusters"][0], "settings", "the cluster's own settings")
            .into_iter()
            .filter_map(|setting| {
                Some((
                    setting["name"].as_str()?.to_string(),
                    setting["value"].as_str()?.to_string(),
                ))
            })
            .collect();
    assert_eq!(
        settings.get("containerInsights").map(String::as_str),
        Some("enabled"),
        "the cluster reports the setting it was created with, got {described}"
    );

    ok(
        &router,
        "RegisterTaskDefinition",
        json!({
            "family": FAMILY,
            "networkMode": "awsvpc",
            "requiresCompatibilities": ["FARGATE"],
            "cpu": "256",
            "memory": "512",
            "containerDefinitions": [{
                "name": "app",
                "image": IMAGE,
                "essential": true,
                "command": STAYS_UP,
            }],
        }),
        "the revision every row's service runs",
    )
    .await;

    // Every row's service is created up front, so the clock-owed work of all of them falls due
    // at once rather than one row's budget after another's.
    for row in ROWS {
        let created = ok(
            &router,
            "CreateService",
            json!({
                "cluster": CLUSTER,
                "serviceName": row.service,
                "taskDefinition": FAMILY,
                "desiredCount": row.desired,
                "launchType": "FARGATE",
                "networkConfiguration": {
                    "awsvpcConfiguration": {
                        "subnets": [subnet],
                        "securityGroups": [group],
                        "assignPublicIp": "ENABLED",
                    }
                },
            }),
            row.case,
        )
        .await;
        assert_eq!(
            created["service"]["desiredCount"].as_i64(),
            Some(row.desired),
            "case {:?}: the create keeps the count the caller asked for, got {created}",
            row.case
        );
    }

    let mut held: Vec<(&Row, Vec<String>)> = Vec::new();
    for row in ROWS {
        let case = row.case;
        let service = converged(&router, row).await;
        assert_eq!(
            count(&service, "desiredCount", case),
            row.desired,
            "case {case:?}: the converged service still wants the count it was asked for, got \
             {service}"
        );
        let ran = running_tasks(&router, row).await;
        assert_eq!(
            ran.len() as i64,
            row.desired,
            "case {case:?}: the service holds one task per task it wants, got {ran:?}"
        );
        held.push((row, ran));
    }

    // The count a converged service holds STAYS held: a container whose whole program is a
    // wait is still there, so the scheduler is replacing nothing — watched over a window, with
    // the task set re-read inside it, so what the assertion rests on is the same tasks running
    // read after read rather than a count caught between two replacements.
    let until = Instant::now() + WATCH_WINDOW;
    while Instant::now() < until {
        ::tokio::time::sleep(POLL_INTERVAL).await;
        for (row, ran) in &held {
            let case = row.case;
            let service = describe(&router, row).await;
            assert_eq!(
                count(&service, "runningCount", case),
                row.desired,
                "case {case:?}: a converged service goes on holding its count, got {service}"
            );
            assert_eq!(
                &running_tasks(&router, row).await,
                ran,
                "case {case:?}: the very tasks it converged on are the ones still running"
            );
        }
    }

    for (row, ran) in &held {
        let case = row.case;
        // The two counts of that service's own series, each reaching the count the service
        // itself reports.
        for (metric, wanted) in [
            (RUNNING_TASK_COUNT, row.desired),
            (DESIRED_TASK_COUNT, row.desired),
        ] {
            let points = series_reaching(&router, row, metric, wanted as f64).await;
            for point in &points {
                // Per-service scoping: a datapoint of one service never carries more than the
                // tasks that service ran.
                let maximum = point["Maximum"].as_f64().unwrap_or(f64::INFINITY);
                assert!(
                    maximum <= ran.len() as f64,
                    "case {case:?}: a {metric} datapoint of {} must not exceed the {} \
                     task(s) it ran, got {point}",
                    row.service,
                    ran.len()
                );
                assert!(
                    point["Minimum"].as_f64().is_some_and(|value| value >= 0.0),
                    "case {case:?}: every {metric} datapoint carries its Minimum, got {point}"
                );
                assert_eq!(
                    point["Unit"],
                    json!(COUNT_UNIT),
                    "case {case:?}: expected {metric} in {COUNT_UNIT}, got {point}"
                );
            }
        }
    }

    // The counts are per SERVICE, so no row's series carries another row's count: the cluster
    // runs services of four different sizes at once.
    for row in ROWS {
        let case = row.case;
        let points = service_datapoints(&router, row.service, DESIRED_TASK_COUNT).await;
        for point in &points {
            assert_eq!(
                point["Maximum"].as_f64(),
                Some(row.desired as f64),
                "case {case:?}: {} wants {} task(s) throughout, got {point}",
                row.service,
                row.desired
            );
        }
        assert_eq!(
            string(&describe(&router, row).await, "serviceName", case),
            row.service,
            "case {case:?}: the series above belong to the service that was read back"
        );
    }

    // And the series of all of them are ONE collection of the cluster: each metric has a
    // minute every row's series carries, reading that row's OWN count — which is what a
    // client comparing two services of a cluster at one instant reads.
    for metric in [RUNNING_TASK_COUNT, DESIRED_TASK_COUNT] {
        common_minute(&router, metric).await;
    }
}

/// Wait until one `metric` has a minute every row's series reports, each reading that row's
/// own count, and answer the instant that minute opens at.
async fn common_minute(router: &MultiServiceRouter, metric: &str) -> String {
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let mut per_row: Vec<(&Row, BTreeMap<String, f64>)> = Vec::new();
        for row in ROWS {
            let points = service_datapoints(router, row.service, metric).await;
            per_row.push((
                row,
                points
                    .iter()
                    .filter_map(|point| {
                        // The instant a datapoint stands at, as the response spells it: a
                        // minute two series share is the same instant in both.
                        Some((point["Timestamp"].to_string(), point["Maximum"].as_f64()?))
                    })
                    .collect(),
            ));
        }
        let shared = per_row
            .first()
            .map(|(_, held)| held.keys().cloned().collect::<Vec<String>>())
            .unwrap_or_default()
            .into_iter()
            .find(|minute| {
                per_row.iter().all(|(row, held)| {
                    held.get(minute) == Some(&(row.desired as f64))
                })
            });
        if let Some(minute) = shared {
            return minute;
        }
        assert!(
            Instant::now() < deadline,
            "expected one minute of {metric} where every service reports its own count, got \
             {per_row:?}"
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}
