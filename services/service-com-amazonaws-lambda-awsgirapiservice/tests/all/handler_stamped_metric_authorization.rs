// SPDX-License-Identifier: CC0-1.0

//! The ANSWER a handler that STAMPS its own datapoint gets: Lambda, Amazon CloudWatch, IAM and
//! the token service on ONE engine, a `python3.x` function whose handler publishes a datapoint
//! with a native date-time on `MetricDatum.Timestamp`, first under an execution role that is
//! granted nothing on CloudWatch and then under one that is.
//!
//! The RULE pinned here: the AWS SDK a managed runtime ships marshals a native date-time given
//! for an input member the called service's model types as a timestamp into the form that
//! service's protocol carries a timestamp in and DISPATCHES the call, so what the handler is
//! handed is the serving service's own answer — the work done, which a later read of that
//! service shows, or that service's own authorization refusal naming the action it judged —
//! and never a client-side refusal of the date-time itself.
//!
//! The rows differ along what the rule generalises over: the FORM of the date-time the handler
//! puts on the member (one carrying no zone, as `datetime.utcnow()` answers with; one carrying
//! UTC; one at a negative and one at a positive zone offset) and whether the policies in force
//! allow the publish. Both halves of every row are asserted — what the SDK handed the guest,
//! which the invocation's payload carries, and what CloudWatch itself holds for that series
//! afterwards, read where a client reads it — so a publish that was refused by the SDK before
//! it was dispatched, a refusal that still stored a datapoint, or a served publish stored at
//! some other instant than the one the handler named all fail here.
//!
//! The suite RUNS a handler, so it takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]), and it makes TWO
//! invocations in all — every row's publish under the narrow grant, and every row's publish
//! under the wider one — because an invocation of its own costs this target a share of the run
//! budget every other invocation suite draws on.

use std::sync::Arc;
use std::time::Instant;

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The IAM calls and the statement writers every suite in this target writes a role with, owned
// by the suites that first needed them rather than copied.
use crate::event_source_mapping_role_access::{allow, given_role, iam};
// The Lambda calls, the activation wait and the poll budget every suite makes them with.
use crate::event_source_mappings::{
    await_function_active, call, router_on, CLOCK_BUDGET, LOOK_AGAIN,
};
// The ARNs of the role a function runs as and of the session its own calls are made under.
use crate::handler_s3_object_access::{role_arn, session_arn};
// The whole-minute window every metric read of this target is laid from.
use crate::handler_metric_timestamp::window_start;
// The CloudWatch call this target reads a series with.
use crate::kinesis_event_source_delivery::cloudwatch;
// The log-write actions a function's own delivery needs, so the narrow grant is narrow in
// CloudWatch alone.
use crate::invocation_log_delivery::{LOGS, LOG_WRITES};
// The one .zip writer this target packages a running handler with.
use crate::synchronous_invoke_run::package;

/// Where `CreateFunction` and `Invoke` are bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The execution role the function runs as, the function's own name, and the policy name the
/// wider grant is written under.
const ROLE_NAME: &str = "lambda-stamped-metric";
const FUNCTION: &str = "stamped-metric";
const WIDER_POLICY: &str = "metric-publication";

/// The series every row publishes into: a custom namespace, so nothing here touches a reserved
/// `AWS/` one ("you should not specify a namespace that begins with `AWS/`",
/// `PutMetricDataInput$Namespace`).
const NAMESPACE: &str = "Emulator/HandlerStampedMetric";

/// The IAM action namespace CloudWatch's own calls are named in: "Actions defined by Amazon
/// CloudWatch" are published under this prefix, which is not the name the API is signed under
/// (`monitoring`).
const CLOUDWATCH: &str = "cloudwatch";

/// The action one publish asks for.
const PUBLISH_ACTION: &str = "PutMetricData";

/// The error code and status CloudWatch refuses a call the policies in force do not allow
/// with: its model declares no denial shape, so the refusal is the service-common one.
const DENIAL_CODE: &str = "AccessDenied";
const DENIAL_STATUS: u16 = 403;

/// The resource CloudWatch judges an action its service-authorization reference lists under
/// "actions with no resource types" against: every resource.
const EVERY_RESOURCE: &str = "*";

/// One minute, in seconds — the granularity every read below is answered at
/// (`GetMetricStatisticsInput$Period`: "The granularity, in seconds … must be a multiple of
/// 60").
const MINUTE: f64 = 60.0;

