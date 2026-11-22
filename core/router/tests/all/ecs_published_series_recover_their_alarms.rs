// SPDX-License-Identifier: CC0-1.0

//! The RECOVERY direction of an alarm built over a series Amazon ECS publishes: `OK` once the
//! datapoints that series carries stop breaching.
//!
//! The RULE under test: **a metric alarm over a series Amazon ECS publishes for a running
//! service — its Container Insights task counts, and the `AWS/ECS` percentages of its own
//! utilization — is created in `INSUFFICIENT_DATA` while the series it names holds nothing, and
//! then reports `OK` on the evaluation the clock owes it, with no call forcing the state, once
//! the datapoints that series holds stand on the NON-breaching side of the alarm's own
//! comparison; an alarm whose datapoints keep standing on the breaching side never reports it.**
//!
//! Where each half comes from. `PutMetricAlarm` states the start: "When this operation creates
//! an alarm, the alarm state is immediately set to `INSUFFICIENT_DATA`. The alarm is then
//! evaluated and its state is set appropriately"; `MetricAlarm$ComparisonOperator` is "the
//! arithmetic operation to use when comparing the specified statistic and threshold", which is
//! the condition that puts the alarm in `ALARM`, so its NEGATION over the graded datapoints is
//! what `OK` means; and `MetricAlarm$StateReasonData` is "an explanation for the alarm state, in
//! JSON format", which is where each row reads back the values the evaluation really graded. The
//! series are Amazon ECS's own: `Cluster$settings` "indicates whether CloudWatch Container
//! Insights is on or off for a cluster" (`ClusterSetting$name` is `containerInsights`), under
//! which a cluster publishes its service-level task counts, and a service publishes the CPU and
//! memory share of what its tasks were given in the base namespace for as long as it holds a
//! running task — each a PERCENTAGE, so no value of either is below none or past a hundred.
//!
//! # Why this suite lives here
//!
//! Two crates have to agree before any of it is observable: Amazon ECS owns the cluster, the
//! capacity, the service and the collection that publishes, and CloudWatch owns the metric store,
//! the alarm and the evaluation that grades it. The sibling router suites drive the ALARM
//! direction of the same chain (`ecs_container_insights_alarm_fans_out_to_its_granted_queues.rs`)
//! and the arrival of the series themselves (`ecs_service_utilization_metrics.rs`,
//! `ecs_ec2_container_insights_running_task_count.rs`); what only this one reaches is the
//! RECOVERY — the state a client waiting on a deployment gate or an autoscaling action waits for.
//!
//! The rows differ along what the rule generalises over: WHICH namespace and metric the alarm
//! watches (a Container Insights count, and both `AWS/ECS` percentages), the STATISTIC each
//! period is folded by, the DIRECTION and the THRESHOLD of the comparison whose negation the data
//! satisfies, HOW MANY tasks the service runs, and HOW MANY periods the look-back covers. The
//! recorded case — one `desiredCount=1` service whose running task count a client alarms under —
//! is one row.
//!
//! Nothing here reaches into either store: the cluster, the capacity and the services are created
//! through the Amazon ECS front door, and every alarm is written and read back through
//! CloudWatch's own `PutMetricAlarm` and `DescribeAlarms`. Every wait polls the CONDITION rather
//! than a span, and the deadlines are generous on purpose: they are here so a failure is a
//! failure rather than a hang.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for, and the account id an instance identity
/// document names.
const REGION: &str = "us-west-2";
const ACCOUNT: &str = "000000000000";

/// The two namespaces Amazon ECS publishes into, the three metrics this suite watches and the
/// dimensions they carry — spelled out here rather than imported from the implementation, so a
/// wrong constant there cannot make a test pass. Their source is the CloudWatch User Guide's
/// "Amazon ECS Container Insights metrics" table, whose service-level rows are dimensioned by
/// `ClusterName` and `ServiceName`, and the ECS developer guide's service utilization, published
/// under the base namespace on that same pair.
const INSIGHTS_NAMESPACE: &str = "ECS/ContainerInsights";
const SERVICE_NAMESPACE: &str = "AWS/ECS";
const RUNNING_TASK_COUNT: &str = "RunningTaskCount";
const CPU_UTILIZATION: &str = "CPUUtilization";
const MEMORY_UTILIZATION: &str = "MemoryUtilization";
const CLUSTER_DIMENSION: &str = "ClusterName";
const SERVICE_DIMENSION: &str = "ServiceName";

