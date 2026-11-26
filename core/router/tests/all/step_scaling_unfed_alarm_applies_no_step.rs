// SPDX-License-Identifier: CC0-1.0

//! A step scaling policy fires only on an alarm that BREACHED: the never-fed twin of a fed
//! alarm applies no step, while the fed one moves its ECS service's task count.
//!
//! RULE, in two halves, each grounded in the models:
//!
//! 1. A service whose placement is satisfiable RUNS the tasks it was asked for: `CreateService`
//!    "runs and maintains your desired number of tasks from a specified task definition", and
//!    `Service$runningCount` is "The number of tasks in the cluster that are in the RUNNING
//!    state" — so a created Fargate service converges on its `desiredCount`, every one of its
//!    tasks reports `RUNNING`, and the rollout that placed them completes with no failed task
//!    (`Deployment$failedTasks`: "the number of consecutively failed tasks in the deployment").
//! 2. A scaling policy is executed by an alarm ACTION, and `PutMetricAlarm$AlarmActions` are
//!    "the actions to execute when this alarm transitions to the ALARM state from any other
//!    state" — so an alarm no datapoint was ever published to applies no step: it holds the
//!    state its missing-data treatment leaves it in, its target keeps the capacity it was
//!    registered at, and its log records no movement. An alarm that IS fed breaches, and the
//!    step its policy names moves `ecs:service:DesiredCount` — "the task count of an ECS
//!    service" — which Amazon ECS then runs.
//!
//! The two services are twins on one estate, differing in ONE thing: whether anything publishes
//! the metric their identically configured alarms watch. The fed twin is the positive control
//! that proves the chain is live at the instant the never-fed twin is judged, so "no step" is
//! read off a working estate rather than off a stalled one.
//!
//! The rows differ along what the rule generalises over: the `TreatMissingData` treatment both
//! twins' alarms carry — omitted (so the documented default `missing` applies), `ignore` and
//! `notBreaching`, which are the values of that member that do NOT substitute a breaching
//! datapoint (SOURCE: `PutMetricAlarmInput$TreatMissingData`, "Valid Values: breaching |
//! notBreaching | ignore | missing", and "If TreatMissingData is omitted, the default behavior
//! of missing is used") — and along the count the services start at and the step the policy
//! takes. Each row is its own test, over its own emulator, so one row's clock-owed work is not
//! another's wait.
//!
//! Why this suite is here rather than beside one service's own: the chain spans five services
//! over one engine — Amazon EC2 holds the network the tasks are placed in, IAM the execution
//! role the revision names, Amazon ECS runs the tasks, CloudWatch grades (or fails to grade)
//! the alarms, and Application Auto Scaling decides and records the movement.
//!
//! Nothing here reaches into a state struct: every value asserted is read back through
//! `DescribeServices`, `ListTasks`, `DescribeTasks`, `DescribeAlarms`,
//! `DescribeScalableTargets` and `DescribeScalingActivities`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder and element readers this test crate keeps for awsQuery suites
// (`all/query_wire.rs`: "Nothing new should write another one — call this module").
use crate::query_wire::{element, form_encode, opt_element};

/// The region every request below is signed for.
const REGION: &str = "us-east-1";

/// The ECS cluster, task-definition family and container every row creates. Each row builds its
/// own emulator, so the names are the same from row to row.
const CLUSTER: &str = "unfedstep";
const FAMILY: &str = "unfedstepfamily";
const CONTAINER: &str = "app";

/// The capacity window both twins' scalable targets are registered with: both bounds leave
/// every row's step room, so no row's movement is held at a bound.
const MIN_CAPACITY: i64 = 1;
const MAX_CAPACITY: i64 = 6;

/// The namespace the twins' metrics live in — a customer namespace, so it is one a client could
/// publish itself — and the value published to the FED series, above the shared threshold.
const METRIC_NAMESPACE: &str = "Emulator/UnfedStepScaling";
const THRESHOLD: f64 = 50.0;
const BREACHING_VALUE: f64 = 75.0;

