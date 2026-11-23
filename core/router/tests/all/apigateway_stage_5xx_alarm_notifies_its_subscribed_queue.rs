// SPDX-License-Identifier: CC0-1.0

//! The whole chain a client pages on a REST API's server errors with: the `5XXError` series a
//! deployed stage publishes, the alarm over it, and the alarm's SNS action reaching the queue
//! subscribed to the topic.
//!
//! The RULE under test, one level above the recorded case:
//!
//! > **The `AWS/ApiGateway` `5XXError` series a deployed REST API stage publishes about the
//! > requests it answered — one observation per answered request, 1 where the status is in the
//! > server-error class and 0 where it is not, under `ApiName` alone and under `ApiName` with
//! > `Stage` — drives an alarm over that series exactly as an ingested datapoint does: once the
//! > `Sum` over a graded period reaches the alarm's threshold the alarm leaves `OK` for `ALARM`,
//! > and that transition EXECUTES the alarm's `AlarmActions`, so the SNS topic one of them names
//! > publishes one notification — naming the alarm, carrying the state it left and the state it
//! > moved to, and a `Trigger` that names the watched series — to the SQS queue subscribed to
//! > it. A stage whose traffic lands in no other class than success and client error leaves the
//! > same alarm `OK` and its queue empty.**
//!
//! Five crates have to agree for that to hold — IAM holds the execution role, CloudFormation
//! archives the inline handler into a package this emulator can run, Lambda runs it, API Gateway
//! resolves the stage, relays the call and publishes what it answered, CloudWatch grades the
//! alarm over the store those datapoints landed in and publishes its action, SNS fans that
//! publish out and SQS holds what arrived — so the suite lives here, on the emulator as
//! `aws_emulator_router::build_router` assembles it.
//!
//! What the sibling suites pin is the halves: [`crate::apigateway_stage_request_metrics`] what a
//! request is OBSERVED as and the state an alarm over the series settles in,
//! [`crate::apigateway_rest_stage_metric_keying`] which dimension sets hold a REST api's traffic,
//! and [`crate::cloudwatch_alarm_action_reaches_its_subscribed_queue`] that an alarm FORCED into
//! `ALARM` reaches a subscribed queue. What only this suite reaches is the join: a transition no
//! caller forced, decided by traffic a stage really served through a Lambda integration, carrying
//! an action all the way to the queue a client reads its page off.
//!
//! The rows differ along what the rule generalises over: the dimension set the alarm watches (the
//! stage-scoped pair, and the api-wide set), the server-error STATUS the integration answers (two
//! different ones, because the CLASS is what counts), how many errors the threshold takes to cross
//! (one, and several), and a control row whose traffic is a success and a client error, which is
//! what tells an alarm that crossed on server errors from one that crosses on any traffic at all.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::cloudwatch_alarm_action_reaches_its_subscribed_queue::{
    authorization, cw, drain, drain_until, given_topic_with_subscriber, router, Grant,
};
use crate::query_wire::{form_encode, opt_element};

/// The Region every call is signed for, every ARN names and the apis' invoke hosts carry.
const REGION: &str = "us-east-1";

/// The account this emulator serves, which the invoke grant's source ARN names.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The namespace API Gateway publishes what its apis answered in, the dimensions a REST api's
/// series are keyed by, and the counter this suite alarms on.
const NAMESPACE: &str = "AWS/ApiGateway";
const API_NAME_DIMENSION: &str = "ApiName";
const STAGE_DIMENSION: &str = "Stage";
const SERVER_ERRORS: &str = "5XXError";

/// The stage every api below is deployed to, the greedy resource its one method is on, and the
/// verb that method is deployed under.
const STAGE: &str = "live";
const PROXY_PATH_PART: &str = "{proxy+}";
const VERB: &str = "ANY";

/// The function behind every method, the role it runs as, and the stack that archives its inline
/// handler into a deployment package.
const FUNCTION: &str = "xsvc-rest-5xx-alarm-proxy";
const EXECUTION_ROLE: &str = "xsvc-rest-5xx-alarm-exec";
const STACK: &str = "xsvc-rest-5xx-alarm-stack";
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// How long one invocation is given, as the function's own `Timeout`: well past what a run needs
/// on any host, because what is under test is the chain and not how fast an interpreter gets
/// through the handler.
const FUNCTION_TIMEOUT: u64 = 120;

