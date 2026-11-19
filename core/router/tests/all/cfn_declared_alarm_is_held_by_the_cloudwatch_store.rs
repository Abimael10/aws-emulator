// SPDX-License-Identifier: CC0-1.0

//! The alarm an `AWS::CloudWatch::Alarm` declares, read back through the WHOLE of the service
//! that owns it — on the assembled emulator, the one `core/router/src/lib.rs` builds for the
//! binary.
//!
//! The RULE under test: **every resource a stack reports created exists in the service that OWNS
//! it, under the identifier the stack publishes for it, before the stack reaches
//! `CREATE_COMPLETE` — and it exists in that service's own STORE, so every call of that service
//! which resolves the resource by its identifier finds it, not only the one read a client
//! happens to make.**
//!
//! For an `AWS::CloudWatch::Alarm` that means the alarm `PutMetricAlarm` would have created:
//!
//! * `DescribeAlarms(AlarmNames=[<the published name>])` reports EXACTLY ONE `MetricAlarms`
//!   entry, carrying the declaration's namespace, metric, statistic, period, evaluation
//!   periods, threshold, comparison operator and missing-data treatment, and a `StateValue` its
//!   own evaluation reached;
//! * `DescribeAlarmsForMetric`, which resolves alarms through the SERIES rather than the name,
//!   reports the same alarm;
//! * `PutCompositeAlarm`, whose rule may only name alarms the account holds ("You can use
//!   either alarm names or ARNs to reference the other alarms that are to be evaluated",
//!   `PutCompositeAlarmInput$AlarmRule`), ACCEPTS a rule over the declared alarm and REFUSES one
//!   over a name no stack created — so the store tells an alarm that exists apart from one that
//!   does not, which is what a client reading a state cannot otherwise do;
//! * `DisableAlarmActions` and `EnableAlarmActions` move that alarm's `ActionsEnabled`, so the
//!   deployed alarm is configurable and not merely readable.
//!
//! The reported case deployed an alarm on a custom series beside an execution role and a
//! function, reached `CREATE_COMPLETE`, and then found `MetricAlarms` EMPTY for the name the
//! template declared: a resource reported created that the owning service did not hold, which
//! makes a stack status unusable as a readiness signal and leaves a client unable to tell a
//! missing alarm from an alarm that has no data.
//!
//! The rows differ along what the rule generalises over rather than restating that one
//! template: how much of the SAME template belongs to OTHER services (an alarm on its own; an
//! alarm beside the IAM role and the Lambda function of the reported case; those plus the
//! DynamoDB table they work on), where the alarm's name comes from (a literal in the
//! declaration, a template parameter resolved by `Ref`), WHICH state the alarm's own evaluation
//! reaches with no datapoint published (over every value of `StateValue`, [`settled_state`]),
//! and the configuration itself — four statistics, four comparison operators, one and several
//! evaluation periods, three periods, and a threshold spelled as an integer and as a fraction.
//!
//! Nothing is waited out: the stack is polled to its terminal status and the alarm's state is
//! polled until the evaluation the clock owes it has been materialised.

use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, elements, form_encode, opt_element, outputs_of};

/// The Region every request below is signed for, and so the Region every resource it makes
/// belongs to.
const REGION: &str = "us-west-2";

/// The status a stack whose resources were all created reports.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// CloudWatch's `X-Amz-Target` prefix and its signing name (`sigv4Name: monitoring`).
const CLOUDWATCH_TARGET_PREFIX: &str = "GraniteServiceVersion20100801";
const CLOUDWATCH_SIGNING_NAME: &str = "monitoring";

/// DynamoDB's `X-Amz-Target` prefix.
const DYNAMODB_TARGET_PREFIX: &str = "DynamoDB_20120810";

/// The namespace every row's alarm watches: a custom one, so nothing here reads a series a
/// service publishes by itself.
const NAMESPACE: &str = "Emulator/DeployedAlarms";

/// The values `StateValue` publishes
/// (`smithy_query(query="shape",shape="StateValue")`) — the three states a readable alarm can
/// report, each one reached by a row below.
const STATE_VALUE_VALUES: [&str; 3] = ["OK", "ALARM", "INSUFFICIENT_DATA"];