/// The alarm's evaluation cadence, in seconds: a sub-minute period, so the window that grades a
/// just-published datapoint rolls past it inside the poll below.
const ALARM_PERIOD: i64 = 10;

/// `StateValue` of an alarm nothing has decided, which is the state `PutMetricAlarm` leaves a
/// new alarm in, and of one that breached.
const INSUFFICIENT_DATA: &str = "INSUFFICIENT_DATA";
const OK: &str = "OK";
const ALARM: &str = "ALARM";

/// How often a poll re-reads and how long it keeps trying. Generous: every leg of the chain is
/// owed by a clock — the tasks coming up, the alarm's evaluation, the execution that follows —
/// and each poll stops the instant its own condition holds, so the deadline only turns a
/// failure into a failure rather than a hang.
const POLL: Duration = Duration::from_millis(200);
const BUDGET: Duration = Duration::from_secs(150);

/// One row: the missing-data treatment both twins' alarms carry, the state that treatment
/// leaves the never-fed twin's alarm in, the count the services start at and the step the
/// policies take.
struct Row {
    case: &'static str,
    /// The `TreatMissingData` both alarms are written with — `None` for the row that omits the
    /// member and so gets the documented default.
    treatment: Option<&'static str>,
    /// The `StateValue` the never-fed alarm holds under that treatment.
    unfed_state: &'static str,
    /// The `desiredCount` both services are created with, inside the registered window.
    starting: i64,
    /// The `ScalingAdjustment` of each policy's single step.
    adjustment: i64,
}

/// The two twins of one row: the service that is fed its metric and the one that never is.
struct Twin {
    /// The service's name, which is also the last segment of its scalable target's `ResourceId`.
    service: &'static str,
    /// The metric the twin's alarm watches, and the alarm and policy names.
    metric: &'static str,
    alarm: &'static str,
    policy: &'static str,
}

const FED: Twin = Twin {
    service: "fedtwin",
    metric: "FedLoad",
    alarm: "unfedstep-fed-alarm",
    policy: "fed-scale-out",
};

const UNFED: Twin = Twin {
    service: "unfedtwin",
    metric: "UnfedLoad",
    alarm: "unfedstep-unfed-alarm",
    policy: "unfed-scale-out",
};

/// The `Authorization` header a call carries while the router authenticates nothing: the
/// credential scope, which is what names the target service and its Region.
fn credential_scope(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}\
         /aws4_request, SignedHeaders=host, Signature=0"
    )
}

/// One emulator: every service the production router assembles, over one shared engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<aws_emulator_router::InMemoryEngine> =
        Arc::new(aws_emulator_router::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    router.set_request_authentication(false);
    router
}

/// One `awsJson` call that must succeed, answered with the parsed body.
async fn json_call(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    operation: &str,
    body: Value,
    case: &str,
) -> Value {
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", &host)
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .header("authorization", credential_scope(signing_name))
        .body(Bytes::from(body.to_string().into_bytes()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed: Value = serde_json::from_slice(&payload)
        .unwrap_or_else(|_| json!({ "__unparsed": String::from_utf8_lossy(&payload) }));
    assert_eq!(
        status, 200,
        "case {case:?}: expected {operation} to answer 200, got {status} {parsed}"
    );
    parsed
}

/// One Amazon ECS call.
async fn ecs(router: &MultiServiceRouter, operation: &str, body: Value, case: &str) -> Value {
    json_call(
        router,
        "ecs",
        "AmazonEC2ContainerServiceV20141113",
        operation,
        body,
        case,
    )
    .await
}

/// One Application Auto Scaling call.
async fn aas(router: &MultiServiceRouter, operation: &str, body: Value, case: &str) -> Value {
    json_call(
        router,
        "application-autoscaling",
        "AnyScaleFrontendService",
        operation,
        body,
        case,
    )
    .await
}

/// One CloudWatch call.
async fn cloudwatch(
    router: &MultiServiceRouter,
    operation: &str,
    body: Value,
    case: &str,
) -> Value {
    json_call(
        router,
        "monitoring",
        "GraniteServiceVersion20100801",
        operation,
        body,
        case,
    )
    .await
}

/// One `awsQuery`/`ec2Query` call that must succeed, answered with its XML body.
async fn query_call(
    router: &MultiServiceRouter,
    signing_name: &str,
    version: &str,
    action: &str,
    params: &[(&str, &str)],
    case: &str,
) -> String {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", &host)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", credential_scope(signing_name))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: expected {action} to answer 200, got {status} {text}"
    );
    text
}

