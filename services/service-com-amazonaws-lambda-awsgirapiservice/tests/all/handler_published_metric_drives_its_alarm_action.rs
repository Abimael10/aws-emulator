// SPDX-License-Identifier: CC0-1.0

//! The loop a client closes around an instrumented function: a running `python3.x` handler
//! publishes a datapoint about its own work with the AWS SDK its managed runtime ships, the
//! monitoring service stores it, and a metric alarm over that series invokes a FUNCTION when it
//! transitions — Lambda, Amazon CloudWatch, CloudWatch Logs, IAM and the token service on one
//! engine, so every answer asserted is one a client reads.
//!
//! The RULE pinned here: **the AWS SDK a managed runtime ships resolves a CLIENT for every
//! service that runtime's call seam binds requests for, so `boto3.client(<service>)` inside a
//! handler is constructed rather than refused and the call it makes reaches the serving service
//! — which decides it against the identity-based policies in force on the execution role,
//! serving it or refusing it with its own authorization error; a datapoint such a served
//! publish stores is the series a metric alarm grades, and the transition that series drives
//! executes the alarm's action list, where an action naming a function invokes THAT function
//! only where the function's own resource-based policy grants this service the invocation.**
//!
//! Each clause is a documented one. "Publishes metric data to Amazon CloudWatch. CloudWatch
//! associates the data with the specified metric" (`PutMetricData`); "The execution role grants
//! the function permission to use Amazon Web Services services"
//! (`FunctionConfiguration$Role`), so the role is what an in-handler call is judged against;
//! "Lambda actions: Invoke the latest version of a Lambda function:
//! arn:aws:lambda:region:account-id:function:function-name" (`PutMetricAlarmInput$AlarmActions`);
//! and "When this operation creates an alarm, the alarm state is immediately set to
//! INSUFFICIENT_DATA" (`PutMetricAlarm`), which with the default `TreatMissingData` of `missing`
//! is where an alarm whose series holds nothing stays.
//!
//! The rows differ along what the rule generalises over: how many datapoints the handler
//! publishes (so the graded `Sum` differs row to row), where that figure falls against the
//! threshold and WHICH comparison decides it (the strict one, the inclusive one), whether the
//! execution role allows the publish at all, and whether the function's resource policy grants
//! the invocation the transition fires. The two rows that publish nothing into the alarm's
//! verdict are read against a row that DID move, and each absence has a single cause: one row
//! is granted the invocation and has no datapoint to breach on, the other breaches and is
//! granted nothing, so neither silence can be the other's.
//!
//! What the neighbours pin and this one does not repeat: `crate::handler_observability_sdk` pins
//! the DECISION a handler's metric and log calls are subject to over a table of grants, and
//! `services/service-com-amazonaws-cloudwatch-graniteserviceversion20100801/tests/all/lambda_alarm_actions.rs`
//! pins the policy forms an alarm action is admitted by against a transition a client FORCES.
//! What is pinned HERE is the loop joined: the series the alarm grades is the one the FUNCTION
//! filled through its bundled SDK, and the function the action names is the one that filled it.
//!
//! The suite RUNS handlers, so it takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]), and it makes one invocation
//! per row, each of which also receives whatever the alarm action delivers.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use ::service_common::iam_authorization as iam_wording;

// The IAM calls and the statement writers every suite in this target writes a role with, owned
// by the suites that first needed them rather than copied.
use crate::event_source_mapping_role_access::{allow, given_role};
// The Lambda calls, the activation wait and the poll interval every suite makes them with.
use crate::event_source_mappings::{await_function_active, call, router_on, LOOK_AGAIN};
// The ARNs of the role a function runs as and of the session its own calls are made under.
use crate::handler_s3_object_access::{role_arn, session_arn};
// The CloudWatch Logs readers this target reads a delivered log with, and the log writes a
// function's own delivery needs.
use crate::invocation_log_delivery::{events, streams, LOGS, LOG_WRITES};
// The CloudWatch call this target reads a series and an alarm with.
use crate::kinesis_event_source_delivery::cloudwatch;
// The one .zip writer this target packages a running handler with.
use crate::synchronous_invoke_run::package;

