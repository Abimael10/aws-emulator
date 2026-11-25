// SPDX-License-Identifier: CC0-1.0

//! A COMPOSITE alarm whose child a caller DRIVES with `SetAlarmState`, end to end over the wire
//! with CloudWatch, SNS and SQS on one engine — so what a client sees is what is asserted: the
//! state `DescribeAlarms` reports for the composite, and the notification that reaches the queue
//! subscribed to the topic on its `AlarmActions`.
//!
//! The RULE asserted here: **a state `SetAlarmState` forces on an alarm stands until that
//! alarm's own next evaluation and is the state every composite alarm's `AlarmRule` is evaluated
//! over ("It returns to its actual state only once any of its children alarms change state",
//! `smithy_query(query="operation",operation="SetAlarmState")`), so a composite whose rule the
//! forced child states make true moves into `ALARM` and executes "the actions to execute when
//! this alarm transitions to the ALARM state" (`PutCompositeAlarmInput$AlarmActions`) — ONE
//! notification on each SNS topic of that list, whose message names the composite
//! (`AlarmName`), the state it moved to (`NewStateValue`), the rule as `PutCompositeAlarm`
//! stored it (`AlarmRule`) and, in `TriggeringChildren`, the children that rule read with the
//! state each was in, the forced one among them.**
//!
//! The rows differ along what the rule generalises over: how the state functions are combined
//! (one child, a disjunction, a conjunction, a negation), which state function the rule reads
//! (`ALARM`, `OK`, `INSUFFICIENT_DATA`), how the rule references a child (quoted name, bare
//! name, ARN), how many children it reads and which of them the caller forces, the state they
//! are forced into, and the missing-data treatment that decides the children's own evaluated
//! state. The recorded case — one child referenced by quoted name, forced into `ALARM`, under
//! `notBreaching` — is the first row.

use serde_json::{json, Value};
use std::time::Instant;

use crate::harness::{
    emulator, given_topic_with_subscriber, rpc, set_alarm_state, until_notified, Emulator,
    LOOK_INTERVAL, TARGET_PREFIX, WORK_WAIT,
};

/// The namespace the children of every row watch. Nothing is published into it, so each child's
/// own evaluated state is whatever its missing-data treatment says.
const NAMESPACE: &str = "sim/CompositePaging";

/// The two children every row creates.
const CHILDREN: [&str; 2] = ["kid-a", "kid-b"];

/// The composite alarm every row creates: the one thing wired to the paging topic.
const COMPOSITE: &str = "paging-composite";

/// How a row's rule REFERENCES a child — "you can use either the alarm name or the ARN"
/// (`PutCompositeAlarmInput$AlarmRule`).
#[derive(Debug, Clone, Copy)]
enum Reference {
    /// The alarm's name in quotes.
    QuotedName,
    /// The same name with no quotes at all.
    BareName,
    /// The ARN `DescribeAlarms` reports for that alarm, quoted.
    Arn,
}

impl Reference {
    /// The text a rule names the alarm `name`, whose ARN is `arn`, by.
    fn of(self, name: &str, arn: &str) -> String {
        match self {
            Self::QuotedName => format!("\"{name}\""),
            Self::BareName => name.to_string(),
            Self::Arn => format!("\"{arn}\""),
        }
    }
}

/// One row of the table: a rule over the two children, and the states the caller forces on
/// them.
#[derive(Debug, Clone, Copy)]
struct Row {
    /// What this row is, for the failure message of every assertion it makes.
    case: &'static str,
    /// The `TreatMissingData` the children are configured with, which decides the state their
    /// own evaluation computes.
    treat_missing: &'static str,
    /// How the rule references them.
    reference: Reference,
    /// The rule, over the placeholders `{a}` and `{b}` for the two children.
    template: &'static str,
    /// The (child index, state) pairs the caller forces, in the order it forces them.
    forced: &'static [(usize, &'static str)],
}

/// One CloudWatch call, requiring a 200.
async fn cw(emulator: &Emulator, operation: &str, body: &Value) -> Value {
    rpc(emulator, "monitoring", TARGET_PREFIX, operation, body).await
}

/// A child metric alarm of `name` over a series of its own, with no action list of its own, so
/// every notification on the row's queue is the composite's.
async fn given_child(emulator: &Emulator, name: &str, treat_missing: &str) -> String {
    cw(
        emulator,
        "PutMetricAlarm",
        &json!({
            "AlarmName": name,
            "Namespace": NAMESPACE,
            "MetricName": "Errors",
            "Dimensions": [{"Name": "Child", "Value": name}],
            "Statistic": "Average",
            "Period": 60,
            "EvaluationPeriods": 1,
            "Threshold": 1.0,
            "ComparisonOperator": "GreaterThanThreshold",
            "TreatMissingData": treat_missing,
        }),
    )
    .await;
    let described = cw(emulator, "DescribeAlarms", &json!({"AlarmNames": [name]})).await;
    described["MetricAlarms"][0]["AlarmArn"]
        .as_str()
        .unwrap_or_else(|| panic!("DescribeAlarms {name}: no AlarmArn in {described}"))
        .to_string()
}

/// The composite alarm `name` as `DescribeAlarms` reports it, read through the alarm-type filter
/// a client reading composite alarms uses.
async fn composite_alarm(emulator: &Emulator, name: &str) -> Value {
    let described = cw(
        emulator,
        "DescribeAlarms",
        &json!({"AlarmNames": [name], "AlarmTypes": ["CompositeAlarm"]}),
    )
    .await;
    described["CompositeAlarms"][0].clone()
}