/// The error class this service refuses a write it would not store with.
const REFUSAL_CODE: &str = "ValidationException";

/// The logical ids the templates below declare, and the stack output each name is published
/// under.
const ALARM_ID: &str = "Alarm";
const ROLE_ID: &str = "Role";
const FUNCTION_ID: &str = "Function";
const TABLE_ID: &str = "Table";
const ALARM_NAME_OUTPUT: &str = "AlarmName";

/// The template parameter a row may take the alarm's name from, which is also the key
/// `CreateStack` passes its value under.
const ALARM_NAME_PARAMETER: &str = "DeclaredAlarmName";

/// How long a row waits for a stack to settle or for an alarm's evaluation, and how often it
/// looks again. Generous only so a failure is a failure rather than a hang: every wait is on
/// the CONDITION it is about rather than on a span of the clock.
const DEADLINE: Duration = Duration::from_secs(60);
const LOOK_AGAIN: Duration = Duration::from_millis(100);

/// The inline handler a row's function carries: it is never invoked here — what is under test
/// is that the function the stack reported created exists in Lambda beside the alarm.
const HANDLER: &str = "def handler(event, context):\n    return {}\n";

/// The whole emulator over one engine, exactly as the binary assembles it.
fn router() -> MultiServiceRouter {
    let engine = aws_emulator_router::new_engine();
    let mut router = aws_emulator_router::build_router(engine);
    // Signature verification is the front door's concern, not this suite's.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `service` in [`REGION`], which is what
/// routes the request. The signature is never checked: the router above runs with request
/// authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

/// One `awsQuery` form call, answering the HTTP status and the body.
async fn query(
    router: &MultiServiceRouter,
    signing_name: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(signing_name))
        .body(Bytes::from(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One CloudFormation call, which must answer 200.
async fn cfn(router: &MultiServiceRouter, case: &str, action: &str, members: &[(&str, &str)]) -> String {
    let (status, body) = query(router, "cloudformation", "2010-05-15", action, members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CloudFormation {action}, got {status} with {body}"
    );
    body
}

/// One `X-Amz-Target` RPC call, as its status and parsed body.
async fn rpc(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .header("authorization", authorization(signing_name))
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One CloudWatch call (`awsJson1_0`), as its status and parsed body.
async fn try_cw(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    rpc(
        router,
        CLOUDWATCH_SIGNING_NAME,
        CLOUDWATCH_TARGET_PREFIX,
        operation,
        body,
    )
    .await
}

/// One CloudWatch call that must answer 200.
async fn cw(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = try_cw(router, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CloudWatch {operation}, got {status} with {parsed}"
    );
    parsed
}

/// One DynamoDB call that must answer 200.
async fn ddb(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = rpc(router, "dynamodb", DYNAMODB_TARGET_PREFIX, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DynamoDB {operation}, got {status} with {parsed}"
    );
    parsed
}

/// One Lambda `restJson1` read, as its status and parsed body.
async fn lambda_get(router: &MultiServiceRouter, uri: &str) -> (u16, Value) {
    let request = http::Request::builder()
        .method("GET")
        .uri(format!("https://lambda.{REGION}.amazonaws.com{uri}"))
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("authorization", authorization("lambda"))
        .header("content-type", "application/json")
        .body(Bytes::new())
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// The wire error code of a refusal body (`__type` on the awsJson protocols), without its
/// namespace prefix.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .map(|name| name.rsplit('#').next().unwrap_or(name).to_string())
        .unwrap_or_default()
}

/// The text a client reads off a refusal body.
fn error_message(body: &Value) -> String {
    ["message", "Message"]
        .iter()
        .find_map(|member| body.get(*member).and_then(Value::as_str))
        .unwrap_or_default()
        .to_string()
}

/// The refusal a composite rule naming an alarm the account holds none of answers with, which
/// names it by the ARN that name resolves to.
fn missing_alarms_refusal(arn: &str) -> String {
    format!("Could not save the composite alarm as alarms [{arn}] in the alarm rule do not exist")
}

/// How much of a row's template belongs to services OTHER than CloudWatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Estate {
    /// The alarm alone, so nothing else in the stack can be what made it exist.
    AlarmOnly,
    /// The execution role and the function of the reported case.
    RoleAndFunction,
    /// Those, plus the table the function works on.
    RoleFunctionAndTable,
}

impl Estate {
    fn declares_function(self) -> bool {
        matches!(self, Self::RoleAndFunction | Self::RoleFunctionAndTable)
    }

    fn declares_table(self) -> bool {
        self == Self::RoleFunctionAndTable
    }
}

/// Where the alarm's name comes from.
#[derive(Debug, Clone, Copy)]
enum Naming {
    /// The declaration states this name outright.
    Literal(&'static str),
    /// The declaration's `AlarmName` is `Ref` of a template parameter carrying this value.
    FromParameter(&'static str),
}

impl Naming {
    fn name(self) -> &'static str {
        match self {
            Self::Literal(name) | Self::FromParameter(name) => name,
        }
    }
}

/// One row: the estate a template declares, and what CloudWatch must then hold.
struct Row {
    case: &'static str,
    stack: &'static str,
    estate: Estate,
    naming: Naming,
    /// The metric the alarm watches: one per row, so `DescribeAlarmsForMetric` resolves a row's
    /// own alarm and no other.
    metric: &'static str,
    statistic: &'static str,
    comparison_operator: &'static str,
    period: i64,
    evaluation_periods: i64,
    /// The threshold exactly as the template spells it — an integer or a fraction, both of
    /// which CloudWatch holds as one `Double` (`PutMetricAlarmInput$Threshold`).
    threshold: Value,
    /// The `TreatMissingData` the declaration states, or `None` where it states none.
    treat_missing_data: Option<&'static str>,
}

/// The state an alarm no datapoint has been published for settles in, decided by its
/// missing-data treatment alone.
///
/// "Sets how this alarm is to handle missing data points. If `TreatMissingData` is omitted, the
/// default behavior of `missing` is used. […] Valid Values: `breaching | notBreaching | ignore |
/// missing`"
/// (`smithy_query(query="doc",shape="PutMetricAlarmInput",member="TreatMissingData")`):
/// `notBreaching` substitutes a non-breaching datapoint for every empty window, so the
/// evaluation reaches `OK`; `breaching` substitutes a breaching one, so it reaches `ALARM`;
/// `missing` substitutes nothing, which leaves the window short of the datapoints either
/// direction needs; and `ignore` keeps the state the alarm is in, which for an alarm nothing has
/// evaluated yet is the `INSUFFICIENT_DATA` its creation set ("When this operation creates an
/// alarm, the alarm state is immediately set to INSUFFICIENT_DATA", `PutMetricAlarm`).
fn settled_state(treat_missing_data: Option<&str>) -> &'static str {
    match treat_missing_data {
        Some("notBreaching") => "OK",
        Some("breaching") => "ALARM",
        _ => "INSUFFICIENT_DATA",
    }
}

/// The rows, spanning the class the rule covers. The statistics and comparison operators are the
/// model's own (`smithy_query(query="shape",shape="Statistic")`,
/// `smithy_query(query="shape",shape="ComparisonOperator")`).
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an alarm beside the execution role and the function of the reported case",
            stack: "deployed-alarm-role-function",
            estate: Estate::RoleAndFunction,
            naming: Naming::Literal("deployed-alarm-role-function"),
            metric: "RoleFunctionFailures",
            statistic: "Sum",
            comparison_operator: "GreaterThanOrEqualToThreshold",
            period: 60,
            evaluation_periods: 1,
            threshold: json!(1),
            treat_missing_data: None,
        },
        Row {
            case: "an alarm named from a template parameter, beside a role, a function and the \
                   table they work on",
            stack: "deployed-alarm-whole-estate",
            estate: Estate::RoleFunctionAndTable,
            naming: Naming::FromParameter("deployed-alarm-whole-estate"),
            metric: "WholeEstateLatency",
            statistic: "Average",
            comparison_operator: "LessThanThreshold",
            period: 300,
            evaluation_periods: 3,
            threshold: json!(0.5),
            treat_missing_data: Some("notBreaching"),
        },
        Row {
            case: "an alarm the template declares on its own",
            stack: "deployed-alarm-alone",
            estate: Estate::AlarmOnly,
            naming: Naming::Literal("deployed-alarm-alone"),
            metric: "AloneBacklog",
            statistic: "Maximum",
            comparison_operator: "GreaterThanThreshold",
            period: 60,
            evaluation_periods: 2,
            threshold: json!(42),
            treat_missing_data: Some("breaching"),
        },
        Row {
            case: "an alarm whose missing windows are ignored, beside a role and a function",
            stack: "deployed-alarm-ignoring",
            estate: Estate::RoleAndFunction,
            naming: Naming::FromParameter("deployed-alarm-ignoring"),
            metric: "IgnoringDepth",
            statistic: "Minimum",
            comparison_operator: "LessThanOrEqualToThreshold",
            period: 120,
            evaluation_periods: 1,
            threshold: json!(7.25),
            treat_missing_data: Some("ignore"),
        },
    ]
}