/// Where `CreateFunction`, `AddPermission` and `Invoke` are bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The service token the handler builds its metric client with — what the AWS SDK for Python
/// names the monitoring service, which is not the name that service signs under (`monitoring`).
const CLIENT_TOKEN: &str = "cloudwatch";

/// The IAM action namespace the monitoring service's own calls are named in, and the one action
/// a publish performs.
const CLOUDWATCH: &str = "cloudwatch";
/// See [`CLOUDWATCH`].
const PUBLISH_ACTION: &str = "PutMetricData";

/// The error code and status the monitoring service refuses a call the policies in force do not
/// allow with: its model declares no denial shape, so the refusal is the service-common one, and
/// the action it judges is one its service-authorization reference lists with no resource type,
/// so the resource named is every resource.
const DENIAL_CODE: &str = "AccessDenied";
/// See [`DENIAL_CODE`].
const DENIAL_STATUS: i64 = 403;
/// See [`DENIAL_CODE`].
const EVERY_RESOURCE: &str = "*";

/// The custom series the handler publishes into — a namespace of the client's own, so nothing
/// here touches a reserved `AWS/` one — and the dimension each row's own datapoints carry, which
/// the handler fills from the run's own context rather than from the event.
const NAMESPACE: &str = "Emulator/HandlerPublishedMetric";
/// See [`NAMESPACE`].
const METRIC: &str = "HandledEvents";
/// See [`NAMESPACE`].
const DIMENSION: &str = "FunctionName";

/// The value of each datapoint the handler publishes, so the `Sum` over a period is the NUMBER
/// of datapoints it published.
const UNIT_VALUE: f64 = 1.0;

/// The `Period`, in seconds, the alarms grade and the reads below are answered at. Five minutes:
/// a multiple of the 60 `GetMetricStatisticsInput$Period` admits, and wide enough that a
/// datapoint published while this suite runs is inside the window the alarm's one evaluation
/// period covers however loaded the host is.
const PERIOD: i64 = 300;

/// The principal AWS has a client grant a Lambda alarm action to, the action such a grant has to
/// name, and the `StatementId` it is written under.
const ALARM_ACTION_PRINCIPAL: &str = "lambda.alarms.cloudwatch.amazonaws.com";
/// See [`ALARM_ACTION_PRINCIPAL`].
const INVOKE_ACTION: &str = "lambda:InvokeFunction";
/// See [`ALARM_ACTION_PRINCIPAL`].
const STATEMENT_ID: &str = "cloudwatch-alarm-action";

/// The prefix the handler prints when an alarm action hands it a transition, which is the line a
/// client reading the function's log group looks for.
const PRINTED_PREFIX: &str = "ALARMED:";

/// The three modelled states an alarm reports (`smithy_query(query="shape",shape="StateValue",
/// model_path="services/service-com-amazonaws-cloudwatch-graniteserviceversion20100801/\
/// smithy-model/com-amazonaws-cloudwatch-graniteserviceversion20100801.json")`: `OK |
/// ALARM | INSUFFICIENT_DATA`) — the two this suite's rows end in.
const ALARM: &str = "ALARM";
/// See [`ALARM`].
const INSUFFICIENT_DATA: &str = "INSUFFICIENT_DATA";

/// The `Timeout` every function below carries, in seconds. It bounds the WALL CLOCK one run may
/// spend, and each handler returns as soon as its one call is answered, so what it covers is the
/// host's own scheduling rather than the handler's work.
const FUNCTION_TIMEOUT_SECONDS: i64 = 25;

/// How long this suite waits for work another service's clock owes — a created function becoming
/// `Active`, an alarm's own evaluation, the invocation an action fires and the log delivery of
/// the run it started — before giving up on it. Generous on purpose: nothing here is asserted
/// about how fast the host runs, and every wait below looks at the CONDITION again rather than
/// sleeping a span out.
const WORK_WAIT: Duration = Duration::from_secs(120);