/// The three `StateValue` values this suite reads, of the modelled catalog
/// (`StateValue`: `ALARM | INSUFFICIENT_DATA | OK`).
const OK_STATE: &str = "OK";
const ALARM_STATE: &str = "ALARM";
const INSUFFICIENT_DATA: &str = "INSUFFICIENT_DATA";

/// The cluster every row's service lives in, with Container Insights turned on, and the one
/// container instance its tasks are placed on.
const CLUSTER: &str = "alarmrecovery";
const CONTAINER_INSIGHTS_SETTING: &str = "containerInsights";
const INSIGHTS_ENABLED: &str = "enabled";
const CAPACITY_INSTANCE_ID: &str = "i-0c0ffee0c0ffee0c0";

/// The image every container names, the memory its task definition reserves for it — which is
/// what the memory percentage is a share OF — and the program it runs: one that stays up, so a
/// task that is running is still running while its alarm is graded.
const IMAGE: &str = "public.ecr.aws/nginx/nginx:latest";
const CONTAINER_MEMORY: i64 = 128;
const STAYS_UP: [&str; 3] = ["sh", "-c", "while true; do sleep 1; done"];

/// The period every alarm of this suite is evaluated over, in seconds: the one-minute
/// granularity a service's own metrics are stored at ("Setting this to 60 specifies this metric
/// as regular resolution, which CloudWatch stores at 1-minute resolution",
/// `MetricDatum$StorageResolution`).
const ALARM_PERIOD: i32 = 60;

/// How often a poll looks again, and how long it keeps looking.
///
/// Generous: the datapoints arrive as the clock moves the tasks they count, the alarm's own
/// evaluation is a span behind each of them, and this suite runs beside every other test of the
/// workspace. Every poll stops the instant its own condition holds, and nothing asserts on how
/// long it took.
const POLL: Duration = Duration::from_millis(100);
const DEADLINE: Duration = Duration::from_secs(240);

/// How many further looks an assertion that a state NEVER arrives takes. Together with [`POLL`]
/// a window far longer than the spacing the collections publish at, so a state a row says is
/// never reported is one that was never reported rather than one read too early.
const QUIET_LOOKS: usize = 20;

/// One row: the series its alarm watches, how that series is folded and compared, and the service
/// whose records the series is collected off.
#[derive(Debug, Clone, Copy)]
struct Row {
    /// What this row is about, named by every assertion it makes.
    case: &'static str,
    /// The alarm, and the service and task definition family it watches.
    alarm: &'static str,
    service: &'static str,
    /// The series: the namespace it lives in and the metric name inside it.
    namespace: &'static str,
    metric: &'static str,
    /// The statistic each period is folded by, the comparison that fold is graded with and the
    /// value it is compared against. The comparison is the BREACHING condition, so a row's data
    /// reaches `OK` by failing it.
    statistic: &'static str,
    comparison: &'static str,
    threshold: f64,
    /// How many periods the look-back covers.
    evaluation_periods: i32,
    /// How many tasks the scheduler holds the service at.
    desired_count: i64,
}

impl Row {
    /// The dimension list the row's series is identified by: the cluster and the service, the
    /// pair that names one service's series in either namespace.
    fn dimensions(self) -> Value {
        json!([
            {"Name": CLUSTER_DIMENSION, "Value": CLUSTER},
            {"Name": SERVICE_DIMENSION, "Value": self.service},
        ])
    }

    /// Whether `value` BREACHES this row's comparison — the arithmetic the four operators this
    /// table uses name, out of the modelled catalog (`ComparisonOperator`).
    fn breaches(self, value: f64) -> bool {
        match self.comparison {
            "GreaterThanThreshold" => value > self.threshold,
            "GreaterThanOrEqualToThreshold" => value >= self.threshold,
            "LessThanThreshold" => value < self.threshold,
            "LessThanOrEqualToThreshold" => value <= self.threshold,
            other => panic!("case {:?}: this suite writes no {other} alarm", self.case),
        }
    }
}

