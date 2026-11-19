// SPDX-License-Identifier: CC0-1.0

//! The MONITORING a `RollbackConfiguration`'s rollback triggers put on a stack operation.
//!
//! The rule every case here holds the service to: **a stack operation monitored by a
//! rollback trigger whose CloudWatch alarm is in `ALARM` when the operation begins is rolled
//! back whole — the stack takes the rolled-back terminal status of the operation it opened,
//! reports the alarms that were in that state as the reason, and changes NOTHING in the
//! services that own its resources, so a resource the operation would have created has no
//! event of its own and does not exist there; an operation whose trigger alarms are not in
//! that state is carried out and completes.**
//!
//! "If any of the alarms you specify goes to ALARM state during the stack operation or this
//! monitoring period, CloudFormation rolls back the entire stack operation"
//! (`RollbackConfiguration$RollbackTriggers`) — the stack operation ITSELF is monitored, so
//! an alarm already in that state aborts it before any resource is touched.
//!
//! The recorded trace this suite was written for is ONE row of the table: a change set whose
//! configuration named an alarm a client had put into `ALARM`. The rows differ along what the
//! rule generalises over rather than restating that one case:
//!
//! * the OPERATION the configuration is supplied on — `CreateStack`, `UpdateStack` and the
//!   `ExecuteChangeSet` of a change set that carries it;
//! * WHICH state the trigger's alarm is in, over every value of `StateValue` (`ALARM`, `OK`,
//!   `INSUFFICIENT_DATA`), and how it got there — a client's `SetAlarmState` and the alarm's
//!   own evaluation;
//! * the alarm TYPE a trigger may name (`RollbackTrigger$Type`): a metric alarm and a
//!   composite alarm over one;
//! * the number of triggers and the POSITION of the firing one, and a trigger whose ARN names
//!   an alarm CloudWatch holds none of;
//! * whether the configuration states a `MonitoringTimeInMinutes` at all. A row that asks for
//!   a NONZERO period states one of zero instead, because such a period holds the operation
//!   open while it is watched (`crate::state::MAX_TRIGGER_MONITORING`) and what a period does
//!   after the resources are deployed is the cross-service suite
//!   `cloudformation_rollback_triggers_revert_the_operation`'s own rule; every row here is
//!   about an alarm that is in `ALARM` when the operation BEGINS.
//!
//! Nothing is waited out: an alarm's state and a stack's terminal status are both due at an
//! instant this emulator records, so each is POLLED for until it holds.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_cloudformation_cloudformation::state::status;
use smithy_server_runtime::MultiServiceRouter;

use crate::change_set_lifecycle::{create_change_set, describe_change_set, execute_change_set};
use crate::cloudwatch_alarm_and_ecs_service_resources::cloudwatch;
use crate::harness::{
    call, create_stack, member, router, settled_stack_status, stack_event_reasons, xml_text,
    xml_texts, REGION,
};
use crate::stack_lifecycle::{resource_event_statuses, sns};
use crate::stack_resources::sns_arn;

/// The metric every metric alarm below watches: one metric for all of them, so what a row
/// varies is the alarm's STATE and not its configuration.
const METRIC_NAMESPACE: &str = "Sim/Deploys";
const METRIC_NAME: &str = "DeployErrors";

/// The alarm lists `DescribeAlarms` reports, one per alarm type a rollback trigger may name
/// (`DescribeAlarmsOutput$MetricAlarms` / `$CompositeAlarms`).
const ALARM_LISTS: [&str; 2] = ["MetricAlarms", "CompositeAlarms"];

/// The alarm types the read asks about, so a composite trigger is reported too
/// (`DescribeAlarms$AlarmTypes`: "If you omit this parameter, only metric alarms are
/// returned").
const ALARM_TYPES: [&str; 2] = ["MetricAlarm", "CompositeAlarm"];

/// The `Type` a trigger naming each kind of alarm carries, as `RollbackTrigger$Type`
/// documents ("Specify either AWS::CloudWatch::Alarm or AWS::CloudWatch::CompositeAlarm
/// resource types").
const METRIC_TRIGGER_TYPE: &str = "AWS::CloudWatch::Alarm";
const COMPOSITE_TRIGGER_TYPE: &str = "AWS::CloudWatch::CompositeAlarm";

