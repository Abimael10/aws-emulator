// SPDX-License-Identifier: CC0-1.0

//! An execution's terminal status change reaches the SQS target of a rule that selects it by
//! BOTH the state machine and the status, however the queue's own policy SCOPES the grant it
//! gives EventBridge — driven on the emulator a client starts.
//!
//! The RULE the rows assert, one level above the recorded case:
//!
//! > A status an execution of a STANDARD state machine moves into is published onto the
//! > account's default EventBridge bus, and EventBridge delivers that envelope to an SQS
//! > target of every rule whose pattern matches it, as long as the queue's resource policy
//! > grants `sqs:SendMessage` to the EventBridge service principal for a delivery of THAT
//! > rule — whether the statement grants it unconditionally or scopes it by the invoking
//! > rule's ARN or by the rule's account, which are the condition keys such a grant is
//! > written with. A rule selecting a status the execution never entered receives nothing.
//!
//! The rows span that axis — the three shapes of grant — against the pattern shape the
//! scenario this suite is written for uses: `source`, `detail-type` and `detail` naming the
//! state machine AND the status together. Each row also pins what the envelope carries: the
//! execution's ARN in `resources` and `startDate`/`stopDate` as whole epoch MILLISECONDS
//! bracketing the run.
//!
//! A closing leg spans the other thing a client's bus varies: HOW MANY rules are watching
//! the same status change. Every one of them is a target the one envelope must reach, so the
//! leg writes more rules than a delivery count spent across a whole publication would admit
//! and asserts one copy per rule — a delivery dropped for want of such an allowance is an
//! envelope that silently never arrives, which is what a client watching only its queue sees.
//!
//! Starting the built binary costs what starting the emulator costs, so the one start is
//! shared by every row, and every failure quotes the emulator's own log, which is where a
//! delivery it could not make says so.

use std::time::{Duration, Instant, SystemTime};

use serde_json::{json, Value};

use crate::cfn_deployed_pipeline_on_a_started_emulator::Started;
use crate::query_wire::element;

/// The `source` and `detail-type` Step Functions publishes an execution's status changes
/// under, spelled out here rather than imported so a wrong constant in the implementation
/// cannot make this suite pass.
const SFN_SOURCE: &str = "aws.states";
const EXECUTION_STATUS_CHANGE: &str = "Step Functions Execution Status Change";

/// The bus an AWS service's own events go to.
const DEFAULT_BUS: &str = "default";

/// The principal EventBridge delivers to an SQS target as, and the one a state machine's
/// execution role is trusted by.
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";
const STATES_PRINCIPAL: &str = "states.amazonaws.com";

/// The API version IAM's form wire carries.
const IAM_API_VERSION: &str = "2010-05-08";

/// The terminal status a `Pass`-only definition reaches, and one it can never reach.
const REACHED: &str = "SUCCEEDED";
const NEVER_REACHED: &str = "FAILED";

/// How long a row waits for what it watches for — the execution reaching its terminal status,
/// the envelope arriving — and how long it leaves between looks. Generous: every wait is on a
/// CONDITION, so a loaded host spends the span instead of failing on it.
const DEADLINE: Duration = Duration::from_secs(120);
const LOOK_INTERVAL: Duration = Duration::from_millis(100);

/// The definition every row runs: one `Pass` state, which succeeds without reaching anything.
const DEFINITION: &str = r#"{"StartAt":"Passthrough","States":{"Passthrough":{"Type":"Pass","Result":{"ok":true,"n":7},"End":true}}}"#;

/// How many rules the busy-bus leg has watching the one status change.
///
/// More than a hundred, because that is the figure a count of deliveries spent across
/// a whole request used to stop at, and a client may write up to
/// `service_com_amazonaws_eventbridge_awsevents::state::MAX_RULES_PER_BUS` of them.
const BUSY_BUS_RULES: usize = 110;