/// How long a created function is given to report itself `Active`, and how often to look. The
/// span is Lambda's own, so the wait is on the CONDITION and the deadline is many times what the
/// transition needs, so a loaded host cannot decide the outcome.
const ACTIVE_DEADLINE: Duration = Duration::from_secs(120);
const LOOK: Duration = Duration::from_millis(50);

/// How long ONE request to a deployed stage is given to be answered. A request still open after
/// this was never going to be answered at all, so it is asserted rather than waited out.
const ANSWER_DEADLINE: Duration = Duration::from_secs(120);

/// How long an alarm is given to reach the state its stage's traffic implies, and how often to
/// look.
///
/// An evaluation grades the periods that have CLOSED, so a datapoint is graded by the evaluation
/// that follows the close of the period it landed in — up to one period after the traffic, plus
/// the span a transition the clock owes takes. The deadline is several of this alarm's periods
/// with room to spare for a machine under load; nothing is asserted about how long the crossing
/// took, and an alarm that never crosses fails on what the suite last read instead of hanging it.
const SETTLED_WITHIN: Duration = Duration::from_secs(240);
/// See [`SETTLED_WITHIN`].
const LOOK_AGAIN: Duration = Duration::from_millis(100);

/// The `Period` the alarms below are evaluated on, in seconds, and the period the series are read
/// at: the finest cadence a regular-resolution series can be graded at. `PutMetricAlarm` admits
/// sub-minute values ("Valid values are 10, 20, 30, and any multiple of 60") but only "for
/// metrics that are stored by a PutMetricData call with a StorageResolution of 1", and a stage's
/// own series carries no sub-minute detail at all.
const PERIOD: i64 = 60;

/// How far either side of the traffic a read of the series reaches, in periods — generous,
/// because the only thing that decides which window a datapoint falls in is the instant its
/// request was answered at, and the traffic takes as long as the machine takes.
const WINDOW_MARGIN_PERIODS: f64 = 3.0;

/// How long the suite keeps driving a row's burst until one period of the watched series really
/// holds what the row's threshold needs. See [`drive_until_a_period_breaches`].
const BREACH_DEADLINE: Duration = Duration::from_secs(120);

/// The two alarm states a row expects to read.
const OK: &str = "OK";
const ALARM: &str = "ALARM";

/// The handler behind every method: the path it was called for decides the `statusCode` of the
/// envelope it returns, so one package serves every row and every status a row asserts is one a
/// RUN returned — "API Gateway ... uses the Lambda function's output as the HTTP response body,
/// status code and headers" (API Gateway Developer Guide, *Lambda proxy integration*).
const PROXY_HANDLER: &str = r#"
def handler(event, context):
    path = event.get("path") or event.get("rawPath") or ""
    if path.endswith("/boom"):
        return {"statusCode": 500, "body": "LAMBDA-500"}
    if path.endswith("/unavailable"):
        return {"statusCode": 503, "body": "LAMBDA-503"}
    if path.endswith("/missing"):
        return {"statusCode": 404, "body": "LAMBDA-404"}
    return {"statusCode": 200, "body": "LAMBDA-200"}
"#;

/// The path whose answer is a success, which every row's stage serves once before its alarm is
/// read: that request is an explicit ZERO of the series the alarms watch, so the state they
/// settle in is one real data decided rather than one a missing-data arm supplied.
const SUCCESS_PATH: &str = "/ok";

/// One burst leg: what the client asks for within the stage, the status the handler's envelope
/// carries for it, and how many requests the leg drives.
struct Leg {
    asked_for: &'static str,
    status: u16,
    requests: usize,
}

/// One row: an api, the alarm over its traffic, and the burst its stage serves.
struct Row {
    case: &'static str,
    /// The api's NAME, which is the whole of its identity in these series — and, suffixed, the
    /// name of the alarm over it and of the topic and queue its action reaches.
    api: &'static str,
    /// Whether the alarm watches the STAGE-scoped dimension pair; otherwise the api-wide set.
    stage_scoped: bool,
    /// The alarm's `Threshold`: how much of the series' `Sum` over one period it takes to cross.
    threshold: f64,
    /// The traffic the stage serves after its alarm has settled.
    burst: &'static [Leg],
    /// Whether that traffic must carry the alarm into `ALARM` and execute its action.
    crosses: bool,
}

