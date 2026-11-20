// SPDX-License-Identifier: CC0-1.0

//! The `AWS/ApplicationELB` series a load balancer's own traffic feeds, read back the three ways
//! a client WATCHES a load balancer: a statistic over one series, a metric-math RATE derived from
//! two of them, and an alarm graded on either.
//!
//! The rule under test: **every request an Application Load Balancer answers is metered as it is
//! answered into the metric store CloudWatch's own readers serve, so a `GetMetricStatistics`
//! read, a `GetMetricData` expression over two of those series and a `PutMetricAlarm` alarm over
//! them all answer from the traffic the load balancer really served — a status the TARGET
//! returned counts into the target class series of the group that answered, a status the LOAD
//! BALANCER generated itself counts into its own class and status series under the load balancer
//! alone, a failure of a `lambda` target's own making counts into that target type's user-error
//! series, and a series no request gave a value to holds no datapoint at all, so a rate derived
//! from target counts stays low while a count over the same traffic breaches and an alarm
//! narrowed to a target group that answered nothing never leaves `INSUFFICIENT_DATA`.**
//!
//! The metric names, their dimensions and their reporting criteria ("there is a non-zero value")
//! are Elastic Load Balancing's published CloudWatch metrics for the Application Load Balancer
//! family. What a reader then does with them is CloudWatch's own: "The maximum number of data
//! points returned from a single call is 1,440"
//! (`smithy_query(query="doc",shape="GetMetricStatistics")`) aggregates one series into the
//! periods this suite reads, "A math expression can use the Id of the other metrics or queries to
//! refer to those metrics" (`smithy_query(query="doc",shape="MetricDataQuery",member="Expression")`)
//! derives the rate, and "Use the `Metrics` parameter to create an alarm based on a metric math
//! expression" (`smithy_query(query="doc",shape="PutMetricAlarmInput",member="Metrics")`) is what
//! grades that rate.
//!
//! The rows differ along what the rule generalises over: the OUTCOME class of the request that
//! fed the series (a status the target returned as a success, a server error of the target's own,
//! a run that raised so the load balancer answered for it), the SERIES read (the request counter,
//! a target class count, a load-balancer class count, a load-balancer status count, the Lambda
//! user-error count, a class no request ended in), the resource the series is read ABOUT (the
//! load balancer, the target group that answered, a target group that answered nothing), and the
//! READER it is read through (statistic, expression, alarm over a metric, alarm over an
//! expression).
//!
//! Every request is made over a real TCP connection to the address the load balancer PUBLISHES,
//! every datapoint is read back through CloudWatch's own API on the shared engine — the store
//! `GetMetricStatistics`, `GetMetricData` and the alarm evaluation all serve — and that the
//! function really ran is asserted where nothing of this service can fake it: the handler reports
//! every invocation to an Amazon SQS queue through its own SDK before it decides what to return.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::alb_data_plane::{create_load_balancer, settle};
use crate::alb_lambda_target_published_address::{
    activate_functions, exchange, execution_role, lambda, reported, sqs, HANDLER, RUNTIME,
};
use crate::alb_request_metrics::package;
use crate::provisioning_loop::{action, call, p, REGION};

/// The namespace an Application Load Balancer's metrics live in, and the dimensions its series
/// are keyed by — Elastic Load Balancing's published metric identities for that family.
const APPLICATION_NAMESPACE: &str = "AWS/ApplicationELB";
const LOAD_BALANCER_DIMENSION: &str = "LoadBalancer";
const TARGET_GROUP_DIMENSION: &str = "TargetGroup";

/// The `X-Amz-Target` prefix CloudWatch's model declares, which every read below is addressed
/// with.
const CLOUDWATCH_TARGET_PREFIX: &str = "GraniteServiceVersion20100801";

/// The width of every window this suite reads and every alarm it grades, in seconds: the finest
/// period an alarm over a regular-resolution metric can be evaluated at, which is the resolution
/// this service stores these series at.
const PERIOD_SECS: i64 = 60;

/// The same width as the clock measures it, for the window arithmetic below.
const PERIOD: f64 = PERIOD_SECS as f64;

