// SPDX-License-Identifier: CC0-1.0

//! The two OBSERVABILITY surfaces of one request a load balancer forwarded to a `lambda`
//! target group: the access-log record it delivers, and the `AWS/ApplicationELB` series a
//! client reads back with `GetMetricData`.
//!
//! The rule under test: **a request answered through a `lambda` target group is observed once
//! and reported the same way on both surfaces — the status the FUNCTION returned is the
//! record's target status and counts into the target class series of the group that answered,
//! while an answer the load balancer generated itself for a failure of the function's own
//! making carries no target status and names that failure class in the record, and counts into
//! the load balancer's own class and status series and the Lambda user-error series rather than
//! into any target class series; and because such a target is reached by INVOKING it rather
//! than by connecting to an address, neither address field of the record names a target at
//! all.**
//!
//! `TargetGroup$TargetType` gives `lambda` as "Register a single Lambda function as a target"
//! and `TargetDescription$Id` as "the ARN of the Lambda function" for such a group;
//! `LoadBalancerAttribute$Key` names the destination of the record
//! (`access_logs.s3.enabled` / `access_logs.s3.bucket` / `access_logs.s3.prefix`). The
//! record's field order, the `error_reason` token each Lambda-target failure class carries, and
//! the metric names, dimensions and reporting criteria ("there is a non-zero value") are
//! Elastic Load Balancing's documented access-log format and published CloudWatch metrics for
//! the Application Load Balancer family. What a reader then does with those series is
//! CloudWatch's own: "A math expression can use the Id of the other metrics or queries to refer
//! to those metrics" (`smithy_query(query="doc",shape="MetricDataQuery",member="Expression")`)
//! is what derives the 5xx rate read here.
//!
//! What this suite drives that its siblings do not: the record and the series are read for the
//! SAME traffic, so neither surface can report an outcome the other contradicts, and the two
//! record fields that carry a Lambda target's IDENTITY — the address fields, which such an
//! entry leaves absent, and the target-status list beside them — are read back out of the
//! bucket a client reads them from.
//!
//! The rows differ along what the rule generalises over: what the FUNCTION did with the
//! request (answered a success, answered a server error of its own, answered a different
//! server error, raised, returned a payload that is no response object), which spans both
//! halves of the attribution — the statuses the target produced and the ones the load balancer
//! generated for it. That the function really ran is asserted where nothing of this service can
//! fake it: the handler reports every invocation to an Amazon SQS queue through its own SDK
//! before it decides what to return.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::alb_data_plane::{create_load_balancer, settle};
use crate::alb_lambda_target_access_log::{fields, log_bucket, record_of};
use crate::alb_lambda_target_published_address::{
    activate_functions, exchange, execution_role, lambda, reported, sqs, HANDLER, RUNTIME,
};
use crate::alb_request_metrics::package;
// The crate's ONE addressing of a CloudWatch read and ONE derivation of each Elastic Load
// Balancing dimension value, kept by the sibling suite that reads these same series back
// through CloudWatch's own readers: two suites must not disagree about how a series is keyed.
use crate::alb_target_metric_rate_alarms::{
    cloudwatch_ok, load_balancer_dimension, now_secs, target_group_dimension,
};
use crate::provisioning_loop::{action, call, p};

/// The namespace an Application Load Balancer's metrics live in, and the dimensions its series
/// are keyed by — Elastic Load Balancing's published metric identities for that family.
const APPLICATION_NAMESPACE: &str = "AWS/ApplicationELB";
const LOAD_BALANCER_DIMENSION: &str = "LoadBalancer";
const TARGET_GROUP_DIMENSION: &str = "TargetGroup";

/// The granularity every series is read at, in seconds: the resolution this service stores
/// these series at.
const PERIOD_SECS: i64 = 60;

/// The same width as the clock measures it, for the window arithmetic below.
const PERIOD: f64 = PERIOD_SECS as f64;

/// The function every request is forwarded to, the role it runs as, and the queue it reports
/// each invocation to.
const FUNCTION: &str = "alb-record-and-series-target";
const ROLE_NAME: &str = "alb-record-and-series-exec";
const SIDE_CHANNEL: &str = "alb-record-and-series-side-channel";