/// How long a condition is polled for before it is judged not to hold, and how often it is
/// looked at. The deadline is far longer than the spans this emulator gives an alarm's
/// evaluation and a stack's transition, so a loaded host cannot fail a case: what each case
/// asserts is the CONDITION, never the time it took.
const POLL_DEADLINE: Duration = Duration::from_secs(60);
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The operation the row's `RollbackConfiguration` is supplied on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Operation {
    CreateStack,
    UpdateStack,
    ExecuteChangeSet,
}

/// What an alarm a row sets up IS.
#[derive(Debug, Clone, Copy)]
enum AlarmKind {
    /// A metric alarm, whose own evaluation of a window no datapoint was published in is
    /// decided by this `TreatMissingData` (`breaching` reaches `ALARM`, `notBreaching`
    /// reaches `OK`, `missing` leaves it short of either).
    Metric(&'static str),
    /// A composite alarm whose rule is `ALARM("<the metric alarm named here>")`.
    CompositeOver(&'static str),
}

/// One alarm a row stands up before it makes its monitored call.
#[derive(Debug, Clone, Copy)]
struct AlarmSpec {
    name: &'static str,
    kind: AlarmKind,
    /// A state driven in through `SetAlarmState`, as a client testing its deployment gate
    /// does, or `None` for an alarm left to its own evaluation.
    forced: Option<&'static str>,
    /// The state CloudWatch must report for this alarm before the monitored call is made.
    state: &'static str,
    /// Whether the row's `RollbackConfiguration` names this alarm as a trigger.
    trigger: bool,
}

/// One row: the alarms, the operation they monitor, and what that operation must then do.
struct Row {
    case: &'static str,
    stack: &'static str,
    operation: Operation,
    alarms: Vec<AlarmSpec>,
    /// Whether the configuration also names a trigger ARN no alarm in CloudWatch answers for.
    absent_trigger: bool,
    /// `RollbackConfiguration$MonitoringTimeInMinutes`, where the row states one.
    monitoring_minutes: Option<&'static str>,
    /// The alarms the rolled-back operation must report, by NAME and in the order the
    /// configuration declared the triggers naming them — empty for a row nothing rolls back.
    fired: Vec<&'static str>,
}

/// The rows, spanning the class the rule covers.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an update whose trigger alarm a client put into ALARM",
            stack: "rbm-update-forced",
            operation: Operation::UpdateStack,
            alarms: vec![AlarmSpec {
                name: "rbm-update-forced-errors",
                kind: AlarmKind::Metric("breaching"),
                forced: Some("ALARM"),
                state: "ALARM",
                trigger: true,
            }],
            absent_trigger: false,
            monitoring_minutes: Some("5"),
            fired: vec!["rbm-update-forced-errors"],
        },
        Row {
            case: "an update monitored by two triggers, the second a composite alarm in ALARM",
            stack: "rbm-update-composite",
            operation: Operation::UpdateStack,
            alarms: vec![
                AlarmSpec {
                    name: "rbm-update-composite-quiet",
                    kind: AlarmKind::Metric("notBreaching"),
                    forced: None,
                    state: "OK",
                    trigger: true,
                },
                AlarmSpec {
                    name: "rbm-update-composite-child",
                    kind: AlarmKind::Metric("breaching"),
                    forced: None,
                    state: "ALARM",
                    trigger: false,
                },
                AlarmSpec {
                    name: "rbm-update-composite-roll-up",
                    kind: AlarmKind::CompositeOver("rbm-update-composite-child"),
                    forced: None,
                    state: "ALARM",
                    trigger: true,
                },
            ],
            absent_trigger: false,
            monitoring_minutes: None,
            fired: vec!["rbm-update-composite-roll-up"],
        },
        Row {
            case: "an update whose trigger alarm is OK",
            stack: "rbm-update-quiet",
            operation: Operation::UpdateStack,
            alarms: vec![AlarmSpec {
                name: "rbm-update-quiet-errors",
                kind: AlarmKind::Metric("notBreaching"),
                forced: Some("OK"),
                state: "OK",
                trigger: true,
            }],
            absent_trigger: false,
            monitoring_minutes: Some("0"),
            fired: Vec::new(),
        },
        Row {
            case: "an update whose only trigger names an alarm CloudWatch holds none of",
            stack: "rbm-update-absent",
            operation: Operation::UpdateStack,
            alarms: Vec::new(),
            absent_trigger: true,
            monitoring_minutes: Some("0"),
            fired: Vec::new(),
        },
        Row {
            case: "an executed change set whose trigger alarm evaluated itself into ALARM",
            stack: "rbm-execute-evaluated",
            operation: Operation::ExecuteChangeSet,
            alarms: vec![AlarmSpec {
                name: "rbm-execute-evaluated-errors",
                kind: AlarmKind::Metric("breaching"),
                forced: None,
                state: "ALARM",
                trigger: true,
            }],
            absent_trigger: true,
            monitoring_minutes: Some("5"),
            fired: vec!["rbm-execute-evaluated-errors"],
        },
        Row {
            case: "an executed change set whose trigger alarm has insufficient data",
            stack: "rbm-execute-insufficient",
            operation: Operation::ExecuteChangeSet,
            alarms: vec![AlarmSpec {
                name: "rbm-execute-insufficient-errors",
                kind: AlarmKind::Metric("missing"),
                forced: None,
                state: "INSUFFICIENT_DATA",
                trigger: true,
            }],
            absent_trigger: false,
            monitoring_minutes: None,
            fired: Vec::new(),
        },
        Row {
            case: "a create whose trigger alarm a client put into ALARM",
            stack: "rbm-create-forced",
            operation: Operation::CreateStack,
            alarms: vec![AlarmSpec {
                name: "rbm-create-forced-errors",
                kind: AlarmKind::Metric("breaching"),
                forced: Some("ALARM"),
                state: "ALARM",
                trigger: true,
            }],
            absent_trigger: false,
            monitoring_minutes: Some("5"),
            fired: vec!["rbm-create-forced-errors"],
        },
        Row {
            case: "a create whose trigger alarm is OK",
            stack: "rbm-create-quiet",
            operation: Operation::CreateStack,
            alarms: vec![AlarmSpec {
                name: "rbm-create-quiet-errors",
                kind: AlarmKind::Metric("notBreaching"),
                forced: None,
                state: "OK",
                trigger: true,
            }],
            absent_trigger: false,
            monitoring_minutes: Some("0"),
            fired: Vec::new(),
        },
    ]
}

