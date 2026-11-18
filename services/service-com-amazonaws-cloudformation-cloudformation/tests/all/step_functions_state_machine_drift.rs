// SPDX-License-Identifier: CC0-1.0

//! What a drift check reports about a stack-managed `AWS::StepFunctions::StateMachine` after
//! its configuration was changed outside the stack, end to end over each service's own wire.
//!
//! **The rule under test.** A drift check compares a managed resource against the
//! configuration its template declared using what the service that OWNS it reports for it
//! NOW, over every declared property that read answers for — so a property changed outside
//! the stack is reported `MODIFIED` with one `PropertyDifference` per declared leaf the two
//! sides disagree about (the leaf's fully-qualified path, the template's value, the service's
//! value and `NOT_EQUAL`), the stack is `DRIFTED` with that resource counted, and the
//! resources nothing touched stay `IN_SYNC`. Putting the changed value back the way the
//! template declares it makes the next detection report `IN_SYNC` again, so the comparison is
//! carried out live on every check rather than latched.
//!
//! For this type the properties `DescribeStateMachine` answers for include the three
//! CONFIGURATION structures the declaration states — `LoggingConfiguration`,
//! `TracingConfiguration` and `EncryptionConfiguration`
//! (`smithy_query(query="operation",operation="DescribeStateMachine")`) — whose members that
//! service spells in lower camel case and the type declares in upper camel case, so a
//! declared configuration and the reported one are the same document where nothing changed.
//!
//! The rows differ along what the rule generalises over rather than repeating one recorded
//! case: WHICH declared property is changed out of band (a member of a logging configuration,
//! another member of the same configuration, a member of the tracing configuration, and the
//! role, which is a scalar property rather than a structure), in WHICH direction (a declared
//! level lowered, a declared `true` turned off, a declared `false` turned on), and a machine
//! nothing touched at all whose declaration states all three configurations.
//!
//! Everything goes through the one router `harness.rs` builds: CloudFormation over awsQuery,
//! AWS Step Functions and Amazon CloudWatch Logs as a client's SDK calls them.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ec2_network_stack_drift::{drift_report, record_of, settled_detection};
use crate::stack_lifecycle::{call, create_stack, list_members, member, router, xml_text};
use crate::stack_owned_secret_teardown::poll_to_terminal;
use crate::step_functions_state_machine_replacement::sfn_json;
use service_com_amazonaws_cloudformation_cloudformation::state::{
    drift::difference_type, drift_detection_status, resource_drift_status, DRIFT_STATUS_DRIFTED,
    DRIFT_STATUS_IN_SYNC,
};

/// The logical id of the log group every row's state machine vends its own logs to, and the
/// name the declaration gives it — the `/aws/vendedlogs/states/<name>` form a workflow's own
/// log destination carries.
const LOG_GROUP: &str = "WorkflowLogGroup";
const LOG_GROUP_NAME: &str = "/aws/vendedlogs/states/drift-suite";

/// The workflow every row declares: the smallest valid Amazon States Language document.
const DEFINITION: &str = r#"{"StartAt":"Done","States":{"Done":{"Type":"Pass","End":true}}}"#;

/// The role every row's machine runs its executions as, and the other one a row moves to
/// outside the stack.
const DECLARED_ROLE: &str = "arn:aws:iam::000000000000:role/StepFunctionsRole";
const OTHER_ROLE: &str = "arn:aws:iam::000000000000:role/OtherStepFunctionsRole";

/// One difference a record must carry: the path, the expected value, the actual value and the
/// kind of difference.
type Difference = (&'static str, &'static str, &'static str, &'static str);

