// SPDX-License-Identifier: CC0-1.0

//! WHEN a load balancer's host-count series begins, and what an unhealthy-host alarm created
//! over it before that reads.
//!
//! RULE: a host count is a number of targets the load balancer CONSIDERS healthy or unhealthy,
//! so it publishes a datapoint for a target group only once its health checks have determined
//! the health of the registrations it holds: while every registration of the group is still
//! `initial` — the state the model words as the load balancer sending "the minimum number of
//! health checks required to determine its health status" — the group has no count to report
//! and the series holds NO datapoint, rather than a zero asserting that the checks found no
//! unhealthy target among registrations they have found nothing about yet. A group holding no
//! registration at all has nothing undetermined and reports the counts it has.
//!
//! What a client sees follows from that: an alarm on `UnHealthyHostCount` over a group whose
//! registrations nothing answers for — `Statistic` `Maximum`, `Period` 60,
//! `EvaluationPeriods` 2, `Threshold` 0, `GreaterThanThreshold`, `TreatMissingData` breaching —
//! leaves `INSUFFICIENT_DATA` for `ALARM` and is never reported `OK` on the way there, because
//! no datapoint the checks did not determine is ever graded; the same alarm over a group whose
//! registrations the emulator does answer for, and over one holding no registration at all,
//! reaches `OK` instead, off the zeros those groups do have to report. (It may pass through
//! `ALARM` first: under `breaching` a window holding no datapoint counts as a breach, which is
//! CloudWatch's own documented treatment and not what this suite is about.)
//!
//! Three crates have to agree for that (ELBv2 checks and counts, EC2 holds the inventory a
//! check reads, CloudWatch stores the series and grades the alarm), so the suite lives here.
//!
//! The rows differ along what the rule generalises over: how many registrations the group
//! holds (none, one, two, three), whether the emulator answers for them, and the cadence and
//! threshold the verdict is reached on (`HealthCheckIntervalSeconds` 5 and 6,
//! `UnhealthyThresholdCount` 2 and 3). The recorded configuration an issue happens to mention
//! is a single row. One row is driven per test, each over an estate of its own, so no row waits
//! behind another.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode, opt_element};

/// The region every request here is signed for.
const REGION: &str = "us-east-1";

/// The API version an ec2Query request carries.
const EC2_API_VERSION: &str = "2016-11-15";

/// The API version an awsQuery request naming the ELBv2 model carries.
const ELB_VERSION: &str = "2015-12-01";

/// CloudWatch's `X-Amz-Target` prefix and signing name (`sigv4Name: monitoring`).
const CLOUDWATCH_TARGET_PREFIX: &str = "GraniteServiceVersion20100801";
const CLOUDWATCH_SIGNING_NAME: &str = "monitoring";

/// The namespace an Application Load Balancer's metrics live in, and the two host counts it
/// publishes per target group.
const APPLICATION_NAMESPACE: &str = "AWS/ApplicationELB";
const HEALTHY_HOST_COUNT: &str = "HealthyHostCount";
const UNHEALTHY_HOST_COUNT: &str = "UnHealthyHostCount";

/// The period the alarm below grades its series over, which is also the period one datapoint
/// of that series occupies: these counts are published per minute.
const PERIOD: i64 = 60;

/// The three alarm states a `StateValue` may report.
const INSUFFICIENT_DATA: &str = "INSUFFICIENT_DATA";
const ALARM: &str = "ALARM";
const OK: &str = "OK";

/// The `TargetHealth$State` of a registration whose health the load balancer has not
/// determined yet.
const INITIAL: &str = "initial";

/// How long a row waits for the state its configuration implies, and how often it looks.
///
/// Each wait gets this budget of its OWN — never one shared between two waits — and it is
/// several times the longest span a row can need: a verdict is at most
/// `UnhealthyThresholdCount` checks one `HealthCheckIntervalSeconds` apart (18 s for the
/// slowest row below) plus the publish the verdict makes due. So a correct run is far inside
/// it and a broken one is reported promptly rather than sat out. Nothing here asserts on
/// elapsed time.
const DEADLINE: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_secs(1);