impl Row {
    /// The alarm over this row's series.
    fn alarm(&self) -> String {
        format!("{}-5xx", self.api)
    }

    /// The topic the alarm's action names, and the queue subscribed to it.
    fn sink(&self) -> String {
        format!("{}-sink", self.api)
    }

    /// The dimension set the alarm watches, as `PutMetricAlarm` takes it.
    fn dimensions(&self) -> Vec<(&'static str, String)> {
        let mut dimensions = vec![(API_NAME_DIMENSION, self.api.to_string())];
        if self.stage_scoped {
            dimensions.push((STAGE_DIMENSION, STAGE.to_string()));
        }
        dimensions
    }
}

/// The table.
static ROWS: [Row; 3] = [
    Row {
        case: "a stage-scoped alarm one server error already passes the threshold of",
        api: "paging-rest-api",
        stage_scoped: true,
        threshold: 1.0,
        burst: &[Leg {
            asked_for: "/boom",
            status: 500,
            requests: 2,
        }],
        crosses: true,
    },
    Row {
        case: "an api-wide alarm that takes several server errors of another status to cross",
        api: "fanning-rest-api",
        stage_scoped: false,
        threshold: 3.0,
        burst: &[Leg {
            asked_for: "/unavailable",
            status: 503,
            requests: 6,
        }],
        crosses: true,
    },
    Row {
        case: "a stage whose traffic lands in no server-error class at all",
        api: "calm-rest-api",
        stage_scoped: true,
        threshold: 1.0,
        burst: &[
            Leg {
                asked_for: SUCCESS_PATH,
                status: 200,
                requests: 2,
            },
            Leg {
                asked_for: "/missing",
                status: 404,
                requests: 3,
            },
        ],
        crosses: false,
    },
];

/// One answered request: its status and its body verbatim.
struct Answered {
    status: u16,
    body: Bytes,
}

impl Answered {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    fn summary(&self) -> String {
        format!("{} {}", self.status, self.text())
    }
}

/// Dispatch one request as it arrives on the wire.
async fn dispatch(
    router: &MultiServiceRouter,
    method: &str,
    host: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Bytes,
) -> Answered {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let response = router
        .dispatch(builder.body(body).expect("a well-formed request"))
        .await;
    let (parts, body) = response.into_parts();
    Answered {
        status: parts.status.as_u16(),
        body,
    }
}

/// One API Gateway control-plane call that has to be accepted with `expected`.
async fn apigw(
    router: &MultiServiceRouter,
    case: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
    expected: u16,
) -> Value {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let answered = dispatch(
        router,
        method,
        &format!("apigateway.{REGION}.amazonaws.com"),
        path,
        &[
            ("content-type", "application/json"),
            ("authorization", &authorization("apigateway", REGION)),
        ],
        Bytes::from(payload),
    )
    .await;
    assert_eq!(
        answered.status,
        expected,
        "setup {case:?}: expected HTTP {expected} from {method} {path}, got {}",
        answered.summary()
    );
    answered.json()
}

/// One Lambda control-plane call (`restJson1`).
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Answered {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    dispatch(
        router,
        method,
        &format!("lambda.{REGION}.amazonaws.com"),
        path,
        &[
            ("content-type", "application/json"),
            ("authorization", &authorization("lambda", REGION)),
        ],
        Bytes::from(payload),
    )
    .await
}

/// One `awsQuery` call of `service` that has to be accepted, answering its XML body.
async fn accepted_query(
    router: &MultiServiceRouter,
    service: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let answered = dispatch(
        router,
        "POST",
        &format!("{service}.{REGION}.amazonaws.com"),
        "/",
        &[
            ("content-type", "application/x-www-form-urlencoded"),
            ("authorization", &authorization(service, REGION)),
        ],
        Bytes::from(body),
    )
    .await;
    assert_eq!(
        answered.status,
        200,
        "setup {service} {action}: expected HTTP 200, got {}",
        answered.summary()
    );
    answered.text()
}

/// One unsigned request to a deployed stage's own invoke URL, as a client of a method whose
/// `authorizationType` is `NONE` makes it. The request must be ANSWERED.
async fn invoke(router: &MultiServiceRouter, case: &str, api: &str, asked_for: &str) -> Answered {
    let host = format!("{api}.execute-api.{REGION}.amazonaws.com");
    let path = format!("/{STAGE}{asked_for}");
    ::tokio::time::timeout(
        ANSWER_DEADLINE,
        dispatch(router, "GET", &host, &path, &[], Bytes::new()),
    )
    .await
    .unwrap_or_else(|_| {
        panic!(
            "case {case:?}: GET {path} must be answered by the stage within {ANSWER_DEADLINE:?}, \
             got no HTTP response at all"
        )
    })
}