/// What a row does to its state machine outside the stack, before the check runs.
#[derive(Debug, Clone, Copy)]
enum Tamper {
    /// Nothing: the machine is as the deployment left it.
    Nothing,
    /// Log at another level, keeping the rest of the reported logging configuration.
    LogAtLevel(&'static str),
    /// Include — or stop including — execution data, keeping the rest of it.
    IncludeExecutionData(bool),
    /// Turn X-Ray tracing on or off, through the configuration of its own.
    Trace(bool),
    /// Run the executions as another role.
    RunAs(&'static str),
}

/// One state machine of the stack the rows deploy, and what the check over it must report.
struct Row {
    case: &'static str,
    logical_id: &'static str,
    /// The `StateMachineName` the declaration states, which the machine is created under.
    machine: &'static str,
    /// The configuration properties the declaration carries beside the name, the definition
    /// and the role every row shares.
    declared: Value,
    tamper: Tamper,
    status: &'static str,
    differences: Vec<Difference>,
}

/// A logging configuration that delivers to the stack's own log group, at `level` and
/// including execution data or not.
///
/// The destination is the group's ARN read off the resource the same template declares, which
/// is how a stack vends its own log destination.
fn logging(level: &str, include_execution_data: bool) -> Value {
    json!({
        "Level": level,
        "IncludeExecutionData": include_execution_data,
        "Destinations": [{
            "CloudWatchLogsLogGroup": {
                "LogGroupArn": {"Fn::GetAtt": [LOG_GROUP, "Arn"]},
            },
        }],
    })
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the logging level lowered outside the stack",
            logical_id: "LoweredLogging",
            machine: "drift-lowered-logging",
            declared: json!({"LoggingConfiguration": logging("ALL", true)}),
            tamper: Tamper::LogAtLevel("ERROR"),
            status: resource_drift_status::MODIFIED,
            differences: vec![(
                "/LoggingConfiguration/Level",
                "ALL",
                "ERROR",
                difference_type::NOT_EQUAL,
            )],
        },
        Row {
            case: "another member of the same configuration: execution data no longer included",
            logical_id: "WithoutExecutionData",
            machine: "drift-without-execution-data",
            declared: json!({"LoggingConfiguration": logging("ALL", true)}),
            tamper: Tamper::IncludeExecutionData(false),
            status: resource_drift_status::MODIFIED,
            differences: vec![(
                "/LoggingConfiguration/IncludeExecutionData",
                "true",
                "false",
                difference_type::NOT_EQUAL,
            )],
        },
        Row {
            // A declaration that states FEWER members than the read reports: only the level,
            // which is the one leaf compared.
            case: "tracing turned on outside the stack, declared off",
            logical_id: "Traced",
            machine: "drift-traced",
            declared: json!({
                "LoggingConfiguration": {"Level": "OFF"},
                "TracingConfiguration": {"Enabled": false},
            }),
            tamper: Tamper::Trace(true),
            status: resource_drift_status::MODIFIED,
            differences: vec![(
                "/TracingConfiguration/Enabled",
                "false",
                "true",
                difference_type::NOT_EQUAL,
            )],
        },
        Row {
            case: "a scalar property rather than a structure: the role changed outside the stack",
            logical_id: "Rerolled",
            machine: "drift-rerolled",
            declared: json!({"LoggingConfiguration": logging("ERROR", false)}),
            tamper: Tamper::RunAs(OTHER_ROLE),
            status: resource_drift_status::MODIFIED,
            differences: vec![(
                "/RoleArn",
                DECLARED_ROLE,
                OTHER_ROLE,
                difference_type::NOT_EQUAL,
            )],
        },
        Row {
            // Every configuration the type states, none of them touched: the machine is in
            // sync on all three rather than differing because the read spells them its own
            // way.
            case: "a machine nothing touched, declaring all three configurations",
            logical_id: "Untouched",
            machine: "drift-untouched",
            declared: json!({
                "LoggingConfiguration": logging("ALL", true),
                "TracingConfiguration": {"Enabled": true},
                "EncryptionConfiguration": {"Type": "AWS_OWNED_KEY"},
            }),
            tamper: Tamper::Nothing,
            status: resource_drift_status::IN_SYNC,
            differences: vec![],
        },
    ]
}

/// One row's resource declaration, as the template carries it.
fn declaration(row: &Row) -> Value {
    let mut properties = json!({
        "StateMachineName": row.machine,
        "DefinitionString": DEFINITION,
        "RoleArn": DECLARED_ROLE,
    });
    let members = properties
        .as_object_mut()
        .expect("the declaration is an object");
    for (property, value) in row
        .declared
        .as_object()
        .expect("the row's declared properties are an object")
    {
        members.insert(property.clone(), value.clone());
    }
    json!({"Type": "AWS::StepFunctions::StateMachine", "Properties": properties})
}