/// The handler every row runs. One event asks it to publish, which is the call a handler
/// instrumenting its own work makes; the other is the transition an alarm action delivers, which
/// it prints.
///
/// It reports whether the SDK CONSTRUCTED the client at all beside what the call answered, so a
/// runtime whose bundled SDK does not know the service is told apart from one whose call was
/// refused by the service.
const HANDLER_SOURCE: &str = r#"
import boto3
import botocore.exceptions


def lambda_handler(event, context):
    data = event.get("alarmData")
    if data:
        state = (data.get("state") or {}).get("value") or "none"
        print("ALARMED:" + str(data.get("alarmName")) + ":" + str(state))
        return {"seen": data.get("alarmName")}
    try:
        client = boto3.client(event["client"])
    except botocore.exceptions.BotoCoreError as error:
        return {"built": False, "raised": type(error).__name__, "sentence": str(error)}
    published = [
        {
            "MetricName": event["metric"],
            "Dimensions": [{"Name": event["dimension"], "Value": context.function_name}],
            "Value": event["value"],
            "Unit": "Count",
        }
        for _ in range(event["datapoints"])
    ]
    try:
        client.put_metric_data(Namespace=event["namespace"], MetricData=published)
    except botocore.exceptions.ClientError as error:
        return {
            "built": True,
            "published": False,
            "code": error.response["Error"]["Code"],
            "message": error.response["Error"].get("Message"),
            "status": error.response["ResponseMetadata"]["HTTPStatusCode"],
        }
    return {"built": True, "published": True, "datapoints": len(published)}
"#;

/// One row: what the publishing handler is allowed to do, what its publish is worth against the
/// alarm it drives, and what the function's own policy says about the invocation that transition
/// fires.
struct Row {
    case: &'static str,
    /// The suffix this row's role, function, alarm and series dimension are named by.
    label: &'static str,
    /// Whether the execution role's policies allow the publish the handler makes.
    publish_granted: bool,
    /// How many one-unit datapoints the handler publishes.
    datapoints: u32,
    /// The threshold the alarm compares the `Sum` of them against, and the comparison it makes.
    threshold: f64,
    /// See [`Row::threshold`].
    comparison: &'static str,
    /// The state the alarm must report once what the publish left is graded.
    state: &'static str,
    /// Whether the function's resource-based policy grants this service the invocation.
    invoke_granted: bool,
    /// Whether the transition therefore reaches the function, so its log carries the line.
    invoked: bool,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a granted publish past a strict threshold, with the invocation granted",
            label: "fires",
            publish_granted: true,
            datapoints: 3,
            threshold: 2.0,
            comparison: "GreaterThanThreshold",
            state: ALARM,
            invoke_granted: true,
            invoked: true,
        },
        Row {
            case: "a granted publish meeting an inclusive threshold, with no grant to invoke",
            label: "ungranted",
            publish_granted: true,
            datapoints: 2,
            threshold: 2.0,
            comparison: "GreaterThanOrEqualToThreshold",
            state: ALARM,
            invoke_granted: false,
            invoked: false,
        },
        Row {
            case: "a publish no identity policy allows, so the alarm grades nothing",
            label: "unpublished",
            publish_granted: false,
            datapoints: 4,
            threshold: 1.0,
            comparison: "GreaterThanOrEqualToThreshold",
            state: INSUFFICIENT_DATA,
            invoke_granted: true,
            invoked: false,
        },
    ]
}

impl Row {
    /// The role this row's function runs as.
    fn role(&self) -> String {
        format!("lambda-published-metric-{}", self.label)
    }

    /// The function itself.
    fn function(&self) -> String {
        format!("published-metric-{}", self.label)
    }

    /// The alarm over this row's own slice of the series.
    fn alarm(&self) -> String {
        format!("published-metric-{}", self.label)
    }

