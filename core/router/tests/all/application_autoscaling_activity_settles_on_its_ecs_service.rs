// SPDX-License-Identifier: CC0-1.0

//! A step scaling movement on an ECS service SETTLES: the scaling activity it recorded reaches
//! its terminal status once Amazon ECS is running the count the movement set.
//!
//! RULE: setting a capacity and that capacity being applied are two things. Application Auto
//! Scaling sets `Service$desiredCount` — "the desired number of instantiations of the task
//! definition to keep running on the service" — and the activity it records says it is
//! "Waiting for change to be fulfilled by ecs." until Amazon ECS is RUNNING that count
//! (`Service$runningCount`: "the number of tasks in the cluster that are in the RUNNING
//! state"); at that instant the activity reaches `Successful` and is stamped with an `EndTime`.
//! So a service whose containers stay up is a service whose scaled count is reached, and the
//! movement that set it ENDS — a client that waits for the activity to end before measuring a
//! cooldown, or before making its next adjustment, gets its answer.
//!
//! Why this suite is here rather than beside one service's own: the chain spans five services
//! over one engine — Amazon EC2 holds the network the tasks are placed in and IAM the execution
//! role the revision names, Amazon ECS runs the tasks, CloudWatch grades the datapoint that
//! moves the alarm, Elastic Load Balancing holds the group the service binds, and Application
//! Auto Scaling decides and records the movement — which is what `build_router` assembles.
//!
//! The rows differ along what the rule generalises over: the estate the service runs in (behind
//! a load balancer and bare), the FORM its container's program states its wait in, and the step
//! the policy takes. The DIRECTION the movement goes is the axis the sibling suite
//! [`crate::application_autoscaling_fargate_activity_settles`] adds, over the same rows and the
//! same runner ([`movement_settles`]).

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

/// The ECS cluster, task-definition family, container and service every row creates. Each row
/// builds its own emulator, so the names are the same from row to row.
const CLUSTER: &str = "stepscale";
const FAMILY: &str = "stepscalefamily";
const CONTAINER: &str = "app";
const SERVICE: &str = "web";

/// The port the container declares in its `portMappings`, and the one a row's binding
/// attaches: a binding may only name a port the revision has.
const CONTAINER_PORT: i64 = 80;

/// The capacity window the scalable target is registered with. Both bounds leave every row's
/// step room, so no row's movement is held at a bound.
const MIN_CAPACITY: i64 = 1;
const MAX_CAPACITY: i64 = 6;

/// The metric the alarm watches, in a customer namespace so it is one a client could publish
/// itself, and the value published to it against the alarm's threshold: a scale-out's alarm
/// breaches ABOVE its threshold and a scale-in's BELOW it, so the breach each row publishes is
/// on the side its own step is an interval of.
const METRIC_NAMESPACE: &str = "Emulator/StepScaleFulfilment";
const METRIC_NAME: &str = "Load";
const THRESHOLD: f64 = 50.0;

/// The alarm's evaluation cadence, in seconds: a sub-minute period, so the window that grades
/// a just-published datapoint rolls past it inside the poll below.
const ALARM_PERIOD: i64 = 10;

/// The alarm that fires the policy, and the policy it fires.
const ALARM: &str = "stepscale-alarm";
const POLICY: &str = "scale-out";

/// How often a poll re-reads and how long it keeps trying. Generous: every leg of the chain is
/// owed by a clock — the tasks coming up, the alarm's evaluation, the execution that follows,
/// the fulfilment that ends the activity — and each poll stops the instant its own condition
/// holds, so the deadline only turns a failure into a failure rather than a hang.
const POLL: Duration = Duration::from_millis(200);
const BUDGET: Duration = Duration::from_secs(120);