/// The template the rows deploy: the log group they vend their logs to, and one state machine
/// per row.
fn template(rows: &[Row]) -> String {
    let mut resources = serde_json::Map::new();
    resources.insert(
        LOG_GROUP.to_string(),
        json!({
            "Type": "AWS::Logs::LogGroup",
            "Properties": {"LogGroupName": LOG_GROUP_NAME},
        }),
    );
    for row in rows {
        resources.insert(row.logical_id.to_string(), declaration(row));
    }
    json!({"Resources": resources}).to_string()
}

/// Deploy `template` as `stack` and wait for the create to settle, asserting it completed.
async fn deployed(case: &str, router: &MultiServiceRouter, stack: &str, template: &str) {
    let (status, body) = create_stack(router, stack, template, &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateStack answered {status} with {body}"
    );
    let described = poll_to_terminal(router, case, stack).await;
    let settled = xml_text(&described, "StackStatus").unwrap_or_default();
    assert_eq!(
        settled, "CREATE_COMPLETE",
        "case {case:?}: expected the create to complete, got {settled:?} in {described}"
    );
}

/// The physical id the stack publishes for `logical_id`, which for this type is the state
/// machine's ARN.
async fn physical_id(
    case: &str,
    router: &MultiServiceRouter,
    stack: &str,
    logical_id: &str,
) -> String {
    let (status, body) = call(
        router,
        "DescribeStackResource",
        &[
            member("StackName", stack),
            member("LogicalResourceId", logical_id),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeStackResource({logical_id}) answered {status} with {body}"
    );
    xml_text(&body, "PhysicalResourceId").unwrap_or_default()
}

/// What Step Functions reports for `arn` right now.
async fn described(case: &str, router: &MultiServiceRouter, arn: &str) -> Value {
    let (status, document) = sfn_json(
        router,
        "DescribeStateMachine",
        &json!({"stateMachineArn": arn}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeStateMachine({arn}) answered {status} with {document}"
    );
    document
}

/// The reported logging configuration with `member` set to `value`, so a change made outside
/// the stack carries the rest of that configuration exactly as the machine holds it — which
/// is what an operator's own SDK call sends.
async fn reported_logging_with(
    case: &str,
    router: &MultiServiceRouter,
    arn: &str,
    name: &str,
    value: Value,
) -> Value {
    let machine = described(case, router, arn).await;
    let mut logging = machine
        .get("loggingConfiguration")
        .cloned()
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected a reported loggingConfiguration, got {machine}")
        });
    let Some(members) = logging.as_object_mut() else {
        panic!("case {case:?}: expected the reported configuration to be an object, got {machine}")
    };
    members.insert(name.to_string(), value);
    logging
}

/// Carry out one row's out-of-band change through Step Functions' own wire, and assert that
/// service reports the changed value afterwards — so the check below is about a machine that
/// really differs from its declaration.
async fn tamper_with(router: &MultiServiceRouter, row: &Row, arn: &str) {
    let case = row.case;
    let (request, pointer, wanted) = match row.tamper {
        Tamper::Nothing => return,
        Tamper::LogAtLevel(level) => (
            json!({
                "stateMachineArn": arn,
                "loggingConfiguration":
                    reported_logging_with(case, router, arn, "level", json!(level)).await,
            }),
            "/loggingConfiguration/level",
            json!(level),
        ),
        Tamper::IncludeExecutionData(included) => (
            json!({
                "stateMachineArn": arn,
                "loggingConfiguration": reported_logging_with(
                    case,
                    router,
                    arn,
                    "includeExecutionData",
                    json!(included),
                )
                .await,
            }),
            "/loggingConfiguration/includeExecutionData",
            json!(included),
        ),
        Tamper::Trace(enabled) => (
            json!({"stateMachineArn": arn, "tracingConfiguration": {"enabled": enabled}}),
            "/tracingConfiguration/enabled",
            json!(enabled),
        ),
        Tamper::RunAs(role) => (
            json!({"stateMachineArn": arn, "roleArn": role}),
            "/roleArn",
            json!(role),
        ),
    };
    let (status, answered) = sfn_json(router, "UpdateStateMachine", &request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected UpdateStateMachine to be accepted, got {status} with {answered}"
    );
    let machine = described(case, router, arn).await;
    let reported = machine.pointer(pointer).cloned();
    assert_eq!(
        reported,
        Some(wanted.clone()),
        "case {case:?}: expected Step Functions to report {wanted} at {pointer}, got \
         {reported:?} in {machine}"
    );
}

