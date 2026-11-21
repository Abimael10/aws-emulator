// SPDX-License-Identifier: CC0-1.0

//! How an alarm over an Application Load Balancer's `UnHealthyHostCount` series is graded under
//! EACH `TreatMissingData` treatment — over a series the load balancer publishes, and over a
//! dimension pair it publishes nothing for.
//!
//! RULE: a metric alarm is graded from the datapoints its series really holds over its
//! evaluation periods, and every period that holds none counts as whatever the alarm's
//! `TreatMissingData` says — "breaching | notBreaching | ignore | missing"
//! (`smithy_query(query="doc",shape="MetricAlarm",member="TreatMissingData")`, whose default is
//! `missing`). So an alarm whose statistic really is above its threshold for its evaluation
//! periods reads `ALARM` under every treatment, while one whose series holds no datapoint at all
//! reads the state its own treatment implies: `ALARM` under `breaching`, `OK` under
//! `notBreaching`, and the `INSUFFICIENT_DATA` it was created in under `missing` (which leaves a
//! hole out of the evaluation) and under `ignore` (which maintains the state the alarm has).
//!
//! The series both halves of that table are graded over is the load balancer's own: an
//! Application Load Balancer publishes `UnHealthyHostCount` and `HealthyHostCount` under the
//! dimension pair `TargetGroup` + `LoadBalancer` for every target group ANY action of its
//! listeners forwards to — a rule's as much as a listener's default action — counting the
//! registrations its health checks have decided, and publishes nothing at all for a pair nothing
//! forwards to. One estate therefore feeds the whole table: a target group a priority
//! path-pattern RULE forwards to, holding two registered addresses nothing in the emulator
//! answers on and checked over HTTPS on a port of its own, and a twin group holding the same two
//! addresses that no action of the listener forwards to.
//!
//! The cases differ along what the rule generalises over: the treatment named (all four the
//! member publishes) and whether the series the alarm grades holds real datapoints or nothing at
//! all. The treatment a recorded diagnosis happens to use (`breaching` over a published series)
//! is a single cell of it.
//!
//! Three crates have to agree for that — ELBv2 checks and counts, EC2 holds the inventory a
//! check reads, CloudWatch stores the series and grades the alarms — so the suite lives here.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{blocks_of, element, elements, form_encode, opt_element};

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

/// The period the alarms below grade their series over, which is also the period one datapoint
/// of that series occupies: these counts are published per minute.
const PERIOD: i64 = 60;

/// The three `StateValue`s a metric alarm reports.
const INSUFFICIENT_DATA: &str = "INSUFFICIENT_DATA";
const ALARM: &str = "ALARM";
const OK: &str = "OK";

/// The `TargetHealth$State` of a registration whose checks have failed their group's threshold,
/// and of one in a target group no load balancer forwards to.
const UNHEALTHY: &str = "unhealthy";
const UNUSED: &str = "unused";

/// `HealthCheckIntervalSeconds` and `UnhealthyThresholdCount` of every target group below: a
/// verdict takes that many consecutive failing checks, one interval apart, so a suite that waits
/// for one asks for the shortest cadence the member admits ("the range is 5-300").
const INTERVAL_SECONDS: &str = "5";
const UNHEALTHY_THRESHOLD: &str = "3";

/// `HealthCheckTimeoutSeconds`, which may last as long as the interval and no longer.
const TIMEOUT_SECONDS: &str = "2";

/// How long a wait gets, and how often it looks. Each wait gets this budget of its OWN — never
/// one shared between two waits — and it is several times the longest span the estate can need (a
/// verdict is three checks five seconds apart, plus the publish that verdict makes due), so a
/// correct run is far inside it and a broken one is reported promptly rather than sat out.
/// Nothing here asserts on elapsed time.
const DEADLINE: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_secs(1);

/// The network the estate runs in: three blocks across two zones, as a client placing an
/// internal load balancer lays them out.
const VPC_CIDR: &str = "10.92.0.0/16";
const SUBNET_CIDRS: [(&str, &str); 3] = [
    ("10.92.0.0/22", "us-east-1a"),
    ("10.92.4.0/22", "us-east-1b"),
    ("10.92.8.0/22", "us-east-1a"),
];

/// Addresses of the first subnet that nothing in the emulator answers on: high in the range, so
/// they are not addresses the inventory hands an interface of its own — which the estate asserts
/// rather than assumes.
const DEAD_ADDRESSES: [&str; 2] = ["10.92.3.200", "10.92.3.201"];

/// The port the target groups below receive traffic on, and the port the listener serves.
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

/// The network the estate runs in, created through EC2 itself so every value a health check
/// reads is one EC2 reported rather than one this suite made up.
struct Network {
    vpc: String,
    subnets: Vec<String>,
    security_group: String,
}

