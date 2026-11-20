// SPDX-License-Identifier: CC0-1.0

//! WHEN a stack operation's rollback triggers are sampled, across CloudFormation, Amazon
//! CloudWatch and Amazon SNS on ONE engine.
//!
//! RULE under test: **a stack operation monitored by a `RollbackConfiguration` whose trigger
//! alarm is ALREADY in `ALARM` when the operation begins carries out NO resource action at all
//! — the triggers are sampled before the first one, not after the changes have landed — so the
//! operation's event stream carries no event for a resource it would have added, no update
//! event for one it would have changed, the stack's resources afterwards are exactly the ones
//! in force before it, and the services that own them are never asked for the change; the same
//! revision monitored by no firing trigger carries its changes out and reports them.**
//!
//! "If any of the alarms you specify goes to ALARM state during the stack operation or this
//! monitoring period, CloudFormation rolls back the entire stack operation"
//! (`RollbackConfiguration$RollbackTriggers`): the STACK OPERATION itself is inside that
//! sentence, so an alarm already in that state when the operation starts is a condition that
//! stops the change rather than one that undoes it afterwards. A client that arms an alarm as a
//! deployment guard must therefore see no trace of the guarded change — neither a resource
//! created and taken away again, nor the event pair that would report it to whatever reads the
//! stack's event stream.
//!
//! The terminal status and the rollback reason of such an operation are pinned by
//! `cloudformation_rollback_triggers_revert_the_operation.rs`, which this suite does not
//! repeat: what only this one asserts is the ORDER — what the operation did to the stack's
//! resources, read through `DescribeStackEvents` and `ListStackResources`, and what SNS was
//! asked for.
//!
//! Only a cross-service suite can pin it: what aborts the operation is an alarm's `StateValue`
//! in CLOUDWATCH's own records, read by CloudFormation in-process, and what says no resource
//! was touched is the topic SNS does not hold afterwards.
//!
//! The rows differ along what the rule generalises over: the operation that deploys the
//! revision (`ExecuteChangeSet` over a change set that carries the configuration, and
//! `UpdateStack` carrying it directly), how the alarm reached `ALARM` (CloudWatch's own
//! evaluation of a series nothing publishes a datapoint for, and a state `SetAlarmState`
//! forced), how many resources the revision would add, whether it would also change the
//! resource the stack already has, and — in the control table — whether the alarm in `ALARM` is
//! named by a trigger at all.
//!
//! The awsQuery form encoder and the XML readers are the test crate's own
//! ([`crate::query_wire`]), not another copy.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{decoded_entries, element, elements, members_of, opt_element};

/// The Region every call below is signed for, and so the Region the stack, its topics and its
/// alarms all live in.
const REGION: &str = "us-west-2";

/// The account id this emulator keys a resource of that Region under, which is what the ARN of
/// a topic the stack DECLARED but no operation created would carry.
const ACCOUNT: &str = "000000000000";

/// CloudWatch's `X-Amz-Target` prefix and its signing name (`sigv4Name: monitoring`).
const CLOUDWATCH_TARGET_PREFIX: &str = "GraniteServiceVersion20100801";
const CLOUDWATCH_SIGNING_NAME: &str = "monitoring";

/// The `StateValue` values this suite reads (`StateValue`: `ALARM | INSUFFICIENT_DATA | OK`).
const ALARM_STATE: &str = "ALARM";
const OK_STATE: &str = "OK";

/// The `TreatMissingData` behaviour that makes an alarm over a series with NO datapoints settle
/// in each of those states: "breaching — `ALARM`", "notBreaching — `OK`".
const TREAT_MISSING_BREACHING: &str = "breaching";
const TREAT_MISSING_NOT_BREACHING: &str = "notBreaching";

/// The logical id the stack already has before the monitored revision, and the two the revision
/// would add.
const EXISTING: &str = "One";
const ADDED: &str = "Two";
const ALSO_ADDED: &str = "Three";

/// The topic property a revision changes, and the two values it changes it between: the stack
/// deploys the first and the revision declares the second, so the value SNS holds afterwards
/// says whether the revision reached SNS at all.
const WATCHED_ATTRIBUTE: &str = "DisplayName";
const BEFORE: &str = "deployed";
const AFTER: &str = "revised";