    /// The log group a function's own runs are delivered into — "By default, Lambda functions
    /// send logs to a default log group named `/aws/lambda/<function name>`"
    /// (`LoggingConfig$LogGroup`).
    fn log_group(&self) -> String {
        format!("/aws/lambda/{}", self.function())
    }
}

/// Lambda, Amazon CloudWatch, CloudWatch Logs, IAM and the token service over ONE shared engine,
/// as `core/router/src/lib.rs` mounts them: the handler's own SDK call is dispatched back into
/// this same router, so the series this suite reads is the one the handler filled, and the
/// invocation an alarm action makes reaches the function this suite created. The token service is
/// mounted because the session a run is signed as is minted there, and that record is the
/// principal an authorization decision is about.
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

/// The refusal a publish no readable identity-based document allows carries, worded by the one
/// place this tree words it: the session that signed, the action it asked for, the resource it
/// was judged over, and IAM's own reason.
fn implicit_denial(row: &Row) -> String {
    let action = format!("{CLOUDWATCH}:{PUBLISH_ACTION}");
    iam_wording::not_authorized_by_identity_policy(
        &session_arn(&row.role(), &row.function()),
        &action,
        EVERY_RESOURCE,
        &iam_wording::no_identity_policy_allows(&action),
    )
}

/// Epoch seconds now, which is the middle of every metric window read below.
fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs_f64())
        .unwrap_or_default()
}

/// Create the row's role and its function, grant the alarm action what the row says, and answer
/// the ARN Lambda assigned the function.
async fn given_function(router: &MultiServiceRouter, row: &Row) -> String {
    let mut statements = vec![allow(LOGS, &LOG_WRITES, EVERY_RESOURCE)];
    if row.publish_granted {
        statements.push(allow(CLOUDWATCH, &[PUBLISH_ACTION], EVERY_RESOURCE));
    }
    let role = row.role();
    let created_role = given_role(router, row.case, &role, Some(json!(statements))).await;
    assert_eq!(
        created_role,
        role_arn(&role),
        "case {:?}: expected the role ARN of this account, got {created_role}",
        row.case
    );

    let function = row.function();
    let created = call(
        router,
        "POST",
        FUNCTIONS_URI,
        &json!({
            "FunctionName": function,
            "Role": role_arn(&role),
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
        "case {:?}: expected HTTP 201 from CreateFunction, got {} ({})",
        row.case,
        created.status,
        created.text()
    );
    let arn = created.json()["FunctionArn"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "case {:?}: expected CreateFunction to answer a FunctionArn, got {}",
                row.case,
                created.text()
            )
        })
        .to_string();

    if row.invoke_granted {
        let granted = call(
            router,
            "POST",
            &format!("{FUNCTIONS_URI}/{function}/policy"),
            &json!({
                "StatementId": STATEMENT_ID,
                "Action": INVOKE_ACTION,
                "Principal": ALARM_ACTION_PRINCIPAL,
            }),
        )
        .await;
        assert_eq!(
            granted.status,
            201,
            "case {:?}: expected HTTP 201 from AddPermission, got {} ({})",
            row.case,
            granted.status,
            granted.text()
        );
    }

    await_function_active(router, row.case, &function).await;
    arn
}

