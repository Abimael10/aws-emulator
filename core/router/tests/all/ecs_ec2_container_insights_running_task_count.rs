// SPDX-License-Identifier: CC0-1.0

//! What a client watching an EC2-BACKED Amazon ECS cluster sees in CloudWatch, and what a
//! scale request below zero earns it — driven end to end over Amazon EC2, Amazon ECS and
//! CloudWatch on ONE shared engine, so the capacity is an instance Amazon EC2 launched, the
//! tasks are placed by the Amazon ECS scheduler, and every number asserted is read back
//! through CloudWatch's own `GetMetricStatistics`.
//!
//! # The rules under test
//!
//! 1. **A RUNNING Amazon EC2 instance whose user data configures the container agent with a
//!    cluster of the account is registered capacity of that cluster, and every service placed
//!    on that capacity has its running task count published into the Container Insights
//!    namespace, dimensioned by the cluster and the service, as the count the Amazon ECS
//!    scheduler holds the service at — whichever call set that count.**
//!
//!    `Cluster$settings` "indicates whether CloudWatch Container Insights is on or off for a
//!    cluster" (`ClusterSetting$name` is `containerInsights`), "the EC2 launch type runs your
//!    tasks on Amazon EC2 instances registered to your cluster"
//!    (`smithy_query(query="shape",shape="LaunchType",model_path="services/service-com-amazonaws-ecs-amazonec2containerservicev20141113/smithy-model/com-amazonaws-ecs-amazonec2containerservicev20141113.json")`),
//!    and `Service$desiredCount` is "the desired number of instantiations of the task
//!    definition to keep running on the service" — which the scheduler then keeps running, so
//!    the count the series carries is the count `DescribeServices` reports for the same
//!    records.
//!
//!    What the rows span is what the rule generalises over: the COUNT the service settles at
//!    (one, two, three and none) and the CALL that set it (`CreateService`, an `UpdateService`
//!    that scales out, and an `UpdateService` that scales in).
//!
//! 2. **`UpdateService` refuses a `desiredCount` below zero with `InvalidParameterException`
//!    and HTTP 400, leaving the stored count as it was — a request the service rejects on its
//!    own parameters, so it is answered whether or not the cluster holds capacity to place a
//!    task on.**
//!
//!    `UpdateService` declares `InvalidParameterException` with HTTP 400
//!    (`smithy_query(query="operation",operation="UpdateService",model_path="services/service-com-amazonaws-ecs-amazonec2containerservicev20141113/smithy-model/com-amazonaws-ecs-amazonec2containerservicev20141113.json")`:
//!    "The specified parameter isn't valid. Review the available parameters for the API
//!    request"), and `UpdateServiceRequest$desiredCount` is "the number of instantiations of
//!    the task definition to place and keep running" — a number of tasks, so no value below
//!    none is one.
//!
//!    Its rows span both axes the rule generalises over: HOW FAR below the bound the count is
//!    (the smallest `Integer` included) and WHETHER the cluster has any capacity at all.
//!
//! # Why this suite lives here
//!
//! Three crates have to agree for either rule to be observable: Amazon EC2 owns the instance
//! and the user data that joins it to a cluster, Amazon ECS owns the registration, the
//! placement and the scale refusal, and CloudWatch owns the metric store the series is
//! published into and read back from.
//!
//! Every wait polls the CONDITION rather than a fixed time, and the deadlines are generous on
//! purpose: they are here so a failure is a failure rather than a hang.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder and element reader this test crate keeps for query-protocol suites
// (`all/query_wire.rs`: "Nothing new should write another one — call this module").
use crate::query_wire::{element, form_encode};

/// The region every request below is signed for.
const REGION: &str = "us-west-2";

/// The ec2Query API version an Amazon EC2 request body carries.
const EC2_API_VERSION: &str = "2016-11-15";

/// The statuses a call of this suite is answered with.
const OK: u16 = 200;
const BAD_REQUEST: u16 = 400;

/// The namespace Amazon ECS publishes its Container Insights metrics in, the dimensions every
/// one of them carries and the three this suite reads — spelled out here rather than imported
/// from the implementation, so a wrong constant there cannot make a test pass. Their source is
/// the CloudWatch User Guide's "Amazon ECS Container Insights metrics" table, whose
/// service-level rows are dimensioned by `ClusterName` and `ServiceName` and whose
/// cluster-level rows by `ClusterName` alone.
const NAMESPACE: &str = "ECS/ContainerInsights";
const CLUSTER_DIMENSION: &str = "ClusterName";
const SERVICE_DIMENSION: &str = "ServiceName";
const RUNNING_TASK_COUNT: &str = "RunningTaskCount";
const DESIRED_TASK_COUNT: &str = "DesiredTaskCount";
const CONTAINER_INSTANCE_COUNT: &str = "ContainerInstanceCount";