/// How long a row waits for something another service's clock owes — an alarm's evaluation, a
/// stack's terminal status — and how often it looks. Generous only so a failure is a failure
/// rather than a hang.
const DEADLINE: Duration = Duration::from_secs(60);
const LOOK_AGAIN: Duration = Duration::from_millis(100);

/// The emulator with the three services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_cloudwatch_graniteserviceversion20100801::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_sns_amazonsimplenotificationservice::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` whose credential scope names `service` in [`REGION`], which is what
/// routes the request. The signature is never checked: this router runs with request
/// authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

/// Call one awsQuery operation of `service`, answered with the HTTP status and the XML body.
async fn query(
    router: &MultiServiceRouter,
    service: &str,
    version: &str,
    action: &str,
    members: &[(String, String)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&crate::query_wire::form_encode(key));
        body.push('=');
        body.push_str(&crate::query_wire::form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{service}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(service))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One request member as the pair [`query`] takes.
fn member(key: &str, value: &str) -> (String, String) {
    (key.to_string(), value.to_string())
}

/// One CloudFormation call (API version 2010-05-15) that must be answered.
async fn cfn_ok(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    members: &[(String, String)],
) -> String {
    let (status, body) = query(router, "cloudformation", "2010-05-15", action, members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CloudFormation {action}, got {status} with {body}"
    );
    body
}

/// Call one CloudWatch operation on the awsJson1_0 wire its model declares, which must be
/// answered.
async fn cw(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header(
            "host",
            format!("{CLOUDWATCH_SIGNING_NAME}.{REGION}.amazonaws.com"),
        )
        .header("authorization", authorization(CLOUDWATCH_SIGNING_NAME))
        .header("content-type", "application/x-amz-json-1.0")
        .header(
            "x-amz-target",
            format!("{CLOUDWATCH_TARGET_PREFIX}.{operation}"),
        )
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CloudWatch {operation}, got {status} with {text}"
    );
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

/// How an alarm reaches the state a row needs it in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reached {
    /// CloudWatch's own evaluation of a series nothing publishes a datapoint for, through
    /// `TreatMissingData`.
    Evaluated(&'static str),
    /// A state a caller FORCED with `SetAlarmState`, over an alarm whose own evaluation would
    /// settle it in the first field.
    Forced(&'static str, &'static str),
}

/// The alarm `name`, in the state `reached` describes, polled until CloudWatch really reports
/// `wanted` for it. Answers the alarm's ARN, which is what a rollback trigger names.
async fn given_alarm(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    reached: Reached,
    wanted: &str,
) -> String {
    let treat_missing = match reached {
        Reached::Evaluated(treat_missing) | Reached::Forced(treat_missing, _) => treat_missing,
    };
    cw(
        router,
        case,
        "PutMetricAlarm",
        &json!({
            "AlarmName": name,
            "Namespace": "Custom/Deploys",
            "MetricName": "Errors",
            "Statistic": "Maximum",
            "Period": 60,
            "EvaluationPeriods": 1,
            "Threshold": 1.0,
            "ComparisonOperator": "GreaterThanOrEqualToThreshold",
            "TreatMissingData": treat_missing,
        }),
    )
    .await;
    if let Reached::Forced(_, state) = reached {
        cw(
            router,
            case,
            "SetAlarmState",
            &json!({
                "AlarmName": name,
                "StateValue": state,
                "StateReason": "the deployment guard this suite arms",
            }),
        )
        .await;
    }
    alarm_settles_in(router, case, name, wanted).await
}

/// Wait until CloudWatch reports `name` in `wanted`, so a row's precondition is a state that
/// service really holds rather than one this suite assumed, and answer the alarm's ARN.
async fn alarm_settles_in(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    wanted: &str,
) -> String {
    let deadline = Instant::now() + DEADLINE;
    let mut seen = Value::Null;
    while Instant::now() < deadline {
        let described = cw(
            router,
            case,
            "DescribeAlarms",
            &json!({"AlarmNames": [name], "AlarmTypes": ["MetricAlarm", "CompositeAlarm"]}),
        )
        .await;
        let alarm = ["MetricAlarms", "CompositeAlarms"]
            .iter()
            .filter_map(|list| described[*list].as_array())
            .flatten()
            .next()
            .cloned();
        if let Some(alarm) = alarm {
            if alarm["StateValue"].as_str() == Some(wanted) {
                return alarm["AlarmArn"]
                    .as_str()
                    .unwrap_or_else(|| panic!("case {case:?}: no AlarmArn for {name} in {alarm}"))
                    .to_string();
            }
            seen = alarm;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    panic!(
        "case {case:?}: CloudWatch must report the alarm {name} in {wanted} within {DEADLINE:?}, \
         got {seen}"
    );
}

/// The topic name the stack's resource `logical` declares, which is unique per row.
fn topic_name(stack: &str, logical: &str) -> String {
    format!("{stack}-{}", logical.to_lowercase())
}

/// The ARN Amazon SNS keys a topic of that name by in the Region and account every call of this
/// suite is signed for — the handle `GetTopicAttributes` takes for a topic the stack declared
/// but no operation created.
fn topic_arn(stack: &str, logical: &str) -> String {
    format!(
        "arn:aws:sns:{REGION}:{ACCOUNT}:{}",
        topic_name(stack, logical)
    )
}

/// A template declaring one SNS topic per entry of `topics`: its logical id, and the
/// `DisplayName` the declaration states for it.
fn template(stack: &str, topics: &[(&str, &str)]) -> String {
    let resources: Map<String, Value> = topics
        .iter()
        .map(|(logical, display)| {
            (
                (*logical).to_string(),
                json!({
                    "Type": "AWS::SNS::Topic",
                    "Properties": {
                        "TopicName": topic_name(stack, logical),
                        "DisplayName": display,
                    },
                }),
            )
        })
        .collect();
    json!({"Resources": resources}).to_string()
}

/// `RollbackConfiguration` members naming one trigger per entry of `arns`, with the monitoring
/// period `monitoring` states.
fn rollback_configuration(arns: &[String], monitoring: i32) -> Vec<(String, String)> {
    let mut members = vec![member(
        "RollbackConfiguration.MonitoringTimeInMinutes",
        &monitoring.to_string(),
    )];
    for (index, arn) in arns.iter().enumerate() {
        let position = index + 1;
        members.push(member(
            &format!("RollbackConfiguration.RollbackTriggers.member.{position}.Arn"),
            arn,
        ));
        members.push(member(
            &format!("RollbackConfiguration.RollbackTriggers.member.{position}.Type"),
            "AWS::CloudWatch::Alarm",
        ));
    }
    members
}

/// Wait until the stack reports a status that is not `*_IN_PROGRESS`, and answer the
/// `DescribeStacks` body it reported it in.
async fn settled_stack(router: &MultiServiceRouter, case: &str, stack: &str) -> String {
    let deadline = Instant::now() + DEADLINE;
    let mut last = String::new();
    while Instant::now() < deadline {
        last = cfn_ok(
            router,
            case,
            "DescribeStacks",
            &[member("StackName", stack)],
        )
        .await;
        let status = opt_element(&last, "StackStatus").unwrap_or_default();
        if !status.ends_with("_IN_PROGRESS") {
            return last;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    panic!("case {case:?}: the stack {stack} never settled within {DEADLINE:?}: {last}");
}

/// Which operation deploys the revision a row's rollback triggers monitor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Deploy {
    /// `UpdateStack`, carrying the rollback configuration itself.
    UpdateStack,
    /// `CreateChangeSet` carrying it, then `ExecuteChangeSet` — the shape a client that reviews
    /// a change before deploying it drives.
    ExecuteChangeSet,
}

/// Deploy a revision onto the stack through the operation `deploy` names, with `members`
/// carrying the template body and the rollback configuration that monitors it.
async fn deploy_revision(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    members: Vec<(String, String)>,
    deploy: Deploy,
) {
    match deploy {
        Deploy::UpdateStack => {
            let mut update = vec![member("StackName", stack)];
            update.extend(members);
            cfn_ok(router, case, "UpdateStack", &update).await;
        }
        Deploy::ExecuteChangeSet => {
            let change_set = format!("{stack}-change-set");
            let mut create = vec![
                member("StackName", stack),
                member("ChangeSetName", &change_set),
            ];
            create.extend(members);
            cfn_ok(router, case, "CreateChangeSet", &create).await;
            let planned = planned_change_set(router, case, stack, &change_set).await;
            assert_eq!(
                planned, "AVAILABLE",
                "case {case:?}: the change set must be executable, got {planned:?}"
            );
            cfn_ok(
                router,
                case,
                "ExecuteChangeSet",
                &[
                    member("StackName", stack),
                    member("ChangeSetName", &change_set),
                ],
            )
            .await;
        }
    }
}

/// The `ExecutionStatus` the change set `name` settles in.
async fn planned_change_set(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    name: &str,
) -> String {
    let deadline = Instant::now() + DEADLINE;
    let mut last = String::new();
    while Instant::now() < deadline {
        last = cfn_ok(
            router,
            case,
            "DescribeChangeSet",
            &[member("StackName", stack), member("ChangeSetName", name)],
        )
        .await;
        let execution = element(&last, "ExecutionStatus");
        if execution != "UNAVAILABLE" {
            return execution;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    panic!("case {case:?}: the change set {name} never settled within {DEADLINE:?}: {last}");
}

/// The `ResourceStatus` of every event the stack recorded for the resource `logical`, in the
/// order `DescribeStackEvents` reports them.
///
/// Each event's two members are read out of the one `<member>` that carries both rather than by
/// zipping two document-ordered lists, so an event missing one of them is left out instead of
/// misaligning the rest.
async fn resource_event_statuses(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    logical: &str,
) -> Vec<String> {
    let body = cfn_ok(
        router,
        case,
        "DescribeStackEvents",
        &[member("StackName", stack)],
    )
    .await;
    members_of(&body)
        .into_iter()
        .filter_map(|event| {
            let reported = elements(event, "LogicalResourceId").into_iter().next()?;
            let status = elements(event, "ResourceStatus").into_iter().next()?;
            (reported == logical).then_some(status)
        })
        .collect()
}

/// The logical ids `ListStackResources` reports for the stack, in the order it reports them.
async fn listed_resources(router: &MultiServiceRouter, case: &str, stack: &str) -> Vec<String> {
    let body = cfn_ok(
        router,
        case,
        "ListStackResources",
        &[member("StackName", stack)],
    )
    .await;
    members_of(&body)
        .into_iter()
        .filter_map(|summary| elements(summary, "LogicalResourceId").into_iter().next())
        .collect()
}

/// The attributes SNS holds for the topic at `arn`, or `None` where SNS holds no such topic.
async fn topic_attributes(
    router: &MultiServiceRouter,
    arn: &str,
) -> Option<BTreeMap<String, String>> {
    let (status, body) = query(
        router,
        "sns",
        "2010-03-31",
        "GetTopicAttributes",
        &[member("TopicArn", arn)],
    )
    .await;
    if status != 200 {
        return None;
    }
    Some(decoded_entries(&body))
}

/// The `DisplayName` SNS holds for the topic the stack's `logical` resource declares, or `None`
/// where SNS holds no such topic at all.
async fn held_display_name(
    router: &MultiServiceRouter,
    stack: &str,
    logical: &str,
) -> Option<String> {
    topic_attributes(router, &topic_arn(stack, logical))
        .await
        .and_then(|attributes| attributes.get(WATCHED_ATTRIBUTE).cloned())
}

/// The stack this suite's rows revise: ONE topic, carrying [`BEFORE`], created with no rollback
/// configuration at all — what the revision is monitored by is the row's own.
async fn given_stack(router: &MultiServiceRouter, case: &str, stack: &str) {
    cfn_ok(
        router,
        case,
        "CreateStack",
        &[
            member("StackName", stack),
            member("TemplateBody", &template(stack, &[(EXISTING, BEFORE)])),
        ],
    )
    .await;
    let created = settled_stack(router, case, stack).await;
    assert_eq!(
        opt_element(&created, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "case {case:?}: the stack under test must be created, got {created}"
    );
    let held = held_display_name(router, stack, EXISTING).await;
    assert_eq!(
        held.as_deref(),
        Some(BEFORE),
        "case {case:?}: SNS must hold the deployed {WATCHED_ATTRIBUTE}, got {held:?}"
    );
}

/// The revision's template members: the resource the stack already has — with the
/// `DisplayName` a row that changes it declares — and the ones it would add.
fn revision(stack: &str, added: &[&str], changes_existing: bool) -> String {
    let existing_display = if changes_existing { AFTER } else { BEFORE };
    let mut topics = vec![(EXISTING, existing_display)];
    for logical in added {
        topics.push((*logical, AFTER));
    }
    template(stack, &topics)
}

/// One row of the ABORT table: how the trigger alarm reached `ALARM`, what the revision would
/// do, and the operation that deploys it.
struct Row {
    case: &'static str,
    /// The stack name, which also seeds the topic and alarm names so rows cannot collide.
    stack: &'static str,
    reached: Reached,
    /// The logical ids the revision would ADD to the stack.
    added: &'static [&'static str],
    /// Whether the revision would ALSO change the resource the stack already has.
    changes_existing: bool,
    deploy: Deploy,
}

/// An alarm in `ALARM` on CloudWatch's own evaluation, and one in `ALARM` because a caller
/// forced it there over an evaluation that says otherwise.
const EVALUATED_ALARM: Reached = Reached::Evaluated(TREAT_MISSING_BREACHING);
const FORCED_ALARM: Reached = Reached::Forced(TREAT_MISSING_NOT_BREACHING, ALARM_STATE);

/// The monitoring period the rows ask for, in minutes — the member is about the period watched
/// AFTER the resources are deployed, so what it is set to cannot decide a rule about an
/// operation that deploys none.
const MONITORING: i32 = 1;

#[tokio::test]
async fn a_trigger_already_in_alarm_aborts_the_operation_before_any_resource_action() {
    let rows: &[Row] = &[
        Row {
            case: "an executed change set adds nothing",
            stack: "rbt-abort-execute",
            reached: EVALUATED_ALARM,
            added: &[ADDED],
            changes_existing: false,
            deploy: Deploy::ExecuteChangeSet,
        },
        Row {
            case: "an executed change set that would add two resources and revise the first",
            stack: "rbt-abort-execute-many",
            reached: FORCED_ALARM,
            added: &[ADDED, ALSO_ADDED],
            changes_existing: true,
            deploy: Deploy::ExecuteChangeSet,
        },
        Row {
            case: "an update carrying the configuration itself adds nothing",
            stack: "rbt-abort-update",
            reached: EVALUATED_ALARM,
            added: &[ADDED],
            changes_existing: true,
            deploy: Deploy::UpdateStack,
        },
    ];

    for row in rows {
        let case = row.case;
        let router = build_router();
        let alarm = format!("{}-alarm", row.stack);
        let arn = given_alarm(&router, case, &alarm, row.reached, ALARM_STATE).await;
        given_stack(&router, case, row.stack).await;

        let mut members = vec![member(
            "TemplateBody",
            &revision(row.stack, row.added, row.changes_existing),
        )];
        members.extend(rollback_configuration(
            std::slice::from_ref(&arn),
            MONITORING,
        ));
        deploy_revision(&router, case, row.stack, members, row.deploy).await;

        let settled = settled_stack(&router, case, row.stack).await;
        let status = opt_element(&settled, "StackStatus").unwrap_or_default();
        assert_eq!(
            status, "UPDATE_ROLLBACK_COMPLETE",
            "case {case:?}: expected the aborted operation to report UPDATE_ROLLBACK_COMPLETE, \
             got {status:?} in {settled}"
        );

        // The rule: no event at all for a resource the aborted operation would have added.
        for logical in row.added {
            let events = resource_event_statuses(&router, case, row.stack, logical).await;
            assert!(
                events.is_empty(),
                "case {case:?}: expected no event for {logical}, which the aborted operation \
                 never touched, got {events:?}"
            );
        }
        // ...and none for the resource it would have CHANGED either: the only events that
        // resource carries are the ones of the create that made it.
        let existing_events = resource_event_statuses(&router, case, row.stack, EXISTING).await;
        let touched: Vec<&String> = existing_events
            .iter()
            .filter(|status| status.starts_with("UPDATE") || status.starts_with("DELETE"))
            .collect();
        assert!(
            touched.is_empty(),
            "case {case:?}: expected no update or delete event for {EXISTING}, got {touched:?} \
             among {existing_events:?}"
        );

        // ...so the stack's resources are exactly the ones in force before the operation.
        let listed = listed_resources(&router, case, row.stack).await;
        assert_eq!(
            listed,
            vec![EXISTING.to_string()],
            "case {case:?}: expected the aborted operation to leave only {EXISTING}, got \
             {listed:?}"
        );

        // ...and nothing was asked of the service that owns them: SNS holds no topic of the
        // added declarations' names, and the one it does hold still carries the deployed
        // property.
        for logical in row.added {
            let held = topic_attributes(&router, &topic_arn(row.stack, logical)).await;
            assert!(
                held.is_none(),
                "case {case:?}: expected SNS to hold no topic for {logical}, got {held:?}"
            );
        }
        let kept = held_display_name(&router, row.stack, EXISTING).await;
        assert_eq!(
            kept.as_deref(),
            Some(BEFORE),
            "case {case:?}: expected SNS to still hold the deployed {WATCHED_ATTRIBUTE}, got \
             {kept:?}"
        );
    }
}

/// One row of the CONTROL table: what monitors the revision while an alarm of the Region is in
/// `ALARM`, and the operation that deploys it.
struct ControlRow {
    case: &'static str,
    stack: &'static str,
    /// The state CloudWatch reports for the alarm this row creates.
    alarm_state: &'static str,
    /// Whether a trigger of the configuration NAMES that alarm.
    monitored: bool,
    deploy: Deploy,
}

/// The same revision, monitored by NO firing trigger, carries its changes out: the resource is
/// added in the service that owns it and the stack's event stream reports it.
///
/// The control half of the rule — an alarm in `ALARM` aborts an operation only where a trigger
/// of that operation's configuration names it, and a configuration whose triggers are not
/// firing stops nothing — so the abort table above cannot pass by refusing every monitored
/// operation.
#[tokio::test]
async fn a_revision_no_trigger_fires_on_adds_its_resource() {
    let rows: &[ControlRow] = &[
        ControlRow {
            case: "an alarm in ALARM that no trigger names stops nothing",
            stack: "rbt-control-unnamed",
            alarm_state: ALARM_STATE,
            monitored: false,
            deploy: Deploy::ExecuteChangeSet,
        },
        ControlRow {
            case: "a trigger whose alarm is in OK stops nothing",
            stack: "rbt-control-quiet",
            alarm_state: OK_STATE,
            monitored: true,
            deploy: Deploy::UpdateStack,
        },
    ];

    for row in rows {
        let case = row.case;
        let router = build_router();
        let alarm = format!("{}-alarm", row.stack);
        let reached = if row.alarm_state == ALARM_STATE {
            EVALUATED_ALARM
        } else {
            Reached::Evaluated(TREAT_MISSING_NOT_BREACHING)
        };
        let arn = given_alarm(&router, case, &alarm, reached, row.alarm_state).await;
        given_stack(&router, case, row.stack).await;

        let arns: Vec<String> = if row.monitored { vec![arn] } else { Vec::new() };
        let mut members = vec![member("TemplateBody", &revision(row.stack, &[ADDED], true))];
        // Zero minutes: a nonzero period holds a monitored operation open while it is watched,
        // and what such a period does is another suite's rule — what this row is about is the
        // change landing.
        members.extend(rollback_configuration(&arns, 0));
        deploy_revision(&router, case, row.stack, members, row.deploy).await;

        let settled = settled_stack(&router, case, row.stack).await;
        let status = opt_element(&settled, "StackStatus").unwrap_or_default();
        assert_eq!(
            status, "UPDATE_COMPLETE",
            "case {case:?}: expected the unmonitored revision to complete, got {status:?} in \
             {settled}"
        );

        // The added resource is reported as created, in the stack's own event stream —
        // "DescribeStackEvents returns all stack related events for a specified stack in
        // reverse chronological order", so the event the create ENDED in is the first.
        let events = resource_event_statuses(&router, case, row.stack, ADDED).await;
        assert_eq!(
            events.first().map(String::as_str),
            Some("CREATE_COMPLETE"),
            "case {case:?}: expected the added {ADDED} to be reported created, got {events:?}"
        );
        assert!(
            events.iter().any(|status| status == "CREATE_IN_PROGRESS"),
            "case {case:?}: expected the added {ADDED} to be reported as it was created, got \
             {events:?}"
        );
        // ...and in the resources the stack holds...
        let listed = listed_resources(&router, case, row.stack).await;
        assert!(
            listed.contains(&ADDED.to_string()) && listed.contains(&EXISTING.to_string()),
            "case {case:?}: expected the stack to hold {EXISTING} and {ADDED}, got {listed:?}"
        );
        // ...and the service that owns it really holds it, with the revised property on the
        // resource the revision changed.
        let added = held_display_name(&router, row.stack, ADDED).await;
        assert_eq!(
            added.as_deref(),
            Some(AFTER),
            "case {case:?}: expected SNS to hold the added topic, got {added:?}"
        );
        let revised = held_display_name(&router, row.stack, EXISTING).await;
        assert_eq!(
            revised.as_deref(),
            Some(AFTER),
            "case {case:?}: expected SNS to hold the revised {WATCHED_ATTRIBUTE}, got {revised:?}"
        );
    }
}