/// Invoke the row's function with the publish event and assert what the SDK gave the guest: the
/// client was CONSTRUCTED either way, and the call it made was served or refused exactly as the
/// policies in force on the execution role say.
async fn publish_made(router: &MultiServiceRouter, row: &Row) {
    let answered = call(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{}/invocations", row.function()),
        &json!({
            "client": CLIENT_TOKEN,
            "namespace": NAMESPACE,
            "metric": METRIC,
            "dimension": DIMENSION,
            "value": UNIT_VALUE,
            "datapoints": row.datapoints,
        }),
    )
    .await;
    let case = row.case;
    assert_eq!(
        answered.status,
        200,
        "case {case:?}: expected HTTP 200 from Invoke, got {} ({})",
        answered.status,
        answered.text()
    );
    let signal = answered.header("x-amz-function-error");
    assert_eq!(
        signal, None,
        "case {case:?}: expected no FunctionError, got {signal:?} with {}",
        answered.text()
    );
    let payload = answered.json();
    assert_eq!(
        payload["built"],
        json!(true),
        "case {case:?}: expected the bundled SDK to build a {CLIENT_TOKEN:?} client, got {payload}"
    );
    if row.publish_granted {
        assert_eq!(
            payload["published"],
            json!(true),
            "case {case:?}: expected the publish to be served, got {payload}"
        );
        assert_eq!(
            payload["datapoints"],
            json!(row.datapoints),
            "case {case:?}: expected {} datapoint(s) published, got {payload}",
            row.datapoints
        );
    } else {
        assert_eq!(
            payload["published"],
            json!(false),
            "case {case:?}: expected the publish to be refused, got {payload}"
        );
        assert_eq!(
            payload["code"],
            json!(DENIAL_CODE),
            "case {case:?}: expected the error code {DENIAL_CODE:?}, got {payload}"
        );
        assert_eq!(
            payload["status"],
            json!(DENIAL_STATUS),
            "case {case:?}: expected HTTP {DENIAL_STATUS} on the refusal, got {payload}"
        );
        assert_eq!(
            payload["message"],
            json!(implicit_denial(row)),
            "case {case:?}: expected the refusal to name the session, the action and the \
             resource, got {payload}"
        );
    }
}