/// How much of the current period has to be left before the traffic is sent, so every datapoint
/// of it falls in ONE window: a metric is aggregated into periods anchored at multiples of the
/// period, so a run that began a second before a boundary would split its counts across two of
/// them. Polled for rather than slept through, and far longer than the handful of seconds the
/// traffic takes in this process.
const ROOM_IN_PERIOD: f64 = 30.0;

/// The `StateValue` values the three alarms must reach.
const ALARM: &str = "ALARM";
const OK: &str = "OK";
const INSUFFICIENT_DATA: &str = "INSUFFICIENT_DATA";

/// The function every request is forwarded to, the role it runs as, and the queue it reports
/// each invocation to.
const FUNCTION: &str = "alb-metric-rate-target";
const ROLE_NAME: &str = "alb-metric-rate-exec";
const SIDE_CHANNEL: &str = "alb-metric-rate-side-channel";

/// The load balancer, the target group every request reaches, and a second group of the same
/// load balancer that no request reaches.
const LOAD_BALANCER: &str = "alb-metric-rate";
const SERVING_GROUP: &str = "alb-metric-rate-served";
const IDLE_GROUP: &str = "alb-metric-rate-idle";

/// The three alarms: a count over the target group's own 5XX series, the rate that count and the
/// request count derive, and the same count narrowed to the group that answered nothing.
const COUNT_ALARM: &str = "alb-metric-rate-count";
const RATE_ALARM: &str = "alb-metric-rate-rate";
const IDLE_ALARM: &str = "alb-metric-rate-idle-count";

/// The query ids of the rate: the two series it reads and the expression that returns it.
const ERRORS_ID: &str = "errors";
const REQUESTS_ID: &str = "requests";
const RATE_ID: &str = "rate";

/// The expression the rate is derived by: the share of the requests the group answered that
/// ended in a status the TARGET returned in the 5XX class, as a percentage. It refers to the two
/// reads by the ids above, which is how a math expression names the queries it reads
/// ([`ERRORS_ID`], [`REQUESTS_ID`]).
const RATE_EXPRESSION: &str = "(errors / requests) * 100";

/// The threshold the rate is graded against, and the one the count is: a rate well above the
/// traffic's own and a count any single breach exceeds, which is what makes one alarm fire over
/// the very traffic the other stays `OK` over.
const RATE_THRESHOLD: f64 = 50.0;
const COUNT_THRESHOLD: f64 = 0.0;

/// How long the suite waits for work the clock owes — the invocation report the handler writes,
/// the period the datapoints fell in to close so an alarm evaluation grades it — and how long
/// between two looks. Generous on purpose: the longest wait is for one period to close, and the
/// deadline is only here so a failure is a failure rather than a hang.
const BUDGET: Duration = Duration::from_secs(30);
const SETTLE_BUDGET: Duration = Duration::from_secs(180);
const LOOK_AGAIN: Duration = Duration::from_millis(100);

/// How close two computed values have to be to count as the same number.
const TOLERANCE: f64 = 1e-9;

/// This service, Lambda, Amazon SQS, IAM, the token service and CloudWatch over ONE shared
/// engine, as `core/router/src/lib.rs` mounts them: the function the load balancer forwards to is
/// run by that Lambda as a session of the role IAM holds, the handler's own SDK call is
/// dispatched back into this router, and the metric store the load balancer publishes into is the
/// store CloudWatch's own readers below answer from.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_elasticloadbalancingv2_elasticloadbalancing_v10::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_cloudwatch_graniteserviceversion20100801::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    (router, engine)
}

/// One CloudWatch call, as that service's wire protocol carries it (`awsJson1_0` over `POST /`
/// with `X-Amz-Target`), answering its status and parsed body.
async fn cloudwatch(
    router: &MultiServiceRouter,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("monitoring.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header(
            "x-amz-target",
            format!("{CLOUDWATCH_TARGET_PREFIX}.{operation}"),
        )
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTEST/20260101/{REGION}/monitoring/aws4_request, \
                 SignedHeaders=host;x-amz-target, Signature=0000"
            ),
        )
        .body(bytes::Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// The same call, requiring the 200 a well-formed read answers with.
async fn cloudwatch_ok(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let (status, parsed) = cloudwatch(router, operation, body).await;
    assert_eq!(
        status, 200,
        "expected {operation} to answer 200, got {status} {parsed}"
    );
    parsed
}

/// Wall-clock now as the epoch seconds every window below is measured against.
fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |since| since.as_secs_f64())
}

