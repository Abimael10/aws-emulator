// SPDX-License-Identifier: CC0-1.0

//! What a client that deployed a stack can DISCOVER about the stack's resources in the services
//! that own them, holding no identifier of its own — on the assembled emulator, the one
//! `core/router/src/lib.rs` builds for the binary.
//!
//! The RULE under test: **every resource a stack reports created exists in the service that OWNS
//! it before the stack reaches `CREATE_COMPLETE`, and it is reported by that service's own
//! LISTING of resources of its kind — the listing narrowed by what the resource is attached to
//! rather than by an id the caller already holds — so a client that knows only the template it
//! deployed finds the resource, reads its configuration off that entry and resolves it by the
//! identifier the entry carries.**
//!
//! For the two types here that is:
//!
//! * AWS Lambda's `ListEventSourceMappings` narrowed by `FunctionName` ("The name or ARN of the
//!   Lambda function", `smithy_query(query="operation",operation="ListEventSourceMappings")`)
//!   reports ONE entry per `AWS::Lambda::EventSourceMapping` the template attached to that
//!   function — under the function's name and under its ARN, both of which that member admits —
//!   each entry carrying the `UUID` the stack published for the resource, the `EventSourceArn`
//!   of the stream the declaration named, the `FunctionArn` of the deployed function and the
//!   declaration's `BatchSize` and `StartingPosition`; and each of those UUIDs is one
//!   `GetEventSourceMapping` resolves;
//! * Amazon CloudWatch's `DescribeAlarms` narrowed by `AlarmNames` reports exactly one
//!   `MetricAlarms` entry for the name the stack published for an `AWS::CloudWatch::Alarm`,
//!   carrying the declaration's namespace, metric, dimensions, statistic, period, evaluation
//!   periods, threshold and comparison operator, and a `StateValue` — while the same read of a
//!   name no stack created reports NONE, so an empty listing really does mean the alarm is
//!   absent.
//!
//! The reported case deployed one stack carrying all of it — a streamed DynamoDB table, an
//! execution role, a function, the mapping from the stream to that function and an alarm on a
//! Lambda metric — reached `CREATE_COMPLETE`, and then found BOTH listings empty: the first
//! element of each was indexed and there was none. A client in that position cannot inspect,
//! disable or delete the trigger, and cannot tell a missing alarm from one that has no data.
//!
//! Neighbours own the halves this suite does not repeat:
//! [`super::cloudformation_provisions_lambda_event_source_mappings`] pins a declared mapping
//! read back BY the id the stack published, over the sources that are not a table's stream, and
//! the refusals a mapping's own create answers;
//! [`super::cfn_declared_alarm_is_held_by_the_cloudwatch_store`] pins a declared alarm's own
//! evaluated state, its resolution through the metric series and its configurability; and
//! [`super::cfn_ddb_stream_esm_lambda_pipeline`] pins that the records of the declared stream
//! really reach the declared function. What only this suite reads is the DISCOVERY both
//! listings are: the stack's function and the stack's alarm name are all the client has.
//!
//! The rows differ along what the rule generalises over: how many mappings hang off the one
//! function (one, and two from two streamed tables), how the declaration NAMES that function
//! (`Ref`, which resolves to the function's name, and `Fn::GetAtt … Arn`, which resolves to its
//! ARN), whether the mapping is declared enabled or disabled (so the state the listing reports
//! is the declared one), where the alarm's name comes from (a literal in the declaration, and a
//! name CloudFormation generates because the declaration states none), and the alarm's own
//! configuration — three statistics, three comparison operators, one and two evaluation
//! periods, three periods, a threshold spelled as an integer and as a fraction, and a stated and
//! an unstated missing-data treatment.
//!
//! Nothing is waited out: every stack is asked for before any is read, each is polled to its
//! terminal status, and the mapping listing is looked at again until the state the clock owes the
//! mappings has been reached.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

// Every wire caller, the assembled router and the vocabulary of both owning services come from
// the suites that own them rather than from second copies here, so this suite cannot drift from
// them.
use crate::cfn_ddb_stream_esm_lambda_pipeline::{create_failures, settled};
use crate::cloudformation_apigatewayv2_resources::{cloudformation, REGION};
use crate::cloudformation_dynamodb_table_in_place_update::described_table;
use crate::cloudformation_provisions_lambda_event_source_mappings::{
    is_mapping_uuid, MAPPINGS_URI, MAPPING_STATE_DISABLED, MAPPING_STATE_ENABLED,
};
use crate::cloudwatch_alarm_action_reaches_its_subscribed_queue::{cw, router as emulator};
use crate::logs_subscription_filter_to_lambda::lambda;
use crate::query_wire::outputs_of;