/// Look at the composite alarm `name` until it reports `wanted`, and answer the last read — a
/// wait for the CONDITION, so a row asserts on a state rather than on a span.
async fn until_composite_state(emulator: &Emulator, name: &str, wanted: &str) -> Value {
    let deadline = Instant::now() + WORK_WAIT;
    loop {
        let alarm = composite_alarm(emulator, name).await;
        if alarm["StateValue"].as_str() == Some(wanted) || Instant::now() >= deadline {
            return alarm;
        }
        ::tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// The `TriggeringChildren` of a notification, as (arn, state value) pairs.
fn triggering_children(notification: &Value) -> Vec<(String, String)> {
    notification["TriggeringChildren"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|child| {
            (
                child["Arn"].as_str().unwrap_or_default().to_string(),
                child["State"]["Value"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect()
}

#[tokio::test]
async fn a_composite_alarm_notifies_its_topic_when_a_forced_child_makes_its_rule_true() {
    let rows: &[Row] = &[
        // The recorded shape: one child, named in quotes, forced into ALARM.
        Row {
            case: "one child in ALARM, named in quotes",
            treat_missing: "notBreaching",
            reference: Reference::QuotedName,
            template: "ALARM({a})",
            forced: &[(0, "ALARM")],
        },
        // A disjunction needs only one of the two children, and the forced one is the second.
        Row {
            case: "a disjunction completed by the second child",
            treat_missing: "notBreaching",
            reference: Reference::BareName,
            template: "ALARM({a}) OR ALARM({b})",
            forced: &[(1, "ALARM")],
        },
        // A conjunction needs both of them, each forced in its own call.
        Row {
            case: "a conjunction over both children",
            treat_missing: "missing",
            reference: Reference::QuotedName,
            template: "ALARM({a}) AND ALARM({b})",
            forced: &[(0, "ALARM"), (1, "ALARM")],
        },
        // The rule reads another state function, over a child referenced by ARN.
        Row {
            case: "an OK rule over a child named by ARN",
            treat_missing: "missing",
            reference: Reference::Arn,
            template: "OK({a})",
            forced: &[(0, "OK")],
        },
        // A negated rule: the force takes the child OUT of the state the rule negates.
        Row {
            case: "a negated INSUFFICIENT_DATA rule",
            treat_missing: "missing",
            reference: Reference::QuotedName,
            template: "NOT INSUFFICIENT_DATA({a})",
            forced: &[(0, "ALARM")],
        },
    ];

    for Row {
        case,
        treat_missing,
        reference,
        template,
        forced,
    } in rows
    {
        let emulator = emulator();
        let (topic, queue_url) = given_topic_with_subscriber(&emulator, "paging").await;
        let mut arns = Vec::new();
        for child in CHILDREN {
            arns.push(given_child(&emulator, child, treat_missing).await);
        }
        let rule = template
            .replace("{a}", &reference.of(CHILDREN[0], &arns[0]))
            .replace("{b}", &reference.of(CHILDREN[1], &arns[1]));
        cw(
            &emulator,
            "PutCompositeAlarm",
            &json!({
                "AlarmName": COMPOSITE,
                "AlarmRule": rule,
                "AlarmActions": [topic],
            }),
        )
        .await;

        // The drive: each child the row names is forced into its state, which is the state the
        // composite's rule is then evaluated over.
        for (child, state) in *forced {
            set_alarm_state(&emulator, CHILDREN[*child], state).await;
        }

        let fired = until_composite_state(&emulator, COMPOSITE, "ALARM").await;
        assert_eq!(
            fired["StateValue"].as_str(),
            Some("ALARM"),
            "case {case:?} (rule {rule:?}): the forced children must put the composite in ALARM, \
             got {fired}"
        );
        // The rule is reported exactly as it was stored, which is also what the notification
        // below has to carry.
        assert_eq!(
            fired["AlarmRule"].as_str(),
            Some(rule.as_str()),
            "case {case:?}: the composite must report the rule as stored, got {fired}"
        );

        let delivered = until_notified(&emulator, &queue_url).await;
        assert_eq!(
            delivered.len(),
            1,
            "case {case:?} (rule {rule:?}): the composite's ALARM transition must publish exactly \
             one notification to its action topic, got {delivered:?}"
        );
        let notification = &delivered[0];
        assert_eq!(
            notification["AlarmName"].as_str(),
            Some(COMPOSITE),
            "case {case:?}: the notification must name the composite alarm, got {notification}"
        );
        assert_eq!(
            notification["NewStateValue"].as_str(),
            Some("ALARM"),
            "case {case:?}: the notification must carry the state moved to, got {notification}"
        );
        assert_ne!(
            notification["OldStateValue"].as_str(),
            Some("ALARM"),
            "case {case:?}: the notification must report the state the composite left, got \
             {notification}"
        );
        assert_eq!(
            notification["AlarmRule"].as_str(),
            Some(rule.as_str()),
            "case {case:?}: the notification must carry the rule as stored, got {notification}"
        );

        // The children the rule read, each by the ARN `DescribeAlarms` reports for it and with
        // the state it was in: the forced ones are named with the state they were forced into,
        // and no alarm the rule never read is named at all.
        let named = triggering_children(notification);
        assert!(
            !named.is_empty(),
            "case {case:?}: the notification must name the children the rule read, got \
             {notification}"
        );
        let referenced: &[String] = if template.contains("{b}") {
            &arns
        } else {
            &arns[..1]
        };
        for (arn, _) in &named {
            assert!(
                referenced.contains(arn),
                "case {case:?}: {arn:?} is no child of the rule {rule:?}, got {notification}"
            );
        }
        for (child, state) in *forced {
            let expected = (arns[*child].clone(), (*state).to_string());
            assert!(
                named.contains(&expected),
                "case {case:?}: the notification must name the forced child {expected:?} among \
                 the triggering children, got {named:?}"
            );
        }
    }
}