/// That same minute as `Period` carries it, which the model types as an integer.
const PERIOD: i64 = 60;

/// The `Timeout` the function carries, in seconds.
///
/// It bounds the WALL CLOCK one run may spend, and the handler here returns as soon as its
/// calls are answered, so what it has to cover is not its work but the host's own scheduling:
/// this target runs its suites in parallel over the emulator's run slots, so a run can sit
/// mid-flight while other suites' runs hold the CPU.
const FUNCTION_TIMEOUT_SECONDS: i64 = 25;

/// Lambda, Amazon CloudWatch, IAM and the token service over ONE shared engine, as
/// `core/router/src/lib.rs` mounts them: the handler's SDK calls are dispatched back into this
/// same router, so the series it publishes into is the series this suite reads. The token
/// service is mounted because the session a run is signed as is minted — and recorded — there,
/// and that record is the principal an authorization decision is about.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = router_on(&engine);
    router.register_service(
        service_com_amazonaws_cloudwatch_graniteserviceversion20100801::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(engine),
    );
    router
}

/// The refusal the publish carries while no readable identity-based document allows it, spelled
/// out as AWS words one: the principal that signed, the action it asked for, the resource it
/// addressed, and the fact that no identity-based policy allows that action.
fn denial_sentence() -> String {
    format!(
        "User: {} is not authorized to perform: {CLOUDWATCH}:{PUBLISH_ACTION} on resource: \
         {EVERY_RESOURCE} because no identity-based policy allows the \
         {CLOUDWATCH}:{PUBLISH_ACTION} action",
        session_arn(ROLE_NAME, FUNCTION)
    )
}

/// The handler every invocation runs: for each publish the event names it builds the instant in
/// the FORM the row names, puts it on `MetricDatum.Timestamp` and reports what the SDK gave it.
///
/// A refusal the serving service answered is reported as its code, its message and its status,
/// and anything else the SDK raised is reported under `refused` — which is how a client-side
/// refusal of the value, were there one, would reach this suite.
const HANDLER_SOURCE: &str = r#"
import datetime

import boto3
import botocore.exceptions

EPOCH = datetime.datetime(1970, 1, 1)


def stamp(asked):
    """The instant the row names, in the form the row names it in."""
    moment = EPOCH + datetime.timedelta(seconds=asked["seconds"])
    form = asked["form"]
    if form == "naive":
        return moment
    moment = moment.replace(tzinfo=datetime.timezone.utc)
    if form == "utc":
        return moment
    return moment.astimezone(
        datetime.timezone(datetime.timedelta(minutes=asked["offset_minutes"]))
    )


def lambda_handler(event, context):
    client = boto3.client("cloudwatch")
    results = []
    for asked in event["publishes"]:
        datum = {
            "MetricName": asked["metric"],
            "Value": asked["value"],
            "Unit": "Count",
            "Timestamp": stamp(asked),
        }
        try:
            answered = client.put_metric_data(
                Namespace=event["namespace"], MetricData=[datum]
            )
        except botocore.exceptions.ClientError as error:
            results.append(
                {
                    "ok": False,
                    "code": error.response["Error"]["Code"],
                    "message": error.response["Error"].get("Message"),
                    "status": error.response["ResponseMetadata"]["HTTPStatusCode"],
                }
            )
            continue
        except Exception as error:
            results.append(
                {"ok": False, "refused": "%s: %s" % (type(error).__name__, error)}
            )
            continue
        results.append(
            {"ok": True, "status": answered["ResponseMetadata"]["HTTPStatusCode"]}
        )
    return {"results": results}
"#;