/// The network every row runs in.
const VPC_CIDR: &str = "10.90.0.0/16";
const SUBNET_CIDRS: [(&str, &str); 2] = [
    ("10.90.1.0/24", "us-east-1a"),
    ("10.90.2.0/24", "us-east-1b"),
];

/// Addresses of the first subnet that nothing in the emulator answers on: high in the range,
/// so they are not addresses the inventory hands an interface of its own.
const DEAD_ADDRESSES: [&str; 3] = ["10.90.1.200", "10.90.1.201", "10.90.1.202"];

/// The port every target group below receives traffic on, which is also the port its health
/// check connects to.
const TARGET_PORT: &str = "80";

/// The emulator with the three services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_elasticloadbalancingv2_elasticloadbalancing_v10::EmulatedService::new(
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

/// Epoch seconds, the unit a CloudWatch timestamp is read and written in.
fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the clock is after the Unix epoch")
        .as_secs_f64()
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

/// Send one form-encoded request to the service `signing_name` addresses, and answer its status
/// and body.
async fn send(router: &MultiServiceRouter, signing_name: &str, body: String) -> (u16, String) {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{signing_name}.{REGION}.amazonaws.com/"))
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// Drive one ec2Query action that must succeed, and answer its body.
async fn ec2(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> String {
    let (status, body) = send(router, "ec2", form(action, EC2_API_VERSION, params)).await;
    assert_eq!(
        status, 200,
        "setup: EC2 {action} must succeed, got {status} with body {body}"
    );
    body
}

/// Drive one awsQuery action against the ELBv2 model that must succeed, and answer its body.
async fn elbv2(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> String {
    let (status, body) = send(
        router,
        "elasticloadbalancingv2",
        form(action, ELB_VERSION, params),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: ELBv2 {action} must succeed, got {status} with body {body}"
    );
    body
}

/// Call one CloudWatch operation over its awsJson wire, requiring it to succeed.
async fn cloudwatch(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    request: &Value,
) -> Value {
    let http_request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header(
            "host",
            format!("{CLOUDWATCH_SIGNING_NAME}.{REGION}.amazonaws.com"),
        )
        .header("content-type", "application/x-amz-json-1.0")
        .header(
            "x-amz-target",
            format!("{CLOUDWATCH_TARGET_PREFIX}.{operation}"),
        )
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 \
                 Credential=AKIDEXAMPLE/20260101/{REGION}/{CLOUDWATCH_SIGNING_NAME}/aws4_request, \
                 SignedHeaders=host;x-amz-target, Signature=0123456789abcdef"
            ),
        )
        .body(Bytes::from(
            serde_json::to_vec(request).expect("the request serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(http_request).await;
    let status = response.status().as_u16();
    let parsed: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CloudWatch {operation}, got {status} with body \
         {parsed}"
    );
    parsed
}

/// The network every row runs in: one VPC, two subnets in different zones, and a security group
/// of that VPC.
struct Network {
    vpc: String,
    subnets: Vec<String>,
    security_group: String,
}

/// Create that network through EC2 itself, so every value a check reads is one EC2 reported
/// rather than one this suite made up.
async fn seed_network(router: &MultiServiceRouter) -> Network {
    let vpc = element(
        &ec2(router, "CreateVpc", &[("CidrBlock", VPC_CIDR)]).await,
        "vpcId",
    );
    let mut subnets = Vec::new();
    for (cidr, zone) in SUBNET_CIDRS {
        let created = ec2(
            router,
            "CreateSubnet",
            &[
                ("VpcId", vpc.as_str()),
                ("CidrBlock", cidr),
                ("AvailabilityZone", zone),
            ],
        )
        .await;
        subnets.push(element(&created, "subnetId"));
    }
    let security_group = element(
        &ec2(
            router,
            "CreateSecurityGroup",
            &[
                ("GroupName", "elbv2-unhealthy-host-alarm"),
                ("GroupDescription", "the load balancers own VPC"),
                ("VpcId", vpc.as_str()),
            ],
        )
        .await,
        "groupId",
    );
    Network {
        vpc,
        subnets,
        security_group,
    }
}

/// Create an `ip` target group checked at `interval_seconds` with `unhealthy_threshold`
/// consecutive failures to a verdict, and answer its ARN.
async fn ip_target_group(
    router: &MultiServiceRouter,
    name: &str,
    vpc: &str,
    interval_seconds: i32,
    unhealthy_threshold: i32,
) -> String {
    let interval = interval_seconds.to_string();
    let threshold = unhealthy_threshold.to_string();
    element(
        &elbv2(
            router,
            "CreateTargetGroup",
            &[
                ("Name", name),
                ("Protocol", "HTTP"),
                ("Port", TARGET_PORT),
                ("TargetType", "ip"),
                ("VpcId", vpc),
                ("HealthCheckIntervalSeconds", interval.as_str()),
                ("UnhealthyThresholdCount", threshold.as_str()),
            ],
        )
        .await,
        "TargetGroupArn",
    )
}

/// Register every address in `addresses` with the target group `group`.
async fn register(router: &MultiServiceRouter, group: &str, addresses: &[&str]) {
    if addresses.is_empty() {
        return;
    }
    let keys: Vec<String> = (0..addresses.len())
        .map(|index| format!("Targets.member.{}.Id", index + 1))
        .collect();
    let mut params: Vec<(&str, &str)> = vec![("TargetGroupArn", group)];
    for (key, address) in keys.iter().zip(addresses) {
        params.push((key.as_str(), address));
    }
    elbv2(router, "RegisterTargets", &params).await;
}

/// The `app/<name>/<id>` suffix of a load balancer ARN, which is how an Elastic Load Balancing
/// dimension names it.
fn load_balancer_dimension(arn: &str) -> String {
    arn.split_once(":loadbalancer/")
        .map(|(_, suffix)| suffix.to_string())
        .unwrap_or_else(|| panic!("a load balancer ARN carries its family and name: {arn}"))
}

/// The `targetgroup/<name>/<id>` suffix of a target group ARN, which is how that dimension
/// names it.
fn target_group_dimension(arn: &str) -> String {
    arn.split_once(":targetgroup/")
        .map(|(_, suffix)| format!("targetgroup/{suffix}"))
        .unwrap_or_else(|| panic!("a target group ARN carries its resource type: {arn}"))
}

/// Every datapoint CloudWatch reports for one host-count series, as the `Maximum` of each
/// one-minute window, oldest first.
async fn host_counts(
    router: &MultiServiceRouter,
    case: &str,
    metric: &str,
    target_group: &str,
    load_balancer: &str,
) -> Vec<f64> {
    let now = now_secs();
    let reported = cloudwatch(
        router,
        case,
        "GetMetricStatistics",
        &json!({
            "Namespace": APPLICATION_NAMESPACE,
            "MetricName": metric,
            "Dimensions": [
                {"Name": "TargetGroup", "Value": target_group_dimension(target_group)},
                {"Name": "LoadBalancer", "Value": load_balancer_dimension(load_balancer)},
            ],
            "StartTime": now - 1_800.0,
            "EndTime": now + 60.0,
            "Period": PERIOD,
            "Statistics": ["Maximum"],
        }),
    )
    .await;
    let mut points: Vec<(f64, f64)> = reported["Datapoints"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|point| {
            let stamp = point["Timestamp"].as_f64().unwrap_or_else(|| {
                panic!("case {case:?}: every datapoint carries a Timestamp, got {reported}")
            });
            let maximum = point["Maximum"].as_f64().unwrap_or_else(|| {
                panic!("case {case:?}: every datapoint carries a Maximum, got {reported}")
            });
            (stamp, maximum)
        })
        .collect();
    points.sort_by(|left, right| left.0.total_cmp(&right.0));
    points.into_iter().map(|(_, maximum)| maximum).collect()
}

/// The `TargetHealth$State` of every registration of `group`.
async fn target_health_states(router: &MultiServiceRouter, group: &str) -> Vec<String> {
    let body = elbv2(
        router,
        "DescribeTargetHealth",
        &[("TargetGroupArn", group)],
    )
    .await;
    let mut states: Vec<String> = Vec::new();
    for health in body.split("<TargetHealth").skip(1) {
        // `<TargetHealthDescriptions>` carries the same prefix, so only a chunk where the name
        // ENDS — at the tag's closing bracket or at an attribute — is one health.
        if !health.starts_with('>') && !health.starts_with(' ') {
            continue;
        }
        states.push(opt_element(health, "State").unwrap_or_default());
    }
    states
}

/// The `StateValue` CloudWatch reports for the alarm `name`.
async fn alarm_state(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let described = cloudwatch(
        router,
        case,
        "DescribeAlarms",
        &json!({"AlarmNames": [name]}),
    )
    .await;
    described["MetricAlarms"][0]["StateValue"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected the alarm's StateValue in {described}"))
        .to_string()
}

/// Create the unhealthy-hosts alarm of one row: the configuration the recorded diagnosis uses,
/// over the group's own dimension pair.
async fn put_alarm(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    target_group: &str,
    load_balancer: &str,
) {
    cloudwatch(
        router,
        case,
        "PutMetricAlarm",
        &json!({
            "AlarmName": name,
            "Namespace": APPLICATION_NAMESPACE,
            "MetricName": UNHEALTHY_HOST_COUNT,
            "Dimensions": [
                {"Name": "TargetGroup", "Value": target_group_dimension(target_group)},
                {"Name": "LoadBalancer", "Value": load_balancer_dimension(load_balancer)},
            ],
            "Statistic": "Maximum",
            "Period": PERIOD,
            "EvaluationPeriods": 2,
            "Threshold": 0.0,
            "ComparisonOperator": "GreaterThanThreshold",
            "TreatMissingData": "breaching",
        }),
    )
    .await;
}

/// One row: a target group to build, what its registrations stand for, the cadence its verdict
/// is reached on, and what the series and the alarm over it must then read.
///
/// One row is driven per test ([`drive`]), each over an estate of its own, so the rows are
/// independent: one row's verdict cannot be waited out behind another's, and a row that fails
/// names itself.
struct Row {
    case: &'static str,
    /// The target group's name, which is also the alarm's.
    name: &'static str,
    /// `HealthCheckIntervalSeconds`, the span between two checks of one registration.
    interval_seconds: i32,
    /// `UnhealthyThresholdCount`, how many consecutive failing checks the verdict takes.
    unhealthy_threshold: i32,
    /// How many of [`DEAD_ADDRESSES`] are registered with it.
    dead: usize,
    /// Whether the address the account's inventory answers on is registered with it too.
    served: bool,
    /// The state the alarm over its `UnHealthyHostCount` series must reach, and the one it
    /// must never be reported in on the way there.
    reaches: &'static str,
    /// The count every datapoint of each of its two series must carry.
    unhealthy: f64,
    healthy: f64,
}

/// The recorded configuration: two addresses nothing answers on, at the threshold of two
/// checks a target group defaults to.
const TWO_DEAD: Row = Row {
    case: "the recorded configuration: two addresses nothing answers on, a threshold of two \
           checks five seconds apart",
    name: "alarm-two-dead",
    interval_seconds: 5,
    unhealthy_threshold: 2,
    dead: 2,
    served: false,
    reaches: ALARM,
    unhealthy: 2.0,
    healthy: 0.0,
};

/// One such address, at a threshold of three checks.
const ONE_DEAD: Row = Row {
    case: "one address nothing answers on, at a threshold of three checks",
    name: "alarm-one-dead",
    interval_seconds: 5,
    unhealthy_threshold: 3,
    dead: 1,
    served: false,
    reaches: ALARM,
    unhealthy: 1.0,
    healthy: 0.0,
};

/// Three such addresses, at a cadence one second longer.
const THREE_DEAD: Row = Row {
    case: "three addresses nothing answers on, at a cadence one second longer",
    name: "alarm-three-dead",
    interval_seconds: 6,
    unhealthy_threshold: 2,
    dead: 3,
    served: false,
    reaches: ALARM,
    unhealthy: 3.0,
    healthy: 0.0,
};

/// An address the account's own inventory answers on, which the load balancer can reach.
const SERVED: Row = Row {
    case: "an address a network interface of the account holds",
    name: "alarm-served",
    interval_seconds: 5,
    unhealthy_threshold: 2,
    dead: 0,
    served: true,
    reaches: OK,
    unhealthy: 0.0,
    healthy: 1.0,
};

/// A routed group holding no registration at all, which has nothing undetermined.
const EMPTY: Row = Row {
    case: "a group holding no registration at all",
    name: "alarm-empty",
    interval_seconds: 5,
    unhealthy_threshold: 2,
    dead: 0,
    served: false,
    reaches: OK,
    unhealthy: 0.0,
    healthy: 0.0,
};

#[tokio::test]
async fn the_series_of_two_addresses_nothing_answers_on_begins_at_their_verdict() {
    drive(&TWO_DEAD).await;
}

#[tokio::test]
async fn the_series_of_one_such_address_begins_at_its_threshold_of_three_checks() {
    drive(&ONE_DEAD).await;
}

#[tokio::test]
async fn the_series_of_three_such_addresses_begins_at_their_verdict() {
    drive(&THREE_DEAD).await;
}

#[tokio::test]
async fn the_series_of_an_address_the_inventory_answers_on_counts_it_healthy() {
    drive(&SERVED).await;
}

#[tokio::test]
async fn the_series_of_a_group_holding_no_registration_reports_its_zeros() {
    drive(&EMPTY).await;
}

/// Build `row`'s estate and assert the rule over it: its series begins at the verdict its
/// checks reached, every datapoint carries the counts its registrations imply, and the alarm
/// created before any of that reaches the state those datapoints imply out of
/// `INSUFFICIENT_DATA` alone.
async fn drive(row: &Row) {
    let router = build_router();
    let network = seed_network(&router).await;

    // One interface of the account, in the load balancer's own subnet and its own security
    // group: its address is one something in this VPC answers on, and that group admits the
    // load balancer's health check on the checked port — which together are what make a target
    // registered at it one the load balancer can reach.
    let interface = ec2(
        &router,
        "CreateNetworkInterface",
        &[
            ("SubnetId", network.subnets[0].as_str()),
            ("SecurityGroupId.1", network.security_group.as_str()),
            ("Description", "a workload of the account"),
        ],
    )
    .await;
    ec2(
        &router,
        "AuthorizeSecurityGroupIngress",
        &[
            ("GroupId", network.security_group.as_str()),
            ("IpPermissions.1.IpProtocol", "tcp"),
            ("IpPermissions.1.FromPort", TARGET_PORT),
            ("IpPermissions.1.ToPort", TARGET_PORT),
            (
                "IpPermissions.1.Groups.1.GroupId",
                network.security_group.as_str(),
            ),
        ],
    )
    .await;
    let served_address = element(&interface, "privateIpAddress");
    assert!(
        !DEAD_ADDRESSES.contains(&served_address.as_str()),
        "setup: the inventory's own address must differ from the ones nothing answers on, got \
         {served_address}"
    );

    let load_balancer = element(
        &elbv2(
            &router,
            "CreateLoadBalancer",
            &[
                ("Name", row.name),
                ("Scheme", "internal"),
                ("Subnets.member.1", network.subnets[0].as_str()),
                ("Subnets.member.2", network.subnets[1].as_str()),
                ("SecurityGroups.member.1", network.security_group.as_str()),
            ],
        )
        .await,
        "LoadBalancerArn",
    );

    // The group is registered, routed and alarmed BEFORE anything is waited for, so the alarm
    // is created while its group's checks have determined nothing — the moment the rule is
    // about.
    let group = ip_target_group(
        &router,
        row.name,
        &network.vpc,
        row.interval_seconds,
        row.unhealthy_threshold,
    )
    .await;
    let mut addresses: Vec<&str> = DEAD_ADDRESSES[..row.dead].to_vec();
    if row.served {
        addresses.push(served_address.as_str());
    }
    register(&router, &group, &addresses).await;
    // The listener forwarding to the group is what makes it one the load balancer checks and
    // counts.
    elbv2(
        &router,
        "CreateListener",
        &[
            ("LoadBalancerArn", load_balancer.as_str()),
            ("Protocol", "HTTP"),
            ("Port", TARGET_PORT),
            ("DefaultActions.member.1.Type", "forward"),
            ("DefaultActions.member.1.TargetGroupArn", group.as_str()),
        ],
    )
    .await;
    put_alarm(&router, row.case, row.name, &group, &load_balancer).await;

    // Every state the alarm was reported in, in the order it was seen, from before its group's
    // checks had determined anything.
    let mut seen: Vec<String> = Vec::new();
    // One budget per WAIT, never one shared between them: a series that took most of its own
    // budget to begin still leaves the alarm the whole of its own, so a run that was merely
    // slow is not read as a failure.
    let series_deadline = Instant::now() + DEADLINE;
    // The series, once it has begun: a group whose checks have determined nothing holds no
    // datapoint, so what this waits for is the verdict those checks reach.
    let (unhealthy_points, healthy_points) = loop {
        // The series is read BEFORE the health, so a reading that finds every registration
        // still undetermined is one the series was read no later than.
        let unhealthy_points =
            host_counts(&router, row.case, UNHEALTHY_HOST_COUNT, &group, &load_balancer).await;
        let healthy_points =
            host_counts(&router, row.case, HEALTHY_HOST_COUNT, &group, &load_balancer).await;
        let states = target_health_states(&router, &group).await;
        if !states.is_empty() && states.iter().all(|state| state == INITIAL) {
            assert!(
                unhealthy_points.is_empty() && healthy_points.is_empty(),
                "case {:?}: a group whose checks have determined nothing publishes no host \
                 count, got {UNHEALTHY_HOST_COUNT} {unhealthy_points:?} and \
                 {HEALTHY_HOST_COUNT} {healthy_points:?} while its registrations are \
                 {states:?}",
                row.case
            );
        }
        let state = alarm_state(&router, row.case, row.name).await;
        if seen.last().map(String::as_str) != Some(state.as_str()) {
            seen.push(state.clone());
        }
        if !unhealthy_points.is_empty() && !healthy_points.is_empty() {
            break (unhealthy_points, healthy_points);
        }
        assert!(
            Instant::now() < series_deadline,
            "case {:?}: expected both host-count series to begin at the verdict the checks \
             reached, got {UNHEALTHY_HOST_COUNT} {unhealthy_points:?} and \
             {HEALTHY_HOST_COUNT} {healthy_points:?} with the registrations {states:?}",
            row.case
        );
        ::tokio::time::sleep(POLL).await;
    };

    // Every datapoint of both series carries the counts the group's registrations imply —
    // there is no datapoint of any other value anywhere in them.
    for (metric, expected, points) in [
        (UNHEALTHY_HOST_COUNT, row.unhealthy, &unhealthy_points),
        (HEALTHY_HOST_COUNT, row.healthy, &healthy_points),
    ] {
        assert!(
            points.iter().all(|count| *count == expected),
            "case {:?}: expected every {metric} datapoint to be {expected}, got {points:?}",
            row.case
        );
    }

    // And the alarm over the unhealthy-host series reaches the state those datapoints imply, on
    // a budget of its own.
    let alarm_deadline = Instant::now() + DEADLINE;
    let reached = loop {
        let state = alarm_state(&router, row.case, row.name).await;
        if seen.last().map(String::as_str) != Some(state.as_str()) {
            seen.push(state.clone());
        }
        if state == row.reaches || Instant::now() >= alarm_deadline {
            break state;
        }
        ::tokio::time::sleep(POLL).await;
    };
    assert_eq!(
        reached, row.reaches,
        "case {:?}: expected the alarm to reach {}, got {reached} (states seen: {seen:?}, \
         {UNHEALTHY_HOST_COUNT} {unhealthy_points:?})",
        row.case, row.reaches
    );

    // A breaching alarm over a group nothing answers for was never reported OK on the way
    // there: it is graded from the datapoints the series holds, and none of them reports a
    // count no check determined.
    if row.reaches == ALARM {
        assert!(
            seen.iter().all(|state| state != OK),
            "case {:?}: expected the alarm to reach {ALARM} out of {INSUFFICIENT_DATA} alone, \
             got the states {seen:?} with {UNHEALTHY_HOST_COUNT} {unhealthy_points:?}",
            row.case
        );
    }
}