/// A required, non-empty string member of a response body.
fn as_str(what: &str, body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| panic!("setup {what:?}: expected a non-empty {member:?}, got {body}"))
}

/// Wall-clock now as epoch seconds, the clock the read windows are measured against.
fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |since| since.as_secs_f64())
}

/// The execution role the function runs as, answering the ARN IAM minted for it.
async fn execution_role(router: &MultiServiceRouter) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "lambda.amazonaws.com" },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let created = accepted_query(
        router,
        "iam",
        "2010-05-08",
        "CreateRole",
        &[
            ("RoleName", EXECUTION_ROLE),
            ("AssumeRolePolicyDocument", &trust),
        ],
    )
    .await;
    opt_element(&created, "Arn")
        .unwrap_or_else(|| panic!("setup CreateRole({EXECUTION_ROLE:?}): expected the role's ARN"))
}

/// Deploy the stack declaring the function: the inline source is archived into a deployment
/// package by CloudFormation, which is how a function whose handler is written here comes to
/// exist at all. Answers the ARN Lambda reports for it, once it reports it `Active`.
async fn deployed_function(router: &MultiServiceRouter, role_arn: &str) -> String {
    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Proxy": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": FUNCTION,
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": role_arn,
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": { "ZipFile": PROXY_HANDLER },
                },
            },
        },
    })
    .to_string();
    accepted_query(
        router,
        "cloudformation",
        "2010-05-15",
        "CreateStack",
        &[("StackName", STACK), ("TemplateBody", &template)],
    )
    .await;
    let (reached, described) = wait_for_stack_status(router, STACK, CREATE_COMPLETE).await;
    assert_eq!(
        reached, CREATE_COMPLETE,
        "setup CreateStack: expected the stack to reach {CREATE_COMPLETE}, got {described}"
    );

    // The invoke each integration makes needs the function RUNNABLE, which is a transition its
    // own service owes: look until it reports it rather than sleeping a span on its behalf.
    let deadline = Instant::now() + ACTIVE_DEADLINE;
    let mut configuration;
    loop {
        configuration = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{FUNCTION}/configuration"),
            None,
        )
        .await
        .json();
        if configuration.get("State").and_then(Value::as_str) == Some("Active")
            || Instant::now() >= deadline
        {
            break;
        }
        ::tokio::time::sleep(LOOK).await;
    }
    assert_eq!(
        configuration.get("State").and_then(Value::as_str),
        Some("Active"),
        "setup GetFunctionConfiguration: expected Lambda to report {FUNCTION:?} Active, got \
         {configuration}"
    );
    as_str("GetFunctionConfiguration", &configuration, "FunctionArn")
}

/// A REGIONAL REST api named `name` whose greedy proxy resource carries one open `ANY` method run
/// by `function_arn` through an `AWS_PROXY` integration, deployed to [`STAGE`]. Answers the api's
/// id, which its invoke host is addressed by.
async fn deployed_api(router: &MultiServiceRouter, name: &str, function_arn: &str) -> String {
    let created = apigw(
        router,
        name,
        "POST",
        "/restapis",
        Some(json!({
            "name": name,
            "endpointConfiguration": { "types": ["REGIONAL"] },
        })),
        201,
    )
    .await;
    let api = as_str(name, &created, "id");
    let root = as_str(name, &created, "rootResourceId");

    let child = apigw(
        router,
        name,
        "POST",
        &format!("/restapis/{api}/resources/{root}"),
        Some(json!({ "pathPart": PROXY_PATH_PART })),
        201,
    )
    .await;
    let proxy = as_str(name, &child, "id");

    let method = format!("/restapis/{api}/resources/{proxy}/methods/{VERB}");
    apigw(
        router,
        name,
        "PUT",
        &method,
        Some(json!({ "authorizationType": "NONE" })),
        201,
    )
    .await;
    apigw(
        router,
        name,
        "PUT",
        &format!("{method}/integration"),
        Some(json!({
            "type": "AWS_PROXY",
            // `integrationHttpMethod` travels under its `jsonName`, which is what
            // `smithy_query(query="operation",operation="PutIntegration")` reports.
            "httpMethod": "POST",
            "uri": format!(
                "arn:aws:apigateway:{REGION}:lambda:path/2015-03-31/functions/{function_arn}/\
                 invocations"
            ),
        })),
        201,
    )
    .await;

    // "Grants a principal permission to use a function" (`AddPermission`): the grant that admits
    // the invocation API Gateway makes as its own service principal.
    let granted = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{FUNCTION}/policy"),
        Some(json!({
            "StatementId": format!("apigateway-invoke-{api}"),
            "Action": "lambda:InvokeFunction",
            "Principal": "apigateway.amazonaws.com",
            "SourceArn": format!("arn:aws:execute-api:{REGION}:{ACCOUNT}:{api}/*"),
        })),
    )
    .await;
    assert_eq!(
        granted.status,
        201,
        "setup AddPermission({name:?}): expected HTTP 201 from Lambda, got {}",
        granted.summary()
    );

    apigw(
        router,
        name,
        "POST",
        &format!("/restapis/{api}/deployments"),
        Some(json!({ "stageName": STAGE })),
        201,
    )
    .await;
    api
}