/// The cluster the EC2-launched capacity joins, and a second cluster of the same account that
/// is given none — the axis the refusal rows span.
const CLUSTER: &str = "insightsec2fleet";
const BARE_CLUSTER: &str = "insightsec2bare";

/// The `containerInsights` value that turns the namespace on for a cluster.
const INSIGHTS_ENABLED: &str = "enabled";

/// The AMI the launch below boots. Well-formed (`ami-<17 hexadecimal digits>`), which is all a
/// launch needs; which AMI carries the container agent is not a fact this emulator publishes.
const IMAGE_ID: &str = "ami-0123456789abcdef0";

/// The instance type the capacity comes from: large enough that the resources it offers are
/// never what keeps a task of this suite from being placed.
const INSTANCE_TYPE: &str = "m5.4xlarge";

/// The network the instance is launched into.
const VPC_CIDR: &str = "10.90.0.0/16";
const SUBNET_CIDR: &str = "10.90.0.0/20";

/// The image every container below names, and a program that stays up — so a task that is
/// running is still running while the series is read.
const IMAGE: &str = "public.ecr.aws/nginx/nginx:latest";
const STAYS_UP: [&str; 3] = ["sh", "-c", "while true; do sleep 1; done"];

/// How much memory each container reserves. Small, so the instance's own resources are never
/// the reason a row's tasks are not all placed.
const CONTAINER_MEMORY: i64 = 128;

/// The modelled error code and the sentence Amazon ECS states the bound in. Both spelled out,
/// because they are the contract a client keys on.
const INVALID_PARAMETER: &str = "InvalidParameterException";
const TASK_COUNT_NEGATIVE: &str = "Task count can not be lesser than 0";

/// How often a poll re-reads, and how long it keeps trying.
///
/// Generous: a Container Insights datapoint is stored at the one-minute resolution every
/// metric a service publishes for itself takes, so a WHOLE period that reports one steady
/// count only exists once a clock minute has passed entirely inside that steady state, and
/// this suite runs beside every other test of the workspace. Every poll stops the instant its
/// own condition holds.
const POLL: Duration = Duration::from_millis(100);
const DEADLINE: Duration = Duration::from_secs(240);

/// One row of the first rule: the service, the count `CreateService` asks for, the count an
/// `UpdateService` then asks for (and `None` for a row that makes no update), and the count
/// the service therefore settles at.
struct Row {
    case: &'static str,
    service: &'static str,
    created_with: i64,
    updated_to: Option<i64>,
}

impl Row {
    /// The count the Amazon ECS scheduler holds the service at once this row's calls are in.
    fn settles_at(&self) -> i64 {
        self.updated_to.unwrap_or(self.created_with)
    }

    /// The most tasks this row ever asks the scheduler for: the ceiling the published series
    /// may never report more than.
    fn ceiling(&self) -> i64 {
        self.created_with.max(self.settles_at())
    }
}

const ROWS: &[Row] = &[
    Row {
        case: "one task, placed at create",
        service: "insightsone",
        created_with: 1,
        updated_to: None,
    },
    Row {
        case: "two tasks, placed at create",
        service: "insightstwo",
        created_with: 2,
        updated_to: None,
    },
    Row {
        case: "three tasks, placed at create",
        service: "insightsthree",
        created_with: 3,
        updated_to: None,
    },
    Row {
        case: "a service scaled out from none by UpdateService",
        service: "insightsscaledout",
        created_with: 0,
        updated_to: Some(2),
    },
    Row {
        case: "a service scaled in to none by UpdateService",
        service: "insightsscaledin",
        created_with: 2,
        updated_to: Some(0),
    },
];

/// How far below the bound the refused counts sit, the smallest `Integer` included — the
/// sentence states the bound, never the value, so every one of them earns the same text.
const NEGATIVE_COUNTS: [i64; 4] = [-1, -2, -1000, i32::MIN as i64];

/// One emulator carrying Amazon EC2, Amazon ECS and CloudWatch over ONE shared engine, exactly
/// as `core/router/src/lib.rs` mounts them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
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

