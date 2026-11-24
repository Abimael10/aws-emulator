// SPDX-License-Identifier: CC0-1.0

//! The `CapacityProviderReservation` series Amazon ECS PUBLISHES for a capacity provider whose
//! managed scaling it runs — the series the Amazon ECS-owned target-tracking policy on that
//! provider's Auto Scaling group tracks, over the WHOLE mounted service set the binary
//! assembles.
//!
//! The RULE under test, from `ManagedScaling`'s own documentation ("Amazon ECS manages a target
//! tracking scaling policy using an Amazon ECS managed CloudWatch metric with the specified
//! `targetCapacity` value as the target value for the metric") and the cluster-auto-scaling
//! ratio the Amazon ECS developer guide states for that metric:
//!
//! > **A capacity provider whose managed scaling is ENABLED over a caller's Auto Scaling group,
//! > associated with a cluster, has Amazon ECS PUBLISHING the `CapacityProviderReservation`
//! > series of the `AWS/ECS/ManagedScaling` namespace for that `(provider, cluster)` pair — the
//! > instances the cluster's work through the provider needs, over the instances the provider's
//! > group holds, as a percentage — so the alarms that policy owns are evaluated from real data
//! > instead of grading an empty series.** A pair that holds no capacity and needs none reads
//! > 100 (in balance), a pair whose capacity nothing is working reads 0, and a pair whose work
//! > waits for capacity its group holds none of reads 200, above every `targetCapacity` a
//! > provider may be given. A provider Amazon ECS manages the scaling of nothing for — managed
//! > scaling off, or no cluster to reserve capacity for — has no series at all.
//!
//! Three crates have to agree over one engine for any of that to hold, which is why this suite
//! lives here: Amazon ECS decides and publishes the series, Amazon EC2 Auto Scaling owns the
//! group whose capacity it is read against, and CloudWatch owns the metric store
//! `GetMetricStatistics` answers from and the alarms the series drives.
//!
//! The rows differ along what the rule generalises over: the capacity the group holds (none,
//! one instance, and a group that cannot hold any) and the work that needs it (none, and a task
//! parked waiting for capacity) — each row a different pair of the ratio's two terms.

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder and element reader this test crate keeps for awsQuery suites
// (`all/query_wire.rs`: "Nothing new should write another one — call this module").
use crate::query_wire::{block_of, element, elements, form_encode, members_of, opt_element};

/// The Region every request below is signed for.
const REGION: &str = "us-west-2";

/// The API versions an ec2Query and an awsQuery request carries.
const EC2_API_VERSION: &str = "2016-11-15";
const ASG_VERSION: &str = "2011-01-01";

/// CloudWatch's own API version, for the awsQuery form an AWS SDK uses to read its alarms.
const CLOUDWATCH_VERSION: &str = "2010-08-01";

/// The `X-Amz-Target` prefixes Amazon ECS's and CloudWatch's models fix, and the signing name
/// CloudWatch's credential scope carries (`sigv4Name: monitoring`).
const ECS_TARGET_PREFIX: &str = "AmazonEC2ContainerServiceV20141113";
const CLOUDWATCH_TARGET_PREFIX: &str = "GraniteServiceVersion20100801";
const CLOUDWATCH_SIGNING_NAME: &str = "monitoring";

/// The access key id of the account's own published credential.
const ACCOUNT_KEY_ID: &str = "AKIAIOSFODNN7EXAMPLE";

/// The AMI and instance type the launch template below names.
const IMAGE_ID: &str = "ami-0abcdef1234567890";
const INSTANCE_TYPE: &str = "t3.micro";

/// The launch template every group of this suite launches from. It configures no container
/// agent, so an instance it launches joins no cluster: the capacity the group holds is the
/// group's, and nothing registers it as a container instance.
const TEMPLATE: &str = "ecsreservation-template";

/// What the managed policy IS on the wire, spelled out here rather than imported from the
/// implementation, so a wrong constant in the implementation cannot make a test pass.
const TARGET_TRACKING: &str = "TargetTrackingScaling";

/// The series under test, spelled out here rather than imported from the implementation, so a
/// wrong constant in the implementation cannot make a test pass.
const METRIC_NAMESPACE: &str = "AWS/ECS/ManagedScaling";
const METRIC_NAME: &str = "CapacityProviderReservation";
const PROVIDER_DIMENSION: &str = "CapacityProviderName";
const CLUSTER_DIMENSION: &str = "ClusterName";

/// The comparison the alarm that makes the policy add capacity grades the series with, and the
/// state an alarm carries while it has graded nothing.
const SCALE_OUT_COMPARISON: &str = "GreaterThanThreshold";
const INSUFFICIENT_DATA: &str = "INSUFFICIENT_DATA";
const ALARM_STATE_OK: &str = "OK";

/// The image the parked task's container runs, and the memory it reserves.
const TASK_IMAGE: &str = "public.ecr.aws/docker/library/busybox:1.36";
const TASK_MEMORY: i32 = 128;

/// The error code and the sentence a `targetCapacity` outside the published range is refused
/// with — AWS's fixed text, naming the bounds and nothing from the request.
const CLIENT_EXCEPTION: &str = "ClientException";
const TARGET_CAPACITY_MESSAGE: &str =
    "The managed scaling target capacity value is invalid. Specify an integer greater than 0 \
     and less than or equal to 100 and try again.";

/// How often a poll re-reads a surface it waits on, and how long it keeps trying. The deadline
/// is generous and only here so a failure is a failure rather than a hang.
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);
const POLL_BUDGET: std::time::Duration = std::time::Duration::from_secs(60);

