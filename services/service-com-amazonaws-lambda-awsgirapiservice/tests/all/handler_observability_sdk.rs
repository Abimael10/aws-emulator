// SPDX-License-Identifier: CC0-1.0

//! The OBSERVABILITY calls a running handler makes with the AWS SDK its managed runtime
//! ships: Lambda, Amazon CloudWatch, CloudWatch Logs, IAM and the token service on ONE
//! engine, a `python3.x` function whose code does `import boto3`, and the metric and log
//! writes and reads it makes under its execution role — first under a role granted only the
//! log writes, then under one granted the rest.
//!
//! The RULE pinned here: a managed runtime bundles the AWS SDK and puts the execution role's
//! temporary credentials in the execution environment, so `import boto3` succeeds at module
//! scope and a client built inside the handler is signed as that role's session; the service
//! that SERVES each call decides it against the identity-based policies in force on that role
//! and either serves it — doing the work, which a later read of that service shows — or
//! refuses it with its own authorization refusal, which the handler catches as the SDK's
//! `ClientError` and so is not an invocation failure.
//!
//! The rows differ along what the rule generalises over: WHICH service serves the call
//! (CloudWatch, whose model declares no denial so it refuses with the service-common
//! `AccessDenied` at HTTP 403 judged over every resource, and CloudWatch Logs, which refuses
//! with the AWS-common `AccessDeniedException` at HTTP 400 judged against the ARN of the log
//! group the call names), WHICH kind of operation it is (a write that creates a resource, a
//! write of a datapoint, a write of log events, a read of a series, a read of the account's
//! streams), and whether the policies in force cover the action. Both halves of every row are
//! asserted — what the SDK gave the guest, which the handler returns as the invocation's
//! payload, and what the serving service holds afterwards, read where a client reads it — so a
//! refusal that still did the work, or a grant that did not, fails here. The invocation's own
//! log is asserted too: the request id the `Invoke` answer named is the id the delivered log
//! of that invocation carries.
//!
//! The suite RUNS a handler, so it takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]), and it makes TWO
//! invocations in all — the calls under the log-writes-only role and the calls under the wider
//! grant — because an invocation of its own costs this target a share of the run budget every
//! other invocation suite draws on.

use std::sync::Arc;
use std::time::Instant;

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use ::service_common::iam_authorization as iam_wording;
use service_com_amazonaws_cloudwatchlogs_logs_20140328::state::log_group_arn;

// The IAM calls and the statement writers every suite in this target writes a role with, owned
// by the suites that first needed them rather than copied.
use crate::event_source_mapping_role_access::{allow, given_role, iam};
// The Lambda calls, the activation wait and the poll budget every suite makes them with.
use crate::event_source_mappings::{
    await_function_active, call, router_on, CLOCK_BUDGET, LOOK_AGAIN,
};
// The CloudWatch call this target reads a series with.
use crate::kinesis_event_source_delivery::cloudwatch;
// The ARNs of the role a function runs as and of the session its own calls are made under,
// owned by the suite that first needed them rather than spelled again here.
use crate::handler_s3_object_access::{role_arn, session_arn};
// The CloudWatch Logs readers this target reads a delivered log with, and the epoch
// milliseconds a log event's own `timestamp` carries.
use crate::invocation_emf_metric_materialisation::now_ms;
use crate::invocation_log_delivery::{events, streams, LOGS, LOG_WRITES};
// The one .zip writer this target packages a running handler with.
use crate::synchronous_invoke_run::package;

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// Where `CreateFunction` and `Invoke` are bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The execution role the function runs as, the function's own name, and the inline policy
/// name the wider grant is written under.
const ROLE_NAME: &str = "lambda-observability-sdk";
const FUNCTION: &str = "observability-sdk";
const WIDER_POLICY: &str = "observability-calls";

/// The log group and stream the HANDLER writes to with its own client — not the function's own
/// log group, so what the handler wrote and what the runtime delivered are told apart.
const HANDLER_GROUP: &str = "/emulator/handler-observability";
const HANDLER_STREAM: &str = "handler-written";