/// The status a stack whose resources were all created reports.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The logical ids the templates below declare, and the prefixes the per-stream ones are built
/// from.
const SOURCE_TABLE_PREFIX: &str = "SourceTable";
const MAPPING_PREFIX: &str = "StreamMapping";
const ROLE: &str = "FunctionRole";
const FUNCTION: &str = "StreamFunction";
const ALARM: &str = "FunctionAlarm";

/// The stack outputs every template publishes: what a client holds after a deployment, and all
/// this suite reads before it asks the owning services.
const FUNCTION_NAME_OUTPUT: &str = "FunctionName";
const FUNCTION_ARN_OUTPUT: &str = "FunctionArn";
const ALARM_NAME_OUTPUT: &str = "AlarmName";

/// The attribute the source tables are keyed by.
const KEY_ATTRIBUTE: &str = "pk";

/// The billing mode and stream view every source table is declared with
/// (`CreateTable$BillingMode`: "one of: PAY_PER_REQUEST | PROVISIONED").
const PAY_PER_REQUEST: &str = "PAY_PER_REQUEST";
const STREAM_VIEW_TYPE: &str = "NEW_AND_OLD_IMAGES";

/// AWS's own published execution-role policy for a function that reads a DynamoDB stream, which
/// grants the stream reads Lambda's poller makes on the function's behalf — without it the
/// mapping's own create is refused, which is a neighbour's subject rather than this one's.
const DYNAMODB_EXECUTION_POLICY: &str =
    "arn:aws:iam::aws:policy/service-role/AWSLambdaDynamoDBExecutionRole";

/// The `python3.x` runtime every function here declares, and a handler that is never invoked:
/// what is read below is the control plane the deployment left behind.
const RUNTIME: &str = "python3.12";
const HANDLER: &str = "index.handler";
const HANDLER_SOURCE: &str = "def handler(event, context):\n    return {}\n";

/// The namespace and the dimension name the alarms watch a Lambda metric under — the series that
/// service publishes per function.
const LAMBDA_NAMESPACE: &str = "AWS/Lambda";
const FUNCTION_DIMENSION: &str = "FunctionName";

/// The values `StateValue` publishes
/// (`smithy_query(query="shape",shape="StateValue")`) — the states a readable alarm can report,
/// one of which an alarm a client can read is in.
const STATE_VALUE_VALUES: [&str; 3] = ["OK", "ALARM", "INSUFFICIENT_DATA"];

/// What is appended to a published alarm name to address a name NOTHING declared, which is the
/// contrast an empty listing is only meaningful against.
const UNDECLARED_SUFFIX: &str = "-no-stack-declared-this";

/// How long ONE test waits, in all, for the work the clock owes it — the stacks settling and the
/// mappings reaching the state they were declared in. One deadline is taken at the start and
/// every wait looks again until its condition holds or that instant passes, so a test that will
/// fail is bounded by this span rather than by the sum of its waits.
///
/// Generous on purpose: the spans the services behind a stack choose take the time they take, so
/// a loaded host cannot decide the outcome. Nothing here asserts on elapsed time.
const WAIT: Duration = Duration::from_secs(180);
/// See [`WAIT`].
const LOOK_AGAIN: Duration = Duration::from_millis(100);

/// How a mapping's declaration names the function it is attached to — both forms resolve to a
/// value `EventSourceMapping$FunctionName` admits ("The name or ARN of the Lambda function").
#[derive(Clone, Copy, Debug)]
enum FunctionReference {
    /// `Ref` of an `AWS::Lambda::Function`, which "returns the function name".
    ByName,
    /// `Fn::GetAtt … Arn`, the function's ARN.
    ByArn,
}

impl FunctionReference {
    /// The declaration this form is written as.
    fn declaration(self) -> Value {
        match self {
            Self::ByName => json!({ "Ref": FUNCTION }),
            Self::ByArn => json!({ "Fn::GetAtt": [FUNCTION, "Arn"] }),
        }
    }
}

