// SPDX-License-Identifier: CC0-1.0

//! `DeletePolicy`: removing a scaling policy of an Auto Scaling group, over the awsQuery
//! wire.
//!
//! The rules under test:
//!
//! 1. "Deletes the specified scaling policy" — the policy a request names by NAME or by ARN,
//!    the two spellings `DeletePolicyType$PolicyName` states ("the name or Amazon Resource
//!    Name (ARN) of the policy"), is gone from the group's policies afterwards and
//!    `DescribePolicies` no longer reports it, while every OTHER policy of that group stays
//!    exactly as it was.
//! 2. "Deleting either a step scaling policy or a simple scaling policy deletes the
//!    underlying alarm action, but does not delete the alarm": the alarms a TARGET-TRACKING
//!    policy IS — "the CloudWatch alarms created for the target tracking scaling policy"
//!    (`PolicyARNType$Alarms`) — are retired from the account's own CloudWatch store with the
//!    policy, and no policy of another type holds one to retire.
//! 3. A request that names no policy of the account, or a group the account does not hold, is
//!    refused with HTTP 400 `ValidationError` and NOTHING is removed.
//!
//! The table spans what the rules generalise over: the policy's type, the spelling the
//! request names it by, and whether the group is named beside it.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_autoscaling_autoscaling_2011_01_01 as autoscaling;
use service_com_amazonaws_cloudwatch_graniteserviceversion20100801 as cloudwatch_service;

use crate::support::{borrowed, new_engine, p, query, tag_value, tag_values, xml_unescape, REGION};

/// The launch source every group in this suite is created from — a launch TEMPLATE, because
/// no account can create a launch configuration (see
/// `launch_configurations_are_unavailable.rs`).
const LAUNCH_SOURCE: (&str, &str) = ("LaunchTemplate.LaunchTemplateName", "lt-1");

/// The `x-amz-target` prefix CloudWatch's awsJson protocol names its operations under.
const CLOUDWATCH_TARGET_PREFIX: &str = "GraniteServiceVersion20100801";