/// The line the handler puts in that stream.
const HANDLER_LINE: &str = "handled one event with its own logs client";

/// The series the handler publishes into: a custom namespace, so nothing here touches a
/// reserved `AWS/` one.
const NAMESPACE: &str = "Emulator/HandlerObservability";
const METRIC: &str = "HandledEvents";

/// The value of the one datapoint the handler publishes, and the `Period` and statistic every
/// read below is answered at — one minute, the finest granularity
/// `GetMetricStatisticsInput$Period` admits ("The granularity, in seconds … must be a multiple
/// of 60").
const VALUE: f64 = 3.0;
const PERIOD: i64 = 60;
const MINUTE: f64 = 60.0;

/// The IAM action namespace CloudWatch's own calls are named in: "Actions defined by Amazon
/// CloudWatch" are published under this prefix, which is not the name the API is signed under
/// (`monitoring`).
const CLOUDWATCH: &str = "cloudwatch";

/// The error code and status CloudWatch refuses a call the policies in force do not allow
/// with: its model declares no denial shape, so the refusal is the service-common one.
const CLOUDWATCH_DENIAL_CODE: &str = "AccessDenied";
const CLOUDWATCH_DENIAL_STATUS: u16 = 403;

/// The resource CloudWatch judges an action its service-authorization reference lists under
/// "actions with no resource types" against: every resource.
const EVERY_RESOURCE: &str = "*";

/// The error code and status CloudWatch Logs refuses one with: the AWS-common
/// `AccessDeniedException`, at the 400 the "Common Errors" table gives it and every other
/// client error of that `awsJson1_1` endpoint carries.
const LOGS_DENIAL_CODE: &str = "AccessDeniedException";
const LOGS_DENIAL_STATUS: u16 = 400;

/// The `Timeout` the function carries, in seconds.
///
/// It bounds the WALL CLOCK one run may spend, and the handler here returns as soon as its
/// calls are answered, so what it has to cover is not its work but the host's own scheduling:
/// this target runs its suites in parallel over the emulator's run slots, so a run can sit
/// mid-flight while other suites' runs hold the CPU, and a value with no headroom would end
/// such a run on its deadline instead of answering the handler's value.
const FUNCTION_TIMEOUT_SECONDS: i64 = 25;