/// Create that network: one VPC, three blocks across two zones, and a security group of it.
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
                ("GroupName", "unhealthy-host-alarm-treatments"),
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

/// Create an `ip` target group whose checks reach a verdict after [`UNHEALTHY_THRESHOLD`]
/// failures [`INTERVAL_SECONDS`] apart, checked over HTTPS on a port of its own, and answer its
/// ARN.
///
/// The probe's own protocol, port, path and matcher are the members
/// `HealthCheckProtocol`/`HealthCheckPort`/`HealthCheckPath`/`Matcher` name: an HTTP target group
/// whose health check speaks HTTPS to a port the targets do not receive traffic on is a
/// configuration a client writes, and the series is counted off the verdicts it reaches.
async fn https_checked_target_group(
    router: &MultiServiceRouter,
    name: &str,
    vpc: &str,
) -> String {
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
                ("HealthCheckProtocol", "HTTPS"),
                ("HealthCheckPort", "443"),
                ("HealthCheckPath", "/"),
                ("Matcher.HttpCode", "200,301,302"),
                ("HealthCheckIntervalSeconds", INTERVAL_SECONDS),
                ("HealthCheckTimeoutSeconds", TIMEOUT_SECONDS),
                ("HealthyThresholdCount", "2"),
                ("UnhealthyThresholdCount", UNHEALTHY_THRESHOLD),
            ],
        )
        .await,
        "TargetGroupArn",
    )
}

/// Register every address in `addresses` with the target group `group`.
///
/// No `Port` is named, so each registration takes the port the target group's own targets receive
/// traffic on ("you can override the port for a target when you register it",
/// `TargetDescription$Port`) — while the check of it connects to the group's `HealthCheckPort`,
/// which is a port of its own.
async fn register(router: &MultiServiceRouter, group: &str, addresses: &[&str]) {
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

/// The `targetgroup/<name>/<id>` suffix of a target group ARN, which is how that dimension names
/// it.
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

/// The `TargetHealth$State` of every registration of `group`, as a client reads them.
///
/// Each health is read out of its OWN `<TargetHealth>` block — scoped through the shared
/// container scan, which matches only where the tag's NAME ends, so `<TargetHealthDescriptions>`
/// is not one of them — rather than off the first `<State>` of the whole answer.
async fn target_health_states(router: &MultiServiceRouter, group: &str) -> Vec<String> {
    let body = elbv2(router, "DescribeTargetHealth", &[("TargetGroupArn", group)]).await;
    blocks_of(&body, "TargetHealth")
        .into_iter()
        .map(|health| opt_element(health, "State").unwrap_or_default())
        .collect()
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

/// Create one unhealthy-hosts alarm over the dimension pair of `target_group` + `load_balancer`,
/// handling a missing datapoint as `treat_missing` says.
async fn put_alarm(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    target_group: &str,
    load_balancer: &str,
    treat_missing: &str,
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
            "TreatMissingData": treat_missing,
        }),
    )
    .await;
}

/// Look at the alarm `name` until it reports `wanted` or the budget runs out, and answer every
/// state it was reported in, in the order it was seen.
async fn states_until(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    wanted: &str,
) -> Vec<String> {
    let deadline = Instant::now() + DEADLINE;
    let mut seen: Vec<String> = Vec::new();
    loop {
        let state = alarm_state(router, case, name).await;
        if seen.last().map(String::as_str) != Some(state.as_str()) {
            seen.push(state.clone());
        }
        if state == wanted || Instant::now() >= deadline {
            return seen;
        }
        ::tokio::time::sleep(POLL).await;
    }
}

/// One cell of the treatment table: the `TreatMissingData` value, and the state the alarm over
/// each of the two series must read under it.
///
/// The four values are the whole catalog the member publishes ("Valid Values: breaching |
/// notBreaching | ignore | missing"); the recorded diagnosis an issue happens to mention is the
/// `breaching` row's published half alone.
struct Treatment {
    /// The treatment the alarm names, and the case this cell is reported under.
    treat_missing: &'static str,
    /// The state the alarm over the PUBLISHED series must reach: the group's two registrations
    /// are both unhealthy, so its `Maximum` really is above the threshold for every period.
    over_published: &'static str,
    /// The state the alarm over the series nothing publishes must read.
    over_silent: &'static str,
}

const TREATMENTS: [Treatment; 4] = [
    Treatment {
        treat_missing: "breaching",
        over_published: ALARM,
        // "breaching — missing data points are treated as breaching the threshold".
        over_silent: ALARM,
    },
    Treatment {
        treat_missing: "notBreaching",
        over_published: ALARM,
        // "notBreaching — missing data points are treated as being within the threshold".
        over_silent: OK,
    },
    Treatment {
        treat_missing: "missing",
        over_published: ALARM,
        // The default: a hole is left out of the evaluation, which leaves an evaluation over
        // nothing but holes short of the datapoints either direction needs.
        over_silent: INSUFFICIENT_DATA,
    },
    Treatment {
        treat_missing: "ignore",
        over_published: ALARM,
        // "ignore — the current alarm state is maintained", which is the state a created alarm
        // starts in.
        over_silent: INSUFFICIENT_DATA,
    },
];