/// The `ActualProperties` document one logical resource's drift record carries, as the
/// document it is.
async fn actual_properties(
    case: &str,
    router: &MultiServiceRouter,
    stack: &str,
    logical_id: &str,
) -> Value {
    let (status, body) = call(
        router,
        "DescribeStackResourceDrifts",
        &[member("StackName", stack)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeStackResourceDrifts({stack}) answered {status} with {body}"
    );
    let text = list_members(&body, "StackResourceDrifts")
        .into_iter()
        .find(|block| xml_text(block, "LogicalResourceId").as_deref() == Some(logical_id))
        .and_then(|block| xml_text(&block, "ActualProperties"))
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected {logical_id} to report its actual properties")
        });
    serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("case {case:?}: {text:?} is not a document: {error}"))
}

/// Assert one row's record: the status it adds up to, the differences it reports, and that the
/// actual-properties document carries the owning service's value at each difference's path —
/// in the property vocabulary the TYPE declares, which is what makes the path meaningful.
async fn assert_record(
    router: &MultiServiceRouter,
    stack: &str,
    row: &Row,
    arn: &str,
    report: &[(String, crate::ec2_network_stack_drift::ReportedDrift)],
) {
    let case = row.case;
    let record = record_of(case, report, row.logical_id);
    assert_eq!(
        record.physical_resource_id, arn,
        "case {case:?}: expected the record to name the machine the stack published ({arn}), \
         got {:?}",
        record.physical_resource_id
    );
    assert_eq!(
        record.status, row.status,
        "case {case:?}: expected {}, got {} with {:?}",
        row.status, record.status, record.differences
    );
    let wanted: Vec<(String, String, String, String)> = row
        .differences
        .iter()
        .map(|(path, expected, actual, kind)| {
            (
                (*path).to_string(),
                (*expected).to_string(),
                (*actual).to_string(),
                (*kind).to_string(),
            )
        })
        .collect();
    assert_eq!(
        record.differences, wanted,
        "case {case:?}: expected the differences {wanted:?}, got {:?}",
        record.differences
    );
    if row.differences.is_empty() {
        return;
    }
    let actual = actual_properties(case, router, stack, row.logical_id).await;
    for (path, _, value, _) in &row.differences {
        let at = actual.pointer(path).cloned();
        let text = at.as_ref().map(|found| match found {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        });
        assert_eq!(
            text.as_deref(),
            Some(*value),
            "case {case:?}: expected the actual properties to carry {value:?} at {path}, got \
             {at:?} in {actual}"
        );
    }
}