/// Where the alarm's own name comes from.
#[derive(Clone, Copy, Debug)]
enum AlarmNaming {
    /// Stated in the declaration.
    Declared(&'static str),
    /// Stated nowhere, so the name is CloudFormation's to generate — a name the client can only
    /// have from the stack, which is what makes the listing the only way to reach the alarm.
    GeneratedByCloudFormation,
}

/// One row: the estate a template declares, and what the two owning services must then report.
struct Row {
    case: &'static str,
    stack: &'static str,
    /// How many streamed tables the template declares — one mapping per table onto the one
    /// function, so the listing narrowed by that function reports exactly this many entries.
    mappings: usize,
    function_reference: FunctionReference,
    batch_size: i64,
    /// The position the mapping reads the stream from
    /// (`smithy_query(query="shape",shape="EventSourcePosition")`: `TRIM_HORIZON`, `LATEST`,
    /// `AT_TIMESTAMP`; the last is not a DynamoDB stream's).
    starting_position: &'static str,
    /// The `Enabled` the declaration states, or `None` where it states none.
    enabled: Option<bool>,
    alarm_naming: AlarmNaming,
    metric: &'static str,
    /// The statistic and the comparison operator, from the model's own value lists
    /// (`smithy_query(query="shape",shape="Statistic")`,
    /// `smithy_query(query="shape",shape="ComparisonOperator")`).
    statistic: &'static str,
    comparison_operator: &'static str,
    period: i64,
    evaluation_periods: i64,
    /// The threshold exactly as the template spells it — an integer or a fraction, both of which
    /// CloudWatch holds as one `Double` (`PutMetricAlarmInput$Threshold`).
    threshold: Value,
    /// The `TreatMissingData` the declaration states, or `None` where it states none.
    treat_missing_data: Option<&'static str>,
}

impl Row {
    /// The state the listing must report the row's mappings in: a mapping declared disabled is
    /// `Disabled`, and one declared enabled — or not declared either way, since
    /// `EventSourceMapping$Enabled` defaults to enabled — is `Enabled`.
    fn expected_mapping_state(&self) -> &'static str {
        match self.enabled {
            Some(false) => MAPPING_STATE_DISABLED,
            _ => MAPPING_STATE_ENABLED,
        }
    }
}

/// The rows, spanning the class the rule covers.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "one stream mapped onto a function named by Ref, beside an alarm the \
                   declaration names",
            stack: "xsvc-declared-trigger-alarm-one-stream",
            mappings: 1,
            function_reference: FunctionReference::ByName,
            batch_size: 1,
            starting_position: "LATEST",
            enabled: None,
            alarm_naming: AlarmNaming::Declared("xsvc-declared-trigger-alarm-one-stream-errors"),
            metric: "Errors",
            statistic: "Sum",
            comparison_operator: "GreaterThanThreshold",
            period: 60,
            evaluation_periods: 1,
            threshold: json!(1),
            treat_missing_data: None,
        },
        Row {
            case: "two streams mapped onto one function named by its ARN, beside an alarm whose \
                   name CloudFormation generates",
            stack: "xsvc-declared-trigger-alarm-two-streams",
            mappings: 2,
            function_reference: FunctionReference::ByArn,
            batch_size: 10,
            starting_position: "TRIM_HORIZON",
            enabled: Some(true),
            alarm_naming: AlarmNaming::GeneratedByCloudFormation,
            metric: "Duration",
            statistic: "Average",
            comparison_operator: "GreaterThanOrEqualToThreshold",
            period: 300,
            evaluation_periods: 2,
            threshold: json!(0.5),
            treat_missing_data: Some("notBreaching"),
        },
        Row {
            case: "a mapping declared DISABLED, beside an alarm the declaration names",
            stack: "xsvc-declared-trigger-alarm-disabled",
            mappings: 1,
            function_reference: FunctionReference::ByName,
            batch_size: 5,
            starting_position: "TRIM_HORIZON",
            enabled: Some(false),
            alarm_naming: AlarmNaming::Declared("xsvc-declared-trigger-alarm-disabled-throttles"),
            metric: "Throttles",
            statistic: "Maximum",
            comparison_operator: "LessThanThreshold",
            period: 120,
            evaluation_periods: 1,
            threshold: json!(2.5),
            treat_missing_data: Some("breaching"),
        },
    ]
}

/// The logical id of one row's n-th source table, and of the mapping that reads its stream.
fn table_id(index: usize) -> String {
    format!("{SOURCE_TABLE_PREFIX}{index}")
}

