// SPDX-License-Identifier: CC0-1.0

//! A load balancer's target-health alarm reaches the SQS target of the EventBridge rule a
//! client wires to it — over the WHOLE assembled emulator, the service set the binary serves.
//!
//! The RULE under test: **a change of a CloudWatch alarm's `StateValue` is published once onto
//! the account's default EventBridge bus as `source` `aws.cloudwatch` / `detail-type`
//! `CloudWatch Alarm State Change`, carrying the alarm's ARN in `resources` and the state it
//! moved to inside `detail`; a rule whose `EventPattern` matches that envelope delivers it to
//! that rule's targets, and until such a rule exists the same transitions are delivered
//! nowhere.** A `SetAlarmState` into the state the alarm is already in is not such a change —
//! it moves the alarm nowhere and publishes nothing — so a client that asks for one transition
//! again and again gets it from the ask that finds the alarm back in its actual state, which is
//! the sequence every row below drives. Sources: the `SetAlarmState` documentation ("when the
//! updated state differs from the previous value, the action configured for the appropriate
//! state is invoked", and "metric alarms returns to their actual state quickly, often within
//! seconds"), and `PutTargets` for the grant a queue target needs ("for Lambda functions,
//! Amazon SNS topics and Amazon SQS queues, EventBridge relies on resource-based policies").
//!
//! What this suite adds to the crate-local suites that pin the same rule
//! (`services/service-com-amazonaws-cloudwatch-graniteserviceversion20100801/tests/all/alarm_state_change_events.rs`
//! and its sibling `alarm_state_change_reaches_its_rule_target.rs`, which mount CloudWatch,
//! EventBridge and Amazon SQS alone) is the axis neither can reach: the publication CloudWatch
//! makes THROUGH THE EMULATOR'S OWN CALLBACK has to be claimed, routed and delivered by
//! whichever mounted model a client's `events`- and `sqs`-scoped calls reach when EVERY service
//! is mounted — three models resolve to the `events` key — and the alarm here watches a series
//! another service owns (an Application Load Balancer's `UnHealthyHostCount` under the
//! `TargetGroup` + `LoadBalancer` dimension pair), created through ELBv2 and EC2 themselves.
//!
//! The rows differ along what the rule generalises over: which dimension of the envelope the
//! matching rule filters on (the source and detail type alone, the alarm's name inside
//! `detail`, the alarm's ARN in `resources`).
//!
//! Assembling the whole router costs stack (see `boot_serve.rs`), so it is built — and the
//! requests driven — on an explicitly sized thread.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode};

/// Keep this at or above the size `boot_serve.rs` uses: the same assembly runs here.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The Region every call below is signed for.
const REGION: &str = "us-west-2";

/// The `source` and `detail-type` an alarm state change is published under, spelled out here
/// rather than imported, so a wrong constant in the implementation cannot make this suite pass.
const SOURCE: &str = "aws.cloudwatch";
const DETAIL_TYPE: &str = "CloudWatch Alarm State Change";

/// The service principal EventBridge delivers to an SQS target as.
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";

/// The namespace and metric an Application Load Balancer's target health is counted in.
const NAMESPACE: &str = "AWS/ApplicationELB";
const METRIC: &str = "UnHealthyHostCount";

/// The API versions the two form-encoded services take.
const EC2_API_VERSION: &str = "2016-11-15";
const ELB_API_VERSION: &str = "2015-12-01";

/// CloudWatch's `X-Amz-Target` prefix and signing name.
const CLOUDWATCH_TARGET_PREFIX: &str = "GraniteServiceVersion20100801";
const CLOUDWATCH_SIGNING_NAME: &str = "monitoring";

/// How long a row waits for a publication and its delivery before giving up. Generous: what is
/// asserted is that the envelope arrives, never how fast the host runs.
const DELIVERY_WAIT: Duration = Duration::from_secs(60);

/// How often it looks meanwhile.
const LOOK_INTERVAL: Duration = Duration::from_millis(100);

/// The network the load balancer is placed in.
const VPC_CIDR: &str = "10.80.0.0/16";
const SUBNETS: [(&str, &str); 2] = [
    ("10.80.1.0/24", "us-west-2a"),
    ("10.80.2.0/24", "us-west-2b"),
];

