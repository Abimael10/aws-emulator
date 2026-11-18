// SPDX-License-Identifier: CC0-1.0

//! WHEN an alarm's EC2 INSTANCE action is executed, end to end over the wire through the real
//! `MultiServiceRouter` with CloudWatch and EC2 on one engine.
//!
//! The RULE asserted here: **the `ActionsEnabled` switch ("Indicates whether actions should be
//! executed during any changes to the alarm state", `MetricAlarm$ActionsEnabled`, written by
//! `PutMetricAlarm` and by `DisableAlarmActions` / `EnableAlarmActions`) is read at the instant
//! of the TRANSITION: a transition made while the switch is off executes none of the state's
//! actions and records no `Action` history item, and turning the switch back on does not go back
//! and execute it — the action runs on the next transition INTO that state, which then performs
//! the EC2 operation the action names on the instance the alarm's own metric is dimensioned by
//! and records exactly one `Action` item naming that action and that instance.**
//!
//! What [`crate::ec2_instance_action_execution`] asserts is that an enabled alarm's EC2 instance
//! action is performed and a disabled one's is not; what this suite adds is the SWITCH over TIME:
//! the disabled alarm is re-armed with `EnableAlarmActions` and is then shown to execute on a
//! fresh transition and only once, so the suppressed transition is accounted for as never
//! executed rather than as merely late.
//!
//! The rows differ along what the rule generalises over — the action VERB and therefore the EC2
//! operation performed (terminate, stop, reboot), the ARN FORM the action is written in (the
//! `automate` form and the Simple Workflow form), the namespace of the series the alarm watches
//! (a custom one and `AWS/EC2`) and how the alarm identifies its per-instance metric (top-level
//! `Dimensions` or a `Metrics` query array) — because none of those changes WHEN the action is
//! executed.
//!
//! Every row runs an ARMED alarm beside the disarmed one, over an instance of its own: the armed
//! alarm is the positive control, so the row only reads the disarmed alarm's instance and history
//! once the armed one's action has already been executed and recorded, and a row cannot pass on
//! an emulator that executes nothing at all. Nothing here waits for a span — every wait is a look
//! again until the condition holds.

use std::time::Instant;

use serde_json::{json, Value};

use service_com_amazonaws_ec2_amazonec2 as ec2;

use ec2::state::{ComAmazonawsEc2Amazonec2State as Ec2State, ImageSpec};

use crate::harness::{
    ec2 as ec2_api, element, emulator, rpc, Emulator, LOOK_INTERVAL, REGION, TARGET_PREFIX,
    WORK_WAIT,
};

/// The `automate` form of one EC2 instance action:
/// "EC2 actions: arn:aws:automate:region:ec2:stop arn:aws:automate:region:ec2:terminate
/// arn:aws:automate:region:ec2:reboot arn:aws:automate:region:ec2:recover …"
/// (`smithy_query(query="doc",shape="PutMetricAlarmInput",member="AlarmActions")`).
fn automate_action(verb: &str) -> String {
    format!("arn:aws:automate:{REGION}:ec2:{verb}")
}

/// The Simple Workflow form of the same action, from the same documentation:
/// "arn:aws:swf:region:account-id:action/actions/AWS_EC2.InstanceId.Stop/1.0". The account is the
/// one this emulator mints its own ARNs under.
fn swf_action(verb: &str) -> String {
    let capitalised = format!("{}{}", verb[..1].to_uppercase(), &verb[1..]);
    format!(
        "arn:aws:swf:{REGION}:000000000000:action/actions/AWS_EC2.InstanceId.{capitalised}/1.0"
    )
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
        name: "cloudwatch-ec2-instance-action-arming".to_string(),
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
            p("InstanceType", "t3.micro"),
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

/// Look at one instance until EC2 reports `wanted`, and answer the last state it reported.
async fn until_instance_state(emulator: &Emulator, instance_id: &str, wanted: &str) -> String {
    let deadline = Instant::now() + WORK_WAIT;
    let mut last = instance_state(emulator, instance_id).await;
    while last != wanted && Instant::now() < deadline {
        tokio::time::sleep(LOOK_INTERVAL).await;
        last = instance_state(emulator, instance_id).await;
    }
    last
}

/// The boot identities one instance's console carries, oldest first: a guest writes its own on
/// every boot, so a second one is a guest that restarted. The guest's init system publishes what
/// it discovered about its machine as `ci-info: <name>: <value>`.
async fn console_boot_ids(emulator: &Emulator, instance_id: &str) -> Vec<String> {
    let body = ec2_api(
        emulator,
        "GetConsoleOutput",
        &[p("InstanceId", instance_id), p("Latest", "true")],
    )
    .await;
    let encoded = element(&body, "output");
    let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &encoded)
        .unwrap_or_else(|error| panic!("the console output must be base64 ({error}): {body}"));
    let text = String::from_utf8(bytes)
        .unwrap_or_else(|error| panic!("the console text must be UTF-8 ({error})"));
    text.lines()
        .filter_map(|line| line.split_once("ci-info: boot-id: "))
        .map(|(_, value)| value.trim().to_string())
        .collect()
}