/// One row: the estate the service runs in, the program its container runs, the count the
/// service starts at and the step the policy takes.
pub(crate) struct Row {
    pub(crate) case: &'static str,
    /// Whether the service binds a target group of a load balancer.
    pub(crate) behind_a_load_balancer: bool,
    /// The `command` the service's container runs, which is what keeps it up.
    pub(crate) command: &'static [&'static str],
    /// The `desiredCount` the service is created with, inside the registered window.
    pub(crate) starting: i64,
    /// The `ScalingAdjustment` of the policy's single step — negative to remove capacity.
    pub(crate) adjustment: i64,
}

const ROWS: &[Row] = &[
    Row {
        // The recorded estate: a load-balanced Fargate service whose container's program is a
        // long wait.
        case: "a load-balanced service whose container states a long wait",
        behind_a_load_balancer: true,
        command: &["sh", "-c", "sleep 3600"],
        starting: 1,
        adjustment: 1,
    },
    Row {
        // The same movement on a service nothing fronts, by a larger step, with the wait
        // stated in the other form a container keeps itself up with.
        case: "a bare service whose container waits round a loop, scaled by two",
        behind_a_load_balancer: false,
        command: &["sh", "-c", "while true; do sleep 1; done"],
        starting: 1,
        adjustment: 2,
    },
];

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

/// One `awsJson1_1` call that must succeed, answered with the parsed body.
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

/// One Elastic Load Balancing v2 call.
async fn elbv2(
    router: &MultiServiceRouter,
    action: &str,
    params: &[(&str, &str)],
    case: &str,
) -> String {
    query_call(
        router,
        "elasticloadbalancingv2",
        "2015-12-01",
        action,
        params,
        case,
    )
    .await
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

/// The network a row's resources are placed in, as Amazon EC2 minted it.
struct Network {
    vpc: String,
    subnets: Vec<String>,
    /// The VPC's own default security group, which is the group the placements name.
    security_group: Option<String>,
}

/// Create a VPC with two public subnets through Amazon EC2's own API.
async fn network(router: &MultiServiceRouter, case: &str) -> Network {
    let vpc = element(
        &ec2(router, "CreateVpc", &[("CidrBlock", "10.70.0.0/24")], case).await,
        "vpcId",
    );
    let mut subnets = Vec::new();
    for (index, zone) in ["a", "b"].iter().enumerate() {
        let availability_zone = format!("{REGION}{zone}");
        let cidr = format!("10.70.0.{}/28", index * 16);
        subnets.push(element(
            &ec2(
                router,
                "CreateSubnet",
                &[
                    ("VpcId", vpc.as_str()),
                    ("CidrBlock", cidr.as_str()),
                    ("AvailabilityZone", availability_zone.as_str()),
                ],
                case,
            )
            .await,
            "subnetId",
        ));
    }
    // "To use an internet gateway, your subnet's route table must contain a route that directs
    // internet-bound traffic to the internet gateway" — the path Amazon ECS reads off these
    // records for the images the containers run, so the subnets are public ones.
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
    for subnet in &subnets {
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
    }
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
        vpc,
        subnets,
        security_group: opt_element(&groups, "groupId"),
    }
}

/// The task execution role the revision names — "the task execution role that grants the
/// Amazon ECS container agent permission to make Amazon Web Services API calls on your behalf"
/// — created through IAM's own API with the trust policy an Amazon ECS task role carries.
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
            ("RoleName", "stepscale-execution"),
            ("AssumeRolePolicyDocument", document.as_str()),
        ],
        case,
    )
    .await;
    element(&created, "Arn")
}