/// `PutMetricAlarm` over `row`'s series, with `topic` as the one action the crossing executes —
/// the alarm a client that pages on a REST api's 5xx rate writes.
async fn alarm_over_server_errors(router: &MultiServiceRouter, row: &Row, topic: &str) {
    let dimensions: Vec<Value> = row
        .dimensions()
        .into_iter()
        .map(|(name, value)| json!({ "Name": name, "Value": value }))
        .collect();
    cw(
        router,
        row.case,
        REGION,
        "PutMetricAlarm",
        &json!({
            "AlarmName": row.alarm(),
            "Namespace": NAMESPACE,
            "MetricName": SERVER_ERRORS,
            "Dimensions": dimensions,
            "Statistic": "Sum",
            "Period": PERIOD,
            "EvaluationPeriods": 1,
            "Threshold": row.threshold,
            "ComparisonOperator": "GreaterThanOrEqualToThreshold",
            // "the alarm does not consider missing data points when evaluating whether to change
            // state" is the `missing` default; a counter published as an explicit zero for every
            // request outside its class means a window with no datapoint is a window with no
            // traffic, which is not a breach (`PutMetricAlarmInput$TreatMissingData`).
            "TreatMissingData": "notBreaching",
            "AlarmActions": [topic],
        }),
    )
    .await;
}

/// The one `MetricAlarms` entry `DescribeAlarms` reports for `name`.
async fn described_alarm(router: &MultiServiceRouter, case: &str, name: &str) -> Value {
    let answered = cw(
        router,
        case,
        REGION,
        "DescribeAlarms",
        &json!({ "AlarmNames": [name] }),
    )
    .await;
    answered["MetricAlarms"][0].clone()
}