/// The topic the stack already has before the monitored operation, and the one that operation
/// would ADD: the resource whose existence says whether anything was really deployed.
fn held_topic(row: &Row) -> String {
    format!("{}-one", row.stack)
}
fn added_topic(row: &Row) -> String {
    match row.operation {
        // A create's own template is what declares the resource, so the one it would make is
        // the stack's first.
        Operation::CreateStack => held_topic(row),
        _ => format!("{}-two", row.stack),
    }
}

/// The logical id of the resource the monitored operation would act on.
fn added_logical_id(row: &Row) -> &'static str {
    match row.operation {
        Operation::CreateStack => "One",
        _ => "Two",
    }
}

/// The template the stack is created with, and the one the monitored operation would deploy:
/// the second declares one more topic, so what the operation WOULD do is create a resource in
/// the service that owns it.
fn held_template(row: &Row) -> String {
    json!({
        "Resources": {
            "One": {
                "Type": "AWS::SNS::Topic",
                "Properties": {"TopicName": held_topic(row)},
            },
        },
    })
    .to_string()
}
fn revised_template(row: &Row) -> String {
    json!({
        "Resources": {
            "One": {
                "Type": "AWS::SNS::Topic",
                "Properties": {"TopicName": held_topic(row)},
            },
            "Two": {
                "Type": "AWS::SNS::Topic",
                "Properties": {"TopicName": added_topic(row)},
            },
        },
    })
    .to_string()
}