/// The `Sum` of every datapoint the row's own slice of the series holds, read where a client
/// reads it.
async fn published_sum(router: &MultiServiceRouter, row: &Row) -> f64 {
    let stamp = now_secs();
    let period = PERIOD as f64;
    let (status, answered) = cloudwatch(
        router,
        "GetMetricStatistics",
        &json!({
            "Namespace": NAMESPACE,
            "MetricName": METRIC,
            "Dimensions": [{ "Name": DIMENSION, "Value": row.function() }],
            "StartTime": stamp - period,
            "EndTime": stamp + period,
            "Period": PERIOD,
            "Statistics": ["Sum"],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {:?}: expected HTTP 200 from GetMetricStatistics, got {status} ({answered})",
        row.case
    );
    answered["Datapoints"]
        .as_array()
        .map(|points| {
            points
                .iter()
                .filter_map(|point| point["Sum"].as_f64())
                .sum()
        })
        .unwrap_or_default()
}

/// The alarm over the row's own slice of the series, whose one alarm action names the function
/// that filled it. It is created after the publish, so what it grades is what the handler stored.
async fn given_alarm(router: &MultiServiceRouter, row: &Row, function_arn: &str) {
    let (status, answered) = cloudwatch(
        router,
        "PutMetricAlarm",
        &json!({
            "AlarmName": row.alarm(),
            "Namespace": NAMESPACE,
            "MetricName": METRIC,
            "Dimensions": [{ "Name": DIMENSION, "Value": row.function() }],
            // One datapoint per event the handler acted on, so what the period is worth is their
            // SUM.
            "Statistic": "Sum",
            "Period": PERIOD,
            "EvaluationPeriods": 1,
            "Threshold": row.threshold,
            "ComparisonOperator": row.comparison,
            "AlarmActions": [function_arn],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {:?}: expected HTTP 200 from PutMetricAlarm, got {status} ({answered})",
        row.case
    );
}

/// What `DescribeAlarms` reports for the row's alarm.
async fn alarm_state(router: &MultiServiceRouter, row: &Row) -> String {
    let (status, answered) = cloudwatch(
        router,
        "DescribeAlarms",
        &json!({ "AlarmNames": [row.alarm()] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {:?}: expected HTTP 200 from DescribeAlarms, got {status} ({answered})",
        row.case
    );
    answered["MetricAlarms"][0]["StateValue"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// Every line the row's function has had delivered into its own log group.
async fn delivered_lines(router: &MultiServiceRouter, row: &Row) -> Vec<String> {
    let group = row.log_group();
    let mut lines = Vec::new();
    for stream in streams(router, row.case, &group).await {
        for (_, line) in events(router, row.case, &group, &stream).await {
            lines.push(line);
        }
    }
    lines
}

/// RULE: a datapoint a running handler publishes with the SDK its managed runtime ships is
/// stored by the service that serves the call — where the execution role's policies allow it —
/// and the metric alarm over that series invokes the function its action list names where that
/// function's own policy grants this service the invocation.
#[tokio::test]
async fn a_handlers_published_datapoint_drives_the_alarm_action_that_invokes_it() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let router = router();
    let rows = rows();

    // Every row's function, and the publish it makes with its own client. The client is built
    // inside the handler, so a runtime whose bundled SDK did not know the service would report
    // that here instead of an answer from the service.
    let mut arns = Vec::new();
    for row in &rows {
        arns.push(given_function(&router, row).await);
        publish_made(&router, row).await;
    }

    // The other half of each publish: the series holds exactly what that row's handler stored,
    // read where a client reads it. A refused publish stored nothing at all.
    for row in &rows {
        let wanted = if row.publish_granted {
            f64::from(row.datapoints) * UNIT_VALUE
        } else {
            0.0
        };
        let deadline = Instant::now() + WORK_WAIT;
        loop {
            let held = published_sum(&router, row).await;
            if (held - wanted).abs() < f64::EPSILON {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "case {:?}: expected the series to hold a Sum of {wanted}, got {held}",
                row.case
            );
            tokio::time::sleep(LOOK_AGAIN).await;
        }
    }

    // The alarms, each over the slice its own function filled and each firing that function.
    for (row, arn) in rows.iter().zip(&arns) {
        given_alarm(&router, row, arn).await;
    }

    // The state every alarm ends in is the one its own series decided: no call forces a
    // transition here. The rows that breach are waited out; the row whose publish was refused is
    // read once they have moved, so what it reports is read against evaluations that did happen.
    for row in rows.iter().filter(|row| row.state == ALARM) {
        let deadline = Instant::now() + WORK_WAIT;
        loop {
            let reported = alarm_state(&router, row).await;
            if reported == row.state {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "case {:?}: expected the alarm to report {:?}, got {reported:?}",
                row.case,
                row.state
            );
            tokio::time::sleep(LOOK_AGAIN).await;
        }
    }
    for row in rows.iter().filter(|row| row.state != ALARM) {
        let reported = alarm_state(&router, row).await;
        assert_eq!(
            reported, row.state,
            "case {:?}: expected the alarm to report {:?}, got {reported:?}",
            row.case, row.state
        );
    }

    // The action the breaching, granted row fires: the function it names runs and prints the
    // alarm and the state it moved to, delivered into that function's own log group. The wait is
    // on the CONDITION, and this row is the control the absences below are read against.
    for row in rows.iter().filter(|row| row.invoked) {
        let wanted = format!("{PRINTED_PREFIX}{}:{}", row.alarm(), row.state);
        let deadline = Instant::now() + WORK_WAIT;
        loop {
            let lines = delivered_lines(&router, row).await;
            if lines.iter().any(|line| line.contains(&wanted)) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "case {:?}: expected a delivered line carrying {wanted:?}, got {lines:?}",
                row.case
            );
            tokio::time::sleep(LOOK_AGAIN).await;
        }
    }

    // And the rows the transition does not reach: one is granted the invocation but has no
    // datapoint to breach on, the other breached and is granted nothing — neither function ran
    // for an alarm, which is read after the control row's own run was already delivered.
    for row in rows.iter().filter(|row| !row.invoked) {
        let lines = delivered_lines(&router, row).await;
        assert!(
            !lines.iter().any(|line| line.contains(PRINTED_PREFIX)),
            "case {:?}: expected no alarm action to reach this function, got {lines:?}",
            row.case
        );
    }
}
