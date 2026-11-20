// SPDX-License-Identifier: CC0-1.0

//! The target-tracking scaling policy — and its CloudWatch alarms — that Amazon ECS MANAGES
//! on the Auto Scaling group of a capacity provider whose managed scaling is on, over the
//! WHOLE mounted service set the binary assembles.
//!
//! The RULE under test, from `ManagedScaling`'s own documentation ("When managed scaling is
//! turned on, Amazon ECS manages the scale-in and scale-out actions of the Auto Scaling
//! group. Amazon ECS manages a target tracking scaling policy using an Amazon ECS managed
//! CloudWatch metric with the specified `targetCapacity` value as the target value for the
//! metric"):
//!
//! > **A capacity provider over a caller's Auto Scaling group whose managed scaling is
//! > ENABLED, associated with a cluster, has exactly ONE Amazon ECS-owned
//! > `TargetTrackingScaling` policy on that group — tracking the `CapacityProviderReservation`
//! > metric of the `AWS/ECS/ManagedScaling` namespace, dimensioned by the provider and the
//! > cluster, at a target value equal to the provider's `managedScaling.targetCapacity` — with
//! > the CloudWatch alarms that policy is driven by.** The policy FOLLOWS the provider's
//! > configuration: a re-targeted provider re-targets the same policy and its alarms, and a
//! > provider whose managed scaling is turned off, whose association is removed, or which is
//! > deleted has the policy and the alarms taken away. A `targetCapacity` outside the range
//! > the member publishes (`@range` 1..100) is refused with `ClientException` and changes
//! > nothing.
//!
//! Three crates have to agree over one engine for any of that to hold, which is why this
//! suite lives here: Amazon ECS decides the policy, Amazon EC2 Auto Scaling owns the group and
//! reports the policy through `DescribePolicies`, and CloudWatch owns the alarms
//! `DescribeAlarms` resolves.
//!
//! The rows differ along what the rule generalises over: the target capacity (each published
//! bound of the range, a mid-range value the recording does not name, and the member left out
//! altogether so its documented default stands), the value a re-target moves it to, and —
//! for the refusal table — each way a value can fall outside the range, on the create and on
//! the update alike.

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder and element readers this test crate keeps for awsQuery suites
// (`all/query_wire.rs`: "Nothing new should write another one — call this module").
use crate::query_wire::{block_of, element, elements, form_encode, opt_element};

/// The Region every request below is signed for.
const REGION: &str = "us-west-2";

/// The API versions an ec2Query and each awsQuery request carries.
const EC2_API_VERSION: &str = "2016-11-15";
const ASG_VERSION: &str = "2011-01-01";

/// The `X-Amz-Target` prefixes Amazon ECS's and CloudWatch's models fix (service shapes
/// `AmazonEC2ContainerServiceV20141113` and `GraniteServiceVersion20100801`), and the signing
/// name CloudWatch's credential scope carries (`sigv4Name: monitoring`).
const ECS_TARGET_PREFIX: &str = "AmazonEC2ContainerServiceV20141113";
const CLOUDWATCH_TARGET_PREFIX: &str = "GraniteServiceVersion20100801";
const CLOUDWATCH_SIGNING_NAME: &str = "monitoring";

/// The access key id of the account's own published credential.
const ACCOUNT_KEY_ID: &str = "AKIAIOSFODNN7EXAMPLE";

/// The AMI and instance type the launch template below names.
const IMAGE_ID: &str = "ami-0abcdef1234567890";
const INSTANCE_TYPE: &str = "t3.micro";

/// The launch template every group of this suite launches from.
const TEMPLATE: &str = "ecsmanagedscaling-template";

/// What the managed policy IS, spelled out here rather than imported from the implementation,
/// so a wrong constant in the implementation cannot make a test pass.
const TARGET_TRACKING: &str = "TargetTrackingScaling";
const METRIC_NAMESPACE: &str = "AWS/ECS/ManagedScaling";
const METRIC_NAME: &str = "CapacityProviderReservation";
const PROVIDER_DIMENSION: &str = "CapacityProviderName";
const CLUSTER_DIMENSION: &str = "ClusterName";

/// The comparison the alarm that makes the policy add capacity grades the metric with.
const SCALE_OUT_COMPARISON: &str = "GreaterThanThreshold";