/// Drive one ec2Query action that must succeed, and answer its XML body.
async fn ec2(
    router: &MultiServiceRouter,
    action: &str,
    params: &[(&str, &str)],
    case: &str,
) -> String {
    let mut body = format!("Action={action}&Version={EC2_API_VERSION}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://ec2.{REGION}.amazonaws.com/"))
        .header("host", format!("ec2.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDECSTEST/20260101/{REGION}\
                 /ec2/aws4_request, SignedHeaders=host, Signature=00"
            ),
        )
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, OK,
        "case {case:?}: expected Amazon EC2 {action} to answer {OK}, got {status} with {text}"
    );
    text
}

/// One awsJson1_1 call to Amazon ECS, answered with its status and parsed body.
async fn ecs(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("ecs.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header(
            "x-amz-target",
            format!("AmazonEC2ContainerServiceV20141113.{operation}"),
        )
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDECSTEST/20260101/{REGION}\
                 /ecs/aws4_request, SignedHeaders=host;x-amz-target, Signature=00"
            ),
        )
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
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
    (status, parsed)
}

/// One Amazon ECS call that must succeed, answered with its parsed body.
async fn ecs_ok(router: &MultiServiceRouter, operation: &str, body: &Value, case: &str) -> Value {
    let (status, parsed) = ecs(router, operation, body).await;
    assert_eq!(
        status, OK,
        "case {case:?}: expected Amazon ECS {operation} to answer {OK}, got {status} with \
         {parsed}"
    );
    parsed
}

/// One CloudWatch call that must succeed, the way a client makes it.
async fn cloudwatch(
    router: &MultiServiceRouter,
    operation: &str,
    body: &Value,
    case: &str,
) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("monitoring.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header(
            "x-amz-target",
            format!("GraniteServiceVersion20100801.{operation}"),
        )
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDECSTEST/20260101/{REGION}\
                 /monitoring/aws4_request, SignedHeaders=host;x-amz-target, Signature=00"
            ),
        )
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
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
        status, OK,
        "case {case:?}: expected CloudWatch {operation} to answer {OK}, got {status} with \
         {parsed}"
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