/// The whole emulator over one engine, exactly as `core/router/src/lib.rs` assembles it for the
/// binary.
fn router() -> MultiServiceRouter {
    let engine = aws_emulator_router::new_engine();
    let mut router = aws_emulator_router::build_router(Arc::clone(&engine));
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request. The signature is never checked: the router above runs with request authentication
/// off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
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

/// Send one form-encoded request to the service `signing_name` addresses, requiring a 200.
async fn query(
    router: &MultiServiceRouter,
    case: &str,
    signing_name: &str,
    action: &str,
    version: &str,
    params: &[(&str, &str)],
) -> String {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{signing_name}.{REGION}.amazonaws.com/"))
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("authorization", authorization(signing_name))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Bytes::from(form(action, version, params)))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let body = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {signing_name} {action}, got {status} with {body}"
    );
    body
}

/// One `X-Amz-Target` RPC call, requiring a 200.
async fn rpc(
    router: &MultiServiceRouter,
    case: &str,
    signing_name: &str,
    target_prefix: &str,
    json_version: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("authorization", authorization(signing_name))
        .header(
            "content-type",
            format!("application/x-amz-json-{json_version}"),
        )
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
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
        "case {case:?}: expected HTTP 200 from {signing_name} {operation}, got {status} with \
         {parsed}"
    );
    parsed
}

/// One CloudWatch call (`awsJson1_0`).
async fn monitoring(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    rpc(
        router,
        case,
        CLOUDWATCH_SIGNING_NAME,
        CLOUDWATCH_TARGET_PREFIX,
        "1.0",
        operation,
        body,
    )
    .await
}

/// One EventBridge call (`awsJson1_1`).
async fn events(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    rpc(router, case, "events", "AWSEvents", "1.1", operation, body).await
}

/// One Amazon SQS call (`awsJson1_0`).
async fn sqs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    rpc(router, case, "sqs", "AmazonSQS", "1.0", operation, body).await
}

/// The load balancer and target group an alarm's dimension pair names.
struct Balanced {
    /// The load balancer's own ARN, as ELBv2 reported it.
    load_balancer_arn: String,
    /// The target group's own ARN, as ELBv2 reported it.
    target_group_arn: String,
    /// The `LoadBalancer` dimension value: the load balancer ARN's own
    /// `app/<name>/<id>` suffix.
    load_balancer: String,
    /// The `TargetGroup` dimension value: the target group ARN's own
    /// `targetgroup/<name>/<id>` suffix.
    target_group: String,
}

/// Create a VPC, two subnets, an internal Application Load Balancer and a target group through
/// EC2 and ELBv2 themselves, and answer the dimension pair an alarm over its target health
/// carries.
async fn given_balanced(router: &MultiServiceRouter, case: &str, suffix: &str) -> Balanced {
    let vpc = element(
        &query(
            router,
            case,
            "ec2",
            "CreateVpc",
            EC2_API_VERSION,
            &[("CidrBlock", VPC_CIDR)],
        )
        .await,
        "vpcId",
    );
    let mut subnets: Vec<String> = Vec::new();
    for (cidr, zone) in SUBNETS {
        let created = query(
            router,
            case,
            "ec2",
            "CreateSubnet",
            EC2_API_VERSION,
            &[
                ("VpcId", vpc.as_str()),
                ("CidrBlock", cidr),
                ("AvailabilityZone", zone),
            ],
        )
        .await;
        subnets.push(element(&created, "subnetId"));
    }
    let created = query(
        router,
        case,
        "elasticloadbalancingv2",
        "CreateLoadBalancer",
        ELB_API_VERSION,
        &[
            ("Name", &format!("lb-{suffix}")),
            ("Scheme", "internal"),
            ("Type", "application"),
            ("Subnets.member.1", subnets[0].as_str()),
            ("Subnets.member.2", subnets[1].as_str()),
        ],
    )
    .await;
    let load_balancer_arn = element(&created, "LoadBalancerArn");
    let group = query(
        router,
        case,
        "elasticloadbalancingv2",
        "CreateTargetGroup",
        ELB_API_VERSION,
        &[
            ("Name", &format!("tg-{suffix}")),
            ("Protocol", "HTTP"),
            ("Port", "80"),
            ("TargetType", "ip"),
            ("VpcId", vpc.as_str()),
        ],
    )
    .await;
    let target_group_arn = element(&group, "TargetGroupArn");
    // The two dimension values are the ARNs' own resource suffixes, which is how a load
    // balancer's own metrics are dimensioned.
    let load_balancer = load_balancer_arn
        .split_once(":loadbalancer/")
        .map(|(_, suffix)| suffix.to_string())
        .unwrap_or_else(|| panic!("case {case:?}: {load_balancer_arn} names no load balancer"));
    let target_group = target_group_arn
        .rsplit_once(':')
        .map(|(_, suffix)| suffix.to_string())
        .unwrap_or_else(|| panic!("case {case:?}: {target_group_arn} names no target group"));
    Balanced {
        load_balancer_arn,
        target_group_arn,
        load_balancer,
        target_group,
    }
}