/// The error code and the sentence a `targetCapacity` outside the published range is refused
/// with — AWS's fixed text, naming the bounds and nothing from the request.
const CLIENT_EXCEPTION: &str = "ClientException";
const TARGET_CAPACITY_MESSAGE: &str =
    "The managed scaling target capacity value is invalid. Specify an integer greater than 0 \
     and less than or equal to 100 and try again.";

/// How often a poll re-reads a surface it waits on, and how long it keeps trying. The
/// deadline is generous and only here so a failure is a failure rather than a hang.
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
async fn form_call(
    router: &MultiServiceRouter,
    signing_name: &str,
    body: String,
) -> (u16, String) {
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
async fn ec2(router: &MultiServiceRouter, case: &str, action: &str, params: &[(&str, &str)]) -> String {
    let (status, body) = form_call(router, "ec2", form(action, EC2_API_VERSION, params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: EC2 {action} must succeed, got {status} with {body}"
    );
    body
}

/// One Auto Scaling action that must succeed.
async fn asg(router: &MultiServiceRouter, case: &str, action: &str, params: &[(&str, &str)]) -> String {
    let (status, body) = form_call(router, "autoscaling", form(action, ASG_VERSION, params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: Auto Scaling {action} must succeed, got {status} with {body}"
    );
    body
}

/// One Amazon ECS call, answered with its status and parsed body.
async fn ecs(
    router: &MultiServiceRouter,
    operation: &str,
    request: &Value,
) -> (u16, Value) {
    rpc_call(router, "ecs", ECS_TARGET_PREFIX, operation, request).await
}

/// One Amazon ECS call that must answer 200.
async fn ecs_ok(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    request: &Value,
) -> Value {
    let (status, answer) = ecs(router, operation, request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected Amazon ECS {operation} to answer 200, got {status} with \
         {answer}"
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
        "case {case:?}: expected CloudWatch {operation} to answer 200, got {status} with \
         {answer}"
    );
    answer
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

/// The error code an Amazon ECS refusal carries, as a client reads it off the envelope.
fn error_code(answer: &Value) -> String {
    answer["__type"]
        .as_str()
        .unwrap_or_default()
        .rsplit(['#', '/'])
        .next()
        .unwrap_or_default()
        .to_string()
}

/// Create the network and the launch template every group of this suite is built on, and
/// answer the subnet the groups place their instances in.
async fn seed_launch_source(router: &MultiServiceRouter, case: &str) -> String {
    let vpc = element(
        &ec2(router, case, "CreateVpc", &[("CidrBlock", "10.91.0.0/16")]).await,
        "vpcId",
    );
    let subnet = element(
        &ec2(
            router,
            case,
            "CreateSubnet",
            &[
                ("VpcId", vpc.as_str()),
                ("CidrBlock", "10.91.1.0/24"),
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

/// Create one Auto Scaling group holding no instances, through that service's own API.
async fn given_group(router: &MultiServiceRouter, case: &str, subnet: &str, name: &str) {
    asg(
        router,
        case,
        "CreateAutoScalingGroup",
        &[
            ("AutoScalingGroupName", name),
            ("MinSize", "0"),
            ("MaxSize", "2"),
            ("DesiredCapacity", "0"),
            ("LaunchTemplate.LaunchTemplateName", TEMPLATE),
            ("VPCZoneIdentifier", subnet),
        ],
    )
    .await;
}

/// The `autoScalingGroupProvider` a create names: the group, managed scaling in `status`, and
/// the target capacity where the row states one.
fn group_provider(group: &str, status: &str, target_capacity: Option<i32>) -> Value {
    let mut scaling = json!({ "status": status });
    if let Some(target) = target_capacity {
        scaling["targetCapacity"] = json!(target);
    }
    json!({
        "autoScalingGroupArn": group,
        "managedScaling": scaling,
        "managedTerminationProtection": "DISABLED",
    })
}

/// One target-tracking policy as a client reads it off `DescribePolicies`.
#[derive(Debug, Clone, PartialEq)]
struct Policy {
    name: String,
    arn: String,
    policy_type: String,
    namespace: String,
    metric_name: String,
    /// The dimensions of the series it tracks, as `(name, value)` in document order.
    dimensions: Vec<(String, String)>,
    target_value: String,
    /// The names of the CloudWatch alarms the policy reports.
    alarms: Vec<String>,
}

/// The names of every scaling policy Amazon EC2 Auto Scaling reports for `group`, in the
/// order it reports them — the reading a case that expects MORE than one policy on the group
/// takes, where [`group_policies`] deliberately refuses to read a second one's members.
async fn policy_names(router: &MultiServiceRouter, case: &str, group: &str) -> Vec<String> {
    let body = asg(
        router,
        case,
        "DescribePolicies",
        &[("AutoScalingGroupName", group)],
    )
    .await;
    elements(&body, "PolicyName")
}

/// Every scaling policy Amazon EC2 Auto Scaling reports for `group`, read through that
/// service's own API.
///
/// One policy is expected throughout, so the read panics where the answer carries two: a
/// second policy on the group is the rule failing, and it would otherwise be read as the
/// first one's members.
async fn group_policies(router: &MultiServiceRouter, case: &str, group: &str) -> Vec<Policy> {
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
    let Some(name) = names.into_iter().next() else {
        return Vec::new();
    };
    let dimensions = block_of(&body, "Dimensions").map_or_else(Vec::new, |block| {
        elements(block, "Name")
            .into_iter()
            .zip(elements(block, "Value"))
            .collect()
    });
    let alarms = block_of(&body, "Alarms")
        .map_or_else(Vec::new, |block| elements(block, "AlarmName"));
    vec![Policy {
        name,
        arn: element(&body, "PolicyARN"),
        policy_type: element(&body, "PolicyType"),
        namespace: opt_element(&body, "Namespace").unwrap_or_default(),
        metric_name: opt_element(&body, "MetricName").unwrap_or_default(),
        dimensions,
        target_value: opt_element(&body, "TargetValue").unwrap_or_default(),
        alarms,
    }]
}

/// The one policy on `group` once the group has one — looked at again until it does, or the
/// budget runs out.
async fn settled_policy(router: &MultiServiceRouter, case: &str, group: &str) -> Policy {
    let deadline = std::time::Instant::now() + POLL_BUDGET;
    loop {
        let policies = group_policies(router, case, group).await;
        if let Some(policy) = policies.into_iter().next() {
            return policy;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: expected Amazon ECS to put its managed target-tracking policy on \
             {group}, and the group still holds none"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The one policy on `group` once its target value reads `wanted`.
async fn settled_target_value(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    wanted: f64,
) -> Policy {
    let deadline = std::time::Instant::now() + POLL_BUDGET;
    loop {
        let policy = settled_policy(router, case, group).await;
        if policy.target_value.parse::<f64>().ok() == Some(wanted) {
            return policy;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: expected the managed policy on {group} to track {wanted}, got {policy:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The alarms CloudWatch holds under `names`, as `(name, alarm)` in the order it reports them.
async fn alarms_named(
    router: &MultiServiceRouter,
    case: &str,
    names: &[String],
) -> Vec<(String, Value)> {
    if names.is_empty() {
        return Vec::new();
    }
    let described = cloudwatch_ok(
        router,
        case,
        "DescribeAlarms",
        &json!({ "AlarmNames": names }),
    )
    .await;
    described["MetricAlarms"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|alarm| {
            (
                alarm["AlarmName"].as_str().unwrap_or_default().to_string(),
                alarm,
            )
        })
        .collect()
}

/// The one alarm of `names` that makes the policy ADD capacity: the one grading the metric
/// ABOVE its threshold.
async fn scale_out_alarm(router: &MultiServiceRouter, case: &str, names: &[String]) -> Value {
    let held = alarms_named(router, case, names).await;
    let mut above: Vec<Value> = held
        .into_iter()
        .map(|(_, alarm)| alarm)
        .filter(|alarm| alarm["ComparisonOperator"].as_str() == Some(SCALE_OUT_COMPARISON))
        .collect();
    assert_eq!(
        above.len(),
        1,
        "case {case:?}: expected exactly one {SCALE_OUT_COMPARISON} alarm among {names:?}, got \
         {above:?}"
    );
    above.remove(0)
}

/// The dimensions of a CloudWatch alarm, as `(name, value)` in the order it reports them.
fn alarm_dimensions(alarm: &Value) -> Vec<(String, String)> {
    alarm["Dimensions"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|dimension| {
            (
                dimension["Name"].as_str().unwrap_or_default().to_string(),
                dimension["Value"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

/// Associate `provider` with `cluster`, or — with an empty list — take every association away.
async fn associate(
    router: &MultiServiceRouter,
    case: &str,
    cluster: &str,
    providers: &[&str],
) {
    ecs_ok(
        router,
        case,
        "PutClusterCapacityProviders",
        &json!({
            "cluster": cluster,
            "capacityProviders": providers,
            "defaultCapacityProviderStrategy": [],
        }),
    )
    .await;
}

/// Wait until the attachments update an association started has landed, which is what the
/// NEXT association of the same cluster needs ("Cluster attachments must be in
/// UPDATE_COMPLETE or UPDATE_FAILED state before they can be updated"). Looked at again until
/// it has, or the budget runs out.
async fn settled_attachments(router: &MultiServiceRouter, case: &str, cluster: &str) {
    let deadline = std::time::Instant::now() + POLL_BUDGET;
    loop {
        let read = ecs_ok(
            router,
            case,
            "DescribeClusters",
            &json!({ "clusters": [cluster], "include": ["ATTACHMENTS"] }),
        )
        .await;
        let status = read["clusters"][0]["attachmentsStatus"].as_str().unwrap_or_default();
        if status == "UPDATE_COMPLETE" || status == "UPDATE_FAILED" {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: expected the cluster's attachments update to land, got {read}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The series the managed policy must track, as the pair of dimensions the metric is counted
/// by.
fn expected_dimensions(provider: &str, cluster: &str) -> Vec<(String, String)> {
    vec![
        (PROVIDER_DIMENSION.to_string(), provider.to_string()),
        (CLUSTER_DIMENSION.to_string(), cluster.to_string()),
    ]
}

/// One row of the target-capacity table: what the create names, the target value the policy
/// must then hold, and the value a later update re-targets it to.
struct Row {
    case: &'static str,
    group: &'static str,
    cluster: &'static str,
    provider: &'static str,
    /// `managedScaling.targetCapacity` as the create names it, or `None` to leave the member
    /// out so its documented default (100) stands.
    target_capacity: Option<i32>,
    expected_target: f64,
    /// The in-range value an update moves the target capacity to.
    retarget_to: i32,
}

const ROWS: &[Row] = &[
    Row {
        case: "the whole capacity reserved: the member's documented default, named",
        group: "ecsms-full",
        cluster: "ecsmsfull",
        provider: "cpmsfullprovider",
        target_capacity: Some(100),
        expected_target: 100.0,
        retarget_to: 80,
    },
    Row {
        case: "the bottom of the published range",
        group: "ecsms-floor",
        cluster: "ecsmsfloor",
        provider: "cpmsfloorprovider",
        target_capacity: Some(1),
        expected_target: 1.0,
        retarget_to: 100,
    },
    Row {
        case: "a mid-range target capacity",
        group: "ecsms-mid",
        cluster: "ecsmsmid",
        provider: "cpmsmidprovider",
        target_capacity: Some(37),
        expected_target: 37.0,
        retarget_to: 42,
    },
    Row {
        case: "no target capacity at all: the documented default stands",
        group: "ecsms-default",
        cluster: "ecsmsdefault",
        provider: "cpmsdefaultprovider",
        target_capacity: None,
        expected_target: 100.0,
        retarget_to: 1,
    },
];

/// THE RULE: an associated managed-scaling provider has exactly one Amazon ECS-owned
/// target-tracking policy on its group, over the managed-scaling series of that
/// `(provider, cluster)` pair, at the provider's own target capacity — with the alarms it is
/// driven by — and a re-target moves the same policy and the same alarms.
#[tokio::test]
async fn the_managed_scaling_policy_tracks_its_providers_target_capacity() {
    let router = router();
    let subnet = seed_launch_source(&router, "setup").await;

    for row in ROWS {
        let case = row.case;
        given_group(&router, case, &subnet, row.group).await;
        ecs_ok(
            &router,
            case,
            "CreateCluster",
            &json!({ "clusterName": row.cluster }),
        )
        .await;
        ecs_ok(
            &router,
            case,
            "CreateCapacityProvider",
            &json!({
                "name": row.provider,
                "autoScalingGroupProvider": group_provider(
                    row.group,
                    "ENABLED",
                    row.target_capacity,
                ),
            }),
        )
        .await;

        // The metric is counted per capacity provider PER CLUSTER, so a provider that
        // reserves capacity for no cluster has no series to track and no policy.
        let before = group_policies(&router, case, row.group).await;
        assert!(
            before.is_empty(),
            "case {case:?}: a provider associated with no cluster has no series to track, got \
             {before:?}"
        );

        associate(&router, case, row.cluster, &[row.provider]).await;

        let policy = settled_policy(&router, case, row.group).await;
        assert_eq!(
            policy.policy_type, TARGET_TRACKING,
            "case {case:?}: expected a {TARGET_TRACKING} policy, got {policy:?}"
        );
        assert_eq!(
            policy.namespace, METRIC_NAMESPACE,
            "case {case:?}: expected the managed-scaling namespace, got {policy:?}"
        );
        assert_eq!(
            policy.metric_name, METRIC_NAME,
            "case {case:?}: expected the reservation metric, got {policy:?}"
        );
        assert_eq!(
            policy.dimensions,
            expected_dimensions(row.provider, row.cluster),
            "case {case:?}: expected the series to be named by the provider and its cluster, \
             got {policy:?}"
        );
        assert_eq!(
            policy.target_value.parse::<f64>().ok(),
            Some(row.expected_target),
            "case {case:?}: expected the policy to track {}, got {policy:?}",
            row.expected_target
        );
        // The policy is Amazon ECS's own, so the caller never named it: its name is neither
        // the provider's nor the cluster's.
        assert!(
            policy.name != row.provider && policy.name != row.cluster && !policy.name.is_empty(),
            "case {case:?}: expected a name of Amazon ECS's own making, got {policy:?}"
        );
        assert!(
            !policy.alarms.is_empty(),
            "case {case:?}: a target-tracking policy is driven by alarms, got {policy:?}"
        );

        // The alarms the policy reports are alarms CloudWatch resolves, over the same series,
        // and the one that adds capacity grades it against the target value.
        let alarm = scale_out_alarm(&router, case, &policy.alarms).await;
        assert_eq!(
            alarm["Namespace"].as_str(),
            Some(METRIC_NAMESPACE),
            "case {case:?}: expected the alarm over the managed-scaling namespace, got {alarm}"
        );
        assert_eq!(
            alarm["MetricName"].as_str(),
            Some(METRIC_NAME),
            "case {case:?}: expected the alarm over the reservation metric, got {alarm}"
        );
        assert_eq!(
            alarm_dimensions(&alarm),
            expected_dimensions(row.provider, row.cluster),
            "case {case:?}: expected the alarm's series to be the policy's, got {alarm}"
        );
        assert_eq!(
            alarm["Threshold"].as_f64(),
            Some(row.expected_target),
            "case {case:?}: expected the scale-out alarm to grade against {}, got {alarm}",
            row.expected_target
        );
        assert_eq!(
            alarm["AlarmActions"]
                .as_array()
                .and_then(|actions| actions.first())
                .and_then(Value::as_str),
            Some(policy.arn.as_str()),
            "case {case:?}: expected the alarm to execute the policy it drives, got {alarm}"
        );

        // A RE-TARGET follows the provider: the same policy, re-targeted, with its alarms
        // grading against the new value.
        ecs_ok(
            &router,
            case,
            "UpdateCapacityProvider",
            &json!({
                "name": row.provider,
                "cluster": row.cluster,
                "autoScalingGroupProvider": {
                    "managedScaling": { "status": "ENABLED", "targetCapacity": row.retarget_to },
                },
            }),
        )
        .await;
        let retargeted =
            settled_target_value(&router, case, row.group, f64::from(row.retarget_to)).await;
        assert_eq!(
            retargeted.name, policy.name,
            "case {case:?}: expected the SAME policy to be re-targeted, got {retargeted:?} \
             after {policy:?}"
        );
        assert_eq!(
            retargeted.dimensions,
            expected_dimensions(row.provider, row.cluster),
            "case {case:?}: expected the re-targeted policy to track the same series, got \
             {retargeted:?}"
        );
        let retargeted_alarm = scale_out_alarm(&router, case, &retargeted.alarms).await;
        assert_eq!(
            retargeted_alarm["Threshold"].as_f64(),
            Some(f64::from(row.retarget_to)),
            "case {case:?}: expected the alarm to follow the re-target, got {retargeted_alarm}"
        );
    }
}

/// One row of the refusal table: a target capacity outside the range the member publishes.
struct OutsideRow {
    case: &'static str,
    group: &'static str,
    provider: &'static str,
    target_capacity: i32,
}

const OUTSIDE_ROWS: &[OutsideRow] = &[
    OutsideRow {
        case: "one past the top of the published range",
        group: "ecsmsbad-101",
        provider: "cpmsbad101provider",
        target_capacity: 101,
    },
    OutsideRow {
        case: "well past the top of the range",
        group: "ecsmsbad-150",
        provider: "cpmsbad150provider",
        target_capacity: 150,
    },
    OutsideRow {
        case: "one below the bottom of the range",
        group: "ecsmsbad-zero",
        provider: "cpmsbadzeroprovider",
        target_capacity: 0,
    },
    OutsideRow {
        case: "a negative percentage",
        group: "ecsmsbad-negative",
        provider: "cpmsbadnegativeprovider",
        target_capacity: -5,
    },
];

/// THE REFUSAL: a `targetCapacity` outside the published range is refused with
/// `ClientException` and AWS's own sentence — on the create, which then leaves no provider,
/// and on the update, which leaves the provider, its policy and its alarms exactly as they
/// stood.
#[tokio::test]
async fn a_target_capacity_outside_the_published_range_is_refused_and_changes_nothing() {
    let router = router();
    let subnet = seed_launch_source(&router, "setup").await;

    // One served provider the refused updates are aimed at: created in range, associated, and
    // tracking its target capacity.
    let (group, cluster, provider) = ("ecsms-held", "ecsmsheld", "cpmsheldprovider");
    given_group(&router, "setup", &subnet, group).await;
    ecs_ok(
        &router,
        "setup",
        "CreateCluster",
        &json!({ "clusterName": cluster }),
    )
    .await;
    ecs_ok(
        &router,
        "setup",
        "CreateCapacityProvider",
        &json!({
            "name": provider,
            "autoScalingGroupProvider": group_provider(group, "ENABLED", Some(60)),
        }),
    )
    .await;
    associate(&router, "setup", cluster, &[provider]).await;
    let held = settled_target_value(&router, "setup", group, 60.0).await;

    for row in OUTSIDE_ROWS {
        let case = row.case;

        // The CREATE: refused, and nothing is created.
        given_group(&router, case, &subnet, row.group).await;
        let (status, answer) = ecs(
            &router,
            "CreateCapacityProvider",
            &json!({
                "name": row.provider,
                "autoScalingGroupProvider": group_provider(
                    row.group,
                    "ENABLED",
                    Some(row.target_capacity),
                ),
            }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 for targetCapacity {}, got {status} with {answer}",
            row.target_capacity
        );
        assert_eq!(
            error_code(&answer),
            CLIENT_EXCEPTION,
            "case {case:?}: expected the modelled {CLIENT_EXCEPTION}, got {answer}"
        );
        assert_eq!(
            answer["message"].as_str().or(answer["Message"].as_str()),
            Some(TARGET_CAPACITY_MESSAGE),
            "case {case:?}: expected AWS's target-capacity sentence, got {answer}"
        );
        let described = ecs_ok(
            &router,
            case,
            "DescribeCapacityProviders",
            &json!({ "capacityProviders": [row.provider] }),
        )
        .await;
        assert_eq!(
            described["capacityProviders"].as_array().map(Vec::len),
            Some(0),
            "case {case:?}: a refused create leaves no provider behind, got {described}"
        );
        let policies = group_policies(&router, case, row.group).await;
        assert!(
            policies.is_empty(),
            "case {case:?}: a refused create puts no policy on the group, got {policies:?}"
        );

        // The UPDATE of a provider that HAS a policy: refused, and the provider, the policy
        // and the alarm are all left as they were.
        let (status, answer) = ecs(
            &router,
            "UpdateCapacityProvider",
            &json!({
                "name": provider,
                "cluster": cluster,
                "autoScalingGroupProvider": {
                    "managedScaling": {
                        "status": "ENABLED",
                        "targetCapacity": row.target_capacity,
                    },
                },
            }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the update to be refused, got {status} with {answer}"
        );
        assert_eq!(
            error_code(&answer),
            CLIENT_EXCEPTION,
            "case {case:?}: expected the modelled {CLIENT_EXCEPTION} on the update, got {answer}"
        );
        assert_eq!(
            answer["message"].as_str().or(answer["Message"].as_str()),
            Some(TARGET_CAPACITY_MESSAGE),
            "case {case:?}: expected AWS's target-capacity sentence on the update, got {answer}"
        );

        let unchanged = group_policies(&router, case, group).await;
        assert_eq!(
            unchanged,
            vec![held.clone()],
            "case {case:?}: a refused update leaves the managed policy exactly as it was"
        );
        let alarm = scale_out_alarm(&router, case, &held.alarms).await;
        assert_eq!(
            alarm["Threshold"].as_f64(),
            Some(60.0),
            "case {case:?}: a refused update leaves the alarm grading against the target \
             capacity in force, got {alarm}"
        );
        let provider_view = ecs_ok(
            &router,
            case,
            "DescribeCapacityProviders",
            &json!({ "capacityProviders": [provider] }),
        )
        .await;
        assert_eq!(
            provider_view["capacityProviders"][0]["autoScalingGroupProvider"]["managedScaling"]
                ["targetCapacity"]
                .as_i64(),
            Some(60),
            "case {case:?}: a refused update leaves the provider's target capacity in force, \
             got {provider_view}"
        );
    }
}

/// What takes the managed policy away again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Withdrawal {
    /// Managed scaling turned off through an update of the provider.
    ManagedScalingOff,
    /// The association removed, so the provider reserves capacity for no cluster.
    Disassociated,
    /// The provider deleted.
    Deleted,
}

/// One row of the withdrawal table.
struct WithdrawRow {
    case: &'static str,
    group: &'static str,
    cluster: &'static str,
    provider: &'static str,
    withdrawal: Withdrawal,
}

const WITHDRAW_ROWS: &[WithdrawRow] = &[
    WithdrawRow {
        case: "managed scaling turned off: the capacity is the owner's to manage again",
        group: "ecsmsoff-group",
        cluster: "ecsmsoff",
        provider: "cpmsoffprovider",
        withdrawal: Withdrawal::ManagedScalingOff,
    },
    WithdrawRow {
        case: "the association removed: the provider reserves capacity for no cluster",
        group: "ecsmsgone-group",
        cluster: "ecsmsgone",
        provider: "cpmsgoneprovider",
        withdrawal: Withdrawal::Disassociated,
    },
    WithdrawRow {
        case: "the provider deleted",
        group: "ecsmsdel-group",
        cluster: "ecsmsdel",
        provider: "cpmsdelprovider",
        withdrawal: Withdrawal::Deleted,
    },
];

/// THE OTHER HALF OF THE RULE: a provider that no longer asks Amazon ECS to manage its
/// group's scaling has the policy — and its alarms — taken away; and a provider created with
/// managed scaling OFF never had one.
#[tokio::test]
async fn a_provider_that_stops_asking_for_managed_scaling_loses_the_policy() {
    let router = router();
    let subnet = seed_launch_source(&router, "setup").await;

    // Managed scaling off from the start: "If managed scaling is off, the user must manage the
    // scaling of the Auto Scaling group", so Amazon ECS puts no policy on it at all.
    let case = "managed scaling DISABLED from the start";
    given_group(&router, case, &subnet, "ecsmsdisabled-group").await;
    ecs_ok(
        &router,
        case,
        "CreateCluster",
        &json!({ "clusterName": "ecsmsdisabled" }),
    )
    .await;
    ecs_ok(
        &router,
        case,
        "CreateCapacityProvider",
        &json!({
            "name": "cpmsdisabledprovider",
            "autoScalingGroupProvider": group_provider(
                "ecsmsdisabled-group",
                "DISABLED",
                Some(70),
            ),
        }),
    )
    .await;
    associate(&router, case, "ecsmsdisabled", &["cpmsdisabledprovider"]).await;
    let none = group_policies(&router, case, "ecsmsdisabled-group").await;
    assert!(
        none.is_empty(),
        "case {case:?}: a provider whose managed scaling is off has no managed policy, got \
         {none:?}"
    );

    for row in WITHDRAW_ROWS {
        let case = row.case;
        given_group(&router, case, &subnet, row.group).await;
        ecs_ok(
            &router,
            case,
            "CreateCluster",
            &json!({ "clusterName": row.cluster }),
        )
        .await;
        ecs_ok(
            &router,
            case,
            "CreateCapacityProvider",
            &json!({
                "name": row.provider,
                "autoScalingGroupProvider": group_provider(row.group, "ENABLED", Some(90)),
            }),
        )
        .await;
        associate(&router, case, row.cluster, &[row.provider]).await;
        let policy = settled_target_value(&router, case, row.group, 90.0).await;
        let alarms = policy.alarms.clone();
        assert!(
            !alarms.is_empty(),
            "case {case:?}: expected the policy to hold alarms before the withdrawal, got \
             {policy:?}"
        );

        match row.withdrawal {
            Withdrawal::ManagedScalingOff => {
                ecs_ok(
                    &router,
                    case,
                    "UpdateCapacityProvider",
                    &json!({
                        "name": row.provider,
                        "cluster": row.cluster,
                        "autoScalingGroupProvider": {
                            "managedScaling": { "status": "DISABLED" },
                        },
                    }),
                )
                .await;
            }
            Withdrawal::Disassociated => {
                settled_attachments(&router, case, row.cluster).await;
                associate(&router, case, row.cluster, &[]).await;
            }
            Withdrawal::Deleted => {
                settled_attachments(&router, case, row.cluster).await;
                associate(&router, case, row.cluster, &[]).await;
                ecs_ok(
                    &router,
                    case,
                    "DeleteCapacityProvider",
                    &json!({ "capacityProvider": row.provider }),
                )
                .await;
            }
        }

        let left = group_policies(&router, case, row.group).await;
        assert!(
            left.is_empty(),
            "case {case:?}: expected Amazon ECS to take its managed policy away, got {left:?}"
        );
        let held = alarms_named(&router, case, &alarms).await;
        assert!(
            held.is_empty(),
            "case {case:?}: expected the policy's alarms to go with it, got {held:?}"
        );
    }
}

/// The policy a CALLER puts on the group themselves, named nothing like the one Amazon ECS
/// mints.
const CALLER_POLICY: &str = "my-own-reservation-tracking";

/// THE OWNERSHIP BOUNDARY: only the policy Amazon ECS MINTED is Amazon ECS's to withdraw.
///
/// A caller may put a target-tracking policy of their own on the same group over the very
/// same managed-scaling series — nothing stops them — and that policy is theirs: the pass
/// that takes Amazon ECS's policy away when the provider stops wanting one must leave the
/// caller's exactly where it is.
#[tokio::test]
async fn a_callers_own_policy_over_the_same_series_is_not_amazon_ecss_to_withdraw() {
    let case = "a caller's own target-tracking policy over the managed-scaling series";
    let router = router();
    let subnet = seed_launch_source(&router, case).await;
    let (group, cluster, provider) = ("ecsms-shared", "ecsmsshared", "cpmssharedprovider");

    given_group(&router, case, &subnet, group).await;
    ecs_ok(
        &router,
        case,
        "CreateCluster",
        &json!({ "clusterName": cluster }),
    )
    .await;
    ecs_ok(
        &router,
        case,
        "CreateCapacityProvider",
        &json!({
            "name": provider,
            "autoScalingGroupProvider": group_provider(group, "ENABLED", Some(90)),
        }),
    )
    .await;
    associate(&router, case, cluster, &[provider]).await;
    let managed = settled_policy(&router, case, group).await;

    // The caller's own policy, over the same series Amazon ECS tracks, through Amazon EC2
    // Auto Scaling's own API.
    let specification = "TargetTrackingConfiguration.CustomizedMetricSpecification";
    asg(
        &router,
        case,
        "PutScalingPolicy",
        &[
            ("AutoScalingGroupName", group),
            ("PolicyName", CALLER_POLICY),
            ("PolicyType", TARGET_TRACKING),
            ("TargetTrackingConfiguration.TargetValue", "50"),
            (
                &format!("{specification}.MetricName"),
                METRIC_NAME,
            ),
            (&format!("{specification}.Namespace"), METRIC_NAMESPACE),
            (
                &format!("{specification}.Statistic"),
                "Average",
            ),
            (
                &format!("{specification}.Dimensions.member.1.Name"),
                PROVIDER_DIMENSION,
            ),
            (
                &format!("{specification}.Dimensions.member.1.Value"),
                provider,
            ),
            (
                &format!("{specification}.Dimensions.member.2.Name"),
                CLUSTER_DIMENSION,
            ),
            (
                &format!("{specification}.Dimensions.member.2.Value"),
                cluster,
            ),
        ],
    )
    .await;
    let both = policy_names(&router, case, group).await;
    assert!(
        both.contains(&CALLER_POLICY.to_string()) && both.contains(&managed.name),
        "case {case:?}: expected the group to hold the caller's policy beside Amazon ECS's, \
         got {both:?}"
    );

    // The provider stops wanting one: Amazon ECS withdraws ITS policy and leaves the
    // caller's.
    settled_attachments(&router, case, cluster).await;
    associate(&router, case, cluster, &[]).await;
    let left = policy_names(&router, case, group).await;
    assert_eq!(
        left,
        vec![CALLER_POLICY.to_string()],
        "case {case:?}: expected only the caller's own policy to be left, got {left:?}"
    );
}