/// Every datapoint CloudWatch holds for one series of this namespace, over a window wide
/// enough to hold every collection this suite's clusters have made.
///
/// `Period` is the one-minute granularity these datapoints are stored at, so each datapoint is
/// one whole collection period of the series.
async fn datapoints(
    router: &MultiServiceRouter,
    dimensions: &Value,
    metric: &str,
    case: &str,
) -> Vec<Value> {
    let now = epoch_seconds();
    let answer = cloudwatch(
        router,
        "GetMetricStatistics",
        &json!({
            "Namespace": NAMESPACE,
            "MetricName": metric,
            "Dimensions": dimensions,
            "StartTime": now - 900.0,
            "EndTime": now + 300.0,
            "Period": 60,
            "Statistics": ["Minimum", "Maximum", "SampleCount"],
        }),
        case,
    )
    .await;
    answer["Datapoints"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// The dimension list a SERVICE-level metric of one service carries.
fn service_dimensions(cluster: &str, service: &str) -> Value {
    json!([
        { "Name": CLUSTER_DIMENSION, "Value": cluster },
        { "Name": SERVICE_DIMENSION, "Value": service },
    ])
}

/// The dimension list a CLUSTER-level metric carries.
fn cluster_dimensions(cluster: &str) -> Value {
    json!([{ "Name": CLUSTER_DIMENSION, "Value": cluster }])
}

/// The largest value any datapoint of a window reports, and `None` for a series that holds
/// none at all.
fn highest(points: &[Value]) -> Option<f64> {
    points
        .iter()
        .filter_map(|point| point["Maximum"].as_f64())
        .fold(None, |held: Option<f64>, value| {
            Some(held.map_or(value, |held| held.max(value)))
        })
}

/// Whether some whole period of the series reported `expected` and nothing else — a datapoint
/// whose minimum and maximum are both that count, which is a full collection period spent at
/// it.
fn steady_at(points: &[Value], expected: f64) -> bool {
    points.iter().any(|point| {
        point["Minimum"].as_f64() == Some(expected) && point["Maximum"].as_f64() == Some(expected)
    })
}

/// Look again until some whole period of `metric` reports exactly `expected`, checking at
/// every look that no period ever reported more than `ceiling` — the most tasks the service
/// was ever asked for.
async fn settle_series(
    router: &MultiServiceRouter,
    cluster: &str,
    dimensions: &Value,
    metric: &str,
    expected: i64,
    ceiling: i64,
    case: &str,
) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        // One Amazon ECS read per look, so the collection the interval owes is taken even
        // where that service's own clock has nothing else left to move.
        describe_cluster(router, cluster, case).await;
        let points = datapoints(router, dimensions, metric, case).await;
        #[allow(clippy::cast_precision_loss)]
        let ceiling_value = ceiling as f64;
        if let Some(highest) = highest(&points) {
            assert!(
                highest <= ceiling_value,
                "case {case:?}: the {metric} series must never report more than {ceiling}, got \
                 {highest} in {points:?}"
            );
        }
        #[allow(clippy::cast_precision_loss)]
        let wanted = expected as f64;
        if steady_at(&points, wanted) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected a whole {metric} period reporting {expected}, got {points:?}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// One cluster as `DescribeClusters` reports it.
async fn describe_cluster(router: &MultiServiceRouter, cluster: &str, case: &str) -> Value {
    let answer = ecs_ok(
        router,
        "DescribeClusters",
        &json!({ "clusters": [cluster] }),
        case,
    )
    .await;
    answer["clusters"]
        .as_array()
        .and_then(|found| found.first())
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected {cluster} back, got {answer}"))
}

/// One service as `DescribeServices` reports it.
async fn described_service(
    router: &MultiServiceRouter,
    cluster: &str,
    service: &str,
    case: &str,
) -> Value {
    let answer = ecs_ok(
        router,
        "DescribeServices",
        &json!({ "cluster": cluster, "services": [service] }),
        case,
    )
    .await;
    answer["services"]
        .as_array()
        .and_then(|services| services.first())
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected {service} back, got {answer}"))
}

/// An integer member of a described resource.
fn number(found: &Value, member: &str, case: &str) -> i64 {
    found[member]
        .as_i64()
        .unwrap_or_else(|| panic!("case {case:?}: expected an integer {member}, got {found}"))
}

/// Look again until `service` reports `wanted` running tasks, and answer what it last read.
async fn settle_running_count(
    router: &MultiServiceRouter,
    cluster: &str,
    service: &str,
    wanted: i64,
    case: &str,
) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let found = described_service(router, cluster, service, case).await;
        let running = number(&found, "runningCount", case);
        if running == wanted {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {service} to run {wanted} task(s), got {running} in {found}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// Create the VPC and subnet the instances below are launched into, through Amazon EC2's own
/// API, and answer the subnet's id.
async fn network(router: &MultiServiceRouter, case: &str) -> String {
    let vpc = element(
        &ec2(router, "CreateVpc", &[("CidrBlock", VPC_CIDR)], case).await,
        "vpcId",
    );
    let zone = format!("{REGION}a");
    let subnet = ec2(
        router,
        "CreateSubnet",
        &[
            ("VpcId", vpc.as_str()),
            ("CidrBlock", SUBNET_CIDR),
            ("AvailabilityZone", zone.as_str()),
        ],
        case,
    )
    .await;
    element(&subnet, "subnetId")
}

/// Launch ONE instance whose user data configures the container agent for `cluster`, and
/// answer the instance id Amazon EC2 minted.
///
/// The user data is the assignment the agent's configuration file carries
/// (`echo ECS_CLUSTER=<cluster> >> /etc/ecs/ecs.config`), which is the documented way to join
/// an instance to a cluster.
async fn launch_into(router: &MultiServiceRouter, subnet: &str, cluster: &str, case: &str) -> String {
    let user_data = format!("#!/bin/bash\necho ECS_CLUSTER={cluster} >> /etc/ecs/ecs.config\n");
    let launched = ec2(
        router,
        "RunInstances",
        &[
            ("ImageId", IMAGE_ID),
            ("InstanceType", INSTANCE_TYPE),
            ("SubnetId", subnet),
            ("MinCount", "1"),
            ("MaxCount", "1"),
            ("UserData", user_data.as_str()),
        ],
        case,
    )
    .await;
    element(&launched, "instanceId")
}

/// Create one cluster with Container Insights turned on.
async fn create_cluster(router: &MultiServiceRouter, cluster: &str, case: &str) {
    ecs_ok(
        router,
        "CreateCluster",
        &json!({
            "clusterName": cluster,
            "settings": [{ "name": "containerInsights", "value": INSIGHTS_ENABLED }],
        }),
        case,
    )
    .await;
}

/// Register one revision of `family` whose container stays up.
async fn register_definition(router: &MultiServiceRouter, family: &str, case: &str) {
    ecs_ok(
        router,
        "RegisterTaskDefinition",
        &json!({
            "family": family,
            "containerDefinitions": [{
                "name": "app",
                "image": IMAGE,
                "memory": CONTAINER_MEMORY,
                "essential": true,
                "command": STAYS_UP,
            }],
        }),
        case,
    )
    .await;
}

/// Look again until `cluster` reports capacity for exactly the instances `wanted` names, and
/// answer the container instances it last described.
async fn settle_capacity(
    router: &MultiServiceRouter,
    cluster: &str,
    wanted: &[String],
    case: &str,
) -> Vec<Value> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let listed = ecs_ok(
            router,
            "ListContainerInstances",
            &json!({ "cluster": cluster }),
            case,
        )
        .await;
        let arns: Vec<Value> = listed["containerInstanceArns"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if arns.len() == wanted.len() {
            let described = ecs_ok(
                router,
                "DescribeContainerInstances",
                &json!({ "cluster": cluster, "containerInstances": arns }),
                case,
            )
            .await;
            let instances: Vec<Value> = described["containerInstances"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let mut reported: Vec<String> = instances
                .iter()
                .filter_map(|instance| instance["ec2InstanceId"].as_str().map(str::to_string))
                .collect();
            reported.sort();
            let mut expected = wanted.to_vec();
            expected.sort();
            if reported == expected {
                return instances;
            }
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {cluster} to hold capacity for {wanted:?}, got {arns:?}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// RULE 1: on a cluster whose capacity is an Amazon EC2 instance that joined it through its own
/// user data, each service's Container Insights `RunningTaskCount` series reports the count the
/// scheduler holds that service at — whichever call set it.
#[tokio::test]
async fn ec2_backed_container_insights_running_task_count_follows_each_services_desired_count() {
    let router = emulator();
    let setup = "the EC2-backed cluster";

    let subnet = network(&router, setup).await;
    create_cluster(&router, CLUSTER, setup).await;
    let instance = launch_into(&router, &subnet, CLUSTER, setup).await;

    // The capacity exists where Amazon ECS holds it, and it is the very instance Amazon EC2
    // launched: the registration is the agent's, keyed on the cluster its user data named.
    let registered =
        settle_capacity(&router, CLUSTER, std::slice::from_ref(&instance), setup).await;
    assert_eq!(
        registered.len(),
        1,
        "case {setup:?}: expected one container instance, got {registered:?}"
    );
    assert_eq!(
        registered[0]["ec2InstanceId"].as_str(),
        Some(instance.as_str()),
        "case {setup:?}: expected the launched instance's capacity, got {:?}",
        registered[0]
    );

    // Every service of the table, created and then scaled, before anything is read: the series
    // is a collection of the counts the records hold, so the reads below are taken once the
    // scheduler has been given all of its work.
    for row in ROWS {
        register_definition(&router, row.service, row.case).await;
        ecs_ok(
            &router,
            "CreateService",
            &json!({
                "cluster": CLUSTER,
                "serviceName": row.service,
                "taskDefinition": row.service,
                "desiredCount": row.created_with,
                "launchType": "EC2",
            }),
            row.case,
        )
        .await;
    }
    for row in ROWS {
        if let Some(count) = row.updated_to {
            let updated = ecs_ok(
                &router,
                "UpdateService",
                &json!({ "cluster": CLUSTER, "service": row.service, "desiredCount": count }),
                row.case,
            )
            .await;
            assert_eq!(
                number(&updated["service"], "desiredCount", row.case),
                count,
                "case {}: expected UpdateService to answer with the new count, got {updated}",
                row.case
            );
        }
    }

    // The scheduler places what each service asks for on the registered capacity...
    for row in ROWS {
        settle_running_count(&router, CLUSTER, row.service, row.settles_at(), row.case).await;
        let found = described_service(&router, CLUSTER, row.service, row.case).await;
        assert_eq!(
            number(&found, "desiredCount", row.case),
            row.settles_at(),
            "case {}: expected the stored desiredCount to be {}, got {found}",
            row.case,
            row.settles_at()
        );
    }

    // ...and both of the service-level series report that count for a whole collection period,
    // never more than the service was ever asked for.
    for row in ROWS {
        let dimensions = service_dimensions(CLUSTER, row.service);
        settle_series(
            &router,
            CLUSTER,
            &dimensions,
            RUNNING_TASK_COUNT,
            row.settles_at(),
            row.ceiling(),
            row.case,
        )
        .await;
        settle_series(
            &router,
            CLUSTER,
            &dimensions,
            DESIRED_TASK_COUNT,
            row.settles_at(),
            row.ceiling(),
            row.case,
        )
        .await;
    }

    // The cluster-level series carries the capacity that joined it, which is the number
    // `DescribeClusters` reports for the same records.
    let cluster = describe_cluster(&router, CLUSTER, setup).await;
    let instances = number(&cluster, "registeredContainerInstancesCount", setup);
    assert_eq!(
        instances, 1,
        "case {setup:?}: expected one registered container instance, got {cluster}"
    );
    settle_series(
        &router,
        CLUSTER,
        &cluster_dimensions(CLUSTER),
        CONTAINER_INSTANCE_COUNT,
        instances,
        instances,
        setup,
    )
    .await;
}

/// RULE 2: `UpdateService` refuses a `desiredCount` below zero with `InvalidParameterException`
/// and HTTP 400, whatever capacity the cluster holds, and leaves the stored count as it was.
#[tokio::test]
async fn update_service_refuses_a_negative_desired_count_whatever_the_clusters_capacity() {
    let router = emulator();
    let setup = "the two clusters";

    // One cluster whose capacity is an EC2 instance that joined it, and one with none at all.
    let subnet = network(&router, setup).await;
    create_cluster(&router, CLUSTER, setup).await;
    create_cluster(&router, BARE_CLUSTER, setup).await;
    let instance = launch_into(&router, &subnet, CLUSTER, setup).await;
    settle_capacity(&router, CLUSTER, &[instance], setup).await;

    // The second cluster holds no capacity at all, which is what makes the refusal rows below
    // a question about the request's own parameters rather than about where a task could run.
    let bare = describe_cluster(&router, BARE_CLUSTER, setup).await;
    assert_eq!(
        number(&bare, "registeredContainerInstancesCount", setup),
        0,
        "case {setup:?}: expected {BARE_CLUSTER} to hold no capacity, got {bare}"
    );

    const SERVICE: &str = "scaled";
    const HELD_COUNT: i64 = 1;
    register_definition(&router, SERVICE, setup).await;
    for cluster in [CLUSTER, BARE_CLUSTER] {
        ecs_ok(
            &router,
            "CreateService",
            &json!({
                "cluster": cluster,
                "serviceName": SERVICE,
                "taskDefinition": SERVICE,
                "desiredCount": HELD_COUNT,
                "launchType": "EC2",
            }),
            setup,
        )
        .await;
    }

    for cluster in [CLUSTER, BARE_CLUSTER] {
        for count in NEGATIVE_COUNTS {
            let case = format!("UpdateService with desiredCount {count} on {cluster}");
            let (status, parsed) = ecs(
                &router,
                "UpdateService",
                &json!({ "cluster": cluster, "service": SERVICE, "desiredCount": count }),
            )
            .await;
            assert_eq!(
                status, BAD_REQUEST,
                "case {case:?}: expected {BAD_REQUEST}, got {status} with {parsed}"
            );
            let code = parsed["__type"]
                .as_str()
                .unwrap_or_default()
                .rsplit('#')
                .next()
                .unwrap_or_default()
                .to_string();
            assert_eq!(
                code, INVALID_PARAMETER,
                "case {case:?}: expected an {INVALID_PARAMETER}, got {parsed}"
            );
            let message = parsed["message"].as_str().unwrap_or_default();
            assert_eq!(
                message, TASK_COUNT_NEGATIVE,
                "case {case:?}: expected the bound stated as {TASK_COUNT_NEGATIVE:?}, got \
                 {message:?}"
            );

            // Nothing of the refused request is written: the service still wants what it did.
            let found = described_service(&router, cluster, SERVICE, &case).await;
            assert_eq!(
                number(&found, "desiredCount", &case),
                HELD_COUNT,
                "case {case:?}: expected the stored desiredCount to be untouched, got {found}"
            );
        }

        // The bound itself is accepted on either cluster — a service that keeps no task
        // running is a service — so the rule refuses what is below zero and nothing else.
        let case = format!("the bound itself on {cluster}");
        let answered = ecs_ok(
            &router,
            "UpdateService",
            &json!({ "cluster": cluster, "service": SERVICE, "desiredCount": 0 }),
            &case,
        )
        .await;
        assert_eq!(
            number(&answered["service"], "desiredCount", &case),
            0,
            "case {case:?}: expected the bound to be stored, got {answered}"
        );
    }
}