/// The name of each resource a row's template declares beside the alarm.
fn role_name(row: &Row) -> String {
    format!("{}-role", row.stack)
}

fn function_name(row: &Row) -> String {
    format!("{}-fn", row.stack)
}

fn table_name(row: &Row) -> String {
    format!("{}-table", row.stack)
}

/// The trust policy the declared execution role carries: Lambda has to be able to assume the
/// role a function runs as.
fn trust_policy() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": "lambda.amazonaws.com"},
            "Action": "sts:AssumeRole",
        }],
    })
}

/// The template a row deploys: its alarm, and whichever of the other services' resources the
/// row's estate declares beside it.
fn template_of(row: &Row) -> String {
    let mut alarm = json!({
        "Namespace": NAMESPACE,
        "MetricName": row.metric,
        "Statistic": row.statistic,
        "ComparisonOperator": row.comparison_operator,
        "Period": row.period,
        "EvaluationPeriods": row.evaluation_periods,
        "Threshold": row.threshold,
    });
    if let Some(treatment) = row.treat_missing_data {
        alarm["TreatMissingData"] = json!(treatment);
    }
    alarm["AlarmName"] = match row.naming {
        Naming::Literal(name) => json!(name),
        Naming::FromParameter(_) => json!({"Ref": ALARM_NAME_PARAMETER}),
    };

    let mut resources = Map::new();
    resources.insert(
        ALARM_ID.to_string(),
        json!({"Type": "AWS::CloudWatch::Alarm", "Properties": alarm}),
    );
    if row.estate.declares_function() {
        resources.insert(
            ROLE_ID.to_string(),
            json!({
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "RoleName": role_name(row),
                    "AssumeRolePolicyDocument": trust_policy(),
                },
            }),
        );
        resources.insert(
            FUNCTION_ID.to_string(),
            json!({
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": function_name(row),
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": {"Fn::GetAtt": [ROLE_ID, "Arn"]},
                    "Code": {"ZipFile": HANDLER},
                },
            }),
        );
    }
    if row.estate.declares_table() {
        resources.insert(
            TABLE_ID.to_string(),
            json!({
                "Type": "AWS::DynamoDB::Table",
                "Properties": {
                    "TableName": table_name(row),
                    "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
                    "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
                    "BillingMode": "PAY_PER_REQUEST",
                },
            }),
        );
    }

    let mut template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "an alarm and the resources of other services declared beside it",
        "Resources": Value::Object(resources),
        // The name the stack PUBLISHES for the alarm: `Ref` of an `AWS::CloudWatch::Alarm`
        // returns the alarm's name, which is the handle every CloudWatch call below takes.
        "Outputs": {ALARM_NAME_OUTPUT: {"Value": {"Ref": ALARM_ID}}},
    });
    if matches!(row.naming, Naming::FromParameter(_)) {
        template["Parameters"] = json!({ALARM_NAME_PARAMETER: {"Type": "String"}});
    }
    template.to_string()
}

