// SPDX-License-Identifier: CC0-1.0

//! What `DeleteAlarmMuteRule` does, end to end over the wire.
//!
//! ONE rule, asserted over a table: **deleting an alarm mute rule removes the rule of that name
//! in the Region the request was signed for — succeeding whether or not such a rule exists — and
//! the alarms it was muting are unmuted at that instant: an alarm standing in `ALARM` when an
//! ACTIVE rule that targeted it is deleted has the actions configured for that state executed,
//! an alarm in any other state has nothing executed for it, and an alarm another ACTIVE rule
//! still targets stays muted.**
//!
//! "Deletes a specific alarm mute rule. When you delete a mute rule, any alarms that are
//! currently being muted by that rule are immediately unmuted. If those alarms are in an ALARM
//! state, their configured actions will trigger. This operation is idempotent. If you delete a
//! mute rule that does not exist, the operation succeeds without returning an error."
//! (`smithy_query(query="doc",shape="DeleteAlarmMuteRule")`), where a rule is muting something
//! only while it is ACTIVE — "ACTIVE: The mute rule is currently muting alarm actions"
//! (`smithy_query(query="doc",shape="GetAlarmMuteRuleOutput",member="Status")`) — and it is
//! "unique within your Amazon Web Services account and region"
//! (`smithy_query(query="doc",shape="PutAlarmMuteRuleInput",member="Name")`), so the Region the
//! call is signed for is part of which rule it names.
//!
//! The rows differ along what the rule generalises over: whether the deleted rule's window is
//! open at the instant of the delete, which expression form states that window, how many rules
//! target the alarm, which kind of alarm they target, the state that alarm stands in, the Region
//! the delete is signed for, and whether a rule of the named name exists at all.
//!
//! Every row's alarm reaches its state by its OWN evaluation — a `TreatMissingData` arm over a
//! series nothing publishes into — so the state it stands in when the rule is deleted is a state
//! it holds rather than one that expires.

use std::time::Instant;

use serde_json::{json, Value};

use crate::harness::{
    call_in_region, element, emulator, error_code, given_queue, grant_sns_delivery_on,
    notifications, rpc, rpc_in_region, set_alarm_state, sns, subscribe_queue, Emulator,
    LOOK_INTERVAL, QUIET_WATCH, REGION, WORK_WAIT,
};

/// The CloudWatch signing name and target prefix, for `rpc`.
const CLOUDWATCH: (&str, &str) = ("monitoring", "GraniteServiceVersion20100801");

/// Another Region, for the row whose delete is signed for a Region the rule does not live in.
const OTHER_REGION: &str = "us-west-2";

/// The namespace every row's alarm watches, and the metric in it. Nothing is ever published
/// into it: each row's alarm is decided by its missing-data arm alone.
const NAMESPACE: &str = "Custom/MuteRuleDeletion";
const METRIC: &str = "Latency";

/// The error class a read of a rule that is gone answers with
/// (`smithy_query(query="operation",operation="GetAlarmMuteRule")`).
const NOT_FOUND: &str = "ResourceNotFoundException";

/// Call one CloudWatch operation in the Region the alarms live in, requiring a 200.
async fn cw(emulator: &Emulator, operation: &str, body: &Value) -> Value {
    rpc(emulator, CLOUDWATCH.0, CLOUDWATCH.1, operation, body).await
}

/// When a row's mute rule window is open.
#[derive(Debug, Clone, Copy)]
enum Window {
    /// A one-time window that opened a minute ago and stays open for an hour: ACTIVE now.
    OpenNow,
    /// A recurring window that opens every minute and stays open for an hour: ACTIVE now too,
    /// stated in the other expression form.
    OpenEveryMinute,
    /// A one-time window hours away: SCHEDULED, so it is muting nothing.
    StillToCome,
}

/// One mute rule a row writes.
#[derive(Debug, Clone)]
struct Rule {
    /// When its window is open.
    window: Window,
    /// The Region it is written in, which is the Region it lives in.
    region: &'static str,
}