/// See [`table_id`].
fn mapping_id(index: usize) -> String {
    format!("{MAPPING_PREFIX}{index}")
}

/// The output one row's n-th table name, stream and mapping id are published under.
fn table_name_output(index: usize) -> String {
    format!("TableName{index}")
}

/// See [`table_name_output`].
fn stream_arn_output(index: usize) -> String {
    format!("StreamArn{index}")
}

/// See [`table_name_output`].
fn mapping_id_output(index: usize) -> String {
    format!("MappingId{index}")
}

/// The trust policy a Lambda execution role carries.
fn trust_policy() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "lambda.amazonaws.com" },
            "Action": "sts:AssumeRole",
        }],
    })
}

/// One row's whole estate as one template: its streamed tables, the execution role, the
/// function, one mapping per stream onto that function, and the alarm on the function's metric.
fn template_of(row: &Row) -> String {
    let mut resources = Map::new();
    let mut outputs = Map::new();

    for index in 0..row.mappings {
        let table = table_id(index);
        resources.insert(
            table.clone(),
            json!({
                "Type": "AWS::DynamoDB::Table",
                "Properties": {
                    "AttributeDefinitions": [
                        { "AttributeName": KEY_ATTRIBUTE, "AttributeType": "S" }
                    ],
                    "KeySchema": [{ "AttributeName": KEY_ATTRIBUTE, "KeyType": "HASH" }],
                    "BillingMode": PAY_PER_REQUEST,
                    "StreamSpecification": { "StreamViewType": STREAM_VIEW_TYPE },
                },
            }),
        );
        outputs.insert(
            table_name_output(index),
            json!({ "Value": { "Ref": table.clone() } }),
        );
        outputs.insert(
            stream_arn_output(index),
            json!({ "Value": { "Fn::GetAtt": [table, "StreamArn"] } }),
        );
    }

    resources.insert(
        ROLE.to_string(),
        json!({
            "Type": "AWS::IAM::Role",
            "Properties": {
                "AssumeRolePolicyDocument": trust_policy(),
                "ManagedPolicyArns": [DYNAMODB_EXECUTION_POLICY],
            },
        }),
    );
    resources.insert(
        FUNCTION.to_string(),
        json!({
            "Type": "AWS::Lambda::Function",
            "Properties": {
                "Runtime": RUNTIME,
                "Handler": HANDLER,
                "Role": { "Fn::GetAtt": [ROLE, "Arn"] },
                "Code": { "ZipFile": HANDLER_SOURCE },
            },
        }),
    );

    for index in 0..row.mappings {
        let mut properties = Map::new();
        properties.insert(
            "EventSourceArn".to_string(),
            json!({ "Fn::GetAtt": [table_id(index), "StreamArn"] }),
        );
        properties.insert(
            "FunctionName".to_string(),
            row.function_reference.declaration(),
        );
        properties.insert(
            "StartingPosition".to_string(),
            json!(row.starting_position),
        );
        properties.insert("BatchSize".to_string(), json!(row.batch_size));
        if let Some(enabled) = row.enabled {
            properties.insert("Enabled".to_string(), json!(enabled));
        }
        let logical = mapping_id(index);
        resources.insert(
            logical.clone(),
            json!({
                "Type": "AWS::Lambda::EventSourceMapping",
                "Properties": Value::Object(properties),
            }),
        );
        outputs.insert(
            mapping_id_output(index),
            json!({ "Value": { "Ref": logical } }),
        );
    }

    let mut alarm = Map::new();
    if let AlarmNaming::Declared(name) = row.alarm_naming {
        alarm.insert("AlarmName".to_string(), json!(name));
    }
    alarm.insert("Namespace".to_string(), json!(LAMBDA_NAMESPACE));
    alarm.insert("MetricName".to_string(), json!(row.metric));
    alarm.insert(
        "Dimensions".to_string(),
        json!([{ "Name": FUNCTION_DIMENSION, "Value": { "Ref": FUNCTION } }]),
    );
    alarm.insert("Statistic".to_string(), json!(row.statistic));
    alarm.insert("Period".to_string(), json!(row.period));
    alarm.insert("EvaluationPeriods".to_string(), json!(row.evaluation_periods));
    alarm.insert("Threshold".to_string(), row.threshold.clone());
    alarm.insert(
        "ComparisonOperator".to_string(),
        json!(row.comparison_operator),
    );
    if let Some(treatment) = row.treat_missing_data {
        alarm.insert("TreatMissingData".to_string(), json!(treatment));
    }
    resources.insert(
        ALARM.to_string(),
        json!({ "Type": "AWS::CloudWatch::Alarm", "Properties": Value::Object(alarm) }),
    );

    outputs.insert(
        FUNCTION_NAME_OUTPUT.to_string(),
        json!({ "Value": { "Ref": FUNCTION } }),
    );
    outputs.insert(
        FUNCTION_ARN_OUTPUT.to_string(),
        json!({ "Value": { "Fn::GetAtt": [FUNCTION, "Arn"] } }),
    );
    outputs.insert(
        ALARM_NAME_OUTPUT.to_string(),
        json!({ "Value": { "Ref": ALARM } }),
    );

    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "a streamed table triggering a function, watched by an alarm",
        "Resources": Value::Object(resources),
        "Outputs": Value::Object(outputs),
    })
    .to_string()
}