/// Poll until at least [`ROOM_IN_PERIOD`] seconds are left of the period the clock is in.
async fn wait_for_period_room() {
    while PERIOD - now_secs().rem_euclid(PERIOD) < ROOM_IN_PERIOD {
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The `app/<name>/<id>` suffix of a load balancer ARN, which is how an Elastic Load Balancing
/// dimension names it.
fn load_balancer_dimension(arn: &str) -> String {
    arn.split_once(":loadbalancer/")
        .map(|(_, suffix)| suffix.to_string())
        .unwrap_or_else(|| panic!("a load balancer ARN names its family and name: {arn}"))
}

/// The `targetgroup/<name>/<id>` suffix of a target group ARN, which is how that dimension names
/// it.
fn target_group_dimension(arn: &str) -> String {
    arn.split_once(":targetgroup/")
        .map(|(_, suffix)| format!("targetgroup/{suffix}"))
        .unwrap_or_else(|| panic!("a target group ARN names its resource type: {arn}"))
}

/// Which resource a series is read ABOUT.
#[derive(Clone, Copy, Debug)]
enum About {
    /// The load balancer alone, which is what a count of what IT did carries.
    LoadBalancer,
    /// The target group every request reached, as the `TargetGroup` + `LoadBalancer` pair.
    Served,
    /// The target group of the same load balancer that no request reached.
    Idle,
}

/// Every dimension set this suite reads, resolved once the resources exist.
#[derive(Debug)]
struct Keys {
    load_balancer: Value,
    served: Value,
    idle: Value,
}

impl Keys {
    /// The `Dimensions` list a read about `about` carries.
    fn of(&self, about: About) -> Value {
        match about {
            About::LoadBalancer => self.load_balancer.clone(),
            About::Served => self.served.clone(),
            About::Idle => self.idle.clone(),
        }
    }
}

/// One row of the statistic table: the series, the resource it is read about, and the `Sum` the
/// traffic must have left in it — `0.0` meaning the series holds NO datapoint at all, which is
/// what these series' reporting criteria make of a count of nothing.
#[derive(Debug)]
struct SeriesRow {
    case: &'static str,
    metric: &'static str,
    about: About,
    expected: f64,
}

/// One request of the traffic: the path the client asks for, the status the load balancer must
/// answer it with, and the mode the handler reports for it.
#[derive(Debug)]
struct Request {
    case: &'static str,
    target: &'static str,
    status: u16,
}

/// The traffic, in the order it is sent: a warm-up the target answers, four more successes, one
/// server error of the TARGET's own, and one run that RAISES — which the load balancer answers
/// itself. Eight requests, one of them a target 5XX: a low error rate, and a count of one.
const TRAFFIC: &[Request] = &[
    Request {
        case: "a warm-up request the target answered itself",
        target: "/metered/health",
        status: 200,
    },
    Request {
        case: "a success the target answered",
        target: "/metered/ok",
        status: 200,
    },
    Request {
        case: "a second success",
        target: "/metered/ok",
        status: 200,
    },
    Request {
        case: "a third success",
        target: "/metered/ok",
        status: 200,
    },
    Request {
        case: "a fourth success",
        target: "/metered/ok",
        status: 200,
    },
    Request {
        case: "a fifth success",
        target: "/metered/ok",
        status: 200,
    },
    Request {
        case: "a server error of the target's own, passed on as the target answered it",
        target: "/metered/fail",
        status: 500,
    },
    Request {
        case: "a run that raised, which the load balancer answers itself",
        target: "/metered/raise",
        status: 502,
    },
];

/// How many requests the traffic makes, and how many of them ended in a status the target
/// returned in the 5XX class — the two numbers the rate below is the quotient of.
const REQUESTS: f64 = 8.0;
const TARGET_ERRORS: f64 = 1.0;

/// The statistic table.
const SERIES: &[SeriesRow] = &[
    SeriesRow {
        case: "every request the load balancer answered is one request processed",
        metric: "RequestCount",
        about: About::LoadBalancer,
        expected: REQUESTS,
    },
    SeriesRow {
        case: "and one for the target group it was forwarded to",
        metric: "RequestCount",
        about: About::Served,
        expected: REQUESTS,
    },
    SeriesRow {
        case: "the successes the target answered, counted as the target's own class",
        metric: "HTTPCode_Target_2XX_Count",
        about: About::Served,
        expected: 6.0,
    },
    SeriesRow {
        case: "the server error the target returned, narrowed to the group that answered it",
        metric: "HTTPCode_Target_5XX_Count",
        about: About::Served,
        expected: TARGET_ERRORS,
    },
    SeriesRow {
        case: "the same count under the load balancer's own dimension",
        metric: "HTTPCode_Target_5XX_Count",
        about: About::LoadBalancer,
        expected: TARGET_ERRORS,
    },
    SeriesRow {
        case: "the 502 the load balancer generated for the run that raised: its own class",
        metric: "HTTPCode_ELB_5XX_Count",
        about: About::LoadBalancer,
        expected: 1.0,
    },
    SeriesRow {
        case: "and the series of that one status",
        metric: "HTTPCode_ELB_502_Count",
        about: About::LoadBalancer,
        expected: 1.0,
    },
    SeriesRow {
        case: "a status the LOAD BALANCER generated is in no target group's count",
        metric: "HTTPCode_ELB_5XX_Count",
        about: About::Served,
        expected: 0.0,
    },
    SeriesRow {
        case: "the raised run is a failure of the function's own making",
        metric: "LambdaUserError",
        about: About::Served,
        expected: 1.0,
    },
    SeriesRow {
        case: "counted for the load balancer as well",
        metric: "LambdaUserError",
        about: About::LoadBalancer,
        expected: 1.0,
    },
    SeriesRow {
        case: "a class no request ended in holds no datapoint",
        metric: "HTTPCode_Target_4XX_Count",
        about: About::Served,
        expected: 0.0,
    },
    SeriesRow {
        case: "the target group that answered nothing has no request counted against it",
        metric: "RequestCount",
        about: About::Idle,
        expected: 0.0,
    },
];

/// Create a `lambda` target group, register `function_arn` in it, and grant the invocation the
/// group's requests need — all three through the owning API. Answers the group's ARN.
async fn lambda_target_group(
    router: &MultiServiceRouter,
    name: &str,
    function_arn: &str,
) -> String {
    let mut form = action("CreateTargetGroup");
    form.push(p("Name", name));
    form.push(p("TargetType", "lambda"));
    let reply = call(router, &form).await;
    assert_eq!(
        reply.status, 200,
        "setup CreateTargetGroup {name}: expected 200, got {} body {}",
        reply.status, reply.body
    );
    let tg_arn = reply.text("TargetGroupArn").unwrap_or_else(|| {
        panic!(
            "setup CreateTargetGroup {name}: no TargetGroupArn in {}",
            reply.body
        )
    });

    let mut form = action("RegisterTargets");
    form.push(p("TargetGroupArn", &tg_arn));
    form.push(p("Targets.member.1.Id", function_arn));
    let reply = call(router, &form).await;
    assert_eq!(
        reply.status, 200,
        "setup RegisterTargets {name}: expected 200, got {} body {}",
        reply.status, reply.body
    );

    let (status, granted) = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{FUNCTION}/policy"),
        json!({
            "StatementId": format!("elbv2-invoke-{name}"),
            "Action": "lambda:InvokeFunction",
            "Principal": "elasticloadbalancing.amazonaws.com",
            "SourceArn": tg_arn,
        }),
    )
    .await;
    assert_eq!(
        status, 201,
        "setup AddPermission {name}: expected 201, got {status} {granted}"
    );
    tg_arn
}

/// The `Sum` of every datapoint a `GetMetricStatistics` read of `metric` over `dimensions`
/// answers with, and how many datapoints carried it.
async fn statistic(
    router: &MultiServiceRouter,
    metric: &str,
    dimensions: &Value,
    start: f64,
    end: f64,
) -> (f64, usize) {
    let body = cloudwatch_ok(
        router,
        "GetMetricStatistics",
        &json!({
            "Namespace": APPLICATION_NAMESPACE,
            "MetricName": metric,
            "Dimensions": dimensions,
            "StartTime": start,
            "EndTime": end,
            "Period": PERIOD_SECS,
            "Statistics": ["Sum"],
        }),
    )
    .await;
    let points = body["Datapoints"].as_array().cloned().unwrap_or_default();
    let total = points
        .iter()
        .map(|point| point["Sum"].as_f64().unwrap_or_default())
        .sum();
    (total, points.len())
}

/// The `Metrics` query array the rate is computed by: the two series it reads, neither of which
/// returns data, and the expression over them that does — the ONE returning query an alarm over
/// such an array is graded on, and the one a reader asks `GetMetricData` for.
fn rate_queries(dimensions: &Value) -> Value {
    let read = |id: &str, metric: &str| {
        json!({
            "Id": id,
            "ReturnData": false,
            "MetricStat": {
                "Metric": {
                    "Namespace": APPLICATION_NAMESPACE,
                    "MetricName": metric,
                    "Dimensions": dimensions,
                },
                "Period": PERIOD_SECS,
                "Stat": "Sum",
            },
        })
    };
    json!([
        read(ERRORS_ID, "HTTPCode_Target_5XX_Count"),
        read(REQUESTS_ID, "RequestCount"),
        {
            "Id": RATE_ID,
            "Expression": RATE_EXPRESSION,
            "Label": "target 5xx rate",
            "ReturnData": true,
            "Period": PERIOD_SECS,
        },
    ])
}

/// The values the rate query answers over `[start, end)`, newest last.
async fn rate_values(
    router: &MultiServiceRouter,
    dimensions: &Value,
    start: f64,
    end: f64,
) -> Vec<f64> {
    let body = cloudwatch_ok(
        router,
        "GetMetricData",
        &json!({
            "MetricDataQueries": rate_queries(dimensions),
            "StartTime": start,
            "EndTime": end,
            "ScanBy": "TimestampAscending",
        }),
    )
    .await;
    body["MetricDataResults"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .find(|result| result["Id"].as_str() == Some(RATE_ID))
        .map(|result| {
            result["Values"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|value| value.as_f64().unwrap_or_default())
                .collect()
        })
        .unwrap_or_default()
}

/// The `StateValue` `DescribeAlarms` reports for one metric alarm, or the empty string for an
/// alarm it reports no state for.
async fn alarm_state(router: &MultiServiceRouter, name: &str) -> String {
    let body = cloudwatch_ok(
        router,
        "DescribeAlarms",
        &json!({ "AlarmNames": [name], "AlarmTypes": ["MetricAlarm"] }),
    )
    .await;
    body["MetricAlarms"][0]["StateValue"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// RULE: the series a load balancer publishes while answering requests are the series
/// CloudWatch's own readers answer from — a statistic over one of them, an expression deriving a
/// rate from two, and an alarm graded over either.
#[tokio::test]
async fn the_served_traffic_is_what_cloudwatch_reads_back_and_grades() {
    let (router, engine) = emulator();

    // The side channel the handler reports every invocation to, and the role whose one grant
    // admits that report.
    let (status, queue) = sqs(&router, "CreateQueue", &json!({ "QueueName": SIDE_CHANNEL })).await;
    assert_eq!(
        status, 200,
        "setup CreateQueue: expected 200, got {status} {queue}"
    );
    let queue_url = queue["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("setup CreateQueue: no QueueUrl in {queue}"))
        .to_string();
    let (status, attributes) = sqs(
        &router,
        "GetQueueAttributes",
        &json!({ "QueueUrl": queue_url, "AttributeNames": ["QueueArn"] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup GetQueueAttributes: expected 200, got {status} {attributes}"
    );
    let queue_arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup GetQueueAttributes: no QueueArn in {attributes}"))
        .to_string();
    let role = execution_role(&router, ROLE_NAME, &queue_arn).await;

    // The function every request is forwarded to, created through Lambda's own API.
    let (status, created) = lambda(
        &router,
        "POST",
        "/2015-03-31/functions",
        json!({
            "FunctionName": FUNCTION,
            "Role": role,
            "Runtime": RUNTIME,
            "Handler": HANDLER,
            "Timeout": 30,
            "Environment": { "Variables": { "QUEUE_URL": queue_url } },
            "Code": { "ZipFile": aws_smithy_types::base64::encode(package()) },
        }),
    )
    .await;
    assert_eq!(
        status, 201,
        "setup CreateFunction: expected 201, got {status} {created}"
    );
    let function_arn = created["FunctionArn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup CreateFunction: no FunctionArn in {created}"))
        .to_string();
    // The activation span Lambda owes the function, placed rather than slept for.
    activate_functions(&engine);

    // The load balancer, the address it publishes, and its two `lambda` target groups: the one
    // the listener's default action forwards to, and one a rule sends a path no request asks for.
    let (lb_arn, published) = create_load_balancer(&router, LOAD_BALANCER).await;
    let served_arn = lambda_target_group(&router, SERVING_GROUP, &function_arn).await;
    let idle_arn = lambda_target_group(&router, IDLE_GROUP, &function_arn).await;

    let mut form = action("CreateListener");
    form.push(p("LoadBalancerArn", &lb_arn));
    form.push(p("Protocol", "HTTP"));
    form.push(p("Port", "80"));
    form.push(p("DefaultActions.member.1.Type", "forward"));
    form.push(p("DefaultActions.member.1.TargetGroupArn", &served_arn));
    let reply = call(&router, &form).await;
    assert_eq!(
        reply.status, 200,
        "setup CreateListener: expected 200, got {} body {}",
        reply.status, reply.body
    );
    let listener_arn = reply
        .text("ListenerArn")
        .unwrap_or_else(|| panic!("setup CreateListener: no ListenerArn in {}", reply.body));

    let mut form = action("CreateRule");
    form.push(p("ListenerArn", &listener_arn));
    form.push(p("Priority", "10"));
    form.push(p("Conditions.member.1.Field", "path-pattern"));
    form.push(p("Conditions.member.1.Values.member.1", "/idle/*"));
    form.push(p("Actions.member.1.Type", "forward"));
    form.push(p("Actions.member.1.TargetGroupArn", &idle_arn));
    let reply = call(&router, &form).await;
    assert_eq!(
        reply.status, 200,
        "setup CreateRule: expected 200, got {} body {}",
        reply.status, reply.body
    );

    // The move to `active` the clock owes the load balancer, placed rather than slept for.
    settle(&engine, &lb_arn, SystemTime::now() - Duration::from_secs(1));

    let keys = Keys {
        load_balancer: json!([
            { "Name": LOAD_BALANCER_DIMENSION, "Value": load_balancer_dimension(&lb_arn) },
        ]),
        served: json!([
            { "Name": TARGET_GROUP_DIMENSION, "Value": target_group_dimension(&served_arn) },
            { "Name": LOAD_BALANCER_DIMENSION, "Value": load_balancer_dimension(&lb_arn) },
        ]),
        idle: json!([
            { "Name": TARGET_GROUP_DIMENSION, "Value": target_group_dimension(&idle_arn) },
            { "Name": LOAD_BALANCER_DIMENSION, "Value": load_balancer_dimension(&lb_arn) },
        ]),
    };

    // The whole of the traffic falls in one period, so one window holds the counts a rate is
    // derived from and an alarm grades.
    wait_for_period_room().await;
    let window_start = (now_secs() / PERIOD).floor() * PERIOD;

    for request in TRAFFIC {
        let answered = exchange(request.case, &published, request.target).await;
        assert_eq!(
            answered.status, request.status,
            "case {:?}: expected HTTP {}, got {} in {:?}",
            request.case, request.status, answered.status, answered.raw
        );
        // The function RAN, exactly once: the handler reports before it decides what to return.
        let mode = request
            .target
            .rsplit('/')
            .next()
            .expect("a target names a last segment");
        let seen = reported(&router, request.case, &queue_url, BUDGET).await;
        assert_eq!(
            seen,
            vec![mode.to_string()],
            "case {:?}: expected exactly one invocation reported as {mode:?}, got {seen:?}",
            request.case
        );
    }

    // What a client's own statistic read answers, series by series.
    let start = window_start - PERIOD;
    let end = window_start + 2.0 * PERIOD;
    for row in SERIES {
        let dimensions = keys.of(row.about);
        let (total, points) = statistic(&router, row.metric, &dimensions, start, end).await;
        assert!(
            (total - row.expected).abs() < TOLERANCE,
            "case {:?}: expected {} of {} about {:?} to sum to {}, got {total} over {points} \
             datapoint(s)",
            row.case,
            row.metric,
            APPLICATION_NAMESPACE,
            row.about,
            row.expected
        );
        if row.expected == 0.0 {
            assert_eq!(
                points, 0,
                "case {:?}: a series no request gave a value must hold no datapoint, got {points}",
                row.case
            );
        }
    }

    // What the expression over two of those series derives: one window, holding the share of the
    // group's requests that ended in a status the target returned in the 5XX class.
    let expected_rate = TARGET_ERRORS / REQUESTS * 100.0;
    let values = rate_values(&router, &keys.served, start, end).await;
    assert_eq!(
        values.len(),
        1,
        "case {RATE_EXPRESSION:?}: expected the traffic to derive one window's rate, got {values:?}"
    );
    assert!(
        (values[0] - expected_rate).abs() < TOLERANCE,
        "case {RATE_EXPRESSION:?}: expected a rate of {expected_rate}, got {}",
        values[0]
    );

    // And what an alarm over each of them grades: the count breaches while the rate it derives
    // does not, and an alarm narrowed to the group that answered nothing has nothing to grade.
    let count_alarm = |name: &str, dimensions: Value| {
        json!({
            "AlarmName": name,
            "Namespace": APPLICATION_NAMESPACE,
            "MetricName": "HTTPCode_Target_5XX_Count",
            "Dimensions": dimensions,
            "Statistic": "Sum",
            "Period": PERIOD_SECS,
            "EvaluationPeriods": 1,
            "DatapointsToAlarm": 1,
            "Threshold": COUNT_THRESHOLD,
            "ComparisonOperator": "GreaterThanThreshold",
            "TreatMissingData": "missing",
            "ActionsEnabled": false,
        })
    };
    cloudwatch_ok(
        &router,
        "PutMetricAlarm",
        &count_alarm(COUNT_ALARM, keys.of(About::Served)),
    )
    .await;
    cloudwatch_ok(
        &router,
        "PutMetricAlarm",
        &count_alarm(IDLE_ALARM, keys.of(About::Idle)),
    )
    .await;
    cloudwatch_ok(
        &router,
        "PutMetricAlarm",
        &json!({
            "AlarmName": RATE_ALARM,
            "Metrics": rate_queries(&keys.served),
            "EvaluationPeriods": 1,
            "DatapointsToAlarm": 1,
            "Threshold": RATE_THRESHOLD,
            "ComparisonOperator": "GreaterThanThreshold",
            "TreatMissingData": "missing",
            "ActionsEnabled": false,
        }),
    )
    .await;

    // The verdicts are owed once the period the datapoints fell in has CLOSED — an alarm grades
    // the periods that are complete — so each is looked at again until it is there.
    let expected: [(&str, &str); 3] = [
        (COUNT_ALARM, ALARM),
        (RATE_ALARM, OK),
        (IDLE_ALARM, INSUFFICIENT_DATA),
    ];
    let deadline = Instant::now() + SETTLE_BUDGET;
    loop {
        let mut states: Vec<(&str, String)> = Vec::new();
        for (name, _) in expected {
            states.push((name, alarm_state(&router, name).await));
        }
        if states
            .iter()
            .zip(expected)
            .all(|((_, state), (_, wanted))| state == wanted)
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "expected the alarms to grade the window the traffic fell in as {expected:?}, got \
             {states:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}