/// One `DeleteAlarmMuteRule` call a row makes.
#[derive(Debug, Clone, Copy)]
struct Deletion {
    /// Which of the row's rules it names, or `None` for a name no rule of the row carries.
    rule: Option<usize>,
    /// The Region the call is signed for.
    region: &'static str,
    /// How many notifications the alarm's target must have received once the call has answered
    /// and the queue has been given time to receive.
    delivered: usize,
}

/// One row of the table.
struct Row {
    /// What this row is about, named by every assertion it makes.
    case: &'static str,
    /// Whether the alarm the rules target is a COMPOSITE alarm rather than a metric alarm.
    composite: bool,
    /// The `TreatMissingData` arm that decides the alarm, and so the state it holds:
    /// `breaching` holds it in `ALARM`, `notBreaching` in `OK`.
    treat_missing_data: &'static str,
    /// The state that arm puts the alarm in, which it then holds.
    state: &'static str,
    /// How many notifications the transition INTO that state delivers: none while a rule
    /// targeting the alarm is ACTIVE, one when none is.
    delivered_on_transition: usize,
    /// The rules written before the alarm is read, all of them targeting the row's own alarm.
    rules: Vec<Rule>,
    /// The deletes the row makes, in order.
    deletions: Vec<Deletion>,
    /// A state to force afterwards, and how many notifications that transition must deliver —
    /// the other half of "immediately unmuted" for an alarm that was not in `ALARM`.
    then_forced: Option<(&'static str, usize)>,
}

/// An `at` expression naming `at` as the local wall-clock instant UTC puts it at: "At
/// expressions … Format: `at(yyyy-MM-ddThh:mm)`" (`Schedule$Expression`), read in UTC because no
/// row names a time zone ("If you don't specify a time zone, UTC is used by default.",
/// `Schedule$Timezone`).
fn at_expression(at: ::chrono::DateTime<::chrono::Utc>) -> String {
    format!("at({})", at.format("%Y-%m-%dT%H:%M"))
}

/// The schedule one window is stated by.
fn schedule_of(window: Window) -> Value {
    let now = ::chrono::Utc::now();
    match window {
        Window::OpenNow => json!({
            "Expression": at_expression(now - ::chrono::TimeDelta::minutes(1)),
            "Duration": "PT1H",
        }),
        Window::OpenEveryMinute => json!({"Expression": "cron(* * * * *)", "Duration": "PT1H"}),
        Window::StillToCome => json!({
            "Expression": at_expression(now + ::chrono::TimeDelta::hours(3)),
            "Duration": "PT1H",
        }),
    }
}

/// Write one mute rule targeting `alarm_name`.
async fn given_mute_rule(emulator: &Emulator, name: &str, rule: &Rule, alarm_name: &str) {
    rpc_in_region(
        emulator,
        rule.region,
        CLOUDWATCH.0,
        CLOUDWATCH.1,
        "PutAlarmMuteRule",
        &json!({
            "Name": name,
            "Rule": {"Schedule": schedule_of(rule.window)},
            "MuteTargets": {"AlarmNames": [alarm_name]},
        }),
    )
    .await;
}

/// Create the alarm a row's rules target: a metric alarm decided by its missing-data arm, or a
/// composite alarm over such a metric alarm. Only the alarm the rules target carries actions.
async fn given_alarm(emulator: &Emulator, row: &Row, name: &str, topic_arn: &str) {
    let child = format!("{name}-child");
    let watched = if row.composite { &child } else { name };
    let mut metric = json!({
        "AlarmName": watched,
        "Namespace": NAMESPACE,
        "MetricName": METRIC,
        "Dimensions": [{"Name": "Alarm", "Value": name}],
        "Statistic": "Average",
        "Period": 60,
        "EvaluationPeriods": 1,
        "DatapointsToAlarm": 1,
        "Threshold": 10.0,
        "ComparisonOperator": "GreaterThanThreshold",
        "TreatMissingData": row.treat_missing_data,
    });
    if !row.composite {
        metric["AlarmActions"] = json!([topic_arn]);
        metric["OKActions"] = json!([topic_arn]);
    }
    cw(emulator, "PutMetricAlarm", &metric).await;
    if row.composite {
        cw(
            emulator,
            "PutCompositeAlarm",
            &json!({
                "AlarmName": name,
                "AlarmRule": format!("ALARM(\"{child}\")"),
                "AlarmActions": [topic_arn],
                "OKActions": [topic_arn],
            }),
        )
        .await;
    }
}

/// The state the alarm `name` of `alarm_type` reports.
async fn reported_state(emulator: &Emulator, name: &str, alarm_type: &str) -> String {
    let reported = cw(
        emulator,
        "DescribeAlarms",
        &json!({"AlarmNames": [name], "AlarmTypes": [alarm_type]}),
    )
    .await;
    let list = if alarm_type == "CompositeAlarm" {
        "CompositeAlarms"
    } else {
        "MetricAlarms"
    };
    reported[list][0]["StateValue"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// Look at the alarm until it reports `wanted`, and answer the last state it reported — a wait
/// for the CONDITION, so the case asserts on a state rather than on a span.
async fn until_reported(emulator: &Emulator, name: &str, alarm_type: &str, wanted: &str) -> String {
    let deadline = Instant::now() + WORK_WAIT;
    let mut last = reported_state(emulator, name, alarm_type).await;
    while last != wanted && Instant::now() < deadline {
        ::tokio::time::sleep(LOOK_INTERVAL).await;
        last = reported_state(emulator, name, alarm_type).await;
    }
    last
}

/// Collect into `held` every notification the queue at `url` receives, until it holds `want` of
/// them or the wait is up.
///
/// A delivery is asynchronous, so a case that expects one waits for it; a case that expects NONE
/// watches the same path for [`QUIET_WATCH`], which is an answer rather than a race.
async fn collect(emulator: &Emulator, url: &str, held: &mut Vec<Value>, want: usize) {
    if held.len() >= want {
        // Nothing new is expected: the same path is watched for a quiet span, so an empty queue
        // is an answer rather than a race.
        let deadline = Instant::now() + QUIET_WATCH;
        while Instant::now() < deadline {
            held.extend(notifications(emulator, url).await);
            if held.len() > want {
                break;
            }
            ::tokio::time::sleep(LOOK_INTERVAL).await;
        }
        return;
    }
    let deadline = Instant::now() + WORK_WAIT;
    while Instant::now() < deadline {
        held.extend(notifications(emulator, url).await);
        if held.len() >= want {
            break;
        }
        ::tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// Whether a read of the rule `name` in `region` finds it, asserting that a rule that is gone is
/// reported gone by its own error class rather than as an empty answer.
async fn rule_is_held(emulator: &Emulator, case: &str, region: &str, name: &str) -> bool {
    let (status, body) = call_in_region(
        &emulator.router,
        region,
        "GetAlarmMuteRule",
        &json!({"AlarmMuteRuleName": name}),
    )
    .await;
    if status == 200 {
        return true;
    }
    assert_eq!(
        status, 404,
        "case {case:?}: a rule that is gone must be answered for with 404, got {status} {body}"
    );
    assert_eq!(
        error_code(&body),
        NOT_FOUND,
        "case {case:?}: expected {NOT_FOUND} for the deleted rule {name:?}, got {body}"
    );
    false
}

/// The alarm mute rule ARNs `ListAlarmMuteRules` reports in `region`.
///
/// A summary names its rule by ARN rather than by name
/// (`smithy_query(query="shape",shape="AlarmMuteRuleSummary")`), and an alarm mute rule's ARN
/// ends with the rule's own name
/// (`arn:aws:cloudwatch:[REGION]:123456789012:alarm-mute-rule:*`, `PutAlarmMuteRule`).
async fn listed_rule_arns(emulator: &Emulator, region: &str) -> Vec<String> {
    let listed = rpc_in_region(
        emulator,
        region,
        CLOUDWATCH.0,
        CLOUDWATCH.1,
        "ListAlarmMuteRules",
        &json!({}),
    )
    .await;
    listed["AlarmMuteRuleSummaries"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|summary| summary["AlarmMuteRuleArn"].as_str().map(ToString::to_string))
        .collect()
}

#[tokio::test]
async fn deleting_a_mute_rule_unmutes_the_alarms_it_was_muting() {
    let open = Rule {
        window: Window::OpenNow,
        region: REGION,
    };
    let rows: Vec<Row> = vec![
        Row {
            case: "an open window muting an alarm that stands in ALARM",
            composite: false,
            treat_missing_data: "breaching",
            state: "ALARM",
            delivered_on_transition: 0,
            rules: vec![open.clone()],
            deletions: vec![Deletion {
                rule: Some(0),
                region: REGION,
                delivered: 1,
            }],
            then_forced: None,
        },
        Row {
            case: "a recurring window muting a COMPOSITE alarm that stands in ALARM",
            composite: true,
            treat_missing_data: "breaching",
            state: "ALARM",
            delivered_on_transition: 0,
            rules: vec![Rule {
                window: Window::OpenEveryMinute,
                region: REGION,
            }],
            deletions: vec![Deletion {
                rule: Some(0),
                region: REGION,
                delivered: 1,
            }],
            then_forced: None,
        },
        Row {
            case: "an open window muting an alarm that stands in OK",
            composite: false,
            treat_missing_data: "notBreaching",
            state: "OK",
            delivered_on_transition: 0,
            rules: vec![open.clone()],
            // Nothing is configured for the state it is in to execute, and the delete leaves it
            // unmuted: the next transition's actions are executed.
            deletions: vec![Deletion {
                rule: Some(0),
                region: REGION,
                delivered: 0,
            }],
            then_forced: Some(("ALARM", 1)),
        },
        Row {
            case: "two open windows, deleted one after the other",
            composite: false,
            treat_missing_data: "breaching",
            state: "ALARM",
            delivered_on_transition: 0,
            rules: vec![
                open.clone(),
                Rule {
                    window: Window::OpenEveryMinute,
                    region: REGION,
                },
            ],
            deletions: vec![
                Deletion {
                    rule: Some(0),
                    region: REGION,
                    delivered: 0,
                },
                Deletion {
                    rule: Some(1),
                    region: REGION,
                    delivered: 1,
                },
            ],
            then_forced: None,
        },
        Row {
            case: "a window still to come, which was muting nothing",
            composite: false,
            treat_missing_data: "breaching",
            state: "ALARM",
            delivered_on_transition: 1,
            rules: vec![Rule {
                window: Window::StillToCome,
                region: REGION,
            }],
            deletions: vec![Deletion {
                rule: Some(0),
                region: REGION,
                delivered: 1,
            }],
            then_forced: None,
        },
        Row {
            case: "the same name deleted in another Region first",
            composite: false,
            treat_missing_data: "breaching",
            state: "ALARM",
            delivered_on_transition: 0,
            rules: vec![open.clone()],
            deletions: vec![
                Deletion {
                    rule: Some(0),
                    region: OTHER_REGION,
                    delivered: 0,
                },
                Deletion {
                    rule: Some(0),
                    region: REGION,
                    delivered: 1,
                },
            ],
            then_forced: None,
        },
        Row {
            case: "a name no rule carries, beside a rule that keeps muting",
            composite: false,
            treat_missing_data: "breaching",
            state: "ALARM",
            delivered_on_transition: 0,
            rules: vec![open.clone()],
            deletions: vec![Deletion {
                rule: None,
                region: REGION,
                delivered: 0,
            }],
            then_forced: None,
        },
        Row {
            case: "a name no rule carries, with no mute rule in the store at all",
            composite: false,
            treat_missing_data: "breaching",
            state: "ALARM",
            delivered_on_transition: 1,
            rules: Vec::new(),
            deletions: vec![Deletion {
                rule: None,
                region: REGION,
                delivered: 1,
            }],
            then_forced: None,
        },
    ];

    let emulator = emulator();
    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let alarm_name = format!("mute-delete-{index}");
        let alarm_type = if row.composite {
            "CompositeAlarm"
        } else {
            "MetricAlarm"
        };

        // The path a notification travels: a queue subscribed to the topic the alarm's actions
        // name, which is where a client sees the notification — or sees that none arrived.
        let (queue_url, queue_arn) = given_queue(&emulator, &alarm_name).await;
        grant_sns_delivery_on(&emulator, &queue_url, &queue_arn).await;
        let topic_arn = element(
            &sns(&emulator, "CreateTopic", &[("Name", &alarm_name)]).await,
            "TopicArn",
        );
        subscribe_queue(&emulator, &topic_arn, &queue_arn).await;

        let rule_names: Vec<String> = (0..row.rules.len())
            .map(|ordinal| format!("{alarm_name}-rule-{ordinal}"))
            .collect();
        for (name, rule) in rule_names.iter().zip(&row.rules) {
            given_mute_rule(&emulator, name, rule, &alarm_name).await;
        }
        given_alarm(&emulator, row, &alarm_name, &topic_arn).await;

        // The alarm reaches the state its own missing-data arm decides, and holds it.
        let reached = until_reported(&emulator, &alarm_name, alarm_type, row.state).await;
        assert_eq!(
            reached, row.state,
            "case {case:?}: expected the alarm to reach {}, got {reached:?}",
            row.state
        );
        let mut held: Vec<Value> = Vec::new();
        collect(
            &emulator,
            &queue_url,
            &mut held,
            row.delivered_on_transition,
        )
        .await;
        assert_eq!(
            held.len(),
            row.delivered_on_transition,
            "case {case:?}: expected {} notification(s) from the transition into {}, got {held:?}",
            row.delivered_on_transition,
            row.state
        );

        for deletion in &row.deletions {
            let named = deletion
                .rule
                .and_then(|ordinal| rule_names.get(ordinal).cloned())
                .unwrap_or_else(|| format!("{alarm_name}-rule-absent"));
            let (status, body) = call_in_region(
                &emulator.router,
                deletion.region,
                "DeleteAlarmMuteRule",
                &json!({"AlarmMuteRuleName": named}),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: DeleteAlarmMuteRule({named:?}) in {} must answer 200, got \
                 {status} {body}",
                deletion.region
            );

            // The rule it named is gone from the Region it was signed for, and nothing else is.
            assert!(
                !rule_is_held(&emulator, case, deletion.region, &named).await,
                "case {case:?}: the rule {named:?} must be gone from {} after the delete",
                deletion.region
            );
            let listed = listed_rule_arns(&emulator, deletion.region).await;
            let suffix = format!(":alarm-mute-rule:{named}");
            assert!(
                !listed.iter().any(|arn| arn.ends_with(&suffix)),
                "case {case:?}: {named:?} must be listed no more in {}, got {listed:?}",
                deletion.region
            );

            collect(&emulator, &queue_url, &mut held, deletion.delivered).await;
            assert_eq!(
                held.len(),
                deletion.delivered,
                "case {case:?}: expected {} notification(s) in all once {named:?} was deleted in \
                 {}, got {held:?}",
                deletion.delivered,
                deletion.region
            );
            // Whatever the delete released, the alarm itself did not move: a mute decides the
            // actions, never the state.
            let standing = reported_state(&emulator, &alarm_name, alarm_type).await;
            assert_eq!(
                standing, row.state,
                "case {case:?}: the alarm must still report {}, got {standing:?}",
                row.state
            );
        }

        // The rules the row did not delete are still held, in the Region they were written in.
        let deleted: Vec<usize> = row
            .deletions
            .iter()
            .filter(|deletion| deletion.region == REGION)
            .filter_map(|deletion| deletion.rule)
            .collect();
        for (ordinal, (name, rule)) in rule_names.iter().zip(&row.rules).enumerate() {
            if deleted.contains(&ordinal) {
                continue;
            }
            assert!(
                rule_is_held(&emulator, case, rule.region, name).await,
                "case {case:?}: the rule {name:?} no delete named must still be held in {}",
                rule.region
            );
        }

        if let Some((forced, delivered)) = row.then_forced {
            set_alarm_state(&emulator, &alarm_name, forced).await;
            collect(&emulator, &queue_url, &mut held, delivered).await;
            // Counted by the state each notification announces: the alarm's own evaluation takes
            // a forced state back a span later, and the transition BACK fires the actions of the
            // state it returns to — a delivery of its own, which is no part of what this step is
            // about.
            let moved = held
                .iter()
                .filter(|notification| notification["NewStateValue"] == json!(forced))
                .count();
            assert_eq!(
                moved, delivered,
                "case {case:?}: expected {delivered} notification(s) of the move to {forced} \
                 once the unmuted alarm made it, got {held:?}"
            );
        }
    }
}