/// The whole emulator over one engine, exactly as the binary assembles it.
fn router() -> MultiServiceRouter {
    let engine = aws_emulator_router::new_engine();
    let mut router = aws_emulator_router::build_router(engine);
    // Signature verification is the front door's concern, not this suite's: the credential
    // scope in the `Authorization` header is what names the caller.
    router.set_request_authentication(false);
    router
}

/// One form body: the action, the version and the parameters.
fn form(action: &str, version: &str, params: &[(&str, &str)]) -> String {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    body
}

/// Send one form-encoded request to `signing_name`, signed by the account's own credential.
async fn form_call(router: &MultiServiceRouter, signing_name: &str, body: String) -> (u16, String) {
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host)
        .header("content-type", "application/x-www-form-urlencoded")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential={ACCOUNT_KEY_ID}/20260101/{REGION}/{signing_name}/\
                 aws4_request, SignedHeaders=host, Signature=0000"
            ),
        )
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// One ec2Query action that must succeed.
async fn ec2(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let (status, body) = form_call(router, "ec2", form(action, EC2_API_VERSION, params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: EC2 {action} must succeed, got {status} with {body}"
    );
    body
}

/// One Auto Scaling action that must succeed.
async fn asg(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let (status, body) =
        form_call(router, "autoscaling", form(action, ASG_VERSION, params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: Auto Scaling {action} must succeed, got {status} with {body}"
    );
    body
}

/// One AWS-JSON RPC call to the service whose signing name is `signing_name`.
async fn rpc_call(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    operation: &str,
    request: &Value,
) -> (u16, Value) {
    let content_type = if signing_name == CLOUDWATCH_SIGNING_NAME {
        "application/x-amz-json-1.0"
    } else {
        "application/x-amz-json-1.1"
    };
    let http_request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", content_type)
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential={ACCOUNT_KEY_ID}/20260101/{REGION}/{signing_name}/\
                 aws4_request, SignedHeaders=host;x-amz-target, Signature=0000"
            ),
        )
        .body(Bytes::from(request.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(http_request).await;
    let status = response.status().as_u16();
    let payload: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, payload)
}

/// One Amazon ECS call that must answer 200.
async fn ecs_ok(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    request: &Value,
) -> Value {
    let (status, answer) = rpc_call(router, "ecs", ECS_TARGET_PREFIX, operation, request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected Amazon ECS {operation} to answer 200, got {status} with {answer}"
    );
    answer
}

/// One CloudWatch call that must answer 200.
async fn cloudwatch_ok(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    request: &Value,
) -> Value {
    let (status, answer) = rpc_call(
        router,
        CLOUDWATCH_SIGNING_NAME,
        CLOUDWATCH_TARGET_PREFIX,
        operation,
        request,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected CloudWatch {operation} to answer 200, got {status} with {answer}"
    );
    answer
}

/// Create the network and the launch template every group of this suite is built on, and answer
/// the subnet the groups place their instances in.
async fn seed_launch_source(router: &MultiServiceRouter, case: &str) -> String {
    let vpc = element(
        &ec2(router, case, "CreateVpc", &[("CidrBlock", "10.93.0.0/16")]).await,
        "vpcId",
    );
    let subnet = element(
        &ec2(
            router,
            case,
            "CreateSubnet",
            &[
                ("VpcId", vpc.as_str()),
                ("CidrBlock", "10.93.1.0/24"),
                ("AvailabilityZone", &format!("{REGION}a")),
            ],
        )
        .await,
        "subnetId",
    );
    ec2(
        router,
        case,
        "CreateLaunchTemplate",
        &[
            ("LaunchTemplateName", TEMPLATE),
            ("LaunchTemplateData.ImageId", IMAGE_ID),
            ("LaunchTemplateData.InstanceType", INSTANCE_TYPE),
        ],
    )
    .await;
    subnet
}

/// Create one Auto Scaling group holding the capacity the row asks for.
async fn given_group(router: &MultiServiceRouter, row: &Row, subnet: &str) {
    asg(
        router,
        row.case,
        "CreateAutoScalingGroup",
        &[
            ("AutoScalingGroupName", row.group),
            ("MinSize", "0"),
            ("MaxSize", &row.max_size.to_string()),
            ("DesiredCapacity", &row.held.to_string()),
            ("LaunchTemplate.LaunchTemplateName", TEMPLATE),
            ("VPCZoneIdentifier", subnet),
        ],
    )
    .await;
}

/// The dimension pair the series of one `(provider, cluster)` pair is counted under.
fn dimensions(provider: &str, cluster: &str) -> Value {
    json!([
        { "Name": PROVIDER_DIMENSION, "Value": provider },
        { "Name": CLUSTER_DIMENSION, "Value": cluster },
    ])
}

/// Every datapoint CloudWatch holds for the reservation series of one pair.
async fn reservation_points(
    router: &MultiServiceRouter,
    case: &str,
    provider: &str,
    cluster: &str,
) -> Vec<Value> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();
    let stats = cloudwatch_ok(
        router,
        case,
        "GetMetricStatistics",
        &json!({
            "Namespace": METRIC_NAMESPACE,
            "MetricName": METRIC_NAME,
            "Dimensions": dimensions(provider, cluster),
            "StartTime": now - 3_600.0,
            "EndTime": now + 600.0,
            "Period": 60,
            "Statistics": ["Minimum", "Maximum", "Average", "SampleCount"],
        }),
    )
    .await;
    stats["Datapoints"].as_array().cloned().unwrap_or_default()
}