/// Look at one instance's console until it carries at least `wanted` boot identities, and answer
/// what it carried last.
async fn until_boots(emulator: &Emulator, instance_id: &str, wanted: usize) -> Vec<String> {
    let deadline = Instant::now() + WORK_WAIT;
    loop {
        let seen = console_boot_ids(emulator, instance_id).await;
        if seen.len() >= wanted || Instant::now() >= deadline {
            return seen;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// The `Action` items the alarm's history holds, each as the action it names, its summary and its
/// parsed `HistoryData`.
async fn action_items(emulator: &Emulator, alarm: &str) -> Vec<Value> {
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
            json!({
                "action": parsed["notificationResource"].clone(),
                "state": parsed["actionState"].clone(),
                "stateValue": parsed["stateValue"].clone(),
                "error": parsed["error"].clone(),
                "summary": item["HistorySummary"].clone(),
            })
        })
        .collect()
}

/// Look at the alarm's history until it holds at least `wanted` `Action` items, and answer what it
/// held last.
async fn until_action_items(emulator: &Emulator, alarm: &str, wanted: usize) -> Vec<Value> {
    let deadline = Instant::now() + WORK_WAIT;
    loop {
        let items = action_items(emulator, alarm).await;
        if items.len() >= wanted || Instant::now() >= deadline {
            return items;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// What `DescribeAlarms` reports for one alarm's `ActionsEnabled`.
async fn reported_actions_enabled(emulator: &Emulator, alarm: &str) -> Option<bool> {
    let described = cw(
        emulator,
        "DescribeAlarms",
        &json!({"AlarmNames": [alarm], "AlarmTypes": ["MetricAlarm"]}),
    )
    .await;
    described["MetricAlarms"][0]["ActionsEnabled"].as_bool()
}

/// How a row identifies the per-instance metric its alarm watches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MetricShape {
    /// `Namespace`/`MetricName` plus the alarm's own `Dimensions`.
    SingleMetric,
    /// A `Metrics` query array, whose returning query retrieves the per-instance metric.
    QueryArray,
}

/// What EC2 must report of an instance the row's action was performed on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expected {
    /// The instance reached this lifecycle state.
    Moved(&'static str),
    /// The guest booted a second time and the instance is still `running`: a reboot restarts the
    /// operating system rather than the instance.
    Rebooted,
}

/// One row of the table.
struct Row {
    case: &'static str,
    /// The action ARN both of the row's alarms carry.
    action: String,
    /// The namespace of the series they watch.
    namespace: &'static str,
    shape: MetricShape,
    expected: Expected,
}

/// The rows the rule is asserted over. See the suite docs for what they span.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a terminate action on a custom namespace, dimensioned by InstanceId",
            action: automate_action("terminate"),
            namespace: "sim/instance-health",
            shape: MetricShape::SingleMetric,
            expected: Expected::Moved("terminated"),
        },
        Row {
            case: "a stop action on an alarm that watches a Metrics query array",
            action: automate_action("stop"),
            namespace: "AWS/EC2",
            shape: MetricShape::QueryArray,
            expected: Expected::Moved("stopped"),
        },
        Row {
            case: "a reboot action written in its Simple Workflow form",
            action: swf_action("reboot"),
            namespace: "AWS/EC2",
            shape: MetricShape::SingleMetric,
            expected: Expected::Rebooted,
        },
    ]
}

