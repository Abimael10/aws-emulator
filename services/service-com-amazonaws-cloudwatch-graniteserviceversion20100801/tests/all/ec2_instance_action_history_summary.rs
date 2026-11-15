// SPDX-License-Identifier: CC0-1.0

//! What the alarm-history `Action` item of an executed action SAYS, over the real
//! `MultiServiceRouter` with CloudWatch, SNS and EC2 on one engine.
//!
//! The RULE asserted here: **an `Action` history item accounts for the execution by naming the
//! action's own ARN, and where that ARN names an OPERATION rather than the resource it was
//! performed on — an EC2 instance action, `arn:aws:automate:<region>:ec2:<verb>` or its Simple
//! Workflow form — the summary also names the instance the alarm's metric is dimensioned by,
//! in single quotes, whether the operation succeeded or failed. An action whose ARN names its
//! own target (an SNS topic) names that ARN alone.**
//!
//! The rows span what the rule generalises over: the VERB (stop, terminate, reboot, recover),
//! the ARN FORM (`automate` and Simple Workflow), which action list carries it and therefore
//! which transition fires it, the OUTCOME (an instance EC2 holds, and an id it holds no
//! instance for, which fails), and the action KIND (an EC2 instance action against an SNS topic
//! action on an alarm watching the very same instance). One row's actions are disabled, so a
//! suppressed action is shown to account for nothing at all.

use std::time::Duration;

use serde_json::{json, Value};

use service_com_amazonaws_ec2_amazonec2 as ec2;

use ec2::state::{ComAmazonawsEc2Amazonec2State as Ec2State, ImageSpec};

use crate::harness::{
    ec2 as ec2_api, element, emulator, rpc, set_alarm_state, sns, Emulator, REGION, TARGET_PREFIX,
};

/// How long a row waits for an asynchronous effect, and how often it looks again. Generous only
/// so a failure is a failure rather than a hang; every wait below is for a CONDITION.
const DEADLINE: Duration = Duration::from_secs(20);
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// The `automate` form of one EC2 instance action:
/// "EC2 actions: arn:aws:automate:region:ec2:stop arn:aws:automate:region:ec2:terminate
/// arn:aws:automate:region:ec2:reboot arn:aws:automate:region:ec2:recover …"
/// (`smithy_query(query="doc",shape="PutMetricAlarmInput",member="AlarmActions")`).
fn automate_action(verb: &str) -> String {
    format!("arn:aws:automate:{REGION}:ec2:{verb}")
}

/// The Simple Workflow form of the same action, from the same documentation. The account is the
/// one this emulator mints its own ARNs under.
fn swf_action(verb: &str) -> String {
    let capitalised = format!("{}{}", verb[..1].to_uppercase(), &verb[1..]);
    format!("arn:aws:swf:{REGION}:000000000000:action/actions/AWS_EC2.InstanceId.{capitalised}/1.0")
}

/// A `(key, value)` ec2Query form field.
fn p(key: &str, value: &str) -> (String, String) {
    (key.to_string(), value.to_string())
}

/// One CloudWatch call, requiring a 200.
async fn cw(emulator: &Emulator, operation: &str, body: &Value) -> Value {
    rpc(emulator, "monitoring", TARGET_PREFIX, operation, body).await
}

/// One AMI of the calling account, seeded the way a register operation would store it: a launch
/// resolves the image it names, so the suite needs an image before it has an instance.
fn given_image(emulator: &Emulator) -> String {
    let store = emulator.engine.state::<Ec2State>();
    let mut guard = store.lock();
    guard.insert_image(ImageSpec {
        name: "cloudwatch-ec2-action-summary".to_string(),
        ..ImageSpec::default()
    })
}

/// Launch `count` instances of `image_id`, answering their ids in launch order.
async fn launch(emulator: &Emulator, image_id: &str, count: usize) -> Vec<String> {
    let body = ec2_api(
        emulator,
        "RunInstances",
        &[
            p("ImageId", image_id),
            p("InstanceType", "t3.small"),
            p("MinCount", &count.to_string()),
            p("MaxCount", &count.to_string()),
        ],
    )
    .await;
    let ids = crate::harness::elements(&body, "instanceId");
    assert_eq!(
        ids.len(),
        count,
        "RunInstances must report the {count} instance(s) it minted: {body}"
    );
    ids
}