/// The table: one row per shape the rule generalises over.
const ROWS: &[Row] = &[
    Row {
        // The recorded shape: a client alarms on the running task count falling under one task,
        // and waits for OK once the service really runs one.
        case: "a running task count at the one task the service holds",
        alarm: "recovery-running-one",
        service: "runsone",
        namespace: INSIGHTS_NAMESPACE,
        metric: RUNNING_TASK_COUNT,
        statistic: "Maximum",
        comparison: "LessThanThreshold",
        threshold: 1.0,
        evaluation_periods: 1,
        desired_count: 1,
    },
    Row {
        case: "a running task count past the one task the comparison allows, over two periods",
        alarm: "recovery-running-two",
        service: "runstwo",
        namespace: INSIGHTS_NAMESPACE,
        metric: RUNNING_TASK_COUNT,
        statistic: "Maximum",
        comparison: "LessThanOrEqualToThreshold",
        threshold: 1.0,
        evaluation_periods: 2,
        desired_count: 2,
    },
    Row {
        // A percentage is a share of what the containers were given, so no reading of it passes
        // a hundred: what decides this alarm is whether the series carries data at all.
        case: "a CPU utilization inside the percentage it is a share of",
        alarm: "recovery-cpu-share",
        service: "burnscpu",
        namespace: SERVICE_NAMESPACE,
        metric: CPU_UTILIZATION,
        statistic: "Average",
        comparison: "GreaterThanThreshold",
        threshold: 100.0,
        evaluation_periods: 1,
        desired_count: 1,
    },
    Row {
        case: "a memory utilization never below none, over three periods",
        alarm: "recovery-memory-share",
        service: "holdsmemory",
        namespace: SERVICE_NAMESPACE,
        metric: MEMORY_UTILIZATION,
        statistic: "Minimum",
        comparison: "LessThanThreshold",
        threshold: 0.0,
        evaluation_periods: 3,
        desired_count: 1,
    },
];

/// The row the NEGATIVE control watches: a service the scheduler holds at no tasks at all, whose
/// running task count therefore keeps standing on the breaching side of the same comparison the
/// first row recovers from.
const IDLE: Row = Row {
    case: "a service the scheduler holds at no tasks",
    alarm: "recovery-never",
    service: "runsnone",
    namespace: INSIGHTS_NAMESPACE,
    metric: RUNNING_TASK_COUNT,
    statistic: "Maximum",
    comparison: "LessThanThreshold",
    threshold: 1.0,
    evaluation_periods: 1,
    desired_count: 0,
};

/// One emulator carrying Amazon ECS and CloudWatch over ONE shared engine, exactly as
/// `core/router/src/lib.rs` mounts them.
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
        service_com_amazonaws_cloudwatch_graniteserviceversion20100801::EmulatedService::new(
            engine,
        ),
    );
    router
}

/// A SigV4 `Authorization` whose credential scope names `signing_name` in [`REGION`], which is
/// what routes the request. The signature is never checked: the router above runs with request
/// authentication off.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

/// One `X-Amz-Target` RPC call that must answer 200, with its parsed body.
async fn rpc(
    router: &MultiServiceRouter,
    case: &str,
    signing_name: &str,
    content_type: &str,
    target: &str,
    body: &Value,
) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("authorization", authorization(signing_name))
        .header("content-type", content_type)
        .header("x-amz-target", target)
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {target}, got {status} with {parsed}"
    );
    parsed
}

/// One Amazon ECS call (`awsJson1_1`).
async fn ecs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    rpc(
        router,
        case,
        "ecs",
        "application/x-amz-json-1.1",
        &format!("AmazonEC2ContainerServiceV20141113.{operation}"),
        body,
    )
    .await
}

/// One CloudWatch call (`awsJson1_0`).
async fn cloudwatch(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    rpc(
        router,
        case,
        "monitoring",
        "application/x-amz-json-1.0",
        &format!("GraniteServiceVersion20100801.{operation}"),
        body,
    )
    .await
}