/// The one alarm `DescribeAlarms` reports for `name`, from whichever list its type puts it
/// in, or `Value::Null` where CloudWatch holds no alarm of that name.
async fn described_alarm(router: &MultiServiceRouter, case: &str, name: &str) -> Value {
    let (http_status, document) = cloudwatch(
        router,
        "DescribeAlarms",
        &json!({"AlarmNames": [name], "AlarmTypes": ALARM_TYPES}),
    )
    .await;
    assert_eq!(
        http_status, 200,
        "case {case:?}: DescribeAlarms({name:?}) must answer 200, got {http_status} {document}"
    );
    ALARM_LISTS
        .iter()
        .filter_map(|list| document.get(list).and_then(Value::as_array))
        .flatten()
        .find(|alarm| alarm.get("AlarmName").and_then(Value::as_str) == Some(name))
        .cloned()
        .unwrap_or(Value::Null)
}

/// The alarm once CloudWatch reports it in `expected`, with the ARN it reports for it.
///
/// An alarm's state is EVALUATED rather than written by the caller, and the first evaluation
/// is due a span after the write that configured it, so the state is POLLED for until it
/// holds — never waited out for a fixed time.
async fn alarm_in_state(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    expected: &str,
) -> String {
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let alarm = described_alarm(router, case, name).await;
        if alarm.get("StateValue").and_then(Value::as_str) == Some(expected) {
            let arn = alarm
                .get("AlarmArn")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            assert!(
                !arn.is_empty(),
                "case {case:?}: expected CloudWatch to report an ARN for {name:?}, got {alarm}"
            );
            return arn;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected CloudWatch to report {name:?} in {expected:?}, got {alarm}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Stand up one row's alarms, drive in the states it asks for, and answer the trigger ARNs
/// its `RollbackConfiguration` names — each read back from CloudWatch, so the configuration
/// points at the ARN the service itself reports for the alarm.
async fn trigger_arns(router: &MultiServiceRouter, row: &Row) -> Vec<(String, &'static str)> {
    let mut arns = Vec::new();
    for spec in &row.alarms {
        let (operation, request) = match spec.kind {
            AlarmKind::Metric(treatment) => (
                "PutMetricAlarm",
                json!({
                    "AlarmName": spec.name,
                    "Namespace": METRIC_NAMESPACE,
                    "MetricName": METRIC_NAME,
                    "Statistic": "Sum",
                    "ComparisonOperator": "GreaterThanOrEqualToThreshold",
                    "Period": 60,
                    "EvaluationPeriods": 1,
                    "Threshold": 1,
                    "TreatMissingData": treatment,
                }),
            ),
            AlarmKind::CompositeOver(child) => (
                "PutCompositeAlarm",
                json!({
                    "AlarmName": spec.name,
                    "AlarmRule": format!("ALARM(\"{child}\")"),
                }),
            ),
        };
        let (http_status, body) = cloudwatch(router, operation, &request).await;
        assert_eq!(
            http_status, 200,
            "case {:?}: {operation} for {:?} must answer 200, got {http_status} {body}",
            row.case, spec.name
        );
        if let Some(forced) = spec.forced {
            let (http_status, body) = cloudwatch(
                router,
                "SetAlarmState",
                &json!({
                    "AlarmName": spec.name,
                    "StateValue": forced,
                    "StateReason": "driven by the deployment gate under test",
                }),
            )
            .await;
            assert_eq!(
                http_status, 200,
                "case {:?}: SetAlarmState({:?}, {forced:?}) must answer 200, got {http_status} \
                 {body}",
                row.case, spec.name
            );
        }
        let arn = alarm_in_state(router, row.case, spec.name, spec.state).await;
        if spec.trigger {
            let trigger_type = match spec.kind {
                AlarmKind::Metric(_) => METRIC_TRIGGER_TYPE,
                AlarmKind::CompositeOver(_) => COMPOSITE_TRIGGER_TYPE,
            };
            arns.push((arn, trigger_type));
        }
    }
    if row.absent_trigger {
        // An alarm no CloudWatch record answers for is no evidence that anything is in
        // `ALARM`, so a trigger naming one neither rolls an operation back nor stops the
        // triggers beside it from doing so.
        arns.push((
            format!(
                "arn:aws:cloudwatch:{REGION}:000000000000:alarm:{}-no-such-alarm",
                row.stack
            ),
            METRIC_TRIGGER_TYPE,
        ));
    }
    arns
}

/// The members that spell one `RollbackConfiguration` naming `triggers`.
fn rollback_configuration(
    triggers: &[(String, &'static str)],
    monitoring_minutes: Option<&str>,
) -> Vec<(String, String)> {
    let mut members = Vec::new();
    if let Some(minutes) = monitoring_minutes {
        members.push(member("RollbackConfiguration.MonitoringTimeInMinutes", minutes));
    }
    for (index, (arn, trigger_type)) in triggers.iter().enumerate() {
        let n = index + 1;
        members.push(member(
            &format!("RollbackConfiguration.RollbackTriggers.member.{n}.Arn"),
            arn,
        ));
        members.push(member(
            &format!("RollbackConfiguration.RollbackTriggers.member.{n}.Type"),
            trigger_type,
        ));
    }
    members
}

/// The change set once its creation has settled: a plan is executable only from
/// `CREATE_COMPLETE`, which is due at an instant this service records, so it is polled for.
async fn created_change_set(router: &MultiServiceRouter, case: &str, stack: &str, name: &str) {
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let (http_status, body) = describe_change_set(router, name, Some(stack)).await;
        assert_eq!(
            http_status, 200,
            "case {case:?}: DescribeChangeSet({name:?}) must answer 200, got {http_status} {body}"
        );
        let reported = xml_text(&body, "Status").unwrap_or_default();
        if reported == "CREATE_COMPLETE" {
            return;
        }
        assert_ne!(
            reported, "FAILED",
            "case {case:?}: the change set under test must be creatable, got {body}"
        );
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the change set to be created, last saw {reported:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The terminal status the row's operation must settle in.
fn expected_status(row: &Row) -> &'static str {
    match (row.operation, row.fired.is_empty()) {
        (Operation::CreateStack, false) => status::ROLLBACK_COMPLETE,
        (Operation::CreateStack, true) => status::CREATE_COMPLETE,
        (_, false) => status::UPDATE_ROLLBACK_COMPLETE,
        (_, true) => status::UPDATE_COMPLETE,
    }
}

/// Whether Amazon SNS holds the topic `name`, with the answer it gave: the owning service's
/// own read, which is what says whether the monitored operation really deployed anything.
async fn topic_held(router: &MultiServiceRouter, name: &str) -> (bool, String) {
    let (http_status, body) = sns(
        router,
        "GetTopicAttributes",
        &[("TopicArn", &sns_arn(name))],
    )
    .await;
    (http_status == 200, body)
}

/// Drive one row end to end and assert the rule over it.
async fn assert_the_row(row: &Row) {
    let case = row.case;
    let router = router();
    let triggers = trigger_arns(&router, row).await;
    let configuration = rollback_configuration(&triggers, row.monitoring_minutes);

    // The stack an UPDATE or an executed change set is monitored on exists first, carrying
    // the one topic the monitored operation then adds to.
    if row.operation != Operation::CreateStack {
        let (http_status, body) =
            create_stack(&router, row.stack, &held_template(row), &[]).await;
        assert_eq!(
            http_status, 200,
            "case {case:?}: the stack under test must be created, got {http_status} {body}"
        );
        let settled = settled_stack_status(case, &router, REGION, row.stack).await;
        assert_eq!(
            settled,
            status::CREATE_COMPLETE,
            "case {case:?}: the stack under test must reach CREATE_COMPLETE, got {settled:?} \
             because {:?}",
            stack_event_reasons(&router, REGION, row.stack).await
        );
    }

    match row.operation {
        Operation::CreateStack => {
            let (http_status, body) =
                create_stack(&router, row.stack, &held_template(row), &configuration).await;
            assert_eq!(
                http_status, 200,
                "case {case:?}: CreateStack must be accepted, got {http_status} {body}"
            );
        }
        Operation::UpdateStack => {
            let mut members = vec![
                member("StackName", row.stack),
                member("TemplateBody", &revised_template(row)),
            ];
            members.extend(configuration);
            let (http_status, body) = call(&router, "UpdateStack", &members).await;
            assert_eq!(
                http_status, 200,
                "case {case:?}: UpdateStack must be accepted, got {http_status} {body}"
            );
        }
        Operation::ExecuteChangeSet => {
            let change_set = format!("{}-gate", row.stack);
            let mut members = vec![member("TemplateBody", &revised_template(row))];
            members.extend(configuration);
            let (http_status, body) =
                create_change_set(&router, row.stack, &change_set, &members).await;
            assert_eq!(
                http_status, 200,
                "case {case:?}: CreateChangeSet must be accepted, got {http_status} {body}"
            );
            created_change_set(&router, case, row.stack, &change_set).await;
            let (http_status, body) =
                execute_change_set(&router, &change_set, Some(row.stack)).await;
            assert_eq!(
                http_status, 200,
                "case {case:?}: ExecuteChangeSet must be accepted, got {http_status} {body}"
            );
        }
    }

    let settled = settled_stack_status(case, &router, REGION, row.stack).await;
    let reasons = stack_event_reasons(&router, REGION, row.stack).await;
    let expected = expected_status(row);
    assert_eq!(
        settled, expected,
        "case {case:?}: expected the operation to settle {expected:?}, got {settled:?} with \
         the reasons {reasons:?}"
    );

    let added = added_logical_id(row);
    let (holds_added, answer) = topic_held(&router, &added_topic(row)).await;
    if row.fired.is_empty() {
        // Nothing was in `ALARM`, so the operation was carried out: the resource it declares
        // exists in the service that owns it.
        assert!(
            holds_added,
            "case {case:?}: expected Amazon SNS to hold the topic {:?} the completed operation \
             declares, got {answer}",
            added_topic(row)
        );
        return;
    }

    // The failure names the alarms that were in `ALARM`, by name and in the order the
    // configuration declared the triggers naming them. A rollback the service opens words its
    // own follow-on clause onto that sentence, so the sentence is what the reason OPENS with.
    let expected_reason = format!(
        "The following CloudWatch Alarms were in ALARM state: {}",
        row.fired.join(", ")
    );
    assert!(
        reasons
            .iter()
            .any(|reason| reason.starts_with(&expected_reason)),
        "case {case:?}: expected a stack event reporting {expected_reason:?}, got {reasons:?}"
    );

    // ...and the operation changed NOTHING: the resource it would have created has no event
    // of its own, is not among the stack's resources, and does not exist in the service that
    // owns it.
    let statuses = resource_event_statuses(&router, case, row.stack, added).await;
    assert!(
        statuses.is_empty(),
        "case {case:?}: expected the rolled-back operation to leave {added:?} untouched, got \
         the events {statuses:?}"
    );
    assert!(
        !holds_added,
        "case {case:?}: expected Amazon SNS to hold no topic {:?} for an operation that \
         deployed nothing, got {answer}",
        added_topic(row)
    );
    let (http_status, resources) = call(
        &router,
        "DescribeStackResources",
        &[member("StackName", row.stack)],
    )
    .await;
    assert_eq!(
        http_status, 200,
        "case {case:?}: DescribeStackResources must answer 200, got {http_status} {resources}"
    );
    let reported = xml_texts(&resources, "LogicalResourceId");
    assert!(
        !reported.iter().any(|logical_id| logical_id == added),
        "case {case:?}: expected the stack not to report {added:?} as one of its resources, \
         got {reported:?}"
    );

    // An UPDATE and an executed change set are rolled back to the configuration still in
    // force, so the resource the stack already had is still there.
    if row.operation != Operation::CreateStack {
        let (holds_held, answer) = topic_held(&router, &held_topic(row)).await;
        assert!(
            holds_held,
            "case {case:?}: expected the topic {:?} the stack already had to survive the \
             rollback, got {answer}",
            held_topic(row)
        );
    }
}

#[tokio::test]
async fn an_alarm_in_alarm_decides_whether_an_update_deploys_or_rolls_back() {
    for row in rows()
        .iter()
        .filter(|row| row.operation == Operation::UpdateStack)
    {
        assert_the_row(row).await;
    }
}

#[tokio::test]
async fn an_alarm_in_alarm_decides_whether_an_executed_change_set_deploys_or_rolls_back() {
    for row in rows()
        .iter()
        .filter(|row| row.operation == Operation::ExecuteChangeSet)
    {
        assert_the_row(row).await;
    }
}

#[tokio::test]
async fn an_alarm_in_alarm_decides_whether_a_create_deploys_or_rolls_back() {
    for row in rows()
        .iter()
        .filter(|row| row.operation == Operation::CreateStack)
    {
        assert_the_row(row).await;
    }
}