/// The lifecycle state EC2 reports for one instance (`Instance$State`).
async fn instance_state(emulator: &Emulator, instance_id: &str) -> String {
    let body = ec2_api(
        emulator,
        "DescribeInstances",
        &[p("InstanceId.1", instance_id)],
    )
    .await;
    let reported = body
        .split("<instanceState>")
        .nth(1)
        .unwrap_or_else(|| panic!("DescribeInstances reports no instanceState: {body}"));
    element(reported, "name")
}

/// Look again until EC2 reports `wanted` for `instance_id` or the deadline passes, answering
/// what it last reported: a launch's own moves are owed to EC2's clock, so a row waits for the
/// state it needs rather than for a span.
async fn polled_instance_state(emulator: &Emulator, instance_id: &str, wanted: &str) -> String {
    let deadline = std::time::Instant::now() + DEADLINE;
    loop {
        let reported = instance_state(emulator, instance_id).await;
        if reported == wanted || std::time::Instant::now() >= deadline {
            return reported;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The `Action` items the alarm's history holds, each as its `HistorySummary` beside the action
/// its `HistoryData` names.
async fn action_items(emulator: &Emulator, alarm: &str) -> Vec<(String, String)> {
    let history = cw(
        emulator,
        "DescribeAlarmHistory",
        &json!({
            "AlarmName": alarm,
            "HistoryItemType": "Action",
            "AlarmTypes": ["MetricAlarm"],
        }),
    )
    .await;
    history["AlarmHistoryItems"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|item| {
            let data = item["HistoryData"].as_str().unwrap_or_default();
            let parsed: Value = serde_json::from_str(data)
                .unwrap_or_else(|error| panic!("HistoryData is not JSON ({error}): {data}"));
            let resource = parsed["notificationResource"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let summary = item["HistorySummary"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            (resource, summary)
        })
        .collect()
}

/// Look again at the alarm's `Action` history until it holds an item or the deadline passes.
async fn polled_action_items(emulator: &Emulator, alarm: &str) -> Vec<(String, String)> {
    let deadline = std::time::Instant::now() + DEADLINE;
    loop {
        let items = action_items(emulator, alarm).await;
        if !items.is_empty() || std::time::Instant::now() >= deadline {
            return items;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Which instance a row's alarm is dimensioned by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Watched {
    /// One this suite launched, so EC2 carries the operation out.
    Launched,
    /// A well-formed instance id EC2 holds no instance for, so the operation fails.
    Absent,
}

/// What a row expects of the summary the execution records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expected {
    /// `Successfully executed action <arn> on instance '<id>'`.
    SucceededOnInstance,
    /// `Failed to execute action <arn> on instance '<id>'`.
    FailedOnInstance,
    /// `Successfully executed action <arn>`: the ARN names its own target, so the summary names
    /// no instance even though the alarm watches one.
    SucceededOnTarget,
    /// No `Action` item at all: a suppressed action executed nothing to account for.
    NothingRecorded,
}

/// One row of the table.
struct Row {
    case: &'static str,
    /// The action list the action sits in, which decides the transition that fires it.
    member: &'static str,
    /// The states the row forces in order; the LAST is the transition that fires the action.
    states: &'static [&'static str],
    actions_enabled: bool,
    watched: Watched,
    expected: Expected,
    /// The EC2 instance action the row's alarm carries, or `None` for the row that carries the
    /// SNS topic this suite creates instead.
    ec2_action: Option<String>,
}

/// The rows the rule is asserted over. See the suite docs for what they span.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the recorded terminate action",
            member: "AlarmActions",
            states: &["ALARM"],
            actions_enabled: true,
            watched: Watched::Launched,
            expected: Expected::SucceededOnInstance,
            ec2_action: Some(automate_action("terminate")),
        },
        Row {
            case: "a stop action",
            member: "AlarmActions",
            states: &["ALARM"],
            actions_enabled: true,
            watched: Watched::Launched,
            expected: Expected::SucceededOnInstance,
            ec2_action: Some(automate_action("stop")),
        },
        Row {
            case: "a reboot action in its Simple Workflow form, fired by a move to OK",
            member: "OKActions",
            states: &["OK"],
            actions_enabled: true,
            watched: Watched::Launched,
            expected: Expected::SucceededOnInstance,
            ec2_action: Some(swf_action("reboot")),
        },
        Row {
            case: "a recover action in InsufficientDataActions",
            member: "InsufficientDataActions",
            states: &["ALARM", "INSUFFICIENT_DATA"],
            actions_enabled: true,
            watched: Watched::Launched,
            expected: Expected::SucceededOnInstance,
            ec2_action: Some(automate_action("recover")),
        },
        Row {
            case: "a terminate action whose instance EC2 holds none of",
            member: "AlarmActions",
            states: &["ALARM"],
            actions_enabled: true,
            watched: Watched::Absent,
            expected: Expected::FailedOnInstance,
            ec2_action: Some(automate_action("terminate")),
        },
        Row {
            case: "an SNS topic action on an alarm that watches an instance",
            member: "AlarmActions",
            states: &["ALARM"],
            actions_enabled: true,
            watched: Watched::Launched,
            expected: Expected::SucceededOnTarget,
            ec2_action: None,
        },
        Row {
            case: "the disabled twin of the terminate row",
            member: "AlarmActions",
            states: &["ALARM"],
            actions_enabled: false,
            watched: Watched::Launched,
            expected: Expected::NothingRecorded,
            ec2_action: Some(automate_action("terminate")),
        },
    ]
}

/// A long-form instance id of the published form EC2 holds no instance for: the `Absent` row's
/// dimension value, which makes the operation fail where its id is still the one the record has
/// to name.
const ABSENT_INSTANCE_ID: &str = "i-0abcdef0123456789";

/// The `PutMetricAlarm` request one row writes: a per-instance metric carrying `action` in the
/// row's action list.
fn alarm_request(row: &Row, name: &str, instance_id: &str, action: &str) -> Value {
    json!({
        "AlarmName": name,
        "Namespace": "AWS/EC2",
        "MetricName": "CPUUtilization",
        "Dimensions": [{"Name": "InstanceId", "Value": instance_id}],
        "Period": 60,
        "Statistic": "Average",
        "ComparisonOperator": "GreaterThanThreshold",
        "EvaluationPeriods": 1,
        "Threshold": 80.0,
        "ActionsEnabled": row.actions_enabled,
        row.member: [action],
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_executed_action_is_summarised_by_the_action_and_the_instance_it_acted_on() {
    let emulator = emulator();
    let image_id = given_image(&emulator);
    let table = rows();
    // One instance per row, so no row's stop or terminate decides another's outcome and each
    // row's record has its own id to name, plus one for the sentinel the suppressed row needs.
    let instances = launch(&emulator, &image_id, table.len() + 1).await;
    let sentinel_instance = instances[table.len()].clone();
    let topic_arn = element(
        &sns(&emulator, "CreateTopic", &[("Name", "action-summaries")]).await,
        "TopicArn",
    );
    // Every launch has to have reached `running` before an action fires: the rows assert what
    // the ACTION did to a running instance.
    for instance_id in &instances {
        let reported = polled_instance_state(&emulator, instance_id, "running").await;
        assert_eq!(
            reported, "running",
            "case {instance_id:?}: a launched instance must reach running, got {reported:?}"
        );
    }

    for (index, row) in table.iter().enumerate() {
        let case = row.case;
        let watched = match row.watched {
            Watched::Launched => instances[index].clone(),
            Watched::Absent => ABSENT_INSTANCE_ID.to_string(),
        };
        let action = row.ec2_action.clone().unwrap_or_else(|| topic_arn.clone());
        let alarm = format!("instance-health-{index}");
        cw(
            &emulator,
            "PutMetricAlarm",
            &alarm_request(row, &alarm, &watched, &action),
        )
        .await;
        for state in row.states {
            set_alarm_state(&emulator, &alarm, state).await;
        }

        let expected = match row.expected {
            Expected::SucceededOnInstance => Some(format!(
                "Successfully executed action {action} on instance '{watched}'"
            )),
            Expected::FailedOnInstance => Some(format!(
                "Failed to execute action {action} on instance '{watched}'"
            )),
            Expected::SucceededOnTarget => Some(format!("Successfully executed action {action}")),
            Expected::NothingRecorded => None,
        };
        let Some(expected) = expected else {
            // A suppressed action owes nothing, so there is no effect of its own to wait for:
            // the row drives the same transition on a SENTINEL alarm whose actions are enabled,
            // waits for THAT execution to be accounted for, and only then asserts that this
            // alarm accounted for none.
            let sentinel = format!("{alarm}-sentinel");
            let armed = Row {
                case,
                member: row.member,
                states: row.states,
                actions_enabled: true,
                watched: Watched::Launched,
                expected: Expected::SucceededOnInstance,
                ec2_action: row.ec2_action.clone(),
            };
            cw(
                &emulator,
                "PutMetricAlarm",
                &alarm_request(&armed, &sentinel, &sentinel_instance, &action),
            )
            .await;
            for state in row.states {
                set_alarm_state(&emulator, &sentinel, state).await;
            }
            let witnessed = polled_action_items(&emulator, &sentinel).await;
            assert_eq!(
                witnessed.len(),
                1,
                "case {case:?}: the sentinel alarm's action must be accounted for, got \
                 {witnessed:?}"
            );
            let items = action_items(&emulator, &alarm).await;
            assert!(
                items.is_empty(),
                "case {case:?}: a suppressed action must record no Action item, got {items:?}"
            );
            continue;
        };
        let items = polled_action_items(&emulator, &alarm).await;
        assert_eq!(
            items.len(),
            1,
            "case {case:?}: the fired action must be recorded once, got {items:?}"
        );
        let (resource, summary) = &items[0];
        assert_eq!(
            resource, &action,
            "case {case:?}: the item must name the action that was executed, got {items:?}"
        );
        assert_eq!(
            summary, &expected,
            "case {case:?}: expected the summary {expected:?}, got {summary:?}"
        );
    }
}

/// RULE: the instance a record names is the one the alarm that fired the action watches, so two
/// alarms over two instances record one instance each — the summary attributes the execution
/// rather than repeating a value from the call that drove the transition.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn each_records_the_instance_its_own_alarm_watches() {
    let emulator = emulator();
    let image_id = given_image(&emulator);
    let instances = launch(&emulator, &image_id, 2).await;
    for instance_id in &instances {
        let reported = polled_instance_state(&emulator, instance_id, "running").await;
        assert_eq!(
            reported, "running",
            "case {instance_id:?}: a launched instance must reach running, got {reported:?}"
        );
    }

    let action = automate_action("reboot");
    for (index, instance_id) in instances.iter().enumerate() {
        let row = Row {
            case: "one alarm per instance",
            member: "AlarmActions",
            states: &["ALARM"],
            actions_enabled: true,
            watched: Watched::Launched,
            expected: Expected::SucceededOnInstance,
            ec2_action: Some(action.clone()),
        };
        let alarm = format!("instance-health-{index}");
        cw(
            &emulator,
            "PutMetricAlarm",
            &alarm_request(&row, &alarm, instance_id, &action),
        )
        .await;
        set_alarm_state(&emulator, &alarm, "ALARM").await;
    }

    for (index, instance_id) in instances.iter().enumerate() {
        let case = format!("instance {index}");
        let alarm = format!("instance-health-{index}");
        let items = polled_action_items(&emulator, &alarm).await;
        let summaries: Vec<&str> = items.iter().map(|(_, summary)| summary.as_str()).collect();
        let expected = format!("Successfully executed action {action} on instance '{instance_id}'");
        assert_eq!(
            summaries,
            vec![expected.as_str()],
            "case {case:?}: the record must name this alarm's own instance, got {summaries:?}"
        );
        // The OTHER instance is named by no item of this alarm's history.
        let other = instances[1 - index].as_str();
        assert!(
            !summaries.iter().any(|summary| summary.contains(other)),
            "case {case:?}: no item may name {other}, got {summaries:?}"
        );
    }
}