/// Lambda, Amazon CloudWatch, CloudWatch Logs, IAM and the token service over ONE shared
/// engine, as `core/router/src/lib.rs` mounts them: the handler's SDK calls are dispatched back
/// into this same router, so the series and the log group it writes are the ones this suite
/// reads. The token service is mounted because the session a run is signed as is minted — and
/// recorded — there, and that record is the principal an authorization decision is about.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = router_on(&engine);
    router.register_service(
        service_com_amazonaws_cloudwatch_graniteserviceversion20100801::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(Arc::clone(
            &engine,
        )),
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

/// The refusal a request no readable identity-based document allows carries, worded by the one
/// place this tree words it ([`::service_common::iam_authorization`]): the principal that
/// signed, the action it asked for, the resource it addressed, and the fact that no
/// identity-based policy allows that action.
fn implicit_denial(action: &str, resource: &str) -> String {
    iam_wording::not_authorized_by_identity_policy(
        &session_arn(ROLE_NAME, FUNCTION),
        action,
        resource,
        &iam_wording::no_identity_policy_allows(action),
    )
}

/// That same sentence for the publish row, spelled OUT here as AWS words it rather than built
/// from the wording the services under test share, so one row of this suite pins the wording
/// itself and not only its composition.
fn spelled_out_denial_of_the_publish() -> String {
    format!(
        "User: {} is not authorized to perform: cloudwatch:PutMetricData on resource: * because \
         no identity-based policy allows the cloudwatch:PutMetricData action",
        session_arn(ROLE_NAME, FUNCTION)
    )
}

/// And spelled out for the OTHER refusal shape this suite spans — the one CloudWatch Logs
/// answers — where the resource is the ARN of the log group the call named rather than every
/// resource.
fn spelled_out_denial_of_the_stream_read() -> String {
    format!(
        "User: {} is not authorized to perform: logs:DescribeLogStreams on resource: \
         arn:aws:logs:{REGION}:{}:log-group:{HANDLER_GROUP} because no identity-based policy \
         allows the logs:DescribeLogStreams action",
        session_arn(ROLE_NAME, FUNCTION),
        ::engine::emulator_account_id()
    )
}

/// The handler every invocation runs: it builds a client for each call the event names, makes
/// it, and reports what the SDK gave it — so the invocation's payload IS what the guest saw.
///
/// A member the event names under `instants` is handed over as the native date-time the SDK
/// marshals into the timestamp the service's model types it as, because that is how a handler
/// writes a window. What a served call answered is reported member by member in a form the
/// invocation's payload can carry, with a list reported by its length and the datapoints of a
/// metric read by their sums.
const HANDLER_SOURCE: &str = r#"
import datetime

import boto3
import botocore.exceptions

EPOCH = datetime.datetime(1970, 1, 1)


def resolved(parameters, instants):
    """The call's parameters, with each named member as the instant it holds."""
    for name in instants:
        parameters[name] = EPOCH + datetime.timedelta(seconds=parameters[name])
    return parameters


def reported(answered):
    """What a served call answered, in a form the invocation's payload can carry."""
    shown = {}
    for name, value in answered.items():
        if name == "ResponseMetadata":
            continue
        if isinstance(value, list):
            shown[name] = len(value)
        elif value is None or isinstance(value, (str, int, float, bool)):
            shown[name] = value
        else:
            shown[name] = type(value).__name__
    for point in answered.get("Datapoints", []):
        shown.setdefault("Sums", []).append(point["Sum"])
    if "Sums" in shown:
        shown["Sums"] = sorted(shown["Sums"])
    return shown


def lambda_handler(event, context):
    results = []
    for asked in event["calls"]:
        client = boto3.client(asked["service"])
        parameters = resolved(asked["parameters"], asked.get("instants", []))
        try:
            answered = getattr(client, asked["method"])(**parameters)
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
        results.append(
            {
                "ok": True,
                "status": answered["ResponseMetadata"]["HTTPStatusCode"],
                "answered": reported(answered),
            }
        )
    return {"results": results}
"#;

/// What the policies in force mean for one call: the error code and status the serving service
/// refuses it with, and the action and the resource its refusal names.
struct Refusal {
    code: &'static str,
    status: u16,
    action: String,
    resource: String,
}

/// One row: the call the handler makes, and what the policies in force mean for it.
struct Row {
    case: &'static str,
    /// The service token the handler builds its client with.
    service: &'static str,
    method: &'static str,
    parameters: Value,
    /// The parameters the handler hands over as a native date-time.
    instants: &'static [&'static str],
    /// `None` for a call the policies in force allow.
    refused: Option<Refusal>,
    /// The output members a served call must carry, each present and not null.
    present: &'static [&'static str],
}

/// A CloudWatch refusal: judged over every resource, under the service-common code.
fn cloudwatch_refusal(operation: &str) -> Option<Refusal> {
    Some(Refusal {
        code: CLOUDWATCH_DENIAL_CODE,
        status: CLOUDWATCH_DENIAL_STATUS,
        action: format!("{CLOUDWATCH}:{operation}"),
        resource: EVERY_RESOURCE.to_string(),
    })
}

/// A CloudWatch Logs refusal: judged against the ARN of the log group the call named, under
/// this service's own ARN builder so the resource is spelled as that service reports it.
fn logs_refusal(operation: &str, group: &str) -> Option<Refusal> {
    Some(Refusal {
        code: LOGS_DENIAL_CODE,
        status: LOGS_DENIAL_STATUS,
        action: format!("{LOGS}:{operation}"),
        resource: log_group_arn(REGION, group, false),
    })
}