/// A queue a rule can target: its url and ARN, with its own policy allowing EventBridge the
/// send.
async fn given_target_queue(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
) -> (String, String) {
    let created = sqs(router, case, "CreateQueue", &json!({"QueueName": name})).await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a QueueUrl in {created}"))
        .to_string();
    let attributes = sqs(
        router,
        case,
        "GetQueueAttributes",
        &json!({"QueueUrl": &url, "AttributeNames": ["QueueArn"]}),
    )
    .await;
    let arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a QueueArn in {attributes}"))
        .to_string();
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "AllowEventBridgeDelivery",
            "Effect": "Allow",
            "Principal": {"Service": EVENTS_PRINCIPAL},
            "Action": "sqs:SendMessage",
            "Resource": &arn,
        }],
    })
    .to_string();
    sqs(
        router,
        case,
        "SetQueueAttributes",
        &json!({"QueueUrl": &url, "Attributes": {"Policy": policy}}),
    )
    .await;
    (url, arn)
}

/// An alarm over the target health of `balanced`, and its ARN as CloudWatch reports it.
async fn given_alarm(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    balanced: &Balanced,
) -> String {
    monitoring(
        router,
        case,
        "PutMetricAlarm",
        &json!({
            "AlarmName": name,
            "Namespace": NAMESPACE,
            "MetricName": METRIC,
            "Dimensions": [
                {"Name": "TargetGroup", "Value": &balanced.target_group},
                {"Name": "LoadBalancer", "Value": &balanced.load_balancer},
            ],
            "Statistic": "Maximum",
            "Period": 60,
            "EvaluationPeriods": 2,
            "Threshold": 0.0,
            "ComparisonOperator": "GreaterThanThreshold",
        }),
    )
    .await;
    let described = monitoring(
        router,
        case,
        "DescribeAlarms",
        &json!({"AlarmNames": [name]}),
    )
    .await;
    described["MetricAlarms"][0]["AlarmArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected the alarm's ARN in {described}"))
        .to_string()
}

/// Force the alarm `name` into `state`.
async fn force_state(router: &MultiServiceRouter, case: &str, name: &str, state: &str) {
    monitoring(
        router,
        case,
        "SetAlarmState",
        &json!({
            "AlarmName": name,
            "StateValue": state,
            "StateReason": "forced by this suite",
        }),
    )
    .await;
}

/// Everything waiting on the queue at `url`, as the parsed envelopes their bodies carry.
async fn drain(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<Value> {
    let received = sqs(
        router,
        case,
        "ReceiveMessage",
        &json!({"QueueUrl": url, "MaxNumberOfMessages": 10}),
    )
    .await;
    received["Messages"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|message| {
            serde_json::from_str::<Value>(message["Body"].as_str().unwrap_or_default()).ok()
        })
        .collect()
}

/// An address of the load balancer's own VPC that nothing in this emulator answers on, which
/// is a registered target whose health checks cannot succeed.
const DEAD_TARGET: &str = "10.80.1.200";

/// RULE: the health a load balancer reports for a registered target leaves `initial` with no
/// further call, and a target nothing answers on is reported `unhealthy` — and the
/// `UnHealthyHostCount` series of the group's own dimension pair counts it, so an alarm over
/// that series reaches `ALARM` from those datapoints alone.
#[test]
fn a_registered_target_the_balancer_cannot_reach_becomes_unhealthy_and_counts() {
    std::thread::Builder::new()
        .name("elb-target-health-change".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .expect("build a current-thread runtime");
            runtime.block_on(drive_target_health());
        })
        .expect("spawn the sized thread")
        .join()
        .expect("the suite thread runs to completion");
}

