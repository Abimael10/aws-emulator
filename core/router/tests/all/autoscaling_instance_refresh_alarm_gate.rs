// SPDX-License-Identifier: CC0-1.0

//! An instance refresh is gated on the CloudWatch alarms it names — over the WHOLE mounted
//! service set, the one `core/router/src/lib.rs` assembles for the binary.
//!
//! The RULE under test: **the alarms a refresh's `Preferences.AlarmSpecification` names are
//! the monitors Amazon EC2 Auto Scaling watches that refresh against, so a refresh is
//! refused with `ValidationError` and the sentence "A CloudWatch monitor is in alarm. The
//! instance refresh cannot start." while any of them is in the `ALARM` state — creating
//! nothing, so `DescribeInstanceRefreshes` is unchanged — and the identical call starts a
//! refresh once none of them is sounding.**
//!
//! Three services have to agree for that to hold: Amazon EC2 holds the launch template the
//! group is built on, CloudWatch holds the alarm and the state `SetAlarmState` forces on it,
//! and Auto Scaling reads that state out of CloudWatch's own store at the instant the
//! refresh is asked for. The rule's own table — how many monitors are named, which state
//! each is in, where the sounding one sits in the list, a name no alarm answers to — is
//! pinned by the service's own suite,
//! `services/service-com-amazonaws-autoscaling-autoscaling-2011-01-01/tests/all/instance_refresh_lifecycle.rs`;
//! what only this suite reaches is the same path over the production service set.
//!
//! The rows differ along what the rule generalises over: the state the monitor is in and the
//! Region the whole wiring lives in.

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudwatch_alarm_action_reaches_its_subscribed_queue::{authorization, router};
use crate::query_wire::{element, elements, form_encode, opt_element};

/// CloudWatch's `X-Amz-Target` prefix and its signing name (`sigv4Name: monitoring`).
const CLOUDWATCH_TARGET_PREFIX: &str = "GraniteServiceVersion20100801";
const CLOUDWATCH_SIGNING_NAME: &str = "monitoring";

/// The awsQuery API version an Auto Scaling client sends, and the ec2Query one Amazon EC2
/// expects.
const AUTOSCALING_VERSION: &str = "2011-01-01";
const EC2_VERSION: &str = "2016-11-15";

/// The sentence a refresh is refused with while a monitor it names is sounding: fixed text,
/// carrying no part of the request.
const MONITOR_IN_ALARM_MESSAGE: &str =
    "A CloudWatch monitor is in alarm. The instance refresh cannot start.";

/// One form-wire call against the service whose signing name is `signing_name`, answered as
/// `(status, body)`.
async fn form_call(
    router: &MultiServiceRouter,
    region: &str,
    signing_name: &str,
    version: &str,
    action: &str,
    params: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in params {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{region}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(signing_name, region))
        .body(Bytes::from(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    (status, text)
}

/// One form-wire call that must answer 200.
async fn form_ok(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    signing_name: &str,
    version: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let (status, body) = form_call(router, region, signing_name, version, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {signing_name} {action}, got {status} with \
         {body}"
    );
    body
}

/// One CloudWatch RPC call that must answer 200.
async fn cloudwatch_ok(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header(
            "host",
            format!("{CLOUDWATCH_SIGNING_NAME}.{region}.amazonaws.com"),
        )
        .header("content-type", "application/x-amz-json-1.0")
        .header(
            "x-amz-target",
            format!("{CLOUDWATCH_TARGET_PREFIX}.{operation}"),
        )
        .header(
            "authorization",
            authorization(CLOUDWATCH_SIGNING_NAME, region),
        )
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CloudWatch {operation}, got {status} with \
         {text}"
    );
    if text.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&text).unwrap_or_else(|_| json!({"__unparsed": text}))
    }
}

/// Force the alarm `name` into `state`, through CloudWatch's own API.
async fn force_monitor(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    name: &str,
    state: &str,
) {
    cloudwatch_ok(
        router,
        case,
        region,
        "SetAlarmState",
        &json!({
            "AlarmName": name,
            "StateValue": state,
            "StateReason": "forced by this suite",
        }),
    )
    .await;
}

/// The refresh ids `DescribeInstanceRefreshes` reports for one group.
async fn reported_refreshes(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    group: &str,
) -> Vec<String> {
    let body = form_ok(
        router,
        case,
        region,
        "autoscaling",
        AUTOSCALING_VERSION,
        "DescribeInstanceRefreshes",
        &[("AutoScalingGroupName", group)],
    )
    .await;
    elements(&body, "InstanceRefreshId")
}