/// The window every metric read below is answered over: it opens at the whole minute half an
/// hour back that the suite which owns that convention lays
/// ([`crate::handler_metric_timestamp::window_start`]) and is wide enough that the publication
/// the handler makes while this suite runs falls inside it whatever the host's load.
fn window() -> (f64, f64) {
    let start = crate::handler_metric_timestamp::window_start();
    (start, start + 60.0 * MINUTE)
}

/// The metric read as the handler asks for it, over that same window.
fn read_parameters() -> Value {
    let (start, end) = window();
    json!({
        "Namespace": NAMESPACE,
        "MetricName": METRIC,
        "StartTime": start,
        "EndTime": end,
        "Period": PERIOD,
        "Statistics": ["Sum"],
    })
}

/// The rows of the FIRST invocation: the role carries the three log writes a function's own
/// delivery needs and nothing else, so the handler's log writes are served and every other
/// call is outside the policies in force.
fn log_writes_only_rows() -> Vec<Row> {
    vec![
        Row {
            case: "a write that creates a resource, allowed by the log-writes grant",
            service: "logs",
            method: "create_log_group",
            parameters: json!({ "logGroupName": HANDLER_GROUP }),
            instants: &[],
            refused: None,
            present: &[],
        },
        Row {
            case: "a write that creates a child resource, allowed by the same grant",
            service: "logs",
            method: "create_log_stream",
            parameters: json!({
                "logGroupName": HANDLER_GROUP,
                "logStreamName": HANDLER_STREAM,
            }),
            instants: &[],
            refused: None,
            present: &[],
        },
        Row {
            case: "a write of log events the grant allows",
            service: "logs",
            method: "put_log_events",
            parameters: json!({
                "logGroupName": HANDLER_GROUP,
                "logStreamName": HANDLER_STREAM,
                "logEvents": [{ "timestamp": now_ms(), "message": HANDLER_LINE }],
            }),
            instants: &[],
            refused: None,
            present: &["nextSequenceToken"],
        },
        Row {
            case: "a read of the same service the grant does not name",
            service: "logs",
            method: "describe_log_streams",
            parameters: json!({ "logGroupName": HANDLER_GROUP }),
            instants: &[],
            refused: logs_refusal("DescribeLogStreams", HANDLER_GROUP),
            present: &[],
        },
        Row {
            case: "a datapoint no identity policy allows publishing",
            service: "cloudwatch",
            method: "put_metric_data",
            parameters: json!({
                "Namespace": NAMESPACE,
                "MetricData": [{ "MetricName": METRIC, "Value": VALUE, "Unit": "Count" }],
            }),
            instants: &[],
            refused: cloudwatch_refusal("PutMetricData"),
            present: &[],
        },
        Row {
            case: "a read of a series no identity policy allows",
            service: "cloudwatch",
            method: "get_metric_statistics",
            parameters: read_parameters(),
            instants: &["StartTime", "EndTime"],
            refused: cloudwatch_refusal("GetMetricStatistics"),
            present: &[],
        },
    ]
}

/// The rows of the SECOND invocation, with the wider grant in force: the same calls the first
/// invocation was refused, now decided against the document just written.
fn granted_rows() -> Vec<Row> {
    vec![
        Row {
            case: "the same read of the account's streams, now allowed",
            service: "logs",
            method: "describe_log_streams",
            parameters: json!({ "logGroupName": HANDLER_GROUP }),
            instants: &[],
            refused: None,
            present: &["logStreams"],
        },
        Row {
            case: "the same datapoint, now allowed",
            service: "cloudwatch",
            method: "put_metric_data",
            parameters: json!({
                "Namespace": NAMESPACE,
                "MetricData": [{ "MetricName": METRIC, "Value": VALUE, "Unit": "Count" }],
            }),
            instants: &[],
            refused: None,
            present: &[],
        },
        Row {
            case: "the same read of the series, now allowed",
            service: "cloudwatch",
            method: "get_metric_statistics",
            parameters: read_parameters(),
            instants: &["StartTime", "EndTime"],
            refused: None,
            present: &["Datapoints"],
        },
    ]
}