/// The load balancer, its `lambda` target group, and the bucket and prefix its attributes send
/// the access log to.
const LOAD_BALANCER: &str = "alb-record-and-series";
const TARGET_GROUP: &str = "alb-record-series-tg";
const LOG_BUCKET: &str = "elbv2-record-and-series-logs";
const LOG_PREFIX: &str = "observed";

/// The three attributes that turn access logging on and say where it goes, from
/// `LoadBalancerAttribute$Key`'s own catalog.
const ENABLED_ATTRIBUTE: &str = "access_logs.s3.enabled";
const BUCKET_ATTRIBUTE: &str = "access_logs.s3.bucket";
const PREFIX_ATTRIBUTE: &str = "access_logs.s3.prefix";

/// What a record's field carries where it does not apply, which is what Elastic Load Balancing
/// writes for one.
const ABSENT: &str = "-";

/// The positions of the fields this suite reads, in the documented order Elastic Load
/// Balancing publishes for an HTTP entry (1-based in the documentation, 0-based here): the
/// address of the target that answered, the status the client was answered with, the status the
/// target answered, the target group, the error reason, and the two list fields that repeat the
/// target's address and status.
const TARGET_ADDRESS_FIELD: usize = 4;
const ELB_STATUS_FIELD: usize = 8;
const TARGET_STATUS_FIELD: usize = 9;
const TARGET_GROUP_FIELD: usize = 16;
const ERROR_REASON_FIELD: usize = 24;
const TARGET_ADDRESS_LIST_FIELD: usize = 25;
const TARGET_STATUS_LIST_FIELD: usize = 26;

/// The query ids of the rate: the two series it reads and the expression over them.
const ERRORS_ID: &str = "errors";
const REQUESTS_ID: &str = "requests";
const RATE_ID: &str = "rate";

/// The expression the rate is derived by: the share of the requests the group answered that
/// ended in a status the TARGET returned in the 5XX class, as a percentage. It refers to the
/// two reads by the ids above, which is how a math expression names the queries it reads.
const RATE_EXPRESSION: &str = "(errors / requests) * 100";

/// How long a row waits for work the clock owes — the invocation report the handler writes.
/// Generous on purpose: everything waited for happens in this process, and the deadline is
/// only here so a failure is a failure rather than a hang.
const BUDGET: Duration = Duration::from_secs(30);

/// How close two computed values have to be to count as the same number.
const TOLERANCE: f64 = 1e-9;

/// How far apart two timestamps may be and still name the same window: a period's datapoints
/// carry that period's own start, so a match is exact and this is only slack against the
/// seconds a timestamp is carried as.
const SAME_WINDOW: f64 = 0.5;

/// This service, Lambda, Amazon SQS, IAM, the token service, Amazon S3 and CloudWatch over ONE
/// shared engine, as `core/router/src/lib.rs` mounts them: the function the load balancer
/// forwards to is run by that Lambda as a session of the role IAM holds, the handler's own SDK
/// call is dispatched back into this router, the bucket the record is delivered into is the
/// bucket this suite reads, and the metric store the load balancer publishes into is the store
/// CloudWatch's own reader answers from.
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
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_cloudwatch_graniteserviceversion20100801::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    (router, engine)
}