/// One RPC call over the started emulator's TLS endpoint, answered as its parsed body.
fn rpc(
    emulator: &Started,
    signing_name: &str,
    target_prefix: &str,
    json_version: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let (status, answered) = emulator.rpc(
        signing_name,
        &format!("{target_prefix}.{operation}"),
        &format!("application/x-amz-json-{json_version}"),
        body,
    );
    let parsed = serde_json::from_str::<Value>(&answered).unwrap_or(Value::String(answered));
    (status, parsed)
}

fn sfn(emulator: &Started, operation: &str, body: &Value) -> (u16, Value) {
    rpc(
        emulator,
        "states",
        "AWSStepFunctions",
        "1.0",
        operation,
        body,
    )
}

fn events(emulator: &Started, operation: &str, body: &Value) -> (u16, Value) {
    rpc(emulator, "events", "AWSEvents", "1.1", operation, body)
}

fn sqs(emulator: &Started, operation: &str, body: &Value) -> (u16, Value) {
    rpc(emulator, "sqs", "AmazonSQS", "1.0", operation, body)
}

/// A string member of a response, or the empty string when it carries none.
fn text(body: &Value, pointer: &str) -> String {
    body.pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The instant `now` as the whole epoch milliseconds an event's detail reports one in.
fn epoch_millis(now: SystemTime) -> i64 {
    i64::try_from(
        now.duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(i64::MAX)
}

/// An IAM role named `name` that `states.amazonaws.com` may assume, answered by its ARN.
fn role(emulator: &Started, name: &str) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": STATES_PRINCIPAL},
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let (_, created) = emulator.form_call(
        "iam",
        IAM_API_VERSION,
        "CreateRole",
        &[("RoleName", name), ("AssumeRolePolicyDocument", &trust)],
    );
    element(&created, "Arn")
}

/// An SQS queue called `name`, answered by its url and the ARN a rule names it by.
fn queue(emulator: &Started, name: &str) -> (String, String) {
    let (_, created) = sqs(emulator, "CreateQueue", &json!({"QueueName": name}));
    let url = text(&created, "/QueueUrl");
    let (_, attributes) = sqs(
        emulator,
        "GetQueueAttributes",
        &json!({"QueueUrl": &url, "AttributeNames": ["QueueArn"]}),
    );
    (url, text(&attributes, "/Attributes/QueueArn"))
}

/// Grant EventBridge `sqs:SendMessage` on the queue at `url`, under `condition` when the row
/// scopes its grant.
fn grant(emulator: &Started, url: &str, queue_arn: &str, condition: Option<Value>) -> Value {
    let mut statement = json!({
        "Sid": "AllowEventBridge",
        "Effect": "Allow",
        "Principal": {"Service": EVENTS_PRINCIPAL},
        "Action": "sqs:SendMessage",
        "Resource": queue_arn,
    });
    if let Some(condition) = condition {
        statement["Condition"] = condition;
    }
    let policy = json!({"Version": "2012-10-17", "Statement": [statement]}).to_string();
    let (_, set) = sqs(
        emulator,
        "SetQueueAttributes",
        &json!({"QueueUrl": url, "Attributes": {"Policy": policy}}),
    );
    set
}

/// An ENABLED rule called `name` on the default bus carrying `pattern`, answered by its ARN.
fn put_rule(emulator: &Started, name: &str, pattern: &Value) -> String {
    let (_, created) = events(
        emulator,
        "PutRule",
        &json!({
            "Name": name,
            "EventBusName": DEFAULT_BUS,
            "EventPattern": pattern.to_string(),
            "State": "ENABLED",
        }),
    );
    text(&created, "/RuleArn")
}

/// Point the rule `name` at the queue `queue_arn`.
fn put_targets(emulator: &Started, name: &str, queue_arn: &str) -> Value {
    let (_, targeted) = events(
        emulator,
        "PutTargets",
        &json!({
            "Rule": name,
            "EventBusName": DEFAULT_BUS,
            "Targets": [{"Id": "t1", "Arn": queue_arn}],
        }),
    );
    targeted
}

/// The pattern a row watches the execution through: the source, the detail type, and the
/// state machine together with the status inside `detail`.
fn pattern(machine_arn: &str, status: &str) -> Value {
    json!({
        "source": [SFN_SOURCE],
        "detail-type": [EXECUTION_STATUS_CHANGE],
        "detail": {"stateMachineArn": [machine_arn], "status": [status]},
    })
}

/// Everything waiting on the queue at `url`, deleted as it is read, as the parsed envelopes
/// its message bodies carried.
fn drain(emulator: &Started, url: &str, wait: u32) -> Vec<Value> {
    let (_, received) = sqs(
        emulator,
        "ReceiveMessage",
        &json!({"QueueUrl": url, "MaxNumberOfMessages": 10, "WaitTimeSeconds": wait}),
    );
    let mut envelopes = Vec::new();
    for message in received["Messages"].as_array().cloned().unwrap_or_default() {
        if let Some(handle) = message["ReceiptHandle"].as_str() {
            sqs(
                emulator,
                "DeleteMessage",
                &json!({"QueueUrl": url, "ReceiptHandle": handle}),
            );
        }
        envelopes.push(
            serde_json::from_str::<Value>(message["Body"].as_str().unwrap_or_default())
                .unwrap_or(Value::Null),
        );
    }
    envelopes
}

/// Collect off the queue at `url` until at least one envelope has arrived or [`DEADLINE`]
/// passes, with the long poll a client watching a queue uses.
fn collect(emulator: &Started, url: &str) -> Vec<Value> {
    let giving_up_at = Instant::now() + DEADLINE;
    let mut arrived: Vec<Value> = Vec::new();
    while arrived.is_empty() && Instant::now() < giving_up_at {
        arrived.extend(drain(emulator, url, 1));
    }
    arrived
}

/// Poll `DescribeExecution` until it reports something other than `RUNNING`, or [`DEADLINE`]
/// passes; answers what it last reported.
fn await_terminal(emulator: &Started, execution_arn: &str) -> Value {
    let giving_up_at = Instant::now() + DEADLINE;
    let mut last = Value::Null;
    while Instant::now() < giving_up_at {
        let (_, described) = sfn(
            emulator,
            "DescribeExecution",
            &json!({"executionArn": execution_arn}),
        );
        last = described;
        if last["status"].as_str() != Some("RUNNING") {
            break;
        }
        std::thread::sleep(LOOK_INTERVAL);
    }
    last
}

/// One row: how the queue's statement scopes the grant it gives EventBridge.
struct Row {
    case: &'static str,
    /// The `Condition` the row's statement carries, over the invoking rule's ARN and the
    /// rule's account — as a thunk so a row states it in the words a client writes it in.
    condition: fn(&str, &str) -> Option<Value>,
}

/// RULE: the terminal status change of an execution reaches the SQS target of a rule
/// selecting it by state machine and status, under every shape of grant the queue's policy
/// gives EventBridge, and a rule on a status it never reached receives nothing.
#[test]
fn a_terminal_status_change_reaches_a_queue_whose_grant_scopes_the_delivery() {
    let emulator = Started::start();
    let execution_role = role(&emulator, "StatesStatusChangeRole");
    // The account the role belongs to, read off its own ARN
    // (`arn:<partition>:iam::<account>:role/<name>`), which is the account every rule this
    // suite writes belongs to as well.
    let account = execution_role
        .split(':')
        .nth(4)
        .unwrap_or_default()
        .to_string();

    let rows = [
        Row {
            case: "a grant admitting EventBridge unconditionally",
            condition: |_rule_arn, _account| None,
        },
        Row {
            case: "a grant scoped by the invoking rule's ARN",
            condition: |rule_arn, _account| {
                Some(json!({"ArnEquals": {"aws:SourceArn": rule_arn}}))
            },
        },
        Row {
            case: "a grant scoped by the invoking rule's account",
            condition: |_rule_arn, account| {
                Some(json!({"StringEquals": {"aws:SourceAccount": account}}))
            },
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let mut notes: Vec<String> = Vec::new();

        // The workflow this row watches: one state machine of its own, so a rule scoped to
        // it selects this row's execution and no other row's.
        let (_, created) = sfn(
            &emulator,
            "CreateStateMachine",
            &json!({
                "name": format!("status-change-{index}"),
                "definition": DEFINITION,
                "roleArn": &execution_role,
                "type": "STANDARD",
            }),
        );
        notes.push(format!("CreateStateMachine -> {created}"));
        let machine_arn = text(&created, "/stateMachineArn");
        assert!(
            !machine_arn.is_empty(),
            "case {case:?}: the state machine must be created, got {created}\n\nthe \
             emulator's log:\n{}",
            emulator.log()
        );

        // The consumer of the status the execution reaches, and the one watching a status it
        // never reaches. The rule is written first, so the grant can be scoped to it.
        let (reached_url, reached_arn) = queue(&emulator, &format!("status-reached-{index}"));
        let (missed_url, missed_arn) = queue(&emulator, &format!("status-missed-{index}"));
        let reached_rule = format!("status-reached-{index}");
        let missed_rule = format!("status-missed-{index}");
        let reached_rule_arn = put_rule(&emulator, &reached_rule, &pattern(&machine_arn, REACHED));
        let missed_rule_arn = put_rule(
            &emulator,
            &missed_rule,
            &pattern(&machine_arn, NEVER_REACHED),
        );
        notes.push(format!(
            "PutRule -> {reached_rule_arn:?} and {missed_rule_arn:?}"
        ));
        notes.push(format!(
            "SetQueueAttributes -> {} and {}",
            grant(
                &emulator,
                &reached_url,
                &reached_arn,
                (row.condition)(&reached_rule_arn, &account)
            ),
            grant(
                &emulator,
                &missed_url,
                &missed_arn,
                (row.condition)(&missed_rule_arn, &account)
            ),
        ));
        for (rule, queue_arn) in [(&reached_rule, &reached_arn), (&missed_rule, &missed_arn)] {
            let targeted = put_targets(&emulator, rule, queue_arn);
            assert_eq!(
                targeted["FailedEntryCount"],
                json!(0),
                "case {case:?}: every target must be accepted, got {targeted}\n\nthe \
                 emulator's log:\n{}",
                emulator.log()
            );
        }

        // The run the rows are about, bracketed by this client's own clock.
        let before = epoch_millis(SystemTime::now());
        let (started_status, started) = sfn(
            &emulator,
            "StartExecution",
            &json!({
                "stateMachineArn": &machine_arn,
                "name": format!("run-{index}"),
                "input": r#"{"who":"world"}"#,
            }),
        );
        notes.push(format!("StartExecution {started_status} -> {started}"));
        let execution_arn = text(&started, "/executionArn");
        let described = await_terminal(&emulator, &execution_arn);
        notes.push(format!("DescribeExecution -> {described}"));
        assert_eq!(
            described["status"],
            json!(REACHED),
            "case {case:?}: the execution must reach {REACHED}, got {described}\n\ncalls:\n{}\
             \n\nthe emulator's log:\n{}",
            notes.join("\n"),
            emulator.log()
        );
        let after = epoch_millis(SystemTime::now());

        // The matching rule's queue receives the terminal status change, and only it.
        let arrived = collect(&emulator, &reached_url);
        assert_eq!(
            arrived.len(),
            1,
            "case {case:?}: expected exactly one envelope on the queue of the rule matching \
             {REACHED}, got {arrived:?}\n\ncalls:\n{}\n\nthe emulator's log:\n{}",
            notes.join("\n"),
            emulator.log()
        );
        let envelope = &arrived[0];
        assert_eq!(
            envelope["source"],
            json!(SFN_SOURCE),
            "case {case:?}: expected the source {SFN_SOURCE}, got {envelope}"
        );
        assert_eq!(
            envelope["detail-type"],
            json!(EXECUTION_STATUS_CHANGE),
            "case {case:?}: expected the detail-type {EXECUTION_STATUS_CHANGE}, got {envelope}"
        );
        assert_eq!(
            envelope["resources"],
            json!([&execution_arn]),
            "case {case:?}: expected the execution's ARN in resources, got {envelope}"
        );
        assert_eq!(
            envelope["detail"]["stateMachineArn"],
            json!(&machine_arn),
            "case {case:?}: expected the state machine's ARN in the detail, got {envelope}"
        );
        assert_eq!(
            envelope["detail"]["status"],
            json!(REACHED),
            "case {case:?}: expected the status {REACHED} in the detail, got {envelope}"
        );

        // The instants are whole epoch milliseconds bracketing the run, in order.
        let start_date = envelope["detail"]["startDate"].as_i64().unwrap_or_else(|| {
            panic!("case {case:?}: expected an epoch-millisecond startDate, got {envelope}")
        });
        let stop_date = envelope["detail"]["stopDate"].as_i64().unwrap_or_else(|| {
            panic!("case {case:?}: expected an epoch-millisecond stopDate, got {envelope}")
        });
        assert!(
            before <= start_date && start_date <= stop_date && stop_date <= after,
            "case {case:?}: expected {before} <= startDate <= stopDate <= {after} in epoch \
             milliseconds, got startDate {start_date} and stopDate {stop_date}"
        );

        // The rule watching a status the execution never entered received nothing.
        let elsewhere = drain(&emulator, &missed_url, 1);
        assert!(
            elsewhere.is_empty(),
            "case {case:?}: a rule matching {NEVER_REACHED} receives nothing from an \
             execution that reached {REACHED}, got {elsewhere:?}"
        );
    }

    // And the same status change on a bus a client has filled with rules watching it:
    // one envelope, every matching rule's target, however many of them the bus holds.
    let case = "every rule of a busy default bus watching the same status change";
    let (busy_url, busy_arn) = queue(&emulator, "status-busy");
    grant(&emulator, &busy_url, &busy_arn, None);
    let (_, created) = sfn(
        &emulator,
        "CreateStateMachine",
        &json!({
            "name": "status-change-busy",
            "definition": DEFINITION,
            "roleArn": &execution_role,
            "type": "STANDARD",
        }),
    );
    let machine_arn = text(&created, "/stateMachineArn");
    assert!(
        !machine_arn.is_empty(),
        "case {case:?}: the state machine must be created, got {created}\n\nthe emulator's \
         log:\n{}",
        emulator.log()
    );
    for rule in 0..BUSY_BUS_RULES {
        let name = format!("status-busy-{rule}");
        put_rule(&emulator, &name, &pattern(&machine_arn, REACHED));
        let targeted = put_targets(&emulator, &name, &busy_arn);
        assert_eq!(
            targeted["FailedEntryCount"],
            json!(0),
            "case {case:?}: every target must be accepted, got {targeted}"
        );
    }
    let (_, started) = sfn(
        &emulator,
        "StartExecution",
        &json!({
            "stateMachineArn": &machine_arn,
            "name": "run-busy",
            "input": r#"{"who":"world"}"#,
        }),
    );
    let execution_arn = text(&started, "/executionArn");
    let described = await_terminal(&emulator, &execution_arn);
    assert_eq!(
        described["status"],
        json!(REACHED),
        "case {case:?}: the execution must reach {REACHED}, got {described}\n\nthe emulator's \
         log:\n{}",
        emulator.log()
    );
    // Every rule of the bus watches the one status change, so each of them delivers a
    // copy of it: what the fan-out reaches is what the client configured.
    let giving_up_at = Instant::now() + DEADLINE;
    let mut arrived: Vec<Value> = Vec::new();
    while arrived.len() < BUSY_BUS_RULES && Instant::now() < giving_up_at {
        arrived.extend(drain(&emulator, &busy_url, 1));
    }
    assert_eq!(
        arrived.len(),
        BUSY_BUS_RULES,
        "case {case:?}: expected one copy of the status change per matching rule \
         ({BUSY_BUS_RULES}), got {}\n\nthe emulator's log:\n{}",
        arrived.len(),
        emulator.log()
    );
    for envelope in &arrived {
        assert_eq!(
            envelope["detail"]["status"],
            json!(REACHED),
            "case {case:?}: every copy reports the status the execution reached, got {envelope}"
        );
        assert_eq!(
            envelope["detail"]["executionArn"],
            json!(&execution_arn),
            "case {case:?}: every copy names the execution, got {envelope}"
        );
    }
}