/// Ask for one row's stack, which CloudFormation accepts before its resources are provisioned.
async fn create(router: &MultiServiceRouter, row: &Row) {
    let (status, answered) = cloudformation(
        router,
        "CreateStack",
        &[
            ("StackName", row.stack),
            ("TemplateBody", &template_of(row)),
            // The template declares an IAM role, which a create acknowledges before
            // CloudFormation provisions it.
            ("Capabilities.member.1", "CAPABILITY_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {:?}: expected HTTP 200 from CreateStack, got {status} with body {answered}",
        row.case
    );
}

/// One row's stack outputs, once the stack has settled `CREATE_COMPLETE`.
async fn published(
    router: &MultiServiceRouter,
    row: &Row,
    giving_up_at: Instant,
) -> BTreeMap<String, String> {
    let (status, described) = settled(router, row.stack, giving_up_at).await;
    let failures = if status == CREATE_COMPLETE {
        Vec::new()
    } else {
        create_failures(router, row.stack).await
    };
    assert_eq!(
        status, CREATE_COMPLETE,
        "case {:?}: expected the stack to settle {CREATE_COMPLETE}, got {described} with refused \
         resources {failures:?}",
        row.case
    );
    outputs_of(&described)
}

/// One stack output, which the template publishes for every row.
fn output(case: &str, outputs: &BTreeMap<String, String>, name: &str) -> String {
    outputs
        .get(name)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: no {name} output in {outputs:?}"))
}

/// Every mapping AWS Lambda reports for one function, through `ListEventSourceMappings` narrowed
/// by `FunctionName` — the discovery a client holding no mapping id makes.
async fn mappings_of_function(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
) -> Vec<Value> {
    let answered = lambda(
        router,
        "GET",
        &format!("{MAPPINGS_URI}?FunctionName={function}"),
        None,
    )
    .await;
    assert_eq!(
        answered.status,
        200,
        "case {case:?}: expected HTTP 200 from ListEventSourceMappings for {function:?}, got {} \
         with body {:?}",
        answered.status,
        answered.text()
    );
    answered
        .json()
        .get("EventSourceMappings")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// The listing above, looked at again until it reports `wanted` entries all in `state` — the
/// state the clock owes a mapping its owning service has just created.
async fn listed_mappings_in_state(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    wanted: usize,
    state: &str,
    giving_up_at: Instant,
) -> Vec<Value> {
    loop {
        let listed = mappings_of_function(router, case, function).await;
        let settled = listed.len() == wanted
            && listed
                .iter()
                .all(|mapping| mapping.get("State").and_then(Value::as_str) == Some(state));
        if settled || Instant::now() >= giving_up_at {
            return listed;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The `UUID`s one listing reported.
fn uuids_of(case: &str, listed: &[Value]) -> BTreeSet<String> {
    listed
        .iter()
        .map(|mapping| {
            mapping
                .get("UUID")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("case {case:?}: a listed mapping carries no UUID: {mapping}"))
                .to_string()
        })
        .collect()
}

/// The stream one of a row's tables reports as its own, read through the service that owns the
/// table rather than off the stack's answer.
async fn live_stream_arn(router: &MultiServiceRouter, case: &str, table: &str) -> String {
    let (status, described) = described_table(router, table).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeTable for {table:?}, got {status} with \
         body {described}"
    );
    described
        .pointer("/Table/LatestStreamArn")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!("case {case:?}: no LatestStreamArn for the streamed table {table:?}: {described}")
        })
        .to_string()
}

/// The `MetricAlarms` entries `DescribeAlarms` reports for one name.
async fn described_alarms(router: &MultiServiceRouter, case: &str, name: &str) -> Vec<Value> {
    let described = cw(
        router,
        case,
        REGION,
        "DescribeAlarms",
        &json!({ "AlarmNames": [name] }),
    )
    .await;
    described
        .get("MetricAlarms")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// Both listings report what the stack created, under what the client holds.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_completed_stacks_trigger_and_alarm_are_reported_by_their_owning_services_listings() {
    let router = emulator();
    let rows = rows();
    let giving_up_at = Instant::now() + WAIT;

    // Every row's stack is asked for first, so one stack's settling overlaps the others' rather
    // than being waited out in turn.
    for row in &rows {
        create(&router, row).await;
    }

    for row in &rows {
        let case = row.case;
        let outputs = published(&router, row, giving_up_at).await;
        let function_name = output(case, &outputs, FUNCTION_NAME_OUTPUT);
        let function_arn = output(case, &outputs, FUNCTION_ARN_OUTPUT);
        let alarm_name = output(case, &outputs, ALARM_NAME_OUTPUT);
        assert!(
            !alarm_name.is_empty(),
            "case {case:?}: expected the stack to publish a name for the declared alarm, got \
             {outputs:?}"
        );

        // What the stack itself published for the mappings, and the streams the tables report:
        // the values the listing is then checked against, each from a source other than the
        // answer under test.
        let stack_uuids: BTreeSet<String> = (0..row.mappings)
            .map(|index| output(case, &outputs, &mapping_id_output(index)))
            .collect();
        for uuid in &stack_uuids {
            assert!(
                is_mapping_uuid(uuid),
                "case {case:?}: expected the stack to publish a mapping UUID, got {uuid:?}"
            );
        }
        let mut live_streams: BTreeSet<String> = BTreeSet::new();
        for index in 0..row.mappings {
            let table = output(case, &outputs, &table_name_output(index));
            let live = live_stream_arn(&router, case, &table).await;
            let stack_stream = output(case, &outputs, &stream_arn_output(index));
            assert_eq!(
                stack_stream, live,
                "case {case:?}: expected the stream the stack published for {table:?} to be the \
                 one the table reports ({live:?}), got {stack_stream:?}"
            );
            live_streams.insert(live);
        }

        // 1. AWS Lambda's own listing, narrowed by the function the client deployed: one entry
        //    per declared mapping, carrying the declaration and the stack's own ids.
        let expected_state = row.expected_mapping_state();
        let listed = listed_mappings_in_state(
            &router,
            case,
            &function_name,
            row.mappings,
            expected_state,
            giving_up_at,
        )
        .await;
        assert_eq!(
            listed.len(),
            row.mappings,
            "case {case:?}: expected ListEventSourceMappings(FunctionName={function_name:?}) to \
             report {} mapping(s) the stack created, got {listed:?}",
            row.mappings
        );
        let listed_uuids = uuids_of(case, &listed);
        assert_eq!(
            listed_uuids, stack_uuids,
            "case {case:?}: expected the listing to report the mappings the stack published \
             ({stack_uuids:?}), got {listed_uuids:?}"
        );
        let mut listed_streams: BTreeSet<String> = BTreeSet::new();
        for mapping in &listed {
            let state = mapping.get("State").and_then(Value::as_str);
            assert_eq!(
                state,
                Some(expected_state),
                "case {case:?}: expected the listed mapping to report State {expected_state:?}, \
                 got {state:?} in {mapping}"
            );
            let reported_function = mapping.get("FunctionArn").and_then(Value::as_str);
            assert_eq!(
                reported_function,
                Some(function_arn.as_str()),
                "case {case:?}: expected the listed mapping to name the deployed function \
                 ({function_arn:?}), got {reported_function:?}"
            );
            let batch_size = mapping.get("BatchSize").and_then(Value::as_i64);
            assert_eq!(
                batch_size,
                Some(row.batch_size),
                "case {case:?}: expected the listed mapping to carry the declared BatchSize {}, \
                 got {batch_size:?}",
                row.batch_size
            );
            let position = mapping.get("StartingPosition").and_then(Value::as_str);
            assert_eq!(
                position,
                Some(row.starting_position),
                "case {case:?}: expected the listed mapping to carry the declared \
                 StartingPosition {:?}, got {position:?}",
                row.starting_position
            );
            let source = mapping
                .get("EventSourceArn")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            listed_streams.insert(source);
        }
        assert_eq!(
            listed_streams, live_streams,
            "case {case:?}: expected the listed mappings to read the declared streams \
             ({live_streams:?}), got {listed_streams:?}"
        );

        // 2. The same narrowing by the function's ARN, which that member admits too, reports the
        //    same mappings.
        let by_arn = mappings_of_function(&router, case, &function_arn).await;
        let by_arn_uuids = uuids_of(case, &by_arn);
        assert_eq!(
            by_arn_uuids, stack_uuids,
            "case {case:?}: expected ListEventSourceMappings(FunctionName={function_arn:?}) to \
             report the same mappings ({stack_uuids:?}), got {by_arn_uuids:?}"
        );

        // 3. Each id the listing reported is one the by-id read resolves, so a client that
        //    discovered a mapping can then inspect it.
        for uuid in &listed_uuids {
            let answered = lambda(&router, "GET", &format!("{MAPPINGS_URI}/{uuid}"), None).await;
            assert_eq!(
                answered.status,
                200,
                "case {case:?}: expected GetEventSourceMapping to resolve the listed id \
                 {uuid:?}, got {} with body {:?}",
                answered.status,
                answered.text()
            );
            let read = answered.json();
            assert_eq!(
                read.get("UUID").and_then(Value::as_str),
                Some(uuid.as_str()),
                "case {case:?}: expected the by-id read of {uuid:?} to be that mapping, got {read}"
            );
        }

        // 4. Amazon CloudWatch's own listing, narrowed by the name the stack published for the
        //    declared alarm: exactly one entry, carrying the declaration's configuration.
        let alarms = described_alarms(&router, case, &alarm_name).await;
        assert_eq!(
            alarms.len(),
            1,
            "case {case:?}: expected DescribeAlarms(AlarmNames=[{alarm_name:?}]) to report \
             exactly the alarm the stack created, got {alarms:?}"
        );
        let alarm = &alarms[0];
        for (member, expected) in [
            ("AlarmName", alarm_name.as_str()),
            ("Namespace", LAMBDA_NAMESPACE),
            ("MetricName", row.metric),
            ("Statistic", row.statistic),
            ("ComparisonOperator", row.comparison_operator),
        ] {
            let held = alarm.get(member).and_then(Value::as_str);
            assert_eq!(
                held,
                Some(expected),
                "case {case:?}: expected the reported alarm's {member} to be {expected:?}, got \
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
                "case {case:?}: expected the reported alarm's {member} to be {expected}, got \
                 {held:?}"
            );
        }
        let threshold = alarm.get("Threshold").and_then(Value::as_f64);
        assert_eq!(
            threshold,
            row.threshold.as_f64(),
            "case {case:?}: expected the reported alarm's Threshold to be the declared {}, got \
             {threshold:?}",
            row.threshold
        );
        let treatment = alarm.get("TreatMissingData").and_then(Value::as_str);
        assert_eq!(
            treatment, row.treat_missing_data,
            "case {case:?}: expected the reported alarm's TreatMissingData to be {:?}, got \
             {treatment:?}",
            row.treat_missing_data
        );
        let dimensions = alarm
            .get("Dimensions")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            dimensions,
            vec![json!({ "Name": FUNCTION_DIMENSION, "Value": function_name })],
            "case {case:?}: expected the reported alarm to watch the deployed function's series, \
             got {dimensions:?}"
        );
        let state = alarm.get("StateValue").and_then(Value::as_str);
        assert!(
            state.is_some_and(|value| STATE_VALUE_VALUES.contains(&value)),
            "case {case:?}: expected the reported alarm to carry a readable StateValue, one of \
             {STATE_VALUE_VALUES:?}, got {state:?}"
        );

        // 5. The same read of a name nothing declared reports no alarm, so an EMPTY listing is
        //    what an absent alarm looks like and the entry above is what a present one does.
        let undeclared = format!("{alarm_name}{UNDECLARED_SUFFIX}");
        let none = described_alarms(&router, case, &undeclared).await;
        assert!(
            none.is_empty(),
            "case {case:?}: expected DescribeAlarms(AlarmNames=[{undeclared:?}]) to report no \
             alarm, got {none:?}"
        );
    }
}