/// An EC2 instance identity document, as the container agent reads it from the instance metadata
/// service. It carries the `accountId` and the `instanceId` a registration must supply.
fn identity_document() -> String {
    json!({
        "accountId": ACCOUNT,
        "instanceId": CAPACITY_INSTANCE_ID,
        "region": REGION,
        "instanceType": "m5.large",
        "availabilityZone": format!("{REGION}a"),
    })
    .to_string()
}

/// The signature of that document, which a registration presents BESIDE it: base64 text of the
/// length the signature of a document under a 2048-bit key takes. Nothing here signs anything.
fn identity_document_signature() -> String {
    "ZXhhbXBsZQ".repeat(35)
}

/// The cluster every row's service lives in, with Container Insights turned on, and one container
/// instance registered into it — the capacity the EC2 launch type places a task on ("the EC2
/// launch type runs your tasks on Amazon EC2 instances registered to your cluster").
async fn given_cluster_with_capacity(router: &MultiServiceRouter, case: &str) {
    ecs(
        router,
        case,
        "CreateCluster",
        &json!({
            "clusterName": CLUSTER,
            "settings": [{"name": CONTAINER_INSIGHTS_SETTING, "value": INSIGHTS_ENABLED}],
        }),
    )
    .await;
    ecs(
        router,
        case,
        "RegisterContainerInstance",
        &json!({
            "cluster": CLUSTER,
            "instanceIdentityDocument": identity_document(),
            "instanceIdentityDocumentSignature": identity_document_signature(),
        }),
    )
    .await;
}

/// One revision of the row's family, and the service the scheduler then holds at the row's count.
async fn given_service(router: &MultiServiceRouter, row: &Row) {
    ecs(
        router,
        row.case,
        "RegisterTaskDefinition",
        &json!({
            "family": row.service,
            "containerDefinitions": [{
                "name": "app",
                "image": IMAGE,
                "memory": CONTAINER_MEMORY,
                "essential": true,
                "command": STAYS_UP,
            }],
        }),
    )
    .await;
    ecs(
        router,
        row.case,
        "CreateService",
        &json!({
            "cluster": CLUSTER,
            "serviceName": row.service,
            "taskDefinition": row.service,
            "desiredCount": row.desired_count,
            "launchType": "EC2",
        }),
    )
    .await;
}

/// The alarm one row configures over its own series.
///
/// `DatapointsToAlarm` is deliberately left out, so it takes its documented default of
/// `EvaluationPeriods` ("The number of data points that must be breaching to trigger the alarm"),
/// and so is `TreatMissingData`, whose default `missing` leaves a period the collection published
/// nothing into out of the evaluation rather than counting it either way.
async fn put_alarm(router: &MultiServiceRouter, row: &Row) {
    cloudwatch(
        router,
        row.case,
        "PutMetricAlarm",
        &json!({
            "AlarmName": row.alarm,
            "Namespace": row.namespace,
            "MetricName": row.metric,
            "Dimensions": row.dimensions(),
            "Statistic": row.statistic,
            "Period": ALARM_PERIOD,
            "EvaluationPeriods": row.evaluation_periods,
            "Threshold": row.threshold,
            "ComparisonOperator": row.comparison,
        }),
    )
    .await;
}

/// The alarm as `DescribeAlarms` reports it.
async fn described(router: &MultiServiceRouter, row: &Row) -> Value {
    let answer = cloudwatch(
        router,
        row.case,
        "DescribeAlarms",
        &json!({"AlarmNames": [row.alarm]}),
    )
    .await;
    answer["MetricAlarms"][0].clone()
}