/// RULE: a drift check reports a managed state machine whose declared configuration was
/// changed outside the stack as `MODIFIED`, with one difference per declared leaf the two
/// sides disagree about, while the resources nothing touched — the state machines and the log
/// group they vend to — stay `IN_SYNC`.
#[tokio::test]
async fn an_out_of_band_configuration_change_to_a_managed_state_machine_is_reported_modified() {
    let router = router();
    let stack = "sfn-drift";
    let rows = rows();
    deployed("the deployment", &router, stack, &template(&rows)).await;

    let mut arns = Vec::new();
    for row in &rows {
        let arn = physical_id(row.case, &router, stack, row.logical_id).await;
        tamper_with(&router, row, &arn).await;
        arns.push(arn);
    }

    let (detection_status, stack_drift, drifted_count) =
        settled_detection("the out-of-band changes", &router, stack).await;
    assert_eq!(
        detection_status,
        drift_detection_status::DETECTION_COMPLETE,
        "expected the detection to complete, got {detection_status:?}"
    );
    assert_eq!(
        stack_drift, DRIFT_STATUS_DRIFTED,
        "expected the stack to be reported {DRIFT_STATUS_DRIFTED}, got {stack_drift:?}"
    );
    let drifted = rows
        .iter()
        .filter(|row| row.status != resource_drift_status::IN_SYNC)
        .count();
    assert_eq!(
        drifted_count,
        drifted.to_string(),
        "expected {drifted} drifted resources, got {drifted_count:?}"
    );

    let report = drift_report("the out-of-band changes", &router, stack).await;
    for (row, arn) in rows.iter().zip(&arns) {
        assert_record(&router, stack, row, arn, &report).await;
    }
    // The log group the stack vends its logs to is a resource of the same stack, reported
    // beside the machines and untouched by any of them.
    let group = record_of("the out-of-band changes", &report, LOG_GROUP);
    assert_eq!(
        (group.status.as_str(), group.physical_resource_id.as_str()),
        (resource_drift_status::IN_SYNC, LOG_GROUP_NAME),
        "expected the declared log group to be reported in sync, got {} for {:?} with {:?}",
        group.status,
        group.physical_resource_id,
        group.differences
    );
}

/// RULE: the comparison is carried out against the owning service on every check rather than
/// latched, so putting the changed configuration back the way the template declares it returns
/// the resource — and the stack — to `IN_SYNC` with no drifted resource.
#[tokio::test]
async fn reverting_the_out_of_band_logging_level_returns_the_state_machine_to_in_sync() {
    let router = router();
    let stack = "sfn-drift-reverted";
    let case = "the logging level lowered and then set back";
    let row = Row {
        case,
        logical_id: "Reverted",
        machine: "drift-reverted",
        declared: json!({"LoggingConfiguration": logging("ALL", true)}),
        tamper: Tamper::LogAtLevel("ERROR"),
        status: resource_drift_status::MODIFIED,
        differences: vec![],
    };
    let rows = vec![row];
    deployed(case, &router, stack, &template(&rows)).await;
    let arn = physical_id(case, &router, stack, rows[0].logical_id).await;

    // Changed outside the stack: the resource is reported MODIFIED...
    tamper_with(&router, &rows[0], &arn).await;
    let (_, drifted, count) = settled_detection(case, &router, stack).await;
    assert_eq!(
        (drifted.as_str(), count.as_str()),
        (DRIFT_STATUS_DRIFTED, "1"),
        "case {case:?}: expected the stack {DRIFT_STATUS_DRIFTED} with one drifted resource, \
         got {drifted:?} with {count:?}"
    );
    let report = drift_report(case, &router, stack).await;
    let record = record_of(case, &report, rows[0].logical_id);
    assert_eq!(
        record.status,
        resource_drift_status::MODIFIED,
        "case {case:?}: expected {} after the change, got {} with {:?}",
        resource_drift_status::MODIFIED,
        record.status,
        record.differences
    );

    // ...and set back the way the template declares it, the next check reports it IN_SYNC.
    let restored = Row {
        tamper: Tamper::LogAtLevel("ALL"),
        differences: vec![],
        ..rows.into_iter().next().expect("the one row")
    };
    tamper_with(&router, &restored, &arn).await;
    let (_, reverted, count) = settled_detection(case, &router, stack).await;
    assert_eq!(
        (reverted.as_str(), count.as_str()),
        (DRIFT_STATUS_IN_SYNC, "0"),
        "case {case:?}: expected the reverted stack to be in sync with no drifted resource, got \
         {reverted:?} with {count:?}"
    );
    let report = drift_report(case, &router, stack).await;
    let record = record_of(case, &report, restored.logical_id);
    assert!(
        record.status == resource_drift_status::IN_SYNC && record.differences.is_empty(),
        "case {case:?}: expected {} with no differences once the change was reverted, got {} \
         with {:?}",
        resource_drift_status::IN_SYNC,
        record.status,
        record.differences
    );
}