/// The alarm name of one cell, which names the treatment and the series it grades.
fn alarm_name(treat_missing: &str, subject: &str) -> String {
    format!("unhealthy-hosts-{subject}-{treat_missing}")
}

#[tokio::test]
async fn each_missing_data_treatment_grades_the_unhealthy_host_alarm_its_own_way() {
    let router = build_router();
    let network = seed_network(&router).await;

    let load_balancer = element(
        &elbv2(
            &router,
            "CreateLoadBalancer",
            &[
                ("Name", "treatments-alb"),
                ("Scheme", "internal"),
                ("Subnets.member.1", network.subnets[0].as_str()),
                ("Subnets.member.2", network.subnets[1].as_str()),
                ("SecurityGroups.member.1", network.security_group.as_str()),
            ],
        )
        .await,
        "LoadBalancerArn",
    );

    // The premise of the whole table: nothing in the account holds the addresses the groups
    // below register, not even the load balancer's own interfaces, so every check of them is
    // answered by nothing.
    let held = elements(
        &ec2(
            &router,
            "DescribeNetworkInterfaces",
            &[("Filter.1.Name", "vpc-id"), ("Filter.1.Value.1", &network.vpc)],
        )
        .await,
        "privateIpAddress",
    );
    assert!(
        !held.iter().any(|address| DEAD_ADDRESSES.contains(&address.as_str())),
        "setup: no interface of the VPC may hold one of {DEAD_ADDRESSES:?}, got {held:?}"
    );

    // The group the listener's default action forwards to, so the listener routes somewhere
    // other than the group under test.
    let front = https_checked_target_group(&router, "treatments-front", &network.vpc).await;
    register(&router, &front, &DEAD_ADDRESSES[..1]).await;
    // The group a priority path-pattern RULE forwards to: the series under test.
    let published = https_checked_target_group(&router, "treatments-rule", &network.vpc).await;
    register(&router, &published, &DEAD_ADDRESSES).await;
    // Its twin, holding the same two addresses, that no action of the listener forwards to:
    // nothing checks it and nothing publishes its series.
    let silent = https_checked_target_group(&router, "treatments-twin", &network.vpc).await;
    register(&router, &silent, &DEAD_ADDRESSES).await;

    let listener = element(
        &elbv2(
            &router,
            "CreateListener",
            &[
                ("LoadBalancerArn", load_balancer.as_str()),
                ("Protocol", "HTTP"),
                ("Port", TARGET_PORT),
                ("DefaultActions.member.1.Type", "forward"),
                ("DefaultActions.member.1.TargetGroupArn", front.as_str()),
            ],
        )
        .await,
        "ListenerArn",
    );
    elbv2(
        &router,
        "CreateRule",
        &[
            ("ListenerArn", listener.as_str()),
            ("Priority", "10"),
            ("Conditions.member.1.Field", "path-pattern"),
            ("Conditions.member.1.Values.member.1", "/api/*"),
            ("Actions.member.1.Type", "forward"),
            ("Actions.member.1.TargetGroupArn", published.as_str()),
        ],
    )
    .await;

    // Every alarm is created BEFORE anything is waited for, while the checks have determined
    // nothing: each cell's verdict is one the evaluation reached on its own.
    for treatment in &TREATMENTS {
        for (subject, group) in [("rule", &published), ("twin", &silent)] {
            put_alarm(
                &router,
                treatment.treat_missing,
                &alarm_name(treatment.treat_missing, subject),
                group,
                &load_balancer,
                treatment.treat_missing,
            )
            .await;
        }
    }

    // The published series begins at the verdict the rule-routed group's checks reach, and every
    // datapoint of it counts the registrations that verdict covers.
    let series_case = "the series of the group a path-pattern rule forwards to";
    let series_deadline = Instant::now() + DEADLINE;
    let unhealthy_points = loop {
        let points = host_counts(
            &router,
            series_case,
            UNHEALTHY_HOST_COUNT,
            &published,
            &load_balancer,
        )
        .await;
        if !points.is_empty() {
            break points;
        }
        assert!(
            Instant::now() < series_deadline,
            "case {series_case:?}: expected the {UNHEALTHY_HOST_COUNT} series to begin at the \
             verdict its checks reached, got no datapoint with the registrations {:?}",
            target_health_states(&router, &published).await
        );
        ::tokio::time::sleep(POLL).await;
    };
    let expected_unhealthy = DEAD_ADDRESSES.len() as f64;
    assert!(
        unhealthy_points
            .iter()
            .all(|count| *count == expected_unhealthy),
        "case {series_case:?}: expected every {UNHEALTHY_HOST_COUNT} datapoint to be \
         {expected_unhealthy}, got {unhealthy_points:?}"
    );
    let healthy_points = host_counts(
        &router,
        series_case,
        HEALTHY_HOST_COUNT,
        &published,
        &load_balancer,
    )
    .await;
    assert!(
        !healthy_points.is_empty() && healthy_points.iter().all(|count| *count == 0.0),
        "case {series_case:?}: expected the {HEALTHY_HOST_COUNT} series to hold datapoints and \
         every one of them to be 0, got {healthy_points:?}"
    );
    // Counted off the very health a client reads for the same registrations.
    let states = target_health_states(&router, &published).await;
    assert!(
        states.len() == DEAD_ADDRESSES.len() && states.iter().all(|state| state == UNHEALTHY),
        "case {series_case:?}: expected every registration of the rule-routed group to read \
         {UNHEALTHY}, got {states:?} with {UNHEALTHY_HOST_COUNT} {unhealthy_points:?}"
    );

    // And the twin publishes nothing at all: nothing forwards to it, so nothing checks it.
    let silent_case = "the series of the twin no action forwards to";
    for metric in [UNHEALTHY_HOST_COUNT, HEALTHY_HOST_COUNT] {
        let points = host_counts(&router, silent_case, metric, &silent, &load_balancer).await;
        assert!(
            points.is_empty(),
            "case {silent_case:?}: expected no {metric} datapoint for a pair nothing forwards \
             to, got {points:?}"
        );
    }
    let states = target_health_states(&router, &silent).await;
    assert!(
        states.len() == DEAD_ADDRESSES.len() && states.iter().all(|state| state == UNUSED),
        "case {silent_case:?}: expected every registration of the unrouted twin to read \
         {UNUSED}, got {states:?}"
    );

    // Over the PUBLISHED series every treatment reads the same state, because the datapoints
    // decide it and no period of the evaluation is a hole.
    for treatment in &TREATMENTS {
        let case = format!("{} over the published series", treatment.treat_missing);
        let name = alarm_name(treatment.treat_missing, "rule");
        let seen = states_until(&router, &case, &name, treatment.over_published).await;
        assert_eq!(
            seen.last().map(String::as_str),
            Some(treatment.over_published),
            "case {case:?}: expected the alarm to reach {}, got the states {seen:?} with \
             {UNHEALTHY_HOST_COUNT} {unhealthy_points:?}",
            treatment.over_published
        );
        if treatment.treat_missing == "breaching" {
            // A breaching alarm over a group nothing answers for is never reported OK on the way
            // there: a period the checks had determined nothing for is graded as the hole it was.
            assert!(
                seen.iter().all(|state| state != OK),
                "case {case:?}: expected {ALARM} out of {INSUFFICIENT_DATA} alone, got the \
                 states {seen:?}"
            );
        }
    }

    // Over the SILENT series each treatment reads its own state. The ones that move are waited
    // for first, so the evaluations of the alarms over that same series have provably run by the
    // time the ones that must NOT move are read.
    for treatment in TREATMENTS
        .iter()
        .filter(|treatment| treatment.over_silent != INSUFFICIENT_DATA)
    {
        let case = format!("{} over the silent series", treatment.treat_missing);
        let name = alarm_name(treatment.treat_missing, "twin");
        let seen = states_until(&router, &case, &name, treatment.over_silent).await;
        assert_eq!(
            seen.last().map(String::as_str),
            Some(treatment.over_silent),
            "case {case:?}: expected the alarm over a series holding no datapoint to reach {}, \
             got the states {seen:?}",
            treatment.over_silent
        );
    }
    for treatment in TREATMENTS
        .iter()
        .filter(|treatment| treatment.over_silent == INSUFFICIENT_DATA)
    {
        let case = format!("{} over the silent series", treatment.treat_missing);
        let name = alarm_name(treatment.treat_missing, "twin");
        let state = alarm_state(&router, &case, &name).await;
        assert_eq!(
            state, INSUFFICIENT_DATA,
            "case {case:?}: expected the alarm over a series holding no datapoint to stay in \
             {INSUFFICIENT_DATA}, got {state}"
        );
    }
    // The twin's series is still empty, so what those alarms were graded over really was
    // silence.
    let points = host_counts(
        &router,
        silent_case,
        UNHEALTHY_HOST_COUNT,
        &silent,
        &load_balancer,
    )
    .await;
    assert!(
        points.is_empty(),
        "case {silent_case:?}: expected the pair nothing forwards to to publish no \
         {UNHEALTHY_HOST_COUNT} datapoint at all, got {points:?}"
    );
}