/// Ask CloudFormation to create a row's stack, which it must accept.
async fn create(router: &MultiServiceRouter, row: &Row) {
    let template = template_of(row);
    let mut members: Vec<(&str, String)> = vec![
        ("StackName", row.stack.to_string()),
        ("TemplateBody", template),
    ];
    if row.estate.declares_function() {
        // The template declares a NAMED IAM role, which a create acknowledges before
        // CloudFormation provisions it.
        members.push(("Capabilities.member.1", "CAPABILITY_NAMED_IAM".to_string()));
    }
    if let Naming::FromParameter(name) = row.naming {
        members.push((
            "Parameters.member.1.ParameterKey",
            ALARM_NAME_PARAMETER.to_string(),
        ));
        members.push(("Parameters.member.1.ParameterValue", name.to_string()));
    }
    let borrowed: Vec<(&str, &str)> = members
        .iter()
        .map(|(key, value)| (*key, value.as_str()))
        .collect();
    cfn(router, row.case, "CreateStack", &borrowed).await;
}

/// Poll a row's stack until it first reports a TERMINAL status, answering that status and the
/// `DescribeStacks` answer it was read from. The wait is on that CONDITION, looked at again
/// until it holds or [`DEADLINE`] passes, so nothing is waited out past it.
async fn settled_stack(router: &MultiServiceRouter, row: &Row) -> (String, String) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let described = cfn(
            router,
            row.case,
            "DescribeStacks",
            &[("StackName", row.stack)],
        )
        .await;
        let status = opt_element(&described, "StackStatus").unwrap_or_default();
        if status != "CREATE_IN_PROGRESS" || Instant::now() >= deadline {
            return (status, described);
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Wait for a row's stack to reach `CREATE_COMPLETE`, reporting what failed when it does not,
/// and answer the name it published for the alarm.
async fn settled_name(router: &MultiServiceRouter, row: &Row) -> String {
    let (reported, described) = settled_stack(router, row).await;
    if reported != CREATE_COMPLETE {
        let events = cfn(
            router,
            row.case,
            "DescribeStackEvents",
            &[("StackName", row.stack)],
        )
        .await;
        let reasons = elements(&events, "ResourceStatusReason");
        panic!(
            "case {:?}: the stack must reach {CREATE_COMPLETE}, got {reported:?} with resource \
             failures {reasons:?}",
            row.case
        );
    }
    let published = outputs_of(&described)
        .get(ALARM_NAME_OUTPUT)
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        published,
        row.naming.name(),
        "case {:?}: expected the stack to publish the declared alarm name, got {published:?}",
        row.case
    );
    published
}