/// The `PutMetricAlarm` request one of a row's two alarms makes: a per-instance metric, stated the
/// way the row's shape says, carrying the row's action in `AlarmActions`.
fn alarm_request(row: &Row, name: &str, instance_id: &str, actions_enabled: bool) -> Value {
    let dimensions = json!([{"Name": "InstanceId", "Value": instance_id}]);
    let mut request = json!({
        "AlarmName": name,
        "ComparisonOperator": "GreaterThanThreshold",
        "EvaluationPeriods": 1,
        "Threshold": 80.0,
        "ActionsEnabled": actions_enabled,
        "AlarmActions": [row.action.clone()],
    });
    let members = request
        .as_object_mut()
        .expect("the request under construction is an object");
    match row.shape {
        MetricShape::SingleMetric => {
            members.insert("Namespace".to_string(), json!(row.namespace));
            members.insert("MetricName".to_string(), json!("HealthCheckFailed"));
            members.insert("Dimensions".to_string(), dimensions);
            members.insert("Period".to_string(), json!(60));
            members.insert("Statistic".to_string(), json!("Average"));
        }
        MetricShape::QueryArray => {
            members.insert(
                "Metrics".to_string(),
                json!([{
                    "Id": "m1",
                    "MetricStat": {
                        "Metric": {
                            "Namespace": row.namespace,
                            "MetricName": "HealthCheckFailed",
                            "Dimensions": dimensions,
                        },
                        "Period": 60,
                        "Stat": "Average",
                    },
                    "ReturnData": true,
                }]),
            );
        }
    }
    request
}

/// Force one alarm's state.
async fn force(emulator: &Emulator, alarm: &str, state: &str) {
    cw(
        emulator,
        "SetAlarmState",
        &json!({
            "AlarmName": alarm,
            "StateValue": state,
            "StateReason": "driven by the test",
        }),
    )
    .await;
}

/// Assert that the row's action has been performed on `instance_id`, waiting for the condition the
/// action owes rather than for a span.
async fn assert_performed(
    emulator: &Emulator,
    case: &str,
    who: &str,
    instance_id: &str,
    expected: Expected,
    before: &[String],
) {
    match expected {
        Expected::Moved(state) => {
            let reported = until_instance_state(emulator, instance_id, state).await;
            assert_eq!(
                reported, state,
                "case {case:?}: the {who} alarm's action must leave its instance {state:?}, got \
                 {reported:?}"
            );
        }
        Expected::Rebooted => {
            let after = until_boots(emulator, instance_id, before.len() + 1).await;
            assert_eq!(
                after.len(),
                before.len() + 1,
                "case {case:?}: the {who} alarm's action must boot its guest once more, got \
                 {after:?} after {before:?}"
            );
            assert!(
                after.starts_with(before),
                "case {case:?}: the console must keep what the earlier boots wrote, got {after:?} \
                 after {before:?}"
            );
            let reported = instance_state(emulator, instance_id).await;
            assert_eq!(
                reported, "running",
                "case {case:?}: a restarted instance is still running, got {reported:?}"
            );
        }
    }
}