/// Create the load balancer, the `ip` target group and the listener that forwards to it, and
/// answer the group's ARN.
async fn target_group(router: &MultiServiceRouter, network: &Network, case: &str) -> String {
    let subnets: Vec<&str> = network.subnets.iter().map(String::as_str).collect();
    let load_balancer = opt_element(
        &elbv2(
            router,
            "CreateLoadBalancer",
            &[
                ("Name", "stepscale-alb"),
                ("Scheme", "internet-facing"),
                ("Subnets.member.1", subnets[0]),
                ("Subnets.member.2", subnets[1]),
            ],
            case,
        )
        .await,
        "LoadBalancerArn",
    )
    .unwrap_or_else(|| panic!("case {case:?}: expected the load balancer's ARN"));
    let group = opt_element(
        &elbv2(
            router,
            "CreateTargetGroup",
            &[
                ("Name", "stepscale-group"),
                ("Protocol", "HTTP"),
                ("Port", "80"),
                ("TargetType", "ip"),
                ("VpcId", network.vpc.as_str()),
                ("HealthCheckProtocol", "HTTP"),
                ("HealthCheckPath", "/"),
            ],
            case,
        )
        .await,
        "TargetGroupArn",
    )
    .unwrap_or_else(|| panic!("case {case:?}: expected the target group's ARN"));
    // A target group no listener forwards to cannot be bound to a service at all.
    elbv2(
        router,
        "CreateListener",
        &[
            ("LoadBalancerArn", load_balancer.as_str()),
            ("Protocol", "HTTP"),
            ("Port", "80"),
            ("DefaultActions.member.1.Type", "forward"),
            ("DefaultActions.member.1.TargetGroupArn", group.as_str()),
        ],
        case,
    )
    .await;
    group
}

/// The three identity members of the `ecs` scalable target: "ecs:service:DesiredCount — The
/// task count of an ECS service".
fn target() -> Value {
    json!({
        "ServiceNamespace": "ecs",
        "ResourceId": format!("service/{CLUSTER}/{SERVICE}"),
        "ScalableDimension": "ecs:service:DesiredCount",
    })
}

/// What the ECS service reports: the count it wants and the count it is running.
async fn counts(router: &MultiServiceRouter, case: &str) -> (i64, i64) {
    let described = ecs(
        router,
        "DescribeServices",
        json!({ "cluster": CLUSTER, "services": [SERVICE] }),
        case,
    )
    .await;
    let service = &described["services"][0];
    (
        service["desiredCount"].as_i64().unwrap_or_default(),
        service["runningCount"].as_i64().unwrap_or_default(),
    )
}

/// The target's scaling-activity log, newest first.
async fn activities(router: &MultiServiceRouter, case: &str) -> Vec<Value> {
    let mut request = target();
    request["IncludeNotScaledActivities"] = json!(true);
    let body = aas(router, "DescribeScalingActivities", request, case).await;
    body["ScalingActivities"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected a ScalingActivities list, got {body}"))
}

/// Publish one datapoint on the side of the threshold `row`'s step is an interval of, the way
/// the recorded client does on every probe so its alarm reaches `ALARM` and stays there.
async fn publish(router: &MultiServiceRouter, row: &Row, case: &str) {
    let value = if row.adjustment >= 0 {
        THRESHOLD + 25.0
    } else {
        THRESHOLD - 25.0
    };
    cloudwatch(
        router,
        "PutMetricData",
        json!({
            "Namespace": METRIC_NAMESPACE,
            "MetricData": [{
                "MetricName": METRIC_NAME,
                "Value": value,
            }],
        }),
        case,
    )
    .await;
}

/// Build the estate one row describes: the network, the execution role, the cluster, the
/// revision and the service — load-balanced or bare.
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
                "command": row.command,
                "portMappings": [{ "containerPort": CONTAINER_PORT, "protocol": "tcp" }],
            }],
        }),
        case,
    )
    .await;
    let mut create = json!({
        "cluster": CLUSTER,
        "serviceName": SERVICE,
        "taskDefinition": FAMILY,
        "desiredCount": row.starting,
        "launchType": "FARGATE",
        "networkConfiguration": {
            "awsvpcConfiguration": {
                "subnets": network.subnets,
                "securityGroups": network.security_group.iter().collect::<Vec<_>>(),
                // The task's own public address, which is what makes the internet gateway its
                // subnet routes through a path to the image's registry.
                "assignPublicIp": "ENABLED",
            }
        },
    });
    if row.behind_a_load_balancer {
        let group = target_group(router, &network, case).await;
        create["loadBalancers"] = json!([{
            "targetGroupArn": group,
            "containerName": CONTAINER,
            "containerPort": CONTAINER_PORT,
        }]);
        // The row is about the ESTATE the movement is made in, not about the health check, so
        // the grace period it gives the registered targets outlasts the whole suite:
        // `healthCheckGracePeriodSeconds` is "the period of time ... that the Amazon ECS
        // service scheduler ignores unhealthy Elastic Load Balancing target health checks".
        create["healthCheckGracePeriodSeconds"] = json!(7_200);
    }
    ecs(router, "CreateService", create, case).await;
}