/// The target-health half, against one assembled router.
async fn drive_target_health() {
    let case = "a registered target nothing answers on";
    let router = router();
    let balanced = given_balanced(&router, case, "health").await;
    // The listener is what makes the group one the load balancer routes to, which is what it
    // checks and counts.
    query(
        &router,
        case,
        "elasticloadbalancingv2",
        "CreateListener",
        ELB_API_VERSION,
        &[
            ("LoadBalancerArn", &balanced.load_balancer_arn),
            ("Protocol", "HTTP"),
            ("Port", "80"),
            ("DefaultActions.member.1.Type", "forward"),
            (
                "DefaultActions.member.1.TargetGroupArn",
                &balanced.target_group_arn,
            ),
        ],
    )
    .await;
    query(
        &router,
        case,
        "elasticloadbalancingv2",
        "RegisterTargets",
        ELB_API_VERSION,
        &[
            ("TargetGroupArn", &balanced.target_group_arn),
            ("Targets.member.1.Id", DEAD_TARGET),
            ("Targets.member.1.Port", "80"),
        ],
    )
    .await;

    // The health the balancer reports for that registration: it leaves `initial` with no
    // further call, and what it settles on is `unhealthy`.
    let started = Instant::now();
    let mut state = String::new();
    while started.elapsed() < DELIVERY_WAIT {
        let described = query(
            &router,
            case,
            "elasticloadbalancingv2",
            "DescribeTargetHealth",
            ELB_API_VERSION,
            &[("TargetGroupArn", &balanced.target_group_arn)],
        )
        .await;
        state = element(&described, "State");
        if state != "initial" {
            break;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
    assert_eq!(
        state, "unhealthy",
        "case {case:?}: expected the registered target nothing answers on to be reported \
         unhealthy, got {state:?}"
    );

    // The series that health is counted into, under the group's own dimension pair.
    let started = Instant::now();
    let mut maximum = Value::Null;
    while started.elapsed() < DELIVERY_WAIT {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("the clock is after the Unix epoch")
            .as_secs_f64();
        let statistics = monitoring(
            &router,
            case,
            "GetMetricStatistics",
            &json!({
                "Namespace": NAMESPACE,
                "MetricName": METRIC,
                "Dimensions": [
                    {"Name": "TargetGroup", "Value": &balanced.target_group},
                    {"Name": "LoadBalancer", "Value": &balanced.load_balancer},
                ],
                "StartTime": now - 900.0,
                "EndTime": now + 60.0,
                "Period": 60,
                "Statistics": ["Maximum"],
            }),
        )
        .await;
        maximum = statistics["Datapoints"]
            .as_array()
            .and_then(|points| {
                points
                    .iter()
                    .filter_map(|point| point.get("Maximum").cloned())
                    .next_back()
            })
            .unwrap_or(Value::Null);
        if maximum.as_f64().is_some_and(|count| count >= 1.0) {
            break;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
    assert!(
        maximum.as_f64().is_some_and(|count| count >= 1.0),
        "case {case:?}: expected the UnHealthyHostCount series of the group's dimension pair \
         to count the unreachable target, got {maximum}"
    );

    // And the alarm over that series reaches ALARM from those datapoints alone — nothing
    // forces it.
    let alarm_name = "tg-alarm-health";
    given_alarm(&router, case, alarm_name, &balanced).await;
    let started = Instant::now();
    let mut reported = String::new();
    while started.elapsed() < DELIVERY_WAIT {
        let described = monitoring(
            &router,
            case,
            "DescribeAlarms",
            &json!({"AlarmNames": [alarm_name]}),
        )
        .await;
        reported = described["MetricAlarms"][0]["StateValue"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if reported == "ALARM" {
            break;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
    assert_eq!(
        reported, "ALARM",
        "case {case:?}: expected the alarm over the counted series to reach ALARM from its \
         own datapoints, got {reported:?}"
    );

    // And the transition a caller then asks for on that DATA-DRIVEN alarm — the alarm whose own
    // series keeps being published into — publishes its event like any other, however many
    // times the caller has asked for it: the ask that finds the alarm back in its actual state
    // moves it out of that state again.
    let (url, queue_arn) = given_target_queue(&router, case, "q-health").await;
    events(
        &router,
        case,
        "PutRule",
        &json!({
            "Name": "r-health",
            "EventBusName": "default",
            "EventPattern": json!({
                "source": [SOURCE],
                "detail-type": [DETAIL_TYPE],
                "detail": {"alarmName": [alarm_name]},
            })
            .to_string(),
            "State": "ENABLED",
        }),
    )
    .await;
    let wired = events(
        &router,
        case,
        "PutTargets",
        &json!({
            "Rule": "r-health",
            "EventBusName": "default",
            "Targets": [{"Id": "q", "Arn": &queue_arn}],
        }),
    )
    .await;
    assert_eq!(
        wired["FailedEntryCount"],
        json!(0),
        "case {case:?}: expected the queue target to be accepted, got {wired}"
    );
    let started = Instant::now();
    let mut held: Vec<Value> = Vec::new();
    while started.elapsed() < DELIVERY_WAIT
        && !held
            .iter()
            .any(|event| event.pointer("/detail/state/value") == Some(&json!("OK")))
    {
        force_state(&router, case, alarm_name, "OK").await;
        held.extend(drain(&router, case, &url).await);
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
    assert!(
        held.iter()
            .any(|event| event.pointer("/detail/state/value") == Some(&json!("OK"))),
        "case {case:?}: expected an envelope reporting the OK transition of the data-driven \
         alarm {alarm_name:?} on {url}, got {held:?}"
    );
}

/// What one row's matching rule filters on.
struct Row {
    case: &'static str,
    /// The rule's pattern, over the alarm's name and its ARN.
    pattern: fn(&str, &str) -> Value,
}

/// RULE: an alarm transition reaches the SQS target of a rule matching it, and reaches nothing
/// until that rule exists — on the whole assembled emulator.
#[test]
fn an_alarm_transition_reaches_its_rule_s_queue_on_the_assembled_emulator() {
    std::thread::Builder::new()
        .name("elb-alarm-transition-delivery".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .expect("build a current-thread runtime");
            runtime.block_on(drive());
        })
        .expect("spawn the sized thread")
        .join()
        .expect("the suite thread runs to completion");
}

/// Every row, against ONE assembled router per row.
async fn drive() {
    let rows = [
        Row {
            case: "the source and detail type alone",
            pattern: |_, _| json!({"source": [SOURCE], "detail-type": [DETAIL_TYPE]}),
        },
        Row {
            case: "the alarm's name inside the detail",
            pattern: |name, _| json!({
                "source": [SOURCE],
                "detail-type": [DETAIL_TYPE],
                "detail": {"alarmName": [name]},
            }),
        },
        Row {
            case: "the alarm's ARN in the event's resources",
            pattern: |_, arn| json!({
                "source": [SOURCE],
                "detail-type": [DETAIL_TYPE],
                "resources": [arn],
            }),
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let router = router();
        let balanced = given_balanced(&router, case, &format!("{index}")).await;
        let alarm_name = format!("tg-alarm-{index}");
        let (url, queue_arn) =
            given_target_queue(&router, case, &format!("q-{index}")).await;
        let alarm_arn = given_alarm(&router, case, &alarm_name, &balanced).await;

        // The negative half: the transitions a caller forces before any rule exists are
        // delivered nowhere.
        force_state(&router, case, &alarm_name, "ALARM").await;
        force_state(&router, case, &alarm_name, "OK").await;
        let withheld = drain(&router, case, &url).await;
        assert!(
            withheld.is_empty(),
            "case {case:?}: with no rule on the bus the queue must receive nothing, got \
             {withheld:?}"
        );

        // Then the rule, and the transitions again.
        events(
            &router,
            case,
            "PutRule",
            &json!({
                "Name": format!("r-{index}"),
                "EventBusName": "default",
                "EventPattern": (row.pattern)(&alarm_name, &alarm_arn).to_string(),
                "State": "ENABLED",
            }),
        )
        .await;
        let wired = events(
            &router,
            case,
            "PutTargets",
            &json!({
                "Rule": format!("r-{index}"),
                "EventBusName": "default",
                "Targets": [{"Id": "q", "Arn": &queue_arn}],
            }),
        )
        .await;
        assert_eq!(
            wired["FailedEntryCount"],
            json!(0),
            "case {case:?}: expected the queue target to be accepted, got {wired}"
        );

        // The transition is re-asked for while the queue is watched, which is what a client
        // does when it wires a rule to an alarm it has already driven: it asks for the SAME
        // state again, as often as it takes, and nothing asks for the other state in between.
        // Look again until the envelope is there or the deadline passes.
        let started = Instant::now();
        let mut held: Vec<Value> = Vec::new();
        while started.elapsed() < DELIVERY_WAIT
            && !held
                .iter()
                .any(|event| event.pointer("/detail/state/value") == Some(&json!("OK")))
        {
            force_state(&router, case, &alarm_name, "OK").await;
            held.extend(drain(&router, case, &url).await);
            tokio::time::sleep(LOOK_INTERVAL).await;
        }
        let delivered: Vec<&Value> = held
            .iter()
            .filter(|event| event.pointer("/detail/state/value") == Some(&json!("OK")))
            .collect();
        assert!(
            !delivered.is_empty(),
            "case {case:?}: expected an envelope reporting the OK transition of \
             {alarm_name:?} on {url}, got {held:?}"
        );
        for envelope in delivered {
            for (pointer, expected) in [
                ("/source", json!(SOURCE)),
                ("/detail-type", json!(DETAIL_TYPE)),
                ("/region", json!(REGION)),
                ("/resources", json!([&alarm_arn])),
                ("/detail/alarmName", json!(&alarm_name)),
            ] {
                let actual = envelope.pointer(pointer).cloned().unwrap_or(Value::Null);
                assert_eq!(
                    actual, expected,
                    "case {case:?}: expected {pointer} to be {expected}, got {actual} in \
                     {envelope}"
                );
            }
        }
    }
}