/// One `MetricStat` query: one series, summed over each period of the window.
fn metric_query(id: &str, metric: &str, dimensions: &Value) -> Value {
    json!({
        "Id": id,
        "ReturnData": true,
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
}

/// Every query's own series, by query id: the `(timestamp, value)` pairs `GetMetricData`
/// answered for it, oldest first.
async fn read_series(
    router: &MultiServiceRouter,
    queries: &Value,
    start: f64,
    end: f64,
) -> BTreeMap<String, Vec<(f64, f64)>> {
    let body = cloudwatch_ok(
        router,
        "GetMetricData",
        &json!({
            "MetricDataQueries": queries,
            "StartTime": start,
            "EndTime": end,
            "ScanBy": "TimestampAscending",
        }),
    )
    .await;
    let mut read: BTreeMap<String, Vec<(f64, f64)>> = BTreeMap::new();
    for result in body["MetricDataResults"]
        .as_array()
        .cloned()
        .unwrap_or_default()
    {
        let id = result["Id"].as_str().unwrap_or_default().to_string();
        let timestamps = result["Timestamps"].as_array().cloned().unwrap_or_default();
        let values = result["Values"].as_array().cloned().unwrap_or_default();
        let points = read.entry(id).or_default();
        for (timestamp, value) in timestamps.iter().zip(values.iter()) {
            points.push((
                timestamp.as_f64().unwrap_or_default(),
                value.as_f64().unwrap_or_default(),
            ));
        }
    }
    read
}

/// The value `points` carries for the window `timestamp` names, if it carries one.
fn at(points: &[(f64, f64)], timestamp: f64) -> Option<f64> {
    points
        .iter()
        .find(|(stamp, _)| (stamp - timestamp).abs() < SAME_WINDOW)
        .map(|(_, value)| *value)
}

/// One request of the traffic, and what BOTH surfaces must report of it: the status the client
/// is answered with, the status the record reports the target answered — the function's own, or
/// the absent marker for an answer the load balancer generated — and the failure class the
/// record names for a generated answer.
#[derive(Debug)]
struct Row {
    case: &'static str,
    target: &'static str,
    status: u16,
    target_status: &'static str,
    error_reason: &'static str,
}

/// The traffic, in the order it is sent. The paths carry a PREFIX, as a client's own path does,
/// and the handler decides what to return from the last segment.
const TRAFFIC: &[Row] = &[
    Row {
        case: "a success the function answered",
        target: "/observed/health",
        status: 200,
        target_status: "200",
        error_reason: ABSENT,
    },
    Row {
        case: "a server error of the function's own, passed on as it answered it",
        target: "/observed/fail",
        status: 500,
        target_status: "500",
        error_reason: ABSENT,
    },
    Row {
        case: "another server error of the function's own",
        target: "/observed/busy",
        status: 503,
        target_status: "503",
        error_reason: ABSENT,
    },
    Row {
        case: "a run that raised, which the load balancer answers itself",
        target: "/observed/raise",
        status: 502,
        target_status: ABSENT,
        error_reason: "LambdaUnhandled",
    },
    Row {
        case: "a payload that is no response object at all",
        target: "/observed/nonjson",
        status: 502,
        target_status: ABSENT,
        error_reason: "LambdaInvalidResponse",
    },
];

/// How many requests the traffic makes, how many of them ended in a status the FUNCTION
/// returned in the 5XX class, and how many the load balancer answered itself for a failure of
/// the function's own making.
const REQUESTS: f64 = 5.0;
const TARGET_ERRORS: f64 = 2.0;
const GENERATED: f64 = 2.0;

/// One row of the series table: the series, whether it is read about the target group that
/// answered rather than about the load balancer alone, and the `Sum` the traffic must have left
/// in it — `0.0` meaning the series holds NO datapoint at all, which is what these series'
/// reporting criteria make of a count of nothing.
#[derive(Debug)]
struct SeriesRow {
    case: &'static str,
    metric: &'static str,
    /// Whether the series is keyed by the `TargetGroup` + `LoadBalancer` pair of the group that
    /// answered, rather than by the load balancer's own dimension alone.
    narrowed: bool,
    expected: f64,
}

/// The series table: every count the traffic above must have left, on both sides of the
/// attribution.
const SERIES: &[SeriesRow] = &[
    SeriesRow {
        case: "every request the load balancer answered is one request processed",
        metric: "RequestCount",
        narrowed: false,
        expected: REQUESTS,
    },
    SeriesRow {
        case: "and one for the target group it was forwarded to",
        metric: "RequestCount",
        narrowed: true,
        expected: REQUESTS,
    },
    SeriesRow {
        case: "the success the function answered, counted as the target's own class",
        metric: "HTTPCode_Target_2XX_Count",
        narrowed: true,
        expected: 1.0,
    },
    SeriesRow {
        case: "the server errors the function returned, narrowed to the group that answered",
        metric: "HTTPCode_Target_5XX_Count",
        narrowed: true,
        expected: TARGET_ERRORS,
    },
    SeriesRow {
        case: "the same count under the load balancer's own dimension",
        metric: "HTTPCode_Target_5XX_Count",
        narrowed: false,
        expected: TARGET_ERRORS,
    },
    SeriesRow {
        case: "the statuses the load balancer generated: its own class count",
        metric: "HTTPCode_ELB_5XX_Count",
        narrowed: false,
        expected: GENERATED,
    },
    SeriesRow {
        case: "and the series of that one status",
        metric: "HTTPCode_ELB_502_Count",
        narrowed: false,
        expected: GENERATED,
    },
    SeriesRow {
        case: "a status the LOAD BALANCER generated is in no target class count",
        metric: "HTTPCode_Target_4XX_Count",
        narrowed: true,
        expected: 0.0,
    },
    SeriesRow {
        case: "the failures of the function's own making are that target type's user error",
        metric: "LambdaUserError",
        narrowed: true,
        expected: GENERATED,
    },
    SeriesRow {
        case: "counted for the load balancer as well",
        metric: "LambdaUserError",
        narrowed: false,
        expected: GENERATED,
    },
    SeriesRow {
        case: "and none of them is a failure internal to the load balancer or to Lambda",
        metric: "LambdaInternalError",
        narrowed: true,
        expected: 0.0,
    },
];

/// RULE: a request answered through a `lambda` target group is reported the same way by the
/// record the load balancer delivers and by the series a client reads back — the function's own
/// status as the target's, a generated answer as the load balancer's own with the failure class
/// named, and no target address in either of the record's address fields.
#[tokio::test]
async fn one_lambda_target_request_is_reported_the_same_way_by_the_record_and_the_series() {
    let (router, engine) = emulator();
    log_bucket(&router, LOG_BUCKET).await;

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

    // The load balancer and the address it publishes, its `lambda` target group, the
    // registration of the function, the grant that admits the invocation, and the HTTP:80
    // listener that forwards to the group — every one of them through the owning API.
    let (lb_arn, published) = create_load_balancer(&router, LOAD_BALANCER).await;

    let mut form = action("CreateTargetGroup");
    form.push(p("Name", TARGET_GROUP));
    form.push(p("TargetType", "lambda"));
    let reply = call(&router, &form).await;
    assert_eq!(
        reply.status, 200,
        "setup CreateTargetGroup: expected 200, got {} body {}",
        reply.status, reply.body
    );
    let tg_arn = reply
        .text("TargetGroupArn")
        .unwrap_or_else(|| panic!("setup CreateTargetGroup: no TargetGroupArn in {}", reply.body));

    let mut form = action("RegisterTargets");
    form.push(p("TargetGroupArn", &tg_arn));
    form.push(p("Targets.member.1.Id", &function_arn));
    let reply = call(&router, &form).await;
    assert_eq!(
        reply.status, 200,
        "setup RegisterTargets: expected 200, got {} body {}",
        reply.status, reply.body
    );

    let (status, granted) = lambda(
        &router,
        "POST",
        &format!("/2015-03-31/functions/{FUNCTION}/policy"),
        json!({
            "StatementId": "elbv2-invoke",
            "Action": "lambda:InvokeFunction",
            "Principal": "elasticloadbalancing.amazonaws.com",
            "SourceArn": tg_arn,
        }),
    )
    .await;
    assert_eq!(
        status, 201,
        "setup AddPermission: expected 201, got {status} {granted}"
    );

    let mut form = action("CreateListener");
    form.push(p("LoadBalancerArn", &lb_arn));
    form.push(p("Protocol", "HTTP"));
    form.push(p("Port", "80"));
    form.push(p("DefaultActions.member.1.Type", "forward"));
    form.push(p("DefaultActions.member.1.TargetGroupArn", &tg_arn));
    let reply = call(&router, &form).await;
    assert_eq!(
        reply.status, 200,
        "setup CreateListener: expected 200, got {} body {}",
        reply.status, reply.body
    );

    // Access logging on, into the bucket and under the prefix the attributes name.
    let mut form = action("ModifyLoadBalancerAttributes");
    form.push(p("LoadBalancerArn", &lb_arn));
    form.push(p("Attributes.member.1.Key", ENABLED_ATTRIBUTE));
    form.push(p("Attributes.member.1.Value", "true"));
    form.push(p("Attributes.member.2.Key", BUCKET_ATTRIBUTE));
    form.push(p("Attributes.member.2.Value", LOG_BUCKET));
    form.push(p("Attributes.member.3.Key", PREFIX_ATTRIBUTE));
    form.push(p("Attributes.member.3.Value", LOG_PREFIX));
    let reply = call(&router, &form).await;
    assert_eq!(
        reply.status, 200,
        "setup ModifyLoadBalancerAttributes: expected 200, got {} body {}",
        reply.status, reply.body
    );

    // The move to `active` the clock owes the load balancer, placed rather than slept for.
    settle(&engine, &lb_arn, SystemTime::now() - Duration::from_secs(1));

    let traffic_start = now_secs();
    let mut seen: Vec<String> = Vec::new();
    for row in TRAFFIC {
        let answered = exchange(row.case, &published, row.target).await;
        assert_eq!(
            answered.status, row.status,
            "case {:?}: expected HTTP {}, got {} in {:?}",
            row.case, row.status, answered.status, answered.raw
        );

        // The function RAN, exactly once: the handler reports before it decides what to return.
        let mode = row
            .target
            .rsplit('/')
            .next()
            .expect("a target names a last segment");
        let reports = reported(&router, row.case, &queue_url, BUDGET).await;
        assert_eq!(
            reports,
            vec![mode.to_string()],
            "case {:?}: expected exactly one invocation reported as {mode:?}, got {reports:?}",
            row.case
        );

        // ... and the record the load balancer delivered for that very request, read where a
        // client reads it: out of the bucket, through Amazon S3's own API.
        let (key, line) = record_of(&router, LOG_BUCKET, row.case, &mut seen).await;
        assert!(
            key.starts_with(&format!("{LOG_PREFIX}/AWSLogs/")),
            "case {:?}: expected the record under the configured prefix, got {key:?}",
            row.case
        );
        let fields = fields(&line);
        let field = |index: usize| fields.get(index).cloned().unwrap_or_default();
        assert_eq!(
            field(ELB_STATUS_FIELD),
            row.status.to_string(),
            "case {:?}: expected elb_status_code {}, got {line:?}",
            row.case,
            row.status
        );
        assert_eq!(
            field(TARGET_STATUS_FIELD),
            row.target_status,
            "case {:?}: expected target_status_code {:?}, got {line:?}",
            row.case,
            row.target_status
        );
        assert_eq!(
            field(TARGET_STATUS_LIST_FIELD),
            row.target_status,
            "case {:?}: expected the target status list to repeat {:?}, got {line:?}",
            row.case,
            row.target_status
        );
        assert_eq!(
            field(ERROR_REASON_FIELD),
            row.error_reason,
            "case {:?}: expected error_reason {:?}, got {line:?}",
            row.case,
            row.error_reason
        );
        assert_eq!(
            field(TARGET_GROUP_FIELD),
            tg_arn,
            "case {:?}: expected the target group the request was forwarded to, got {line:?}",
            row.case
        );
        // A Lambda target is reached by INVOKING it: there is no address the request was sent
        // to, in either of the record's two address fields.
        assert_eq!(
            field(TARGET_ADDRESS_FIELD),
            ABSENT,
            "case {:?}: a lambda target has no target:port address, got {line:?}",
            row.case
        );
        assert_eq!(
            field(TARGET_ADDRESS_LIST_FIELD),
            ABSENT,
            "case {:?}: a lambda target names no address in the target list, got {line:?}",
            row.case
        );
    }

    // What a client's own read of the same traffic answers. The window spans the traffic with a
    // period either side of it, so a run that straddled a period boundary is read in full.
    let start = traffic_start - PERIOD;
    let end = now_secs() + PERIOD;
    let load_balancer = json!([
        { "Name": LOAD_BALANCER_DIMENSION, "Value": load_balancer_dimension(&lb_arn) },
    ]);
    let target_group = json!([
        { "Name": TARGET_GROUP_DIMENSION, "Value": target_group_dimension(&tg_arn) },
        { "Name": LOAD_BALANCER_DIMENSION, "Value": load_balancer_dimension(&lb_arn) },
    ]);
    let dimensions = |narrowed: bool| {
        if narrowed {
            target_group.clone()
        } else {
            load_balancer.clone()
        }
    };

    let mut queries: Vec<Value> = Vec::new();
    for (index, row) in SERIES.iter().enumerate() {
        queries.push(metric_query(
            &format!("s{index}"),
            row.metric,
            &dimensions(row.narrowed),
        ));
    }
    // The rate, and the two series it is derived FROM: read in the same call, so what the
    // expression returned can be checked against the very counts it read.
    queries.push(metric_query(
        ERRORS_ID,
        "HTTPCode_Target_5XX_Count",
        &target_group,
    ));
    queries.push(metric_query(REQUESTS_ID, "RequestCount", &target_group));
    queries.push(json!({
        "Id": RATE_ID,
        "Expression": RATE_EXPRESSION,
        "Label": "target 5xx rate",
        "ReturnData": true,
        "Period": PERIOD_SECS,
    }));
    let read = read_series(&router, &json!(queries), start, end).await;

    for (index, row) in SERIES.iter().enumerate() {
        let points = read.get(&format!("s{index}")).cloned().unwrap_or_default();
        let total: f64 = points.iter().map(|(_, value)| value).sum();
        let keyed_by = if row.narrowed {
            "the target group that answered"
        } else {
            "the load balancer alone"
        };
        assert!(
            (total - row.expected).abs() < TOLERANCE,
            "case {:?}: expected {} of {APPLICATION_NAMESPACE} keyed by {keyed_by} to sum to {}, \
             got {total} over {} datapoint(s)",
            row.case,
            row.metric,
            row.expected,
            points.len()
        );
        if row.expected.abs() < TOLERANCE {
            assert!(
                points.is_empty(),
                "case {:?}: a series no request gave a value must hold no datapoint, got {points:?}",
                row.case
            );
        }
    }

    // The expression derives from those very counts: every window it returned a rate for is the
    // share of that window's requests that ended in a status the target returned in the 5XX
    // class.
    let errors = read.get(ERRORS_ID).cloned().unwrap_or_default();
    let requests = read.get(REQUESTS_ID).cloned().unwrap_or_default();
    let rate = read.get(RATE_ID).cloned().unwrap_or_default();
    assert!(
        !rate.is_empty(),
        "case {RATE_EXPRESSION:?}: expected the traffic to derive a rate, got {rate:?} from \
         errors {errors:?} and requests {requests:?}"
    );
    for (timestamp, value) in &rate {
        // A window the traffic left no target 5XX in holds no datapoint of that series at all,
        // which the expression reads as a count of nothing: the share of such a window's
        // requests is zero.
        let errors_at = at(&errors, *timestamp).unwrap_or(0.0);
        let expected = match at(&requests, *timestamp) {
            Some(requests_at) if requests_at.abs() > TOLERANCE => errors_at / requests_at * 100.0,
            other => panic!(
                "case {RATE_EXPRESSION:?}: the rate at {timestamp} must derive from a request \
                 count of that window, got {other:?}"
            ),
        };
        assert!(
            (value - expected).abs() < TOLERANCE,
            "case {RATE_EXPRESSION:?}: expected the rate at {timestamp} to be {expected}, got \
             {value}"
        );
    }
    // And over the traffic as a whole: the two counts the rate reads are the traffic's own.
    let errors_total: f64 = errors.iter().map(|(_, value)| value).sum();
    let requests_total: f64 = requests.iter().map(|(_, value)| value).sum();
    assert!(
        (errors_total - TARGET_ERRORS).abs() < TOLERANCE
            && (requests_total - REQUESTS).abs() < TOLERANCE,
        "expected the rate to be derived from {TARGET_ERRORS} target 5XX of {REQUESTS} requests, \
         got {errors_total} of {requests_total}"
    );
}