/// Register the scalable target, put the step scaling policy and the alarm that fires it, and
/// answer the policy's ARN.
async fn given_policy(router: &MultiServiceRouter, row: &Row) {
    let case = row.case;
    let mut registration = target();
    registration["MinCapacity"] = json!(MIN_CAPACITY);
    registration["MaxCapacity"] = json!(MAX_CAPACITY);
    aas(router, "RegisterScalableTarget", registration, case).await;

    let mut request = target();
    request["PolicyName"] = json!(POLICY);
    request["PolicyType"] = json!("StepScaling");
    request["StepScalingPolicyConfiguration"] = json!({
        "AdjustmentType": "ChangeInCapacity",
        "MetricAggregationType": "Maximum",
        // "The amount of time, in seconds, to wait for a previous scaling activity to take
        // effect": nothing here breaches twice, so the wait is named rather than defaulted.
        "Cooldown": 120,
        // "If the metric value is above the breach threshold, the lower bound is inclusive
        // [...] Otherwise, it's exclusive": the step of a scale-out is an interval of the
        // difference ABOVE the threshold, and a scale-in's of the difference below it.
        "StepAdjustments": [if row.adjustment >= 0 {
            json!({ "MetricIntervalLowerBound": 0.0, "ScalingAdjustment": row.adjustment })
        } else {
            json!({ "MetricIntervalUpperBound": 0.0, "ScalingAdjustment": row.adjustment })
        }],
    });
    let put = aas(router, "PutScalingPolicy", request, case).await;
    let policy_arn = put["PolicyARN"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a PolicyARN, got {put}"))
        .to_string();

    // The alarm a client wires itself: its `AlarmActions` name the policy's ARN, which is
    // "the actions to execute when this alarm transitions to the ALARM state from any other
    // state".
    let comparison = if row.adjustment >= 0 {
        "GreaterThanThreshold"
    } else {
        "LessThanThreshold"
    };
    cloudwatch(
        router,
        "PutMetricAlarm",
        json!({
            "AlarmName": ALARM,
            "Namespace": METRIC_NAMESPACE,
            "MetricName": METRIC_NAME,
            "Statistic": "Maximum",
            "Period": ALARM_PERIOD,
            "EvaluationPeriods": 1,
            "DatapointsToAlarm": 1,
            "Threshold": THRESHOLD,
            "ComparisonOperator": comparison,
            "TreatMissingData": "notBreaching",
            "AlarmActions": [policy_arn],
        }),
        case,
    )
    .await;
}

/// Re-read the ECS service until its counts settle on `wanted`, publishing a datapoint on
/// every probe exactly as the recorded client does. Answers what it last read.
async fn until_counts(
    router: &MultiServiceRouter,
    row: &Row,
    wanted: (i64, i64),
    case: &str,
) -> (i64, i64) {
    let deadline = Instant::now() + BUDGET;
    let mut last = counts(router, case).await;
    while last != wanted && Instant::now() < deadline {
        tokio::time::sleep(POLL).await;
        publish(router, row, case).await;
        last = counts(router, case).await;
    }
    last
}

/// The activity of the log whose `Description` is `description` and which has ENDED, read
/// again until there is one. Answers `None` where none ever ends.
async fn until_ended(
    router: &MultiServiceRouter,
    description: &str,
    case: &str,
) -> (Option<Value>, Vec<Value>) {
    let deadline = Instant::now() + BUDGET;
    loop {
        let listed = activities(router, case).await;
        let ended = listed
            .iter()
            .find(|activity| {
                activity["Description"] == json!(description) && activity["EndTime"] != Value::Null
            })
            .cloned();
        if ended.is_some() || Instant::now() >= deadline {
            return (ended, listed);
        }
        tokio::time::sleep(POLL).await;
    }
}

/// Drive ONE row end to end and assert the rule on it: the estate runs the count it was
/// created with, the breach moves that count and Amazon ECS runs the new one, and the activity
/// the movement recorded reaches its terminal status with an `EndTime`.
///
/// `pub(crate)` because the sibling suite
/// [`crate::application_autoscaling_fargate_activity_settles`] drives its own row through it:
/// the rule is one rule, so the two suites differ in their rows and not in what they assert.
pub(crate) async fn movement_settles(row: &Row) {
    let case = row.case;
    let scaled = row.starting + row.adjustment;
    let router = emulator();
    given_estate(&router, row).await;

    // The estate the movement is made in: the service running the count it was created
    // with, which is what a container whose program stays up gets it.
    let started = until_counts(&router, row, (row.starting, row.starting), case).await;
    assert_eq!(
        started,
        (row.starting, row.starting),
        "case {case:?}: expected the created service to run its {} task(s), got {started:?}",
        row.starting
    );

    given_policy(&router, row).await;
    publish(&router, row, case).await;

    // The breach moves the count the resource is driven to, and Amazon ECS runs it.
    let moved = until_counts(&router, row, (scaled, scaled), case).await;
    assert_eq!(
        moved,
        (scaled, scaled),
        "case {case:?}: expected the breach to move the service to {scaled} task(s) and \
         Amazon ECS to run them, got {moved:?}"
    );

    // And the activity the movement recorded ENDS, with the terminal status and the
    // settled message of a change the owning service fulfilled.
    let description = format!("Setting desired count to {scaled}.");
    let (ended, listed) = until_ended(&router, &description, case).await;
    let settled = ended.unwrap_or_else(|| {
        panic!(
            "case {case:?}: expected the activity {description:?} to reach an EndTime, got \
             {listed:?}"
        )
    });
    assert_eq!(
        settled["StatusCode"],
        json!("Successful"),
        "case {case:?}: expected a fulfilled movement to be Successful, got {settled}"
    );
    assert_eq!(
        settled["StatusMessage"],
        json!(format!(
            "Successfully set desired count to {scaled}. Change successfully fulfilled by ecs."
        )),
        "case {case:?}: expected the settled message, got {settled}"
    );
    let start = settled["StartTime"].as_f64().unwrap_or_default();
    let end = settled["EndTime"]
        .as_f64()
        .unwrap_or_else(|| panic!("case {case:?}: expected an EndTime on {settled}"));
    assert!(
        end >= start,
        "case {case:?}: expected EndTime {end} at or after StartTime {start}, got {settled}"
    );
    let cause = settled["Cause"].as_str().unwrap_or_default().to_string();
    assert!(
        cause.contains(ALARM) && cause.contains(POLICY),
        "case {case:?}: expected the Cause to name the alarm {ALARM:?} and the policy \
         {POLICY:?}, got {cause:?}"
    );

    // And reading the log does not change what the next reader sees.
    let again = activities(&router, case).await;
    let once_more = activities(&router, case).await;
    assert_eq!(
        again, once_more,
        "case {case:?}: expected a second read to answer identically, got {once_more:?}"
    );
}

#[tokio::test]
async fn a_step_scaling_movement_settles_once_its_ecs_service_runs_the_count_it_set() {
    for row in ROWS {
        movement_settles(row).await;
    }
}