/// One Amazon EC2 call.
async fn ec2(
    router: &MultiServiceRouter,
    action: &str,
    params: &[(&str, &str)],
    case: &str,
) -> String {
    query_call(router, "ec2", "2016-11-15", action, params, case).await
}

/// One IAM call.
async fn iam(
    router: &MultiServiceRouter,
    action: &str,
    params: &[(&str, &str)],
    case: &str,
) -> String {
    query_call(router, "iam", "2010-05-08", action, params, case).await
}

/// The network the tasks are placed in, as Amazon EC2 minted it.
struct Network {
    subnets: Vec<String>,
    /// The VPC's own default security group, which is the group the placements name.
    security_group: Option<String>,
}

/// Create a VPC with one public subnet through Amazon EC2's own API.
async fn network(router: &MultiServiceRouter, case: &str) -> Network {
    let vpc = element(
        &ec2(router, "CreateVpc", &[("CidrBlock", "10.71.0.0/24")], case).await,
        "vpcId",
    );
    let availability_zone = format!("{REGION}a");
    let subnet = element(
        &ec2(
            router,
            "CreateSubnet",
            &[
                ("VpcId", vpc.as_str()),
                ("CidrBlock", "10.71.0.0/28"),
                ("AvailabilityZone", availability_zone.as_str()),
            ],
            case,
        )
        .await,
        "subnetId",
    );
    // "To use an internet gateway, your subnet's route table must contain a route that directs
    // internet-bound traffic to the internet gateway" — the path Amazon ECS reads off these
    // records for the images the containers run, so the subnet is a public one.
    let gateway = element(
        &ec2(router, "CreateInternetGateway", &[], case).await,
        "internetGatewayId",
    );
    ec2(
        router,
        "AttachInternetGateway",
        &[
            ("InternetGatewayId", gateway.as_str()),
            ("VpcId", vpc.as_str()),
        ],
        case,
    )
    .await;
    let route_table = element(
        &ec2(router, "CreateRouteTable", &[("VpcId", vpc.as_str())], case).await,
        "routeTableId",
    );
    ec2(
        router,
        "AssociateRouteTable",
        &[
            ("RouteTableId", route_table.as_str()),
            ("SubnetId", subnet.as_str()),
        ],
        case,
    )
    .await;
    ec2(
        router,
        "CreateRoute",
        &[
            ("RouteTableId", route_table.as_str()),
            ("DestinationCidrBlock", "0.0.0.0/0"),
            ("GatewayId", gateway.as_str()),
        ],
        case,
    )
    .await;
    let groups = ec2(
        router,
        "DescribeSecurityGroups",
        &[
            ("Filter.1.Name", "vpc-id"),
            ("Filter.1.Value.1", vpc.as_str()),
            ("Filter.2.Name", "group-name"),
            ("Filter.2.Value.1", "default"),
        ],
        case,
    )
    .await;
    Network {
        subnets: vec![subnet],
        security_group: opt_element(&groups, "groupId"),
    }
}

/// The task execution role the revision names — "the task execution role that grants the Amazon
/// ECS container agent permission to make Amazon Web Services API calls on your behalf" —
/// created through IAM's own API with the trust policy an Amazon ECS task role carries.
async fn execution_role(router: &MultiServiceRouter, case: &str) -> String {
    let document = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "ecs-tasks.amazonaws.com" },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let created = iam(
        router,
        "CreateRole",
        &[
            ("RoleName", "unfedstep-execution"),
            ("AssumeRolePolicyDocument", document.as_str()),
        ],
        case,
    )
    .await;
    element(&created, "Arn")
}