/// The `StateValue` CloudWatch reports for one row's alarm.
async fn state_of(router: &MultiServiceRouter, row: &Row) -> String {
    described(router, row).await["StateValue"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// Look again until `row`'s alarm reports `wanted`, and answer it as `DescribeAlarms` last
/// reported it. NOTHING forces the state: the reads are reads, and what moves the alarm is the
/// evaluation the clock owes it over the datapoints Amazon ECS published.
async fn settled_in(router: &MultiServiceRouter, row: &Row, wanted: &str) -> Value {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let reported = described(router, row).await;
        if reported["StateValue"].as_str() == Some(wanted) {
            return reported;
        }
        assert!(
            Instant::now() < deadline,
            "case {:?}: the alarm {} over {} {} must reach {wanted} on its own evaluation, got \
             {reported}",
            row.case,
            row.alarm,
            row.namespace,
            row.metric
        );
        tokio::time::sleep(POLL).await;
    }
}

/// The values the evaluation that decided the reported state really GRADED, out of "an explanation
/// for the alarm state, in JSON format" (`MetricAlarm$StateReasonData`).
fn graded_values(row: &Row, reported: &Value) -> Vec<f64> {
    let text = reported["StateReasonData"].as_str().unwrap_or_else(|| {
        panic!(
            "case {:?}: expected a StateReasonData on {reported}",
            row.case
        )
    });
    let record: Value = serde_json::from_str(text).unwrap_or_else(|error| {
        panic!(
            "case {:?}: StateReasonData is not JSON: {error}",
            row.case
        )
    });
    record["recentDatapoints"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(Value::as_f64)
        .collect()
}

/// RULE: an alarm over a series Amazon ECS publishes is created in `INSUFFICIENT_DATA` and then
/// reports `OK` once the datapoints that series holds stop breaching its comparison.
#[tokio::test]
async fn an_alarm_over_a_published_ecs_series_recovers_into_ok() {
    let router = emulator();
    let setup = "the Container Insights cluster with capacity";
    given_cluster_with_capacity(&router, setup).await;

    // Every alarm is written BEFORE the service whose records feed it exists, so each one starts
    // from the state `PutMetricAlarm` documents: its series holds nothing to grade.
    for row in ROWS {
        put_alarm(&router, row).await;
        let state = state_of(&router, row).await;
        assert_eq!(
            state, INSUFFICIENT_DATA,
            "case {:?}: an alarm over a series holding nothing is created in \
             {INSUFFICIENT_DATA}, got {state:?}",
            row.case
        );
    }

    // Every row's service is created up front, so the clock-owed placements of all of them fall
    // due at once rather than one row's wait after another's.
    for row in ROWS {
        given_service(&router, row).await;
    }

    for row in ROWS {
        let reported = settled_in(&router, row, OK_STATE).await;
        // The state is the alarm's own, over the series the row named...
        assert_eq!(
            reported["MetricName"].as_str(),
            Some(row.metric),
            "case {:?}: expected the alarm to watch {}, got {reported}",
            row.case,
            row.metric
        );
        assert_eq!(
            reported["Namespace"].as_str(),
            Some(row.namespace),
            "case {:?}: expected the alarm to watch {}, got {reported}",
            row.case,
            row.namespace
        );
        // ...and it was decided over datapoints the comparison does NOT hold for, which is what
        // `OK` means for this alarm: the published data, not a state nothing graded.
        let graded = graded_values(row, &reported);
        assert!(
            graded.iter().any(|value| !row.breaches(*value)),
            "case {:?}: expected {OK_STATE} to rest on a {} the comparison {} {} fails for, got \
             {graded:?}",
            row.case,
            row.statistic,
            row.comparison,
            row.threshold
        );
    }
}

/// RULE, the other half: an alarm whose published datapoints keep standing on the breaching side
/// of its comparison reports `ALARM` and never `OK` — so the recovery above is the data's doing
/// and not the alarm's age.
#[tokio::test]
async fn an_alarm_whose_published_count_keeps_breaching_never_reports_ok() {
    let router = emulator();
    let setup = "the Container Insights cluster with capacity";
    given_cluster_with_capacity(&router, setup).await;

    // A service the scheduler holds at no tasks, so its running task count is collected as the
    // nothing it is — under the one task the alarm's comparison breaches below.
    given_service(&router, &IDLE).await;
    put_alarm(&router, &IDLE).await;

    settled_in(&router, &IDLE, ALARM_STATE).await;
    for _ in 0..QUIET_LOOKS {
        tokio::time::sleep(POLL).await;
        let state = state_of(&router, &IDLE).await;
        assert_ne!(
            state, OK_STATE,
            "case {:?}: an alarm whose {} keeps breaching must never report {OK_STATE}, got \
             {state:?}",
            IDLE.case, IDLE.metric
        );
    }
}