/// Make every row's call in ONE invocation and assert what the SDK gave the guest for each,
/// answering the request id the invocation's own answer named and every row's own result.
async fn calls_made(
    router: &MultiServiceRouter,
    label: &str,
    rows: &[Row],
) -> (String, Vec<Value>) {
    let asked: Vec<Value> = rows
        .iter()
        .map(|row| {
            json!({
                "service": row.service,
                "method": row.method,
                "parameters": row.parameters,
                "instants": row.instants,
            })
        })
        .collect();
    let answered = call(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{FUNCTION}/invocations"),
        &json!({ "calls": asked }),
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
        "case {label:?}: expected one result per call, got {payload}"
    );
    for (row, result) in rows.iter().zip(&results) {
        let case = row.case;
        match &row.refused {
            Some(refusal) => {
                assert_eq!(
                    result["ok"],
                    json!(false),
                    "case {case:?}: expected the call to be refused, got {result}"
                );
                assert_eq!(
                    result["code"],
                    json!(refusal.code),
                    "case {case:?}: expected the error code {:?}, got {result}",
                    refusal.code
                );
                assert_eq!(
                    result["status"],
                    json!(refusal.status),
                    "case {case:?}: expected HTTP {} on the refusal, got {result}",
                    refusal.status
                );
                assert_eq!(
                    result["message"],
                    json!(implicit_denial(&refusal.action, &refusal.resource)),
                    "case {case:?}: expected the refusal to name the session, the action and \
                     the resource, got {result}"
                );
            }
            None => {
                assert_eq!(
                    result["ok"],
                    json!(true),
                    "case {case:?}: expected the call to be served, got {result}"
                );
                assert_eq!(
                    result["status"],
                    json!(200),
                    "case {case:?}: expected the SDK to report HTTP 200, got {result}"
                );
                for member in row.present {
                    assert!(
                        !result["answered"][*member].is_null(),
                        "case {case:?}: expected the output member {member:?} to be answered, \
                         got {result}"
                    );
                }
            }
        }
    }
    let request_id = answered
        .header("x-amzn-requestid")
        .unwrap_or_else(|| panic!("case {label:?}: the Invoke answer named no request id"));
    (request_id, results)
}

/// The result of the row `case` names, among the results of one invocation.
fn result_of<'a>(rows: &[Row], results: &'a [Value], case: &str) -> &'a Value {
    rows.iter()
        .zip(results)
        .find_map(|(row, result)| (row.case == case).then_some(result))
        .unwrap_or_else(|| panic!("case {case:?}: no row of this invocation carries that case"))
}

/// The sums of the datapoints the series holds over the whole window, read where a client reads
/// them.
async fn published_sums(router: &MultiServiceRouter, case: &str) -> Vec<f64> {
    let (status, answered) = cloudwatch(router, "GetMetricStatistics", &read_parameters()).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from GetMetricStatistics, got {status} ({answered})"
    );
    let mut sums: Vec<f64> = answered["Datapoints"]
        .as_array()
        .map(|listed| {
            listed
                .iter()
                .map(|point| point["Sum"].as_f64().unwrap_or(f64::NAN))
                .collect()
        })
        .unwrap_or_default();
    sums.sort_unstable_by(f64::total_cmp);
    sums
}