/// The three identity members of one twin's `ecs` scalable target: "ecs:service:DesiredCount —
/// The task count of an ECS service".
fn target(twin: &Twin) -> Value {
    json!({
        "ServiceNamespace": "ecs",
        "ResourceId": format!("service/{CLUSTER}/{}", twin.service),
        "ScalableDimension": "ecs:service:DesiredCount",
    })
}

/// What one twin's ECS service reports: the count it wants and the count it is running.
async fn counts(router: &MultiServiceRouter, twin: &Twin, case: &str) -> (i64, i64) {
    let service = described_service(router, twin, case).await;
    (
        service["desiredCount"].as_i64().unwrap_or_default(),
        service["runningCount"].as_i64().unwrap_or_default(),
    )
}

/// One twin's service, as `DescribeServices` reports it.
async fn described_service(router: &MultiServiceRouter, twin: &Twin, case: &str) -> Value {
    let described = ecs(
        router,
        "DescribeServices",
        json!({ "cluster": CLUSTER, "services": [twin.service] }),
        case,
    )
    .await;
    described["services"][0].clone()
}

/// The `StateValue` CloudWatch reports for one twin's alarm.
async fn alarm_state(router: &MultiServiceRouter, twin: &Twin, case: &str) -> String {
    let described = cloudwatch(
        router,
        "DescribeAlarms",
        json!({ "AlarmNames": [twin.alarm] }),
        case,
    )
    .await;
    described["MetricAlarms"][0]["StateValue"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// One twin's scaling-activity log, newest first, including the activities that scaled nothing.
async fn activities(router: &MultiServiceRouter, twin: &Twin, case: &str) -> Vec<Value> {
    let mut request = target(twin);
    request["IncludeNotScaledActivities"] = json!(true);
    let body = aas(router, "DescribeScalingActivities", request, case).await;
    body["ScalingActivities"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected a ScalingActivities list, got {body}"))
}

/// Publish one breaching datapoint to the FED twin's series, the way the recorded client does on
/// every probe so its alarm reaches `ALARM` and stays there.
async fn publish(router: &MultiServiceRouter, case: &str) {
    cloudwatch(
        router,
        "PutMetricData",
        json!({
            "Namespace": METRIC_NAMESPACE,
            "MetricData": [{
                "MetricName": FED.metric,
                "Value": BREACHING_VALUE,
            }],
        }),
        case,
    )
    .await;
}

/// Build the estate: the network, the execution role, the cluster, the revision and both twins'
/// services, which differ in nothing but their names.
async fn given_estate(router: &MultiServiceRouter, row: &Row) {
    let case = row.case;
    let network = network(router, case).await;
    let role = execution_role(router, case).await;
    ecs(
        router,
        "CreateCluster",
        json!({ "clusterName": CLUSTER }),
        case,
    )
    .await;
    ecs(
        router,
        "RegisterTaskDefinition",
        json!({
            "family": FAMILY,
            "networkMode": "awsvpc",
            "requiresCompatibilities": ["FARGATE"],
            "cpu": "256",
            "memory": "512",
            "executionRoleArn": role,
            "containerDefinitions": [{
                "name": CONTAINER,
                "image": "public.ecr.aws/docker/library/busybox:1.36",
                "essential": true,
                "command": ["sh", "-c", "sleep 3600"],
            }],
        }),
        case,
    )
    .await;
    for twin in [&FED, &UNFED] {
        ecs(
            router,
            "CreateService",
            json!({
                "cluster": CLUSTER,
                "serviceName": twin.service,
                "taskDefinition": FAMILY,
                "desiredCount": row.starting,
                "launchType": "FARGATE",
                "networkConfiguration": {
                    "awsvpcConfiguration": {
                        "subnets": network.subnets,
                        "securityGroups": network.security_group.iter().collect::<Vec<_>>(),
                        // The task's own public address, which is what makes the internet
                        // gateway its subnet routes through a path to the image's registry.
                        "assignPublicIp": "ENABLED",
                    }
                },
            }),
            case,
        )
        .await;
    }
}

/// Register one twin's scalable target, put its step scaling policy and the alarm whose
/// `AlarmActions` name that policy.
async fn given_policy(router: &MultiServiceRouter, row: &Row, twin: &Twin) {
    let case = row.case;
    let mut registration = target(twin);
    registration["MinCapacity"] = json!(MIN_CAPACITY);
    registration["MaxCapacity"] = json!(MAX_CAPACITY);
    aas(router, "RegisterScalableTarget", registration, case).await;

    let mut request = target(twin);
    request["PolicyName"] = json!(twin.policy);
    request["PolicyType"] = json!("StepScaling");
    request["StepScalingPolicyConfiguration"] = json!({
        "AdjustmentType": "ChangeInCapacity",
        "MetricAggregationType": "Maximum",
        // "The amount of time, in seconds, to wait for a previous scaling activity to take
        // effect": nothing here breaches twice, so the wait is named rather than defaulted.
        "Cooldown": 120,
        // "If the metric value is above the breach threshold, the lower bound is inclusive":
        // the step of a scale-out is an interval of the difference ABOVE the threshold.
        "StepAdjustments": [{
            "MetricIntervalLowerBound": 0.0,
            "ScalingAdjustment": row.adjustment,
        }],
    });
    let put = aas(router, "PutScalingPolicy", request, case).await;
    let policy_arn = put["PolicyARN"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a PolicyARN, got {put}"))
        .to_string();

    let mut alarm = json!({
        "AlarmName": twin.alarm,
        "Namespace": METRIC_NAMESPACE,
        "MetricName": twin.metric,
        "Statistic": "Maximum",
        "Period": ALARM_PERIOD,
        "EvaluationPeriods": 1,
        "DatapointsToAlarm": 1,
        "Threshold": THRESHOLD,
        "ComparisonOperator": "GreaterThanThreshold",
        "AlarmActions": [policy_arn],
    });
    if let Some(treatment) = row.treatment {
        alarm["TreatMissingData"] = json!(treatment);
    }
    cloudwatch(router, "PutMetricAlarm", alarm, case).await;
}

/// Re-read one twin's service until its counts settle on `wanted`, publishing a datapoint to the
/// fed series on every probe exactly as the recorded client does. Answers what it last read.
async fn until_counts(
    router: &MultiServiceRouter,
    twin: &Twin,
    wanted: (i64, i64),
    case: &str,
) -> (i64, i64) {
    let deadline = Instant::now() + BUDGET;
    let mut last = counts(router, twin, case).await;
    while last != wanted && Instant::now() < deadline {
        tokio::time::sleep(POLL).await;
        publish(router, case).await;
        last = counts(router, twin, case).await;
    }
    last
}

/// Re-read one twin's alarm until it reports `wanted`, publishing to the fed series on every
/// probe. Answers what it last read.
async fn until_alarm_state(
    router: &MultiServiceRouter,
    twin: &Twin,
    wanted: &str,
    case: &str,
) -> String {
    let deadline = Instant::now() + BUDGET;
    let mut last = alarm_state(router, twin, case).await;
    while last != wanted && Instant::now() < deadline {
        tokio::time::sleep(POLL).await;
        publish(router, case).await;
        last = alarm_state(router, twin, case).await;
    }
    last
}

/// Assert one twin's service is RUNNING exactly `wanted` tasks, each reporting `RUNNING`, under a
/// rollout that completed without losing a launch.
async fn assert_running_tasks(router: &MultiServiceRouter, twin: &Twin, wanted: i64, case: &str) {
    let service = described_service(router, twin, case).await;
    let deployment = &service["deployments"][0];
    assert_eq!(
        deployment["failedTasks"].as_i64(),
        Some(0),
        "case {case:?}: expected the rollout of {} to have lost no launch, got {deployment}",
        twin.service
    );
    let listed = ecs(
        router,
        "ListTasks",
        json!({
            "cluster": CLUSTER,
            "serviceName": twin.service,
            "desiredStatus": "RUNNING",
        }),
        case,
    )
    .await;
    let arns = listed["taskArns"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected a taskArns list, got {listed}"));
    assert_eq!(
        arns.len() as i64,
        wanted,
        "case {case:?}: expected {wanted} task ARN(s) for {}, got {listed}",
        twin.service
    );
    let described = ecs(
        router,
        "DescribeTasks",
        json!({ "cluster": CLUSTER, "tasks": arns }),
        case,
    )
    .await;
    let tasks = described["tasks"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected a tasks list, got {described}"));
    assert_eq!(
        tasks.len() as i64,
        wanted,
        "case {case:?}: every listed task of {} must be describable, got {described}",
        twin.service
    );
    for task in &tasks {
        assert_eq!(
            task["lastStatus"].as_str(),
            Some("RUNNING"),
            "case {case:?}: a task counted as running reports RUNNING, got {task}"
        );
    }
}

/// The activities of one twin's log that SET a capacity, by the description such a movement
/// carries.
fn movements(listed: &[Value], count_set: &str) -> Vec<Value> {
    listed
        .iter()
        .filter(|activity| {
            activity["Description"]
                .as_str()
                .is_some_and(|text| text.starts_with(count_set))
        })
        .cloned()
        .collect()
}

/// Drive ONE row end to end: both twins converge on the count they were created with, the fed
/// twin's breach moves its count and Amazon ECS runs it, and the never-fed twin holds its alarm's
/// undecided state, its registered capacity and an empty movement log throughout.
async fn unfed_alarm_applies_no_step(row: &Row) {
    let case = row.case;
    let scaled = row.starting + row.adjustment;
    let router = emulator();
    given_estate(&router, row).await;

    // Half 1 of the rule: a satisfiable Fargate placement RUNS the count it was asked for —
    // for each twin, with every task reporting RUNNING and no launch lost.
    for twin in [&FED, &UNFED] {
        let started = until_counts(&router, twin, (row.starting, row.starting), case).await;
        assert_eq!(
            started,
            (row.starting, row.starting),
            "case {case:?}: expected {} to run its {} task(s), got (desired, running) \
             {started:?}",
            twin.service,
            row.starting
        );
        assert_running_tasks(&router, twin, row.starting, case).await;
    }

    for twin in [&FED, &UNFED] {
        given_policy(&router, row, twin).await;
    }
    publish(&router, case).await;

    // Half 2, the positive control: the FED alarm breaches and the step its policy names moves
    // the service's count, which Amazon ECS then runs.
    let fed_state = until_alarm_state(&router, &FED, ALARM, case).await;
    assert_eq!(
        fed_state, ALARM,
        "case {case:?}: expected the fed alarm to breach, got {fed_state:?}"
    );
    let moved = until_counts(&router, &FED, (scaled, scaled), case).await;
    assert_eq!(
        moved,
        (scaled, scaled),
        "case {case:?}: expected the breach to move the fed service to {scaled} task(s) and \
         Amazon ECS to run them, got (desired, running) {moved:?}"
    );
    assert_running_tasks(&router, &FED, scaled, case).await;
    let count_set = "Setting desired count to";
    let fed_log = activities(&router, &FED, case).await;
    assert!(
        !movements(&fed_log, count_set).is_empty(),
        "case {case:?}: expected the fed target to record the movement it made, got {fed_log:?}"
    );

    // Half 2 itself: the NEVER-FED twin, judged at the instant its fed twin has already scaled.
    // Its alarm holds the state its missing-data treatment leaves it in — never `ALARM` —
    let unfed_state = until_alarm_state(&router, &UNFED, row.unfed_state, case).await;
    assert_eq!(
        unfed_state, row.unfed_state,
        "case {case:?}: expected the never-fed alarm to hold {:?}, got {unfed_state:?}",
        row.unfed_state
    );
    assert_ne!(
        unfed_state, ALARM,
        "case {case:?}: an alarm no datapoint was published to has not breached, got \
         {unfed_state:?}"
    );
    // its service keeps the count it was created with, running it,
    let held = counts(&router, &UNFED, case).await;
    assert_eq!(
        held,
        (row.starting, row.starting),
        "case {case:?}: expected the never-fed service to hold its {} task(s), got \
         (desired, running) {held:?}",
        row.starting
    );
    assert_running_tasks(&router, &UNFED, row.starting, case).await;
    // its target still reports the window it was registered with,
    // `DescribeScalableTargets` identifies the targets it reads by a LIST of resource ids —
    // `ResourceIds`, beside the same namespace and dimension a registration names.
    let targets = aas(
        &router,
        "DescribeScalableTargets",
        json!({
            "ServiceNamespace": "ecs",
            "ResourceIds": [format!("service/{CLUSTER}/{}", UNFED.service)],
            "ScalableDimension": "ecs:service:DesiredCount",
        }),
        case,
    )
    .await;
    let registered = &targets["ScalableTargets"][0];
    assert_eq!(
        (
            registered["MinCapacity"].as_i64(),
            registered["MaxCapacity"].as_i64()
        ),
        (Some(MIN_CAPACITY), Some(MAX_CAPACITY)),
        "case {case:?}: expected the never-fed target's registered window, got {targets}"
    );
    // and nothing in its log set a capacity.
    let unfed_log = activities(&router, &UNFED, case).await;
    let unfed_movements = movements(&unfed_log, count_set);
    assert!(
        unfed_movements.is_empty(),
        "case {case:?}: expected the never-fed target to record no movement, got \
         {unfed_movements:?}"
    );

    // And reading any of it does not change what the next reader sees.
    let again = activities(&router, &UNFED, case).await;
    assert_eq!(
        again, unfed_log,
        "case {case:?}: expected a second read of the never-fed log to answer identically, got \
         {again:?}"
    );
    let held_again = counts(&router, &UNFED, case).await;
    assert_eq!(
        held_again, held,
        "case {case:?}: expected a second read of the never-fed service to answer identically, \
         got {held_again:?}"
    );
}

/// The row that omits `TreatMissingData` altogether, so "the default behavior of `missing` is
/// used" and the never-fed alarm is one nothing has decided.
#[tokio::test]
async fn a_never_fed_alarm_under_the_default_treatment_applies_no_step() {
    unfed_alarm_applies_no_step(&Row {
        case: "the never-fed twin of an alarm that omits TreatMissingData",
        treatment: None,
        unfed_state: INSUFFICIENT_DATA,
        starting: 1,
        adjustment: 1,
    })
    .await;
}

/// The row whose alarms `ignore` missing data, which "maintains the current alarm state" — the
/// `INSUFFICIENT_DATA` a create leaves — over a larger step and a larger starting count.
#[tokio::test]
async fn a_never_fed_alarm_that_ignores_missing_data_applies_no_step() {
    unfed_alarm_applies_no_step(&Row {
        case: "the never-fed twin of an alarm that ignores missing data, scaled by two",
        treatment: Some("ignore"),
        unfed_state: INSUFFICIENT_DATA,
        starting: 2,
        adjustment: 2,
    })
    .await;
}

/// The row whose alarms treat missing data as `notBreaching`, which decides the never-fed alarm
/// `OK` — a state, and still not one that executes an alarm action.
#[tokio::test]
async fn a_never_fed_alarm_that_treats_missing_data_as_not_breaching_applies_no_step() {
    unfed_alarm_applies_no_step(&Row {
        case: "the never-fed twin of an alarm that treats missing data as notBreaching",
        treatment: Some("notBreaching"),
        unfed_state: OK,
        starting: 1,
        adjustment: 3,
    })
    .await;
}