/// Assert that one `Action` item accounts for the row's action against `instance_id`.
fn assert_item(case: &str, who: &str, item: &Value, action: &str, instance_id: &str) {
    assert_eq!(
        item["action"].as_str(),
        Some(action),
        "case {case:?}: the {who} alarm's item must name the action that was executed, got {item}"
    );
    assert_eq!(
        item["state"].as_str(),
        Some("Succeeded"),
        "case {case:?}: an action EC2 carried out is Succeeded, got {item}"
    );
    assert_eq!(
        item["stateValue"].as_str(),
        Some("ALARM"),
        "case {case:?}: the item must report the state that fired the action, got {item}"
    );
    assert_eq!(
        item["error"],
        Value::Null,
        "case {case:?}: an action that succeeded records no error, got {item}"
    );
    assert_eq!(
        item["summary"].as_str(),
        Some(
            format!("Successfully executed action {action} on instance '{instance_id}'").as_str()
        ),
        "case {case:?}: the {who} alarm's summary must name the action and the instance it was \
         attempted against, got {item}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_suppressed_ec2_instance_action_runs_on_the_first_transition_after_it_is_re_armed() {
    for row in rows() {
        let case = row.case;
        let emulator = emulator();
        let image_id = given_image(&emulator);
        // One instance for the armed alarm (the positive control) and one for the disarmed twin.
        let instances = launch(&emulator, &image_id, 2).await;
        let (armed_instance, disarmed_instance) = (instances[0].clone(), instances[1].clone());
        // Both launches have to be running with a booted guest before any action fires: what the
        // row asserts is what the ACTION did to a running instance.
        let mut armed_boots = Vec::new();
        let mut disarmed_boots = Vec::new();
        for (instance_id, boots) in [
            (&armed_instance, &mut armed_boots),
            (&disarmed_instance, &mut disarmed_boots),
        ] {
            let reported = until_instance_state(&emulator, instance_id, "running").await;
            assert_eq!(
                reported, "running",
                "case {case:?}: a launched instance reaches running, got {reported:?}"
            );
            *boots = until_boots(&emulator, instance_id, 1).await;
            assert_eq!(
                boots.len(),
                1,
                "case {case:?}: a launched guest has booted once, got {boots:?}"
            );
        }

        let armed = "instance-health-armed";
        let disarmed = "instance-health-disarmed";
        cw(
            &emulator,
            "PutMetricAlarm",
            &alarm_request(&row, armed, &armed_instance, true),
        )
        .await;
        cw(
            &emulator,
            "PutMetricAlarm",
            &alarm_request(&row, disarmed, &disarmed_instance, false),
        )
        .await;

        // One transition each, into the state whose action list carries the action.
        force(&emulator, armed, "ALARM").await;
        force(&emulator, disarmed, "ALARM").await;

        // The armed alarm executed its action and recorded it: the positive control, and the
        // instant from which the disarmed twin's silence is a decision rather than a race.
        let control = until_action_items(&emulator, armed, 1).await;
        assert_eq!(
            control.len(),
            1,
            "case {case:?}: the armed alarm's action must be recorded once, got {control:?}"
        );
        assert_item(case, "armed", &control[0], &row.action, &armed_instance);
        assert_performed(
            &emulator,
            case,
            "armed",
            &armed_instance,
            row.expected,
            &armed_boots,
        )
        .await;

        // The twin's transition was made with the switch off: nothing was executed and nothing is
        // claimed to have been.
        let suppressed = action_items(&emulator, disarmed).await;
        assert!(
            suppressed.is_empty(),
            "case {case:?}: a transition made with actions disabled records no Action item, got \
             {suppressed:?}"
        );
        let reported = instance_state(&emulator, &disarmed_instance).await;
        assert_eq!(
            reported, "running",
            "case {case:?}: a suppressed action must leave its instance running, got {reported:?}"
        );
        let still = console_boot_ids(&emulator, &disarmed_instance).await;
        assert_eq!(
            still, disarmed_boots,
            "case {case:?}: a suppressed action must leave its guest on its first boot, got \
             {still:?}"
        );

        // Re-arming the alarm is not a transition, so it executes nothing by itself.
        cw(
            &emulator,
            "EnableAlarmActions",
            &json!({"AlarmNames": [disarmed]}),
        )
        .await;
        assert_eq!(
            reported_actions_enabled(&emulator, disarmed).await,
            Some(true),
            "case {case:?}: EnableAlarmActions must turn the switch back on"
        );
        let re_armed = action_items(&emulator, disarmed).await;
        assert!(
            re_armed.is_empty(),
            "case {case:?}: enabling actions must not go back and execute the suppressed \
             transition, got {re_armed:?}"
        );
        let reported = instance_state(&emulator, &disarmed_instance).await;
        assert_eq!(
            reported, "running",
            "case {case:?}: enabling actions must not move the instance by itself, got \
             {reported:?}"
        );

        // The next transition INTO the action's state executes it.
        force(&emulator, disarmed, "OK").await;
        force(&emulator, disarmed, "ALARM").await;
        let items = until_action_items(&emulator, disarmed, 1).await;
        assert_eq!(
            items.len(),
            1,
            "case {case:?}: the re-armed alarm's fresh transition must execute the action exactly \
             once — the suppressed transition is never executed — got {items:?}"
        );
        assert_item(case, "re-armed", &items[0], &row.action, &disarmed_instance);
        assert_performed(
            &emulator,
            case,
            "re-armed",
            &disarmed_instance,
            row.expected,
            &disarmed_boots,
        )
        .await;

        // And the control executed its action once, for its own transition only.
        let control = action_items(&emulator, armed).await;
        assert_eq!(
            control.len(),
            1,
            "case {case:?}: another alarm's transition must not execute this one's action, got \
             {control:?}"
        );
    }
}