/// One row: the form the handler builds the instant in, where that instant falls, and the value
/// and series of the datapoint it stamps with it.
struct Row {
    case: &'static str,
    /// The form the handler builds the value in.
    form: &'static str,
    /// How far after the window's first minute the instant the handler names falls.
    seconds: f64,
    /// The zone offset, in minutes, the `offset` form expresses that instant at.
    offset_minutes: i64,
    /// The series this row publishes into, so every row's datapoint is read on its own.
    metric: &'static str,
    /// The datapoint's value, distinct per row so the series pins which row wrote it.
    value: f64,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a date-time carrying no zone, the form datetime.utcnow() answers with",
            form: "naive",
            seconds: 0.0,
            offset_minutes: 0,
            metric: "StampedNaive",
            value: 1.0,
        },
        Row {
            case: "a date-time carrying UTC",
            form: "utc",
            seconds: 61.0,
            offset_minutes: 0,
            metric: "StampedUtc",
            value: 2.0,
        },
        Row {
            case: "a date-time at a negative zone offset",
            form: "offset",
            seconds: 122.0,
            offset_minutes: -450,
            metric: "StampedBehindUtc",
            value: 3.0,
        },
        Row {
            case: "a date-time at a positive zone offset",
            form: "offset",
            seconds: 183.0,
            offset_minutes: 330,
            metric: "StampedAheadOfUtc",
            value: 4.0,
        },
    ]
}

/// Make every row's publish in ONE invocation and answer the result the SDK gave the guest for
/// each.
async fn publishes_made(
    router: &MultiServiceRouter,
    label: &str,
    rows: &[Row],
    start: f64,
) -> Vec<Value> {
    let asked: Vec<Value> = rows
        .iter()
        .map(|row| {
            json!({
                "metric": row.metric,
                "value": row.value,
                "form": row.form,
                "seconds": start + row.seconds,
                "offset_minutes": row.offset_minutes,
            })
        })
        .collect();
    let answered = call(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{FUNCTION}/invocations"),
        &json!({ "namespace": NAMESPACE, "publishes": asked }),
    )
    .await;
    assert_eq!(
        answered.status,
        200,
        "case {label:?}: expected HTTP 200 from Invoke, got {} ({})",
        answered.status,
        answered.text()
    );
    let signal = answered.header("x-amz-function-error");
    assert_eq!(
        signal, None,
        "case {label:?}: expected no FunctionError, got {signal:?} with {}",
        answered.text()
    );
    let payload = answered.json();
    let results = payload["results"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        results.len(),
        rows.len(),
        "case {label:?}: expected one result per publish, got {payload}"
    );
    results
}