/// The `StateValue` `DescribeAlarms` reports for `name`.
async fn alarm_state(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    described_alarm(router, case, name).await["StateValue"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// `name`'s state once it is `wanted` or the deadline is up, so the suite asserts on what it last
/// read rather than hanging.
async fn awaited_state(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    wanted: &str,
) -> String {
    let deadline = Instant::now() + SETTLED_WITHIN;
    loop {
        let state = alarm_state(router, case, name).await;
        if state == wanted || Instant::now() >= deadline {
            return state;
        }
        ::tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Whether `name`'s history records a `StateUpdate` out of [`OK`] into [`ALARM`].
async fn crossed_out_of_ok(router: &MultiServiceRouter, case: &str, name: &str) -> bool {
    let answered = cw(
        router,
        case,
        REGION,
        "DescribeAlarmHistory",
        &json!({ "AlarmName": name, "HistoryItemType": "StateUpdate" }),
    )
    .await;
    answered["AlarmHistoryItems"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|item| {
            item["HistoryData"]
                .as_str()
                .and_then(|data| serde_json::from_str::<Value>(data).ok())
        })
        .any(|transition| {
            let reported = |member: &str| {
                transition
                    .pointer(&format!("/{member}/stateValue"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            };
            reported("oldState").as_deref() == Some(OK)
                && reported("newState").as_deref() == Some(ALARM)
        })
}

/// The largest `Sum` any one period of `row`'s watched series holds over the whole window, read
/// through CloudWatch's own reader — which is the number the alarm's threshold is compared
/// against.
async fn widest_period_sum(router: &MultiServiceRouter, row: &Row, since: f64) -> f64 {
    let margin = WINDOW_MARGIN_PERIODS * PERIOD as f64;
    let dimensions: Vec<Value> = row
        .dimensions()
        .into_iter()
        .map(|(name, value)| json!({ "Name": name, "Value": value }))
        .collect();
    let answered = cw(
        router,
        row.case,
        REGION,
        "GetMetricStatistics",
        &json!({
            "Namespace": NAMESPACE,
            "MetricName": SERVER_ERRORS,
            "Dimensions": dimensions,
            "StartTime": since - margin,
            "EndTime": now_secs() + margin,
            "Period": PERIOD,
            "Statistics": ["Sum"],
        }),
    )
    .await;
    answered["Datapoints"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|datapoint| datapoint["Sum"].as_f64())
        .fold(0.0, f64::max)
}

/// Drive `row`'s burst once at `api`, asserting each request is answered with the status its leg
/// expects — so a row that counts N requests of a class really made N answered requests of it.
async fn drive(router: &MultiServiceRouter, row: &Row, api: &str) {
    for leg in row.burst {
        for ordinal in 1..=leg.requests {
            let answered = invoke(router, row.case, api, leg.asked_for).await;
            assert_eq!(
                answered.status,
                leg.status,
                "case {:?} request {ordinal} of {} to {}: expected HTTP {}, got {}",
                row.case,
                leg.requests,
                leg.asked_for,
                leg.status,
                answered.summary()
            );
        }
    }
}

/// Drive `row`'s burst until ONE period of the series its alarm watches holds at least the
/// alarm's threshold.
///
/// An evaluation grades ONE period's value against the threshold, and a burst is answered in
/// milliseconds, so its requests normally all land in the same period. A burst that straddled a
/// period boundary would leave each of two periods holding only part of it, which is a property
/// of when the suite happened to run and not of the rule — so the burst is driven again until a
/// period really holds what the alarm needs, read back through CloudWatch's own reader. The wait
/// is on that CONDITION, bounded by [`BREACH_DEADLINE`].
async fn drive_until_a_period_breaches(router: &MultiServiceRouter, row: &Row, api: &str, since: f64) {
    let deadline = Instant::now() + BREACH_DEADLINE;
    loop {
        drive(router, row, api).await;
        let held = widest_period_sum(router, row, since).await;
        if held >= row.threshold {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {:?}: the burst must leave one period of the {SERVER_ERRORS} series holding at \
             least the alarm's threshold {}, the widest period holds {held}",
            row.case,
            row.threshold
        );
    }
}

#[tokio::test]
async fn a_rest_stages_server_errors_cross_their_alarm_and_its_action_reaches_the_queue() {
    let router = router();
    let role_arn = execution_role(&router).await;
    let function_arn = deployed_function(&router, &role_arn).await;

    // Every row's api, the alarm over its series, and the topic-with-subscriber its one action
    // names. The queue grants SNS the send scoped to the publishing topic, which is the grant a
    // client writes for a notification sink of one topic.
    let mut wired: Vec<(&Row, String, String, String)> = Vec::with_capacity(ROWS.len());
    for row in &ROWS {
        let api = deployed_api(&router, row.api, &function_arn).await;
        let (topic, queue) = given_topic_with_subscriber(
            &router,
            row.case,
            REGION,
            &row.sink(),
            Grant::ScopedToTopic,
        )
        .await;
        alarm_over_server_errors(&router, row, &topic).await;
        wired.push((row, api, topic, queue));
    }

    // One success per stage first: that request is an explicit zero of the watched series, so
    // every alarm settles in OK on data its stage really published, and the crossing each row
    // then asserts is a transition OUT OF OK.
    let since = now_secs();
    for (row, api, _, _) in &wired {
        let answered = invoke(&router, row.case, api, SUCCESS_PATH).await;
        assert_eq!(
            answered.status,
            200,
            "case {:?}: expected the handler's 200 from {SUCCESS_PATH}, got {}",
            row.case,
            answered.summary()
        );
    }
    for (row, _, _, _) in &wired {
        let state = awaited_state(&router, row.case, &row.alarm(), OK).await;
        assert_eq!(
            state, OK,
            "case {:?}: an alarm over a stage that has served no server error settles {OK}, got \
             {state:?}",
            row.case
        );
    }

    // Then every row's burst, so the waits below overlap rather than running end to end.
    for (row, api, _, _) in &wired {
        if row.crosses {
            drive_until_a_period_breaches(&router, row, api, since).await;
        } else {
            drive(&router, row, api).await;
        }
    }

    // The rows whose traffic breached: the alarm crosses on its own evaluation, records the
    // transition out of OK, and its action reaches the subscribed queue ONCE.
    for (row, _, topic, queue) in wired.iter().filter(|(row, _, _, _)| row.crosses) {
        let alarm = row.alarm();
        let state = awaited_state(&router, row.case, &alarm, ALARM).await;
        assert_eq!(
            state, ALARM,
            "case {:?}: the stage served server errors past the threshold {}, so its alarm must \
             report {ALARM}, got {state:?} with {}",
            row.case,
            row.threshold,
            described_alarm(&router, row.case, &alarm).await
        );
        assert!(
            crossed_out_of_ok(&router, row.case, &alarm).await,
            "case {:?}: the alarm must record a StateUpdate out of {OK} into {ALARM}",
            row.case
        );

        let delivered = drain_until(&router, row.case, REGION, queue, 1).await;
        assert_eq!(
            delivered.len(),
            1,
            "case {:?}: the crossing must publish exactly one notification to {topic}, which the \
             subscribed queue receives, got {delivered:?}",
            row.case
        );
        let notification = &delivered[0];
        assert_eq!(
            notification["AlarmName"].as_str(),
            Some(alarm.as_str()),
            "case {:?}: the notification must name the alarm, got {notification}",
            row.case
        );
        assert_eq!(
            notification["OldStateValue"].as_str(),
            Some(OK),
            "case {:?}: the notification must carry the state the alarm left, got {notification}",
            row.case
        );
        assert_eq!(
            notification["NewStateValue"].as_str(),
            Some(ALARM),
            "case {:?}: the notification must carry the state the alarm moved to, got \
             {notification}",
            row.case
        );
        assert_eq!(
            notification["Trigger"]["Namespace"].as_str(),
            Some(NAMESPACE),
            "case {:?}: the trigger must name the namespace the series lives in, got \
             {notification}",
            row.case
        );
        assert_eq!(
            notification["Trigger"]["MetricName"].as_str(),
            Some(SERVER_ERRORS),
            "case {:?}: the trigger must name the watched counter, got {notification}",
            row.case
        );
        let triggered_by: Vec<(String, String)> = notification["Trigger"]["Dimensions"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|dimension| {
                (
                    dimension["name"]
                        .as_str()
                        .or_else(|| dimension["Name"].as_str())
                        .unwrap_or_default()
                        .to_string(),
                    dimension["value"]
                        .as_str()
                        .or_else(|| dimension["Value"].as_str())
                        .unwrap_or_default()
                        .to_string(),
                )
            })
            .collect();
        let expected: Vec<(String, String)> = row
            .dimensions()
            .into_iter()
            .map(|(name, value)| (name.to_string(), value))
            .collect();
        assert_eq!(
            triggered_by, expected,
            "case {:?}: the trigger must name the dimension set the alarm watches, got \
             {notification}",
            row.case
        );

        // One transition, one notification: the evaluations that follow it keep the alarm in
        // ALARM and publish nothing more.
        let again = drain(&router, row.case, REGION, queue).await;
        assert!(
            again.is_empty(),
            "case {:?}: one transition owes ONE notification, got {again:?} after it",
            row.case
        );
    }

    // The control rows, asserted after a crossing row's notification has really arrived: the
    // chain is live, so a queue that holds nothing holds nothing because its alarm never crossed.
    for (row, _, _, queue) in wired.iter().filter(|(row, _, _, _)| !row.crosses) {
        let state = alarm_state(&router, row.case, &row.alarm()).await;
        assert_eq!(
            state, OK,
            "case {:?}: traffic in no server-error class must leave the alarm {OK}, got {state:?}",
            row.case
        );
        let delivered = drain(&router, row.case, REGION, queue).await;
        assert!(
            delivered.is_empty(),
            "case {:?}: an alarm that never crossed must deliver nothing, got {delivered:?}",
            row.case
        );
    }
}