/// RULE: a handler's own observability calls — made with the SDK its managed runtime ships, as
/// the function's execution-role session — are decided by the serving service against the
/// identity-based policies in force on that role, and a served call does its work where a
/// client reads it while a refused one does none of it.
#[tokio::test]
async fn a_handlers_own_metric_and_log_calls_are_decided_against_its_execution_roles_policies() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let router = router();

    // The role the function runs as, written with IAM's own operations so the session the
    // runtime mints speaks for the record `iam:CreateRole` wrote. It starts with the three log
    // writes a function's own delivery needs and nothing else.
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

    // The first invocation: the log writes are served, everything else is outside the grant.
    let under_log_writes = log_writes_only_rows();
    let (request_id, results) =
        calls_made(&router, "the calls under the log writes", &under_log_writes).await;

    // One refusal's wording per refusal SHAPE, against the sentences spelled out in this suite:
    // the refused publish and the refused stream read each name the session, the action and the
    // resource their serving service judged them against, and end on IAM's own reason.
    let case = "a datapoint no identity policy allows publishing";
    let refused = result_of(&under_log_writes, &results, case);
    assert_eq!(
        refused["message"],
        json!(spelled_out_denial_of_the_publish()),
        "case {case:?}: expected IAM's own refusal sentence, got {refused}"
    );
    let case = "a read of the same service the grant does not name";
    let refused = result_of(&under_log_writes, &results, case);
    assert_eq!(
        refused["message"],
        json!(spelled_out_denial_of_the_stream_read()),
        "case {case:?}: expected IAM's own refusal sentence over the group's ARN, got {refused}"
    );

    // The other half of the served log rows: the group, the stream and the event the handler
    // wrote are the ones CloudWatch Logs itself reports.
    let case = "a write of log events the grant allows";
    let held = streams(&router, case, HANDLER_GROUP).await;
    assert_eq!(
        held,
        vec![HANDLER_STREAM.to_string()],
        "case {case:?}: expected the stream the handler created, got {held:?}"
    );
    let written: Vec<String> = events(&router, case, HANDLER_GROUP, HANDLER_STREAM)
        .await
        .into_iter()
        .map(|(_, line)| line)
        .collect();
    assert_eq!(
        written,
        vec![HANDLER_LINE.to_string()],
        "case {case:?}: expected the line the handler put, got {written:?}"
    );

    // The other half of the refused metric row: nothing was published.
    let case = "a datapoint no identity policy allows publishing";
    let sums = published_sums(&router, case).await;
    assert!(
        sums.is_empty(),
        "case {case:?}: expected the refused publish to leave the series empty, got {sums:?}"
    );

    // The invocation's OWN log, delivered by the runtime under that same role: the request id
    // the answer named is the id the delivered log carries. The delivery happens off the
    // response path, so the condition is looked at again until it holds.
    let case = "the invocation's own log names the request the answer named";
    let group = format!("/aws/lambda/{FUNCTION}");
    let deadline = Instant::now() + CLOCK_BUDGET;
    loop {
        let mut carried = false;
        for stream in streams(&router, case, &group).await {
            let lines = events(&router, case, &group, &stream).await;
            carried |= lines.iter().any(|(_, line)| line.contains(&request_id));
        }
        if carried {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected a delivered log line naming {request_id}, got the streams \
             {:?}",
            streams(&router, case, &group).await
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }

    // The wider grant, written where a client writes one.
    let document = json!({
        "Version": "2012-10-17",
        "Statement": [
            allow(LOGS, &["DescribeLogStreams"], EVERY_RESOURCE),
            allow(
                CLOUDWATCH,
                &["PutMetricData", "GetMetricStatistics"],
                EVERY_RESOURCE,
            ),
        ],
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

    // The second invocation: the same calls, now decided against the document just written.
    let granted = granted_rows();
    let (_, granted_results) =
        calls_made(&router, "the calls under the wider grant", &granted).await;

    // What the granted read of the series handed the handler: the datapoint it had just
    // published, so the read really read this series rather than answering an empty page.
    let case = "the same read of the series, now allowed";
    let read = result_of(&granted, &granted_results, case);
    assert_eq!(
        read["answered"]["Sums"],
        json!([VALUE]),
        "case {case:?}: expected the sum of the datapoint the handler published, got {read}"
    );

    // The other half of the granted metric rows: the datapoint the handler published is the one
    // CloudWatch itself reports for that series.
    let case = "the same datapoint, now allowed";
    let deadline = Instant::now() + CLOCK_BUDGET;
    loop {
        let sums = published_sums(&router, case).await;
        if sums == vec![VALUE] {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the series to hold the one datapoint of {VALUE}, got {sums:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}