/// The one `MetricAlarms` entry `DescribeAlarms` reports for `name`, looked at again until its
/// `StateValue` is `wanted` — the state the evaluation the clock owes the alarm reaches, which
/// that read materialises.
async fn settled_alarm(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    wanted: &str,
) -> Value {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let described = cw(router, case, "DescribeAlarms", &json!({"AlarmNames": [name]})).await;
        let metric_alarms = described["MetricAlarms"].as_array().cloned().unwrap_or_default();
        assert_eq!(
            metric_alarms.len(),
            1,
            "case {case:?}: expected exactly one MetricAlarms entry for {name:?}, got {described}"
        );
        assert!(
            described["CompositeAlarms"]
                .as_array()
                .is_none_or(|alarms| alarms.is_empty()),
            // The default this read relies on: "if you omit this parameter or specify
            // MetricAlarms, the operation returns only a list of metric alarms"
            // (`smithy_query(query="doc",shape="DescribeAlarmsInput",member="AlarmTypes")`).
            "case {case:?}: expected no CompositeAlarms entry for the metric alarm {name:?}, got \
             {described}"
        );
        let last = metric_alarms[0].clone();
        if last["StateValue"].as_str() == Some(wanted) {
            return last;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the alarm {name:?} never reported {wanted:?}, last read {last}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_alarm_a_completed_stack_declared_is_one_the_cloudwatch_store_holds() {
    let router = router();
    let rows = rows();

    // Every row's stack is asked for first, so the settling of one overlaps the settling of the
    // others rather than being waited out in turn.
    for row in &rows {
        create(&router, row).await;
    }

    for row in &rows {
        let case = row.case;
        let name = settled_name(&router, row).await;

        // 1. The alarm itself, by the name the stack published: one entry, the declaration's
        //    configuration, and a state its own evaluation reached.
        let expected_state = settled_state(row.treat_missing_data);
        let alarm = settled_alarm(&router, case, &name, expected_state).await;
        for (member, expected) in [
            ("AlarmName", name.as_str()),
            ("Namespace", NAMESPACE),
            ("MetricName", row.metric),
            ("Statistic", row.statistic),
            ("ComparisonOperator", row.comparison_operator),
        ] {
            let held = alarm.get(member).and_then(Value::as_str);
            assert_eq!(
                held,
                Some(expected),
                "case {case:?}: expected CloudWatch to report {member} as {expected:?}, got \
                 {held:?}"
            );
        }
        for (member, expected) in [
            ("Period", row.period),
            ("EvaluationPeriods", row.evaluation_periods),
        ] {
            let held = alarm.get(member).and_then(Value::as_i64);
            assert_eq!(
                held,
                Some(expected),
                "case {case:?}: expected CloudWatch to report {member} as {expected}, got {held:?}"
            );
        }
        let held_threshold = alarm.get("Threshold").and_then(Value::as_f64);
        assert_eq!(
            held_threshold,
            row.threshold.as_f64(),
            "case {case:?}: expected CloudWatch to report the declared threshold {}, got \
             {held_threshold:?}",
            row.threshold
        );
        let held_treatment = alarm.get("TreatMissingData").and_then(Value::as_str);
        assert_eq!(
            held_treatment, row.treat_missing_data,
            "case {case:?}: expected CloudWatch to report TreatMissingData as {:?}, got \
             {held_treatment:?}",
            row.treat_missing_data
        );
        let state = alarm.get("StateValue").and_then(Value::as_str);
        assert!(
            state.is_some_and(|value| STATE_VALUE_VALUES.contains(&value)),
            "case {case:?}: expected a readable StateValue, one of {STATE_VALUE_VALUES:?}, got \
             {state:?}"
        );
        assert_eq!(
            state,
            Some(expected_state),
            "case {case:?}: expected the alarm's own evaluation to reach {expected_state:?} with \
             no datapoint published, got {state:?}"
        );
        let arn = alarm
            .get("AlarmArn")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        assert!(
            arn.ends_with(&format!(":alarm:{name}")),
            "case {case:?}: expected the alarm's ARN to name it, got {arn:?}"
        );

        // 2. The SERIES resolves the same alarm: a read that goes through the metric index
        //    rather than the name index finds the deployed alarm too.
        let for_metric = cw(
            &router,
            case,
            "DescribeAlarmsForMetric",
            &json!({"Namespace": NAMESPACE, "MetricName": row.metric}),
        )
        .await;
        let named: Vec<&str> = for_metric["MetricAlarms"]
            .as_array()
            .map(|alarms| {
                alarms
                    .iter()
                    .filter_map(|alarm| alarm["AlarmName"].as_str())
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            named,
            vec![name.as_str()],
            "case {case:?}: expected DescribeAlarmsForMetric to report exactly the declared \
             alarm, got {for_metric}"
        );

        // 3. A name no stack created is held by nothing: the read that could not tell a missing
        //    alarm from one with no data now tells them apart.
        let ghost = format!("{name}-absent");
        let missing = cw(
            &router,
            case,
            "DescribeAlarms",
            &json!({"AlarmNames": [&ghost]}),
        )
        .await;
        assert!(
            missing["MetricAlarms"]
                .as_array()
                .is_none_or(|alarms| alarms.is_empty()),
            "case {case:?}: expected no alarm for the name {ghost:?} nothing created, got \
             {missing}"
        );

        // 4. The store the deployed alarm lives in is the one another WRITE resolves references
        //    against: a composite rule over it is accepted, and the same rule over the name
        //    nothing created is refused — which no response could fake for an alarm that was
        //    only reported created.
        let roll_up = format!("{name}-rollup");
        let rule = format!("ALARM(\"{name}\")");
        cw(
            &router,
            case,
            "PutCompositeAlarm",
            &json!({"AlarmName": &roll_up, "AlarmRule": &rule}),
        )
        .await;
        let described = cw(
            &router,
            case,
            "DescribeAlarms",
            // "If you omit this parameter, only metric alarms are returned, even if composite
            // alarms or log alarms exist in the account."
            // (`smithy_query(query="doc",shape="DescribeAlarmsInput",member="AlarmTypes")`)
            &json!({"AlarmNames": [&roll_up], "AlarmTypes": ["CompositeAlarm"]}),
        )
        .await;
        assert_eq!(
            described["CompositeAlarms"][0]["AlarmRule"].as_str(),
            Some(rule.as_str()),
            "case {case:?}: expected the composite alarm over the declared alarm to be stored \
             with its rule, got {described}"
        );
        assert_eq!(
            described["CompositeAlarms"][0]["AlarmName"].as_str(),
            Some(roll_up.as_str()),
            "case {case:?}: expected the composite alarm to be stored under its own name, got \
             {described}"
        );

        let ghost_arn = arn.replace(&format!(":alarm:{name}"), &format!(":alarm:{ghost}"));
        let (status, refused) = try_cw(
            &router,
            "PutCompositeAlarm",
            &json!({
                "AlarmName": format!("{roll_up}-over-nothing"),
                "AlarmRule": format!("ALARM(\"{ghost}\")"),
            }),
        )
        .await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (400, REFUSAL_CODE),
            "case {case:?}: expected HTTP 400 {REFUSAL_CODE} for a rule over the name nothing \
             created, got {status} with {refused}"
        );
        assert_eq!(
            error_message(&refused),
            missing_alarms_refusal(&ghost_arn),
            "case {case:?}: expected the refusal to name the missing alarm {ghost_arn:?}, got {}",
            error_message(&refused)
        );

        // 5. The deployed alarm is CONFIGURABLE and not merely readable: its actions are turned
        //    off and on again through the calls that address an alarm by name.
        for (operation, expected) in [
            ("DisableAlarmActions", false),
            ("EnableAlarmActions", true),
        ] {
            cw(&router, case, operation, &json!({"AlarmNames": [&name]})).await;
            let after = cw(
                &router,
                case,
                "DescribeAlarms",
                &json!({"AlarmNames": [&name]}),
            )
            .await;
            let held = after["MetricAlarms"][0]["ActionsEnabled"].as_bool();
            assert_eq!(
                held,
                Some(expected),
                "case {case:?}: expected {operation} to leave ActionsEnabled {expected}, got \
                 {after}"
            );
        }

        // 6. The rule is about EVERY resource the stack reported created, so the row's
        //    neighbours are read back through the services that own them as well.
        if row.estate.declares_function() {
            let role = role_name(row);
            let (status, body) = query(
                &router,
                "iam",
                "2010-05-08",
                "GetRole",
                &[("RoleName", &role)],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected IAM to hold the declared role {role:?}, got {status} \
                 with {body}"
            );
            assert_eq!(
                element(&body, "RoleName"),
                role,
                "case {case:?}: expected IAM to report the declared role name, got {body}"
            );

            let function = function_name(row);
            let (status, configuration) = lambda_get(
                &router,
                &format!("/2015-03-31/functions/{function}/configuration"),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected Lambda to hold the declared function {function:?}, got \
                 {status} with {configuration}"
            );
            assert_eq!(
                configuration["FunctionName"].as_str(),
                Some(function.as_str()),
                "case {case:?}: expected Lambda to report the declared function name, got \
                 {configuration}"
            );
        }
        if row.estate.declares_table() {
            let table = table_name(row);
            let described = ddb(&router, case, "DescribeTable", &json!({"TableName": &table})).await;
            assert_eq!(
                described["Table"]["TableName"].as_str(),
                Some(table.as_str()),
                "case {case:?}: expected DynamoDB to hold the declared table {table:?}, got \
                 {described}"
            );
        }
    }
}