#[tokio::test]
async fn a_refresh_will_not_start_while_its_cloudwatch_monitor_is_in_alarm() {
    // (case, the Region the whole wiring lives in, the state the monitor is forced into,
    //  whether the refresh starts)
    let rows: &[(&str, &str, &str, bool)] = &[
        ("a monitor that is sounding", "us-west-2", "ALARM", false),
        ("a monitor that is well", "eu-west-1", "OK", true),
        (
            "a monitor with nothing to grade",
            "us-east-1",
            "INSUFFICIENT_DATA",
            true,
        ),
    ];

    for (index, (case, region, state, starts)) in rows.iter().enumerate() {
        let router = router();
        let template = format!("router-refresh-template-{index}");
        let group = format!("router-refresh-group-{index}");
        let monitor = format!("router-refresh-monitor-{index}");

        // The launch template the group is built on is Amazon EC2's own resource.
        let created = form_ok(
            &router,
            case,
            region,
            "ec2",
            EC2_VERSION,
            "CreateLaunchTemplate",
            &[
                ("LaunchTemplateName", template.as_str()),
                ("LaunchTemplateData.ImageId", "ami-0123456789abcdef0"),
                ("LaunchTemplateData.InstanceType", "t3.micro"),
            ],
        )
        .await;
        assert!(
            opt_element(&created, "launchTemplateId").is_some(),
            "case {case:?}: Amazon EC2 must report the template it created, got {created}"
        );

        form_ok(
            &router,
            case,
            region,
            "autoscaling",
            AUTOSCALING_VERSION,
            "CreateAutoScalingGroup",
            &[
                ("AutoScalingGroupName", group.as_str()),
                ("LaunchTemplate.LaunchTemplateName", template.as_str()),
                ("MinSize", "0"),
                ("MaxSize", "2"),
                ("DesiredCapacity", "0"),
                ("AvailabilityZones.member.1", &format!("{region}a")),
            ],
        )
        .await;

        // The monitor is CloudWatch's own alarm, in the state CloudWatch holds for it.
        cloudwatch_ok(
            &router,
            case,
            region,
            "PutMetricAlarm",
            &json!({
                "AlarmName": monitor,
                "Namespace": "Custom/Refresh",
                "MetricName": "Latency",
                "Statistic": "Average",
                "Period": 60,
                "EvaluationPeriods": 1,
                "Threshold": 10.0,
                "ComparisonOperator": "GreaterThanThreshold",
            }),
        )
        .await;
        force_monitor(&router, case, region, &monitor, state).await;

        let refresh_params: &[(&str, &str)] = &[
            ("AutoScalingGroupName", group.as_str()),
            ("Strategy", "Rolling"),
            ("Preferences.AlarmSpecification.Alarms.member.1", &monitor),
            ("Preferences.MinHealthyPercentage", "100"),
            ("Preferences.MaxHealthyPercentage", "200"),
        ];
        let (status, body) = form_call(
            &router,
            region,
            "autoscaling",
            AUTOSCALING_VERSION,
            "StartInstanceRefresh",
            refresh_params,
        )
        .await;

        if *starts {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the refresh to start, got {status} with {body}"
            );
            let id = element(&body, "InstanceRefreshId");
            let reported = reported_refreshes(&router, case, region, &group).await;
            assert_eq!(
                reported,
                vec![id],
                "case {case:?}: the group must report the refresh it started, got \
                 {reported:?}"
            );
            continue;
        }

        assert_eq!(
            status, 400,
            "case {case:?}: expected the refresh to be refused, got {status} with {body}"
        );
        assert_eq!(
            element(&body, "Code"),
            "ValidationError",
            "case {case:?}: expected the ValidationError code, got {body}"
        );
        assert_eq!(
            elements(&body, "Message").first().map(String::as_str),
            Some(MONITOR_IN_ALARM_MESSAGE),
            "case {case:?}: expected the fixed monitor sentence, got {body}"
        );
        let reported = reported_refreshes(&router, case, region, &group).await;
        assert!(
            reported.is_empty(),
            "case {case:?}: a refused refresh leaves nothing in flight, got {reported:?}"
        );

        // With the monitor back in OK the identical call starts a refresh: the gate is the
        // alarm's CURRENT state, read from CloudWatch at the instant of the call.
        force_monitor(&router, case, region, &monitor, "OK").await;
        let cleared = form_ok(
            &router,
            case,
            region,
            "autoscaling",
            AUTOSCALING_VERSION,
            "StartInstanceRefresh",
            refresh_params,
        )
        .await;
        let id = element(&cleared, "InstanceRefreshId");
        let reported = reported_refreshes(&router, case, region, &group).await;
        assert_eq!(
            reported,
            vec![id],
            "case {case:?}: the cleared call must leave exactly its own refresh, got \
             {reported:?}"
        );
    }
}