/// A router carrying Amazon EC2 Auto Scaling and CloudWatch on ONE engine: the alarms a
/// target-tracking policy mints live in CloudWatch's own store, so the readback that proves
/// they are retired goes through CloudWatch's own API.
fn router() -> MultiServiceRouter {
    let engine = new_engine();
    crate::support::seed_launch_templates(&engine, &[LAUNCH_SOURCE.1]);
    let mut router = MultiServiceRouter::new();
    router.register_service(autoscaling::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(cloudwatch_service::EmulatedService::new(Arc::clone(&engine)));
    router
}

/// Create one group and assert the create succeeded.
async fn create_group(router: &MultiServiceRouter, name: &str) {
    let (status, body) = query(
        router,
        "CreateAutoScalingGroup",
        &[
            ("AutoScalingGroupName", name),
            LAUNCH_SOURCE,
            ("MinSize", "0"),
            ("MaxSize", "4"),
            ("DesiredCapacity", "0"),
            ("AvailabilityZones.member.1", "us-west-2a"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "the group {name:?} must be created, got {status} with {body}"
    );
}

/// Put one scaling policy and answer the `(PolicyARN, alarm names)` the service answered.
async fn put_policy(
    router: &MultiServiceRouter,
    group: &str,
    policy: &str,
    members: &[(String, String)],
) -> (String, Vec<String>) {
    let mut params = vec![p("AutoScalingGroupName", group), p("PolicyName", policy)];
    params.extend(members.iter().cloned());
    let (status, body) = query(router, "PutScalingPolicy", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "the policy {policy:?} must be put, got {status} with {body}"
    );
    let arn = tag_value(&body, "PolicyARN");
    assert!(
        !arn.is_empty(),
        "PutScalingPolicy must report the policy's ARN, got {body}"
    );
    (arn, tag_values(&body, "AlarmName"))
}

/// The policy names Amazon EC2 Auto Scaling reports for `group` right now.
async fn reported_policies(router: &MultiServiceRouter, group: &str) -> Vec<String> {
    let (status, body) = query(
        router,
        "DescribePolicies",
        &[("AutoScalingGroupName", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribePolicies for {group:?} expected 200, got {status}: {body}"
    );
    tag_values(&body, "PolicyName")
}

/// POST one `awsJson1_0` CloudWatch call and answer its parsed body, requiring HTTP 200.
async fn cloudwatch(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://monitoring.{REGION}.amazonaws.com/"))
        .header("host", format!("monitoring.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header(
            "x-amz-target",
            format!("{CLOUDWATCH_TARGET_PREFIX}.{operation}"),
        )
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20240101/{REGION}/monitoring/aws4_request, \
                 SignedHeaders=host, Signature=abc"
            ),
        )
        .body(bytes::Bytes::from(
            serde_json::to_vec(body).expect("a serializable request"),
        ))
        .expect("a valid request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.body().clone();
    let parsed = if payload.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&payload).unwrap_or(Value::Null)
    };
    assert_eq!(
        status, 200,
        "{operation}: expected HTTP 200, got {status} with {parsed}"
    );
    parsed
}

/// The alarm names CloudWatch's own store resolves for `names`.
async fn resolved_alarm_names(router: &MultiServiceRouter, names: &[String]) -> Vec<String> {
    let answer = cloudwatch(
        router,
        "DescribeAlarms",
        &json!({"AlarmNames": names, "AlarmTypes": ["MetricAlarm"]}),
    )
    .await;
    answer
        .pointer("/MetricAlarms")
        .and_then(Value::as_array)
        .map(|alarms| {
            alarms
                .iter()
                .filter_map(|alarm| alarm.get("AlarmName").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// The members of a `SimpleScaling` put.
fn simple_policy() -> Vec<(String, String)> {
    vec![
        p("PolicyType", "SimpleScaling"),
        p("AdjustmentType", "ChangeInCapacity"),
        p("ScalingAdjustment", "1"),
    ]
}

/// The members of a `StepScaling` put.
fn step_policy() -> Vec<(String, String)> {
    vec![
        p("PolicyType", "StepScaling"),
        p("AdjustmentType", "ChangeInCapacity"),
        p("StepAdjustments.member.1.MetricIntervalLowerBound", "0"),
        p("StepAdjustments.member.1.ScalingAdjustment", "1"),
    ]
}

/// The members of a `TargetTrackingScaling` put, which is the one type that holds alarms.
fn target_tracking_policy() -> Vec<(String, String)> {
    vec![
        p("PolicyType", "TargetTrackingScaling"),
        p("TargetTrackingConfiguration.TargetValue", "40"),
        p(
            "TargetTrackingConfiguration.PredefinedMetricSpecification.PredefinedMetricType",
            "ASGAverageCPUUtilization",
        ),
    ]
}

/// How the delete names the policy it removes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Named {
    /// By its name, inside the group the request names.
    NameWithGroup,
    /// By the ARN the service minted, with the group named too.
    ArnWithGroup,
    /// By that ARN alone, which names the policy wherever it lives.
    ArnAlone,
}

/// One delete: the case, the policy put for it, and how the delete names it.
struct DeleteCase<'a> {
    case: &'a str,
    members: fn() -> Vec<(String, String)>,
    named: Named,
}

/// Rules 1 and 2: the named policy is gone, the other policy of the group is untouched, and
/// the alarms the policy held are retired with it.
#[tokio::test]
async fn a_deleted_policy_is_gone_and_takes_its_alarms_with_it() {
    let cases = [
        DeleteCase {
            case: "a simple-scaling policy named by name",
            members: simple_policy,
            named: Named::NameWithGroup,
        },
        DeleteCase {
            case: "a step-scaling policy named by its ARN",
            members: step_policy,
            named: Named::ArnWithGroup,
        },
        DeleteCase {
            case: "a target-tracking policy named by its ARN alone",
            members: target_tracking_policy,
            named: Named::ArnAlone,
        },
    ];

    for (index, case) in cases.iter().enumerate() {
        let router = router();
        let group = format!("deleting-{index}");
        create_group(&router, &group).await;
        let (arn, alarms) = put_policy(&router, &group, "doomed", &(case.members)()).await;
        // The policy that must survive the delete of the other one.
        put_policy(&router, &group, "kept", &simple_policy()).await;
        if !alarms.is_empty() {
            let resolved = resolved_alarm_names(&router, &alarms).await;
            assert_eq!(
                resolved.len(),
                alarms.len(),
                "case {:?}: CloudWatch must hold the alarms the policy reported {alarms:?}, got \
                 {resolved:?}",
                case.case
            );
        }

        let params: Vec<(String, String)> = match case.named {
            Named::NameWithGroup => vec![
                p("AutoScalingGroupName", group.clone()),
                p("PolicyName", "doomed"),
            ],
            Named::ArnWithGroup => vec![
                p("AutoScalingGroupName", group.clone()),
                p("PolicyName", arn.clone()),
            ],
            Named::ArnAlone => vec![p("PolicyName", arn.clone())],
        };
        let (status, body) = query(&router, "DeletePolicy", &borrowed(&params)).await;
        assert_eq!(
            status, 200,
            "case {:?}: DeletePolicy expected 200, got {status}: {body}",
            case.case
        );
        assert!(
            body.contains("<DeletePolicyResponse"),
            "case {:?}: expected this operation's own success envelope, got {body}",
            case.case
        );

        let remaining = reported_policies(&router, &group).await;
        assert_eq!(
            remaining,
            vec!["kept".to_string()],
            "case {:?}: expected the deleted policy gone and the other one kept, got \
             {remaining:?}",
            case.case
        );
        // The alarms the policy held are CloudWatch's own resources, so CloudWatch is what
        // says they are gone.
        if !alarms.is_empty() {
            let resolved = resolved_alarm_names(&router, &alarms).await;
            assert!(
                resolved.is_empty(),
                "case {:?}: expected the policy's alarms {alarms:?} retired with it, got \
                 {resolved:?}",
                case.case
            );
        }
    }
}

/// One refused delete: the case, the members the request carries, and the sentence the
/// refusal states.
type RefusalCase<'a> = (&'a str, Vec<(String, String)>, String);

/// Rule 3: a delete that names no policy of the account is refused, and removes nothing.
#[tokio::test]
async fn a_delete_that_names_no_policy_is_refused() {
    let router = router();
    create_group(&router, "refusing").await;
    put_policy(&router, "refusing", "kept", &simple_policy()).await;
    // A policy of the same name on ANOTHER group, so the row below asks for a name the
    // account holds but the NAMED group does not.
    create_group(&router, "elsewhere").await;
    put_policy(&router, "elsewhere", "kept-elsewhere", &simple_policy()).await;

    let cases: [RefusalCase<'_>; 5] = [
        (
            "a policy name nothing answers to",
            vec![
                p("AutoScalingGroupName", "refusing"),
                p("PolicyName", "no-such-policy"),
            ],
            "Policy no-such-policy not found".to_string(),
        ),
        (
            "a policy name of ANOTHER group than the one the request names",
            vec![
                p("AutoScalingGroupName", "refusing"),
                p("PolicyName", "kept-elsewhere"),
            ],
            "Policy kept-elsewhere not found".to_string(),
        ),
        // The name is the sentence's only variable part, so the rows below differ in its
        // FORM: an ARN spelling, which "The name or Amazon Resource Name (ARN) of the
        // policy" (`DeletePolicyType$PolicyName`) admits, and a name carrying the spaces
        // and punctuation the member's pattern allows.
        (
            "a policy ARN nothing answers to",
            vec![
                p("AutoScalingGroupName", "refusing"),
                p(
                    "PolicyName",
                    "arn:aws:autoscaling:us-west-2:000000000000:scalingPolicy:abc:\
                     autoScalingGroupName/refusing:policyName/ghost",
                ),
            ],
            "Policy arn:aws:autoscaling:us-west-2:000000000000:scalingPolicy:abc:\
             autoScalingGroupName/refusing:policyName/ghost not found"
                .to_string(),
        ),
        (
            "a policy name carrying spaces and punctuation",
            vec![
                p("AutoScalingGroupName", "refusing"),
                p("PolicyName", "scale out (peak hours)"),
            ],
            "Policy scale out (peak hours) not found".to_string(),
        ),
        (
            "a group the account does not hold",
            vec![
                p("AutoScalingGroupName", "no-such-group"),
                p("PolicyName", "kept"),
            ],
            "AutoScalingGroup name not found - AutoScalingGroup no-such-group not found"
                .to_string(),
        ),
    ];

    for (case, params, message) in &cases {
        let (status, body) = query(&router, "DeletePolicy", &borrowed(params)).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400, got {status}: {body}"
        );
        assert_eq!(
            tag_value(&body, "Code"),
            "ValidationError",
            "case {case:?}: expected the ValidationError code, got {body}"
        );
        let answered = xml_unescape(&tag_value(&body, "Message"));
        assert_eq!(
            &answered, message,
            "case {case:?}: expected the message {message:?}, got {answered:?}"
        );
        let remaining = reported_policies(&router, "refusing").await;
        assert_eq!(
            remaining,
            vec!["kept".to_string()],
            "case {case:?}: a refused delete removes nothing, got {remaining:?}"
        );
        let elsewhere = reported_policies(&router, "elsewhere").await;
        assert_eq!(
            elsewhere,
            vec!["kept-elsewhere".to_string()],
            "case {case:?}: a refused delete removes nothing from the other group either, got \
             {elsewhere:?}"
        );
    }
}