/// The `(timestamp, sum)` of every datapoint `metric` holds, read at one-minute granularity
/// through CloudWatch's own API over the whole window this suite lays.
async fn datapoints(
    router: &MultiServiceRouter,
    case: &str,
    metric: &str,
    start: f64,
) -> Vec<(f64, f64)> {
    let (status, answered) = cloudwatch(
        router,
        "GetMetricStatistics",
        &json!({
            "Namespace": NAMESPACE,
            "MetricName": metric,
            "StartTime": start,
            // Thirty minutes back to ten minutes on: wide enough to hold both the instants the
            // rows name and whatever instant a service's own clock would stamp.
            "EndTime": start + 40.0 * MINUTE,
            "Period": PERIOD,
            "Statistics": ["Sum"],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from GetMetricStatistics, got {status} ({answered})"
    );
    let mut held: Vec<(f64, f64)> = answered["Datapoints"]
        .as_array()
        .map(|listed| {
            listed
                .iter()
                .map(|point| {
                    (
                        point["Timestamp"].as_f64().unwrap_or(f64::NAN),
                        point["Sum"].as_f64().unwrap_or(f64::NAN),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    held.sort_unstable_by(|one, other| one.0.total_cmp(&other.0));
    held
}

/// The first instant of the minute the instant `row` names falls in, which is the minute a
/// one-minute read reports its datapoint at.
fn minute_of(start: f64, row: &Row) -> f64 {
    ((start + row.seconds) / MINUTE).floor() * MINUTE
}

/// The result of one publish carries the serving service's own answer and nothing the SDK
/// decided for itself.
fn the_sdk_dispatched_it(case: &str, result: &Value) {
    assert!(
        result["refused"].is_null(),
        "case {case:?}: expected the date-time on the timestamp member to be marshalled and the \
         call dispatched, got the SDK's own refusal in {result}"
    );
}

/// RULE: a native date-time a handler puts on an input member the called service's model types
/// as a timestamp is marshalled into that service's protocol's timestamp form and the call is
/// dispatched, so the handler is handed the service's own answer — its authorization refusal,
/// naming the action, while the policies in force do not allow the publish, and the stored
/// datapoint, at the instant the handler named, once they do.
#[tokio::test]
async fn a_stamped_datapoint_is_answered_by_the_service_that_serves_it() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let router = router();
    let start = window_start();
    let rows = rows();

    // The role the function runs as, written with IAM's own operations so the session the
    // runtime mints speaks for the record `iam:CreateRole` wrote. It carries the three log
    // writes a function's own delivery needs and nothing on CloudWatch.
    let created_role = given_role(
        &router,
        "setup",
        ROLE_NAME,
        Some(json!([allow(LOGS, &LOG_WRITES, EVERY_RESOURCE)])),
    )
    .await;
    assert_eq!(
        created_role,
        role_arn(ROLE_NAME),
        "setup: expected the role ARN of this account, got {created_role}"
    );

    // The function itself, running as that role.
    let created = call(
        &router,
        "POST",
        FUNCTIONS_URI,
        &json!({
            "FunctionName": FUNCTION,
            "Role": role_arn(ROLE_NAME),
            "Runtime": "python3.12",
            "Handler": "lambda_function.lambda_handler",
            "Timeout": FUNCTION_TIMEOUT_SECONDS,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(HANDLER_SOURCE)),
            },
        }),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "setup: expected HTTP 201 from CreateFunction, got {} ({})",
        created.status,
        created.text()
    );
    await_function_active(&router, "setup", FUNCTION).await;

    // The first invocation: every row's publish, with nothing on CloudWatch allowed.
    let refused = publishes_made(&router, "the publishes outside the grant", &rows, start).await;
    for (row, result) in rows.iter().zip(&refused) {
        let case = row.case;
        the_sdk_dispatched_it(case, result);
        assert_eq!(
            result["ok"],
            json!(false),
            "case {case:?}: expected the publish to be refused by CloudWatch, got {result}"
        );
        assert_eq!(
            result["code"],
            json!(DENIAL_CODE),
            "case {case:?}: expected the error code {DENIAL_CODE:?}, got {result}"
        );
        assert_eq!(
            result["status"],
            json!(DENIAL_STATUS),
            "case {case:?}: expected HTTP {DENIAL_STATUS} on the refusal, got {result}"
        );
        assert_eq!(
            result["message"],
            json!(denial_sentence()),
            "case {case:?}: expected the refusal to name the session, the publish action and the \
             resource, got {result}"
        );
    }

    // The other half of every refused row: nothing was published.
    for row in &rows {
        let case = row.case;
        let held = datapoints(&router, case, row.metric, start).await;
        assert!(
            held.is_empty(),
            "case {case:?}: expected the refused publish to leave the series empty, got {held:?}"
        );
    }

    // The wider grant, written where a client writes one.
    let document = json!({
        "Version": "2012-10-17",
        "Statement": [allow(CLOUDWATCH, &[PUBLISH_ACTION], EVERY_RESOURCE)],
    })
    .to_string();
    let (status, written) = iam(
        &router,
        "PutRolePolicy",
        &[
            ("RoleName", ROLE_NAME),
            ("PolicyName", WIDER_POLICY),
            ("PolicyDocument", &document),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: expected HTTP 200 from PutRolePolicy, got {status} ({written})"
    );

    // The second invocation: the same publishes, now decided against the document just written.
    let served = publishes_made(&router, "the publishes inside the grant", &rows, start).await;
    for (row, result) in rows.iter().zip(&served) {
        let case = row.case;
        the_sdk_dispatched_it(case, result);
        assert_eq!(
            result["ok"],
            json!(true),
            "case {case:?}: expected the publish to be served, got {result}"
        );
        assert_eq!(
            result["status"],
            json!(200),
            "case {case:?}: expected the SDK to report HTTP 200, got {result}"
        );
    }

    // The other half of every served row: the datapoint CloudWatch itself reports for that
    // series is the one the handler published, in the minute of the instant it named — read
    // again until it is there, because what makes it readable is the service's own ingest
    // rather than the answer the handler got.
    for row in &rows {
        let case = row.case;
        let expected = minute_of(start, row);
        let deadline = Instant::now() + CLOCK_BUDGET;
        loop {
            let held = datapoints(&router, case, row.metric, start).await;
            if held.len() == 1
                && (held[0].0 - expected).abs() < 0.5
                && (held[0].1 - row.value).abs() < f64::EPSILON
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: expected exactly one datapoint of {} in the minute at {expected}, \
                 got {held:?}",
                row.value
            );
            tokio::time::sleep(LOOK_AGAIN).await;
        }
    }
}