/// The latest datapoint of a pair's reservation series once that series reads `wanted` — looked
/// at again until it does, or the budget runs out.
///
/// The series FOLLOWS the pair: a row that parks work after the series opened has datapoints
/// from before that work existed, so what the rule is read against is the reservation the series
/// has reached.
async fn settled_reservation(
    router: &MultiServiceRouter,
    case: &str,
    provider: &str,
    cluster: &str,
    wanted: f64,
) -> Value {
    let deadline = std::time::Instant::now() + POLL_BUDGET;
    loop {
        let points = reservation_points(router, case, provider, cluster).await;
        let latest = points.iter().cloned().fold(None, |held: Option<Value>, point| {
            match held {
                Some(held) if held["Timestamp"].as_f64() >= point["Timestamp"].as_f64() => {
                    Some(held)
                }
                _ => Some(point),
            }
        });
        if let Some(latest) = latest
            .clone()
            .filter(|point| point["Maximum"].as_f64() == Some(wanted))
        {
            return latest;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: expected the reservation series of {provider:?} in {cluster:?} to \
             read {wanted}, got {points:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The names of the CloudWatch alarms the one scaling policy on `group` reports.
async fn policy_alarm_names(router: &MultiServiceRouter, case: &str, group: &str) -> Vec<String> {
    let deadline = std::time::Instant::now() + POLL_BUDGET;
    loop {
        let body = asg(
            router,
            case,
            "DescribePolicies",
            &[("AutoScalingGroupName", group)],
        )
        .await;
        let names = elements(&body, "AlarmName");
        if !names.is_empty() {
            return names;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: expected the managed policy on {group} to report its alarms, got \
             {body}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The state the alarm that makes the policy ADD capacity has computed, once it has graded the
/// series at all — looked at again until it has, or the budget runs out.
async fn settled_scale_out_state(
    router: &MultiServiceRouter,
    case: &str,
    names: &[String],
) -> String {
    let deadline = std::time::Instant::now() + POLL_BUDGET;
    loop {
        let described = cloudwatch_ok(
            router,
            case,
            "DescribeAlarms",
            &json!({ "AlarmNames": names }),
        )
        .await;
        let alarms = described["MetricAlarms"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let scale_out: Vec<Value> = alarms
            .into_iter()
            .filter(|alarm| alarm["ComparisonOperator"].as_str() == Some(SCALE_OUT_COMPARISON))
            .collect();
        assert_eq!(
            scale_out.len(),
            1,
            "case {case:?}: expected exactly one {SCALE_OUT_COMPARISON} alarm among {names:?}, \
             got {scale_out:?}"
        );
        let state = scale_out[0]["StateValue"].as_str().unwrap_or_default();
        if state != INSUFFICIENT_DATA {
            return state.to_string();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: expected the scale-out alarm to grade the published series, and it \
             still reports {INSUFFICIENT_DATA}: {:?}",
            scale_out[0]
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One row of the table: the capacity the pair's group holds, the work that needs it, and the
/// percentage the series then reports.
struct Row {
    case: &'static str,
    group: &'static str,
    cluster: &'static str,
    provider: &'static str,
    /// `MaxSize` of the group, and the capacity it is created holding.
    max_size: i32,
    held: i32,
    /// Whether the row runs a task through the provider that the group cannot hold, so the
    /// task is parked waiting for capacity.
    parks_a_task: bool,
    expected: f64,
}

const ROWS: &[Row] = &[
    Row {
        case: "no capacity held and no work needing any: in balance",
        group: "ecsres-empty",
        cluster: "ecsresempty",
        provider: "cpresemptyprovider",
        max_size: 2,
        held: 0,
        parks_a_task: false,
        expected: 100.0,
    },
    Row {
        case: "capacity held that nothing is working: none of it reserved",
        group: "ecsres-idle",
        cluster: "ecsresidle",
        provider: "cpresidleprovider",
        max_size: 2,
        held: 1,
        parks_a_task: false,
        expected: 0.0,
    },
    Row {
        case: "work waiting on a group that can hold no capacity: above every target",
        group: "ecsres-starved",
        cluster: "ecsresstarved",
        provider: "cpresstarvedprovider",
        max_size: 0,
        held: 0,
        parks_a_task: true,
        expected: 200.0,
    },
];

/// Park one task of the row's cluster on the row's provider: a task routed through a provider
/// whose group holds no capacity waits for capacity to be acquired, which is the work the
/// reservation counts.
async fn park_a_task(router: &MultiServiceRouter, row: &Row) {
    let case = row.case;
    ecs_ok(
        router,
        case,
        "RegisterTaskDefinition",
        &json!({
            "family": row.cluster,
            "networkMode": "bridge",
            "requiresCompatibilities": ["EC2"],
            "containerDefinitions": [{
                "name": "app",
                "image": TASK_IMAGE,
                "essential": true,
                "memory": TASK_MEMORY,
            }],
        }),
    )
    .await;
    let run = ecs_ok(
        router,
        case,
        "RunTask",
        &json!({
            "cluster": row.cluster,
            "taskDefinition": row.cluster,
            "count": 1,
            "capacityProviderStrategy": [{ "capacityProvider": row.provider, "weight": 1 }],
        }),
    )
    .await;
    assert_eq!(
        run["tasks"].as_array().map(Vec::len),
        Some(1),
        "case {case:?}: expected the task to be accepted and parked on the provider's capacity, \
         got {run}"
    );
}

/// THE RULE: Amazon ECS publishes the reservation of every pair whose managed scaling it runs —
/// the instances the work needs over the instances the group holds — and the alarms its policy
/// owns are evaluated from it.
#[tokio::test]
async fn the_reservation_series_reports_the_work_over_the_capacity_it_runs_on() {
    let router = router();
    let subnet = seed_launch_source(&router, "setup").await;

    for row in ROWS {
        let case = row.case;
        given_group(&router, row, &subnet).await;
        ecs_ok(&router, case, "CreateCluster", &json!({ "clusterName": row.cluster })).await;
        ecs_ok(
            &router,
            case,
            "CreateCapacityProvider",
            &json!({
                "name": row.provider,
                "autoScalingGroupProvider": {
                    "autoScalingGroupArn": row.group,
                    "managedScaling": { "status": "ENABLED", "targetCapacity": 100 },
                    "managedTerminationProtection": "DISABLED",
                },
            }),
        )
        .await;
        ecs_ok(
            &router,
            case,
            "PutClusterCapacityProviders",
            &json!({
                "cluster": row.cluster,
                "capacityProviders": [row.provider],
                "defaultCapacityProviderStrategy": [],
            }),
        )
        .await;
        if row.parks_a_task {
            park_a_task(&router, row).await;
        }

        // The series itself: its latest datapoint reports the reservation of this pair, as the
        // ratio of what the work needs to what the group holds.
        let point = settled_reservation(&router, case, row.provider, row.cluster, row.expected)
            .await;
        assert_eq!(
            point["Minimum"].as_f64(),
            Some(row.expected),
            "case {case:?}: expected every observation of the datapoint to read {}, got {point}",
            row.expected
        );
        assert_eq!(
            point["Unit"].as_str(),
            Some("Percent"),
            "case {case:?}: expected a percentage, got {point}"
        );

        // What the series is FOR: the alarms the managed policy owns grade it, so the one that
        // adds capacity leaves the state an alarm with no data carries. A reservation at or
        // under the provider's target capacity is not breaching, so that alarm reads OK.
        let names = policy_alarm_names(&router, case, row.group).await;
        let state = settled_scale_out_state(&router, case, &names).await;
        let expected_state = if row.expected > 100.0 {
            // A pair whose work has no capacity at all breaches the target, and the alarm needs
            // more than one such period before it says so: either direction is a state the
            // alarm COMPUTED, which is the observable difference a published series makes.
            None
        } else {
            Some(ALARM_STATE_OK)
        };
        if let Some(expected_state) = expected_state {
            assert_eq!(
                state, expected_state,
                "case {case:?}: expected the scale-out alarm to read {expected_state} over a \
                 reservation of {}, got {state}",
                row.expected
            );
        }
    }

    // A provider Amazon ECS manages the scaling of NOTHING for publishes no series at all: the
    // metric is the managed-scaling policy's, so a provider that has none has no reservation
    // either. Asked after every row above settled, so the pass that publishes has run many
    // times over.
    let case = "managed scaling off: no series at all";
    given_group(
        &router,
        &Row {
            case,
            group: "ecsres-unmanaged",
            cluster: "ecsresunmanaged",
            provider: "cpresunmanagedprovider",
            max_size: 2,
            held: 1,
            parks_a_task: false,
            expected: 0.0,
        },
        &subnet,
    )
    .await;
    ecs_ok(
        &router,
        case,
        "CreateCluster",
        &json!({ "clusterName": "ecsresunmanaged" }),
    )
    .await;
    ecs_ok(
        &router,
        case,
        "CreateCapacityProvider",
        &json!({
            "name": "cpresunmanagedprovider",
            "autoScalingGroupProvider": {
                "autoScalingGroupArn": "ecsres-unmanaged",
                "managedScaling": { "status": "DISABLED" },
                "managedTerminationProtection": "DISABLED",
            },
        }),
    )
    .await;
    ecs_ok(
        &router,
        case,
        "PutClusterCapacityProviders",
        &json!({
            "cluster": "ecsresunmanaged",
            "capacityProviders": ["cpresunmanagedprovider"],
            "defaultCapacityProviderStrategy": [],
        }),
    )
    .await;
    // One of the rows above is read again first, so this read happens after a publish pass that
    // did produce datapoints — an empty answer here is the absence of a series rather than a
    // series that has not opened yet.
    settled_reservation(&router, case, ROWS[0].provider, ROWS[0].cluster, ROWS[0].expected).await;
    let unmanaged =
        reservation_points(&router, case, "cpresunmanagedprovider", "ecsresunmanaged").await;
    assert!(
        unmanaged.is_empty(),
        "case {case:?}: a provider whose managed scaling is off has no series, got {unmanaged:?}"
    );
}

/// What a client reads off the WIRING of one pair: what the managed policy tracks, what its
/// scale-out alarm grades against, and what the provider itself reports.
#[derive(Debug, Clone, PartialEq)]
struct Wiring {
    /// The one target-tracking policy's `TargetValue`.
    target_value: Option<f64>,
    /// Its scale-out alarm's `Threshold`.
    threshold: Option<f64>,
    /// The provider's own `managedScaling.targetCapacity`.
    target_capacity: Option<i64>,
}

/// Read the whole wiring of one pair, exactly as a client reads it: through Amazon EC2 Auto
/// Scaling's own `DescribePolicies`, CloudWatch's own `DescribeAlarms` and Amazon ECS's own
/// `DescribeCapacityProviders`.
async fn read_wiring(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    provider: &str,
) -> Wiring {
    let body = asg(
        router,
        case,
        "DescribePolicies",
        &[("AutoScalingGroupName", group)],
    )
    .await;
    let names = elements(&body, "PolicyName");
    assert!(
        names.len() <= 1,
        "case {case:?}: expected at most one scaling policy on {group}, got {names:?} in {body}"
    );
    let alarms = block_of(&body, "Alarms").map_or_else(Vec::new, |block| elements(block, "AlarmName"));
    let threshold = if alarms.is_empty() {
        None
    } else {
        let described = cloudwatch_ok(
            router,
            case,
            "DescribeAlarms",
            &json!({ "AlarmNames": alarms }),
        )
        .await;
        let scale_out: Vec<Value> = described["MetricAlarms"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|alarm| alarm["ComparisonOperator"].as_str() == Some(SCALE_OUT_COMPARISON))
            .collect();
        assert_eq!(
            scale_out.len(),
            1,
            "case {case:?}: expected exactly one {SCALE_OUT_COMPARISON} alarm among {alarms:?}, \
             got {scale_out:?}"
        );
        scale_out[0]["Threshold"].as_f64()
    };
    let described = ecs_ok(
        router,
        case,
        "DescribeCapacityProviders",
        &json!({ "capacityProviders": [provider] }),
    )
    .await;
    Wiring {
        target_value: opt_element(&body, "TargetValue").and_then(|value| value.parse().ok()),
        threshold,
        target_capacity: described["capacityProviders"][0]["autoScalingGroupProvider"]
            ["managedScaling"]["targetCapacity"]
            .as_i64(),
    }
}

/// The wiring of one pair once it reports `wanted` everywhere — looked at again until it does,
/// or the budget runs out.
async fn settled_wiring(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    provider: &str,
    wanted: i64,
) -> Wiring {
    let deadline = std::time::Instant::now() + POLL_BUDGET;
    loop {
        let wiring = read_wiring(router, case, group, provider).await;
        #[allow(clippy::cast_precision_loss)]
        let wanted_value = Some(wanted as f64);
        if wiring.target_value == wanted_value
            && wiring.threshold == wanted_value
            && wiring.target_capacity == Some(wanted)
        {
            return wiring;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: expected the whole wiring of {provider:?} to read {wanted}, got \
             {wiring:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Ask for one target capacity on an existing provider, and answer the refusal it earned — the
/// error code and the message — or `None` when Amazon ECS accepted it.
///
/// The request names the managed-scaling block it asks for: its `status` and the
/// `targetCapacity`, which is how a client re-targets a provider.
async fn request_target_capacity(
    router: &MultiServiceRouter,
    provider: &str,
    target_capacity: i64,
) -> Option<(String, String)> {
    let (status, answer) = rpc_call(
        router,
        "ecs",
        ECS_TARGET_PREFIX,
        "UpdateCapacityProvider",
        &json!({
            "name": provider,
            "autoScalingGroupProvider": {
                "managedScaling": { "status": "ENABLED", "targetCapacity": target_capacity },
            },
        }),
    )
    .await;
    if status == 200 {
        return None;
    }
    let code = answer["__type"]
        .as_str()
        .unwrap_or_default()
        .rsplit(['#', '/'])
        .next()
        .unwrap_or_default()
        .to_string();
    let message = answer["message"]
        .as_str()
        .or_else(|| answer["Message"].as_str())
        .unwrap_or_default()
        .to_string();
    Some((code, message))
}

/// One row of the re-target table: the target capacity the provider is created with, the value
/// outside the published range an update asks for next, and the two in-range values the updates
/// after it move it to.
struct Sequence {
    case: &'static str,
    group: &'static str,
    cluster: &'static str,
    provider: &'static str,
    created_with: i64,
    /// A value outside the range `ManagedScaling$targetCapacity` publishes (`@range` 1..100).
    refused: i64,
    moved_to: i64,
    moved_again_to: i64,
}

const SEQUENCES: &[Sequence] = &[
    Sequence {
        case: "above the published range: the whole capacity, then down twice",
        group: "ecsseq-high",
        cluster: "ecsseqhigh",
        provider: "cpseqhighprovider",
        created_with: 100,
        refused: 101,
        moved_to: 80,
        moved_again_to: 90,
    },
    Sequence {
        case: "below the published range: the bottom of it, then up twice",
        group: "ecsseq-low",
        cluster: "ecsseqlow",
        provider: "cpseqlowprovider",
        created_with: 1,
        refused: 0,
        moved_to: 100,
        moved_again_to: 50,
    },
];

/// THE RULE for a SEQUENCE of re-targets: every in-range target capacity an update asks for
/// propagates to the whole wiring — the policy's target value, its scale-out alarm's threshold
/// and the provider's own configuration — and one outside the published range is refused with
/// AWS's own sentence and leaves all three exactly as they stood.
#[tokio::test]
async fn every_in_range_retarget_propagates_and_an_out_of_range_one_changes_nothing() {
    let router = router();
    let subnet = seed_launch_source(&router, "setup").await;

    for sequence in SEQUENCES {
        let case = sequence.case;
        given_group(
            &router,
            &Row {
                case,
                group: sequence.group,
                cluster: sequence.cluster,
                provider: sequence.provider,
                max_size: 2,
                held: 0,
                parks_a_task: false,
                expected: 100.0,
            },
            &subnet,
        )
        .await;
        ecs_ok(
            &router,
            case,
            "CreateCluster",
            &json!({ "clusterName": sequence.cluster }),
        )
        .await;
        ecs_ok(
            &router,
            case,
            "CreateCapacityProvider",
            &json!({
                "name": sequence.provider,
                "autoScalingGroupProvider": {
                    "autoScalingGroupArn": sequence.group,
                    "managedScaling": {
                        "status": "ENABLED",
                        "targetCapacity": sequence.created_with,
                    },
                    "managedTerminationProtection": "DISABLED",
                },
            }),
        )
        .await;
        ecs_ok(
            &router,
            case,
            "PutClusterCapacityProviders",
            &json!({
                "cluster": sequence.cluster,
                "capacityProviders": [sequence.provider],
                "defaultCapacityProviderStrategy": [],
            }),
        )
        .await;

        // The wiring the association put in place, at the target capacity the provider was
        // created with.
        let created = settled_wiring(
            &router,
            case,
            sequence.group,
            sequence.provider,
            sequence.created_with,
        )
        .await;

        // A target capacity outside the published range is refused, and changes nothing.
        let refusal = request_target_capacity(&router, sequence.provider, sequence.refused).await;
        assert_eq!(
            refusal,
            Some((
                CLIENT_EXCEPTION.to_string(),
                TARGET_CAPACITY_MESSAGE.to_string()
            )),
            "case {case:?}: expected {} to be refused with AWS's own sentence, got {refusal:?}",
            sequence.refused
        );
        let after_refusal =
            read_wiring(&router, case, sequence.group, sequence.provider).await;
        assert_eq!(
            after_refusal, created,
            "case {case:?}: a refused target capacity leaves the wiring as it stood, got \
             {after_refusal:?} after {created:?}"
        );

        // Each in-range target capacity after it propagates to the whole wiring, one update
        // after another.
        for wanted in [sequence.moved_to, sequence.moved_again_to] {
            let accepted = request_target_capacity(&router, sequence.provider, wanted).await;
            assert_eq!(
                accepted, None,
                "case {case:?}: expected {wanted} to be accepted, got {accepted:?}"
            );
            settled_wiring(&router, case, sequence.group, sequence.provider, wanted).await;
        }
    }
}

/// The whole `managedScaling` block a provider reports, as a client reads it off
/// `DescribeCapacityProviders`.
async fn provider_managed_scaling(
    router: &MultiServiceRouter,
    case: &str,
    provider: &str,
) -> Value {
    let described = ecs_ok(
        router,
        case,
        "DescribeCapacityProviders",
        &json!({ "capacityProviders": [provider] }),
    )
    .await;
    described["capacityProviders"][0]["autoScalingGroupProvider"]["managedScaling"].clone()
}

/// The target value of the one managed policy on `group`, and `None` for a group that holds no
/// policy at all — looked at again until it reads `wanted`, or the budget runs out.
async fn settled_policy_target(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    wanted: Option<f64>,
) {
    let deadline = std::time::Instant::now() + POLL_BUDGET;
    loop {
        let body = asg(
            router,
            case,
            "DescribePolicies",
            &[("AutoScalingGroupName", group)],
        )
        .await;
        let names = elements(&body, "PolicyName");
        assert!(
            names.len() <= 1,
            "case {case:?}: expected at most one scaling policy on {group}, got {names:?} in \
             {body}"
        );
        let target = opt_element(&body, "TargetValue").and_then(|value| value.parse::<f64>().ok());
        if target == wanted && names.is_empty() == wanted.is_none() {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: expected the managed policy on {group} to track {wanted:?}, got \
             {target:?} among {names:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One row of the update table: the `autoScalingGroupProvider` the update carries, the whole
/// managed-scaling block the provider must report afterwards, and what the policy must then
/// track.
struct Partial {
    case: &'static str,
    group: &'static str,
    cluster: &'static str,
    provider: &'static str,
    /// The `autoScalingGroupProvider` the update carries.
    update: fn() -> Value,
    /// The whole `managedScaling` block the provider must report afterwards.
    expected: fn() -> Value,
    /// The target value the managed policy must then track, and `None` where the update asked
    /// for managed scaling to be turned off, which takes the policy away.
    expected_policy_target: Option<f64>,
}

/// The managed-scaling block every row of the table below creates its provider with: a block
/// whose every member is NAMED, away from the default its own documentation gives it, so each
/// row shows what its update left in force.
fn created_managed_scaling() -> Value {
    json!({
        "status": "ENABLED",
        "targetCapacity": 100,
        "minimumScalingStepSize": 2,
        "maximumScalingStepSize": 50,
        "instanceWarmupPeriod": 120,
    })
}

const PARTIALS: &[Partial] = &[
    Partial {
        case: "a re-target: the block the update names, each member it omits at its default",
        group: "ecspart-target",
        cluster: "ecsparttarget",
        provider: "cpparttargetprovider",
        update: || json!({ "managedScaling": { "status": "ENABLED", "targetCapacity": 80 } }),
        expected: || {
            json!({
                "status": "ENABLED",
                "targetCapacity": 80,
                "minimumScalingStepSize": 1,
                "maximumScalingStepSize": 10000,
                "instanceWarmupPeriod": 300,
            })
        },
        expected_policy_target: Some(80.0),
    },
    Partial {
        case: "a new warm-up period: the target capacity at its own documented default",
        group: "ecspart-warmup",
        cluster: "ecspartwarmup",
        provider: "cppartwarmupprovider",
        update: || {
            json!({
                "managedScaling": { "status": "ENABLED", "instanceWarmupPeriod": 600 },
            })
        },
        expected: || {
            json!({
                "status": "ENABLED",
                "targetCapacity": 100,
                "minimumScalingStepSize": 1,
                "maximumScalingStepSize": 10000,
                "instanceWarmupPeriod": 600,
            })
        },
        expected_policy_target: Some(100.0),
    },
    Partial {
        case: "a new minimum step size: the rest of the block at its own defaults",
        group: "ecspart-step",
        cluster: "ecspartstep",
        provider: "cppartstepprovider",
        update: || {
            json!({
                "managedScaling": { "status": "ENABLED", "minimumScalingStepSize": 5 },
            })
        },
        expected: || {
            json!({
                "status": "ENABLED",
                "targetCapacity": 100,
                "minimumScalingStepSize": 5,
                "maximumScalingStepSize": 10000,
                "instanceWarmupPeriod": 300,
            })
        },
        expected_policy_target: Some(100.0),
    },
    Partial {
        case: "the status turned off: the policy and its alarms go with it",
        group: "ecspart-off",
        cluster: "ecspartoff",
        provider: "cppartoffprovider",
        update: || json!({ "managedScaling": { "status": "DISABLED" } }),
        expected: || {
            json!({
                "status": "DISABLED",
                "targetCapacity": 100,
                "minimumScalingStepSize": 1,
                "maximumScalingStepSize": 10000,
                "instanceWarmupPeriod": 300,
            })
        },
        expected_policy_target: None,
    },
    Partial {
        case: "a sibling member alone: the whole managed-scaling block in force stands",
        group: "ecspart-sibling",
        cluster: "ecspartsibling",
        provider: "cppartsiblingprovider",
        update: || json!({ "managedTerminationProtection": "ENABLED" }),
        expected: created_managed_scaling,
        expected_policy_target: Some(100.0),
    },
];

/// THE RULE for an update's configuration: a member the request NAMES replaces what is in force
/// and a member it leaves out keeps it, every member of a `managedScaling` block the request
/// does name standing at the default its own documentation gives it ("If this parameter is
/// omitted, the default value of ... is used") — and the policy Amazon ECS manages tracks
/// whatever that leaves, which an update naming `status` off takes away altogether.
#[tokio::test]
async fn an_update_leaves_the_policy_tracking_the_configuration_in_force() {
    let router = router();
    let subnet = seed_launch_source(&router, "setup").await;

    for partial in PARTIALS {
        let case = partial.case;
        given_group(
            &router,
            &Row {
                case,
                group: partial.group,
                cluster: partial.cluster,
                provider: partial.provider,
                max_size: 2,
                held: 0,
                parks_a_task: false,
                expected: 100.0,
            },
            &subnet,
        )
        .await;
        ecs_ok(
            &router,
            case,
            "CreateCluster",
            &json!({ "clusterName": partial.cluster }),
        )
        .await;
        ecs_ok(
            &router,
            case,
            "CreateCapacityProvider",
            &json!({
                "name": partial.provider,
                "autoScalingGroupProvider": {
                    "autoScalingGroupArn": partial.group,
                    "managedScaling": created_managed_scaling(),
                    "managedTerminationProtection": "DISABLED",
                },
            }),
        )
        .await;
        ecs_ok(
            &router,
            case,
            "PutClusterCapacityProviders",
            &json!({
                "cluster": partial.cluster,
                "capacityProviders": [partial.provider],
                "defaultCapacityProviderStrategy": [],
            }),
        )
        .await;
        settled_policy_target(&router, case, partial.group, Some(100.0)).await;

        let updated = ecs_ok(
            &router,
            case,
            "UpdateCapacityProvider",
            &json!({ "name": partial.provider, "autoScalingGroupProvider": (partial.update)() }),
        )
        .await;

        // What the update left IN FORCE, member by member — as the update itself reports it and
        // as a later read of the provider does, which must be the same configuration.
        let expected = (partial.expected)();
        let reported = updated["capacityProvider"]["autoScalingGroupProvider"]["managedScaling"]
            .clone();
        let read_back = provider_managed_scaling(&router, case, partial.provider).await;
        for (member, wanted) in expected.as_object().expect("an object of members") {
            assert_eq!(
                reported.get(member),
                Some(wanted),
                "case {case:?}: expected the update to leave {member} at {wanted}, got {reported}"
            );
            assert_eq!(
                read_back.get(member),
                Some(wanted),
                "case {case:?}: expected a later read to report {member} as {wanted}, got \
                 {read_back}"
            );
        }

        // And what the policy Amazon ECS manages for the provider then tracks: the target
        // capacity in force, or no policy at all where the update turned managed scaling off.
        settled_policy_target(
            &router,
            case,
            partial.group,
            partial.expected_policy_target,
        )
        .await;
    }
}

/// THE RULE over the WIRE an AWS SDK uses: the whole wiring a client reads is carried by each
/// service's own protocol — the awsQuery form for Amazon EC2 Auto Scaling's policies and for
/// CloudWatch's alarms — so the series identity the policy tracks and the alarm grades reaches
/// a client that reads them the way an SDK does, lists wrapped as that protocol wraps them.
#[tokio::test]
async fn the_wiring_reaches_a_client_reading_it_over_the_query_wire() {
    let case = "the wiring over the awsQuery wire";
    let router = router();
    let subnet = seed_launch_source(&router, case).await;
    let group = "ecswire-group";
    let cluster = "ecswirecluster";
    let provider = "cpwireprovider";
    given_group(
        &router,
        &Row {
            case,
            group,
            cluster,
            provider,
            max_size: 2,
            held: 0,
            parks_a_task: false,
            expected: 100.0,
        },
        &subnet,
    )
    .await;
    ecs_ok(&router, case, "CreateCluster", &json!({ "clusterName": cluster })).await;
    ecs_ok(
        &router,
        case,
        "CreateCapacityProvider",
        &json!({
            "name": provider,
            "autoScalingGroupProvider": {
                "autoScalingGroupArn": group,
                "managedScaling": { "status": "ENABLED", "targetCapacity": 100 },
                "managedTerminationProtection": "DISABLED",
            },
        }),
    )
    .await;
    ecs_ok(
        &router,
        case,
        "PutClusterCapacityProviders",
        &json!({
            "cluster": cluster,
            "capacityProviders": [provider],
            "defaultCapacityProviderStrategy": [],
        }),
    )
    .await;
    policy_alarm_names(&router, case, group).await;

    // The policy, as an SDK's own reader scopes it: the `ScalingPolicies` list holds exactly
    // one policy, carrying the series it tracks and the alarms it owns as members of their own
    // lists.
    let body = asg(
        &router,
        case,
        "DescribePolicies",
        &[("AutoScalingGroupName", group)],
    )
    .await;
    let named = elements(&body, "PolicyName");
    assert_eq!(
        named.len(),
        1,
        "case {case:?}: expected exactly one policy on the wire, got {body}"
    );
    let policy =
        block_of(&body, "ScalingPolicies").expect("the answer carries a ScalingPolicies list");
    assert_eq!(
        element(policy, "PolicyType"),
        TARGET_TRACKING,
        "case {case:?}: expected a {TARGET_TRACKING} policy, got {policy}"
    );
    assert_eq!(
        element(policy, "TargetValue").parse::<f64>().ok(),
        Some(100.0),
        "case {case:?}: expected the policy to track 100, got {policy}"
    );
    assert_eq!(
        element(policy, "Namespace"),
        METRIC_NAMESPACE,
        "case {case:?}: expected the managed-scaling namespace on the wire, got {policy}"
    );
    assert_eq!(
        element(policy, "MetricName"),
        METRIC_NAME,
        "case {case:?}: expected the reservation metric on the wire, got {policy}"
    );
    let alarms = members_of(block_of(policy, "Alarms").expect("the policy carries its alarms"));
    let names: Vec<String> = alarms
        .iter()
        .map(|alarm| element(alarm, "AlarmName"))
        .collect();
    assert!(
        !names.is_empty() && names.iter().all(|name| !name.is_empty()),
        "case {case:?}: a target-tracking policy is driven by alarms it names, got {policy}"
    );

    // The alarms those names resolve to, read the way an SDK reads CloudWatch: the awsQuery
    // form, with the names as members of the `AlarmNames` list. Each alarm is asked for on its
    // own, so every field read below belongs to the alarm it is read from.
    let mut scale_out: Vec<String> = Vec::new();
    for name in &names {
        let (status, body) = form_call(
            &router,
            CLOUDWATCH_SIGNING_NAME,
            form(
                "DescribeAlarms",
                CLOUDWATCH_VERSION,
                &[("AlarmNames.member.1", name.as_str())],
            ),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: CloudWatch DescribeAlarms must succeed over the query wire, got \
             {status} with {body}"
        );
        let alarm = block_of(&body, "MetricAlarms")
            .filter(|block| !block.trim().is_empty())
            .unwrap_or_else(|| panic!("case {case:?}: {name} resolves to no alarm: {body}"));
        assert_eq!(
            element(alarm, "AlarmName"),
            *name,
            "case {case:?}: expected the alarm the policy names, got {alarm}"
        );
        assert_eq!(
            element(alarm, "Namespace"),
            METRIC_NAMESPACE,
            "case {case:?}: expected the managed-scaling namespace on the wire, got {alarm}"
        );
        assert_eq!(
            element(alarm, "MetricName"),
            METRIC_NAME,
            "case {case:?}: expected the reservation metric on the wire, got {alarm}"
        );
        assert_eq!(
            element(alarm, "Statistic"),
            "Average",
            "case {case:?}: expected the statistic the policy tracks, got {alarm}"
        );
        let dimensions: Vec<(String, String)> =
            members_of(block_of(alarm, "Dimensions").expect("the alarm carries its dimensions"))
                .iter()
                .map(|dimension| (element(dimension, "Name"), element(dimension, "Value")))
                .collect();
        assert_eq!(
            dimensions,
            vec![
                (PROVIDER_DIMENSION.to_string(), provider.to_string()),
                (CLUSTER_DIMENSION.to_string(), cluster.to_string()),
            ],
            "case {case:?}: expected the series to be named by the provider and its cluster, \
             got {alarm}"
        );
        assert_eq!(
            element(alarm, "AlarmActions"),
            format!("<member>{}</member>", element(policy, "PolicyARN")),
            "case {case:?}: expected the alarm to execute the policy it drives, got {alarm}"
        );
        if element(alarm, "ComparisonOperator") == SCALE_OUT_COMPARISON {
            scale_out.push(element(alarm, "Threshold"));
        }
    }
    assert_eq!(
        scale_out.len(),
        1,
        "case {case:?}: expected exactly one {SCALE_OUT_COMPARISON} alarm among {names:?}, got \
         {scale_out:?}"
    );
    assert_eq!(
        scale_out[0].parse::<f64>().ok(),
        Some(100.0),
        "case {case:?}: expected the scale-out alarm to grade against the target capacity, got \
         {scale_out:?}"
    );
}
