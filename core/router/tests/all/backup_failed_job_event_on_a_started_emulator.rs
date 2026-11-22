// SPDX-License-Identifier: CC0-1.0

//! A backup job that CANNOT be performed reaches `FAILED` and its terminal transition reaches
//! the rule scoped to that state, driven against the emulator a client STARTS: the built
//! binary, on its own port, with request authentication on, reached over TLS with
//! SigV4-signed calls.
//!
//! RULE: **AWS Backup publishes every state a backup job MOVES INTO onto the account's
//! default EventBridge bus as `source` `aws.backup` / `detail-type`
//! `Backup Job State Change`, carrying the job's own stored members as the event `detail`,
//! and that holds for the terminal `FAILED` a job reaches because the role it runs under may
//! not create the recovery point exactly as it holds for the `COMPLETED` of a backup that was
//! performed; a rule whose pattern scopes `detail.state` then receives the transitions into
//! the states it names and no others, delivered to its Amazon SQS target under that queue's
//! own policy.**
//!
//! The sibling suite [`crate::backup_job_failure_events_reach_their_rule`] pins that rule on a
//! router assembled inside the test process, over a table of what makes a job fail. This
//! suite is the one that pins it on the emulator a client RUNS: the credentials are verified,
//! each call arrives over its own TLS connection, the IAM question AWS Backup asks about the
//! job's role and the publication it makes are served by the binary's own multi-thread
//! runtime through the emulator's callback, and one process serves every call — so a chain
//! that holds in process and not on the started emulator is a failure a test reports rather
//! than one only a client sees.
//!
//! Source for the envelope: the AWS Backup EventBridge notifications, which carry
//! `"source": "aws.backup"` with the `detail-type` of the kind of record that moved; the
//! backup Smithy model describes the control plane and not the bus, so each `detail` member is
//! asserted against what the SAME job's control-plane read reports for it, and the terminal
//! failure state against `smithy_query(query="shape",
//! shape="com.amazonaws.backup#BackupJobState")`, whose failure value is `FAILED`. The source,
//! the detail-type and the states are spelled out here rather than imported from the service
//! crate, so a wrong constant in the implementation cannot make this suite pass.
//!
//! ONE start is paid for here and drives the whole scenario: the protected table, the vault,
//! the powerless role, the `detail.state`-scoped rule with its granted queue, the job that
//! cannot be performed, and — as the negative control on the same queue — a second job under a
//! role that may perform it, whose `COMPLETED` transition that rule must not receive. Every
//! wait is on a CONDITION, under a generous deadline.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::cfn_deployed_pipeline_on_a_started_emulator::Started;
use crate::query_wire::{element, form_encode};

/// The `source` AWS Backup publishes under and the `detail-type` a backup job's own
/// transitions are published with.
const BACKUP_SOURCE: &str = "aws.backup";
const BACKUP_JOB_STATE_CHANGE: &str = "Backup Job State Change";

/// The service principal EventBridge delivers to a queue as, and the write it makes.
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";
const SEND_MESSAGE_ACTION: &str = "sqs:SendMessage";

/// The service principal a backup role trusts, as the role a backup is performed under is
/// assumed by AWS Backup itself.
const BACKUP_PRINCIPAL: &str = "backup.amazonaws.com";

/// The two terminal states a backup job reaches: the one a job whose backup cannot be
/// performed is owed, and the one a job that captured its resource is owed.
const FAILED: &str = "FAILED";
const COMPLETED: &str = "COMPLETED";

/// The state the call that starts a job stores, which is what that call's own response
/// reports rather than a move the bus is owed.
const CREATED: &str = "CREATED";

/// The vault every job of this scenario writes into, the table it protects, and the rule and
/// queue the transitions are read off.
const VAULT: &str = "emu-started-backup-failure-vault";
const TABLE: &str = "emu-started-backup-failure-table";
const RULE: &str = "emu-started-backup-failed-rule";
const QUEUE: &str = "emu-started-backup-failed-queue";

/// The POSITIVE CONTROL beside them: a rule admitting every envelope this source publishes,
/// with a queue of its own. A job whose transition must NOT reach the scoped queue is read
/// against this one having received that transition, so the absence is read at the point the
/// delivery was made rather than after a wait.
const CONTROL_RULE: &str = "emu-started-backup-every-state-rule";
const CONTROL_QUEUE: &str = "emu-started-backup-every-state-queue";

/// How long any one condition is waited for, and how often it is looked at. Generous next to
/// the spans this emulator's own transitions and in-process deliveries take: the convergence
/// SPEED is not what this suite asserts.
const DEADLINE: Duration = Duration::from_secs(90);
/// See [`DEADLINE`].
const LOOK_INTERVAL: Duration = Duration::from_millis(100);

/// One signed call DIALLED AND ADDRESSED at the emulator's own authority — the position a
/// client that was handed an endpoint url is in, which is how a stock SDK configured with
/// this emulator's `endpoint_url` reaches it: the `Host` it carries is `127.0.0.1:<port>`
/// rather than an AWS service host, so every address the emulator mints for it (a queue url
/// among them) is minted from THAT authority.
fn addressed(
    emulator: &Started,
    case: &str,
    signing_name: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Vec<u8>,
) -> (u16, String) {
    emulator
        .call_at(
            &emulator.authority(),
            signing_name,
            method,
            path,
            headers,
            body,
        )
        .unwrap_or_else(|error| {
            panic!(
                "case {case:?}: the emulator must answer {signing_name} {method} {path}: \
                 {error}; its log:\n{}",
                emulator.log()
            )
        })
}

/// One `X-Amz-Target` RPC call that must answer 200, quoting the emulator's own log when it
/// did not.
fn rpc(
    emulator: &Started,
    case: &str,
    signing_name: &str,
    target: &str,
    content_type: &str,
    body: &Value,
) -> Value {
    let (status, text) = addressed(
        emulator,
        case,
        signing_name,
        "POST",
        "/",
        &[("content-type", content_type), ("x-amz-target", target)],
        body.to_string().into_bytes(),
    );
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {target}, got {status} with {text}; the \
         emulator's log:\n{}",
        emulator.log()
    );
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

/// One Amazon SQS call that must answer 200.
fn sqs(emulator: &Started, case: &str, operation: &str, body: &Value) -> Value {
    rpc(
        emulator,
        case,
        "sqs",
        &format!("AmazonSQS.{operation}"),
        "application/x-amz-json-1.0",
        body,
    )
}

/// One EventBridge call that must answer 200.
fn events(emulator: &Started, case: &str, operation: &str, body: &Value) -> Value {
    rpc(
        emulator,
        case,
        "events",
        &format!("AWSEvents.{operation}"),
        "application/x-amz-json-1.1",
        body,
    )
}

/// One Amazon DynamoDB call that must answer 200.
fn dynamodb(emulator: &Started, case: &str, operation: &str, body: &Value) -> Value {
    rpc(
        emulator,
        case,
        "dynamodb",
        &format!("DynamoDB_20120810.{operation}"),
        "application/x-amz-json-1.0",
        body,
    )
}

/// One IAM call over its awsQuery wire that must answer 200.
fn iam(emulator: &Started, case: &str, action: &str, params: &[(&str, &str)]) -> String {
    let mut body = "Action=".to_string();
    body.push_str(&form_encode(action));
    body.push_str("&Version=2010-05-08");
    for (key, value) in params {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let (status, text) = addressed(
        emulator,
        case,
        "iam",
        "POST",
        "/",
        &[("content-type", "application/x-www-form-urlencoded")],
        body.into_bytes(),
    );
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from IAM {action}, got {status} with {text}; the \
         emulator's log:\n{}",
        emulator.log()
    );
    text
}

/// One AWS Backup call over its `restJson1` wire that must answer 200.
fn backup(
    emulator: &Started,
    case: &str,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> Value {
    let (headers, payload): (&[(&str, &str)], Vec<u8>) = match body {
        Some(body) => (
            &[("content-type", "application/json")],
            body.to_string().into_bytes(),
        ),
        None => (&[], Vec::new()),
    };
    let (status, text) = addressed(emulator, case, "backup", method, path, headers, payload);
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Backup {method} {path}, got {status} with \
         {text}; the emulator's log:\n{}",
        emulator.log()
    );
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

/// A string member of a JSON answer, or a panic naming the case and what came back.
fn member(case: &str, body: &Value, pointer: &str) -> String {
    body.pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected {pointer} in {body}"))
        .to_string()
}

/// The trust document of a role AWS Backup may act as.
fn trusting_backup() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": BACKUP_PRINCIPAL },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// Create the role `name` where IAM holds it, trusted by AWS Backup and — when `granted` —
/// allowed everything a backup needs, and answer the ARN IAM ITSELF reports for it. A role
/// created with `granted` false carries NO permissions policy at all, which is the role under
/// which no backup can be performed.
fn backup_role(emulator: &Started, case: &str, name: &str, granted: bool) -> String {
    let created = iam(
        emulator,
        case,
        "CreateRole",
        &[
            ("RoleName", name),
            ("AssumeRolePolicyDocument", &trusting_backup()),
        ],
    );
    if granted {
        let policy = json!({
            "Version": "2012-10-17",
            "Statement": [{ "Effect": "Allow", "Action": "*", "Resource": "*" }],
        })
        .to_string();
        iam(
            emulator,
            case,
            "PutRolePolicy",
            &[
                ("RoleName", name),
                ("PolicyName", "granted"),
                ("PolicyDocument", &policy),
            ],
        );
    }
    element(&created, "Arn")
}

/// Every envelope waiting on the queue at `url`, deleting what it hands over so a later look
/// reads only what arrived since.
fn drain(emulator: &Started, case: &str, url: &str) -> Vec<Value> {
    let received = sqs(
        emulator,
        case,
        "ReceiveMessage",
        &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
    );
    let messages = received["Messages"].as_array().cloned().unwrap_or_default();
    let mut envelopes = Vec::with_capacity(messages.len());
    for message in &messages {
        let raw = message["Body"].as_str().unwrap_or_default();
        if let Ok(envelope) = serde_json::from_str::<Value>(raw) {
            envelopes.push(envelope);
        }
        if let Some(handle) = message["ReceiptHandle"].as_str() {
            sqs(
                emulator,
                case,
                "DeleteMessage",
                &json!({ "QueueUrl": url, "ReceiptHandle": handle }),
            );
        }
    }
    envelopes
}

/// Start a job for `resource_arn` under `role`, wait for the terminal state `terminal`, and
/// answer the job's id beside the read that reported it.
fn job_settling_at(
    emulator: &Started,
    case: &str,
    role: &str,
    resource_arn: &str,
    token: &str,
    terminal: &str,
) -> (String, Value) {
    let started = backup(
        emulator,
        case,
        "PUT",
        "/backup-jobs",
        Some(&json!({
            "BackupVaultName": VAULT,
            "ResourceArn": resource_arn,
            "IamRoleArn": role,
            "IdempotencyToken": token,
        })),
    );
    let job_id = member(case, &started, "/BackupJobId");
    let giving_up_at = Instant::now() + DEADLINE;
    loop {
        let read = backup(
            emulator,
            case,
            "GET",
            &format!("/backup-jobs/{job_id}"),
            None,
        );
        let state = member(case, &read, "/State");
        if state == terminal {
            return (job_id, read);
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: the job never reached {terminal:?} inside {DEADLINE:?}, it reports \
             {state:?} in {read}"
        );
        std::thread::sleep(LOOK_INTERVAL);
    }
}

/// A queue of this emulator whose own policy admits the write EventBridge makes to it ("for …
/// Amazon SQS queues, EventBridge relies on resource-based policies", `PutTargets`), scoped —
/// when `rule_arn` names one — to the deliveries of THAT rule through `aws:SourceArn`, and
/// wired as the only target of the rule `rule`. Answers the queue's url.
fn granted_queue_target(
    emulator: &Started,
    case: &str,
    name: &str,
    rule: &str,
    rule_arn: Option<&str>,
) -> String {
    let created = sqs(emulator, case, "CreateQueue", &json!({ "QueueName": name }));
    let url = member(case, &created, "/QueueUrl");
    let attributes = sqs(
        emulator,
        case,
        "GetQueueAttributes",
        &json!({ "QueueUrl": &url, "AttributeNames": ["QueueArn"] }),
    );
    let queue_arn = member(case, &attributes, "/Attributes/QueueArn");
    let mut statement = json!({
        "Effect": "Allow",
        "Principal": { "Service": EVENTS_PRINCIPAL },
        "Action": SEND_MESSAGE_ACTION,
        "Resource": &queue_arn,
    });
    if let Some(rule_arn) = rule_arn {
        statement["Condition"] = json!({ "ArnEquals": { "aws:SourceArn": rule_arn } });
    }
    let policy = json!({ "Version": "2012-10-17", "Statement": [statement] }).to_string();
    sqs(
        emulator,
        case,
        "SetQueueAttributes",
        &json!({ "QueueUrl": &url, "Attributes": { "Policy": policy } }),
    );
    let wired = events(
        emulator,
        case,
        "PutTargets",
        &json!({
            "Rule": rule,
            "Targets": [{ "Id": "sqs-target", "Arn": &queue_arn }],
        }),
    );
    assert_eq!(
        wired["FailedEntryCount"],
        json!(0),
        "case {case:?}: the queue target must be accepted, got {wired}"
    );
    url
}

/// The job envelopes among `seen` that name `job_id`.
fn job_envelopes<'a>(seen: &'a [Value], job_id: &str) -> Vec<&'a Value> {
    seen.iter()
        .filter(|envelope| {
            envelope["detail-type"] == json!(BACKUP_JOB_STATE_CHANGE)
                && envelope["detail"]["backupJobId"] == json!(job_id)
        })
        .collect()
}

/// RULE: on the emulator a client starts, the terminal `FAILED` transition of a job whose role
/// may not perform the backup is published as an `aws.backup` job-state-change envelope and
/// delivered to the rule scoped to that state, while the `COMPLETED` transition of a job that
/// WAS performed reaches that same rule not at all.
#[test]
fn a_failed_backup_jobs_state_change_reaches_its_rule_on_a_started_emulator() {
    let emulator = Started::start();
    let case = "the FAILED-scoped rule and its granted queue";

    // ---- the rule scoped to the terminal FAILED state, and the queue it delivers to ----
    let pattern = json!({
        "source": [BACKUP_SOURCE],
        "detail-type": [BACKUP_JOB_STATE_CHANGE],
        "detail": { "state": [FAILED] },
    });
    let put = events(
        &emulator,
        case,
        "PutRule",
        &json!({
            "Name": RULE,
            "EventPattern": pattern.to_string(),
            "State": "ENABLED",
        }),
    );
    let rule_arn = member(case, &put, "/RuleArn");
    // "For … Amazon SQS queues, EventBridge relies on resource-based policies" (`PutTargets`),
    // granted to this one rule's deliveries through `aws:SourceArn`.
    let url = granted_queue_target(&emulator, case, QUEUE, RULE, Some(&rule_arn));

    // The positive control beside it: every envelope this source publishes.
    events(
        &emulator,
        case,
        "PutRule",
        &json!({
            "Name": CONTROL_RULE,
            "EventPattern": json!({ "source": [BACKUP_SOURCE] }).to_string(),
            "State": "ENABLED",
        }),
    );
    let control_url = granted_queue_target(&emulator, case, CONTROL_QUEUE, CONTROL_RULE, None);

    // ---- the protected table and the vault the jobs write into ----
    let table = dynamodb(
        &emulator,
        case,
        "CreateTable",
        &json!({
            "TableName": TABLE,
            "AttributeDefinitions": [{ "AttributeName": "id", "AttributeType": "S" }],
            "KeySchema": [{ "AttributeName": "id", "KeyType": "HASH" }],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    );
    let resource_arn = member(case, &table, "/TableDescription/TableArn");
    let giving_up_at = Instant::now() + DEADLINE;
    loop {
        let described = dynamodb(
            &emulator,
            case,
            "DescribeTable",
            &json!({ "TableName": TABLE }),
        );
        let status = member(case, &described, "/Table/TableStatus");
        if status == "ACTIVE" {
            break;
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: the protected table never became ACTIVE inside {DEADLINE:?}, it \
             reports {status:?}"
        );
        std::thread::sleep(LOOK_INTERVAL);
    }
    backup(
        &emulator,
        case,
        "PUT",
        &format!("/backup-vaults/{VAULT}"),
        Some(&json!({})),
    );

    // ---- the job no role may perform: it reaches FAILED carrying why ----
    let powerless = backup_role(&emulator, case, "emu-started-backup-powerless", false);
    let failing = "the job whose role carries no permissions policy";
    let (failed_job, read) = job_settling_at(
        &emulator,
        failing,
        &powerless,
        &resource_arn,
        "emu-started-backup-failure-token",
        FAILED,
    );
    let status_message = member(failing, &read, "/StatusMessage");
    assert!(
        !status_message.trim().is_empty(),
        "case {failing:?}: a failed job reports why it failed, got {status_message:?} in {read}"
    );
    let message_category = member(failing, &read, "/MessageCategory");

    // ---- that transition reaches the rule scoped to it ----
    let mut seen: Vec<Value> = Vec::new();
    let giving_up_at = Instant::now() + DEADLINE;
    let envelope = loop {
        seen.extend(drain(&emulator, failing, &url));
        if let Some(envelope) = job_envelopes(&seen, &failed_job).first().copied().cloned() {
            break envelope;
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {failing:?}: the rule scoped to {FAILED:?} must receive the job's failed \
             transition inside {DEADLINE:?}, what arrived was {seen:?}; the emulator's log:\n{}",
            emulator.log()
        );
        std::thread::sleep(LOOK_INTERVAL);
    };
    for (pointer, expect) in [
        ("/source", json!(BACKUP_SOURCE)),
        ("/detail-type", json!(BACKUP_JOB_STATE_CHANGE)),
        ("/detail/backupJobId", json!(&failed_job)),
        ("/detail/state", json!(FAILED)),
        ("/detail/backupVaultName", json!(VAULT)),
        ("/detail/resourceArn", json!(&resource_arn)),
        ("/detail/iamRoleArn", json!(&powerless)),
        ("/detail/statusMessage", json!(&status_message)),
        ("/detail/messageCategory", json!(&message_category)),
    ] {
        let actual = envelope.pointer(pointer).cloned().unwrap_or(Value::Null);
        assert_eq!(
            actual, expect,
            "case {failing:?}: expected {pointer} to be {expect}, got {actual} in {envelope}"
        );
    }
    assert!(
        job_envelopes(&seen, &failed_job)
            .iter()
            .all(|envelope| envelope["detail"]["state"] == json!(FAILED)),
        "case {failing:?}: only the transitions into {FAILED:?} may reach this rule's queue, \
         got {seen:?}"
    );

    // ---- the negative control: a job that WAS performed owes this rule nothing ----
    let granted = backup_role(&emulator, case, "emu-started-backup-granted", true);
    let succeeding = "the job whose role may perform the backup";
    let (completed_job, _) = job_settling_at(
        &emulator,
        succeeding,
        &granted,
        &resource_arn,
        "emu-started-backup-success-token",
        COMPLETED,
    );
    // The absence on the scoped queue is read against the POSITIVE CONTROL having received
    // that same job's transition: the control rule admits every envelope this source
    // publishes, so once it holds the job's move the delivery pass that would have fed the
    // scoped queue has happened, and a scoped queue still without it is the pattern's refusal
    // rather than a look taken too early.
    let mut control_seen: Vec<Value> = Vec::new();
    let giving_up_at = Instant::now() + DEADLINE;
    loop {
        control_seen.extend(drain(&emulator, succeeding, &control_url));
        seen.extend(drain(&emulator, succeeding, &url));
        if job_envelopes(&control_seen, &completed_job)
            .iter()
            .any(|envelope| envelope["detail"]["state"] == json!(COMPLETED))
        {
            break;
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {succeeding:?}: the job's move into {COMPLETED:?} must be published inside \
             {DEADLINE:?}, what arrived was {control_seen:?}; the emulator's log:\n{}",
            emulator.log()
        );
        std::thread::sleep(LOOK_INTERVAL);
    }
    seen.extend(drain(&emulator, succeeding, &url));
    assert!(
        job_envelopes(&seen, &completed_job).is_empty(),
        "case {succeeding:?}: a job that reached {COMPLETED:?} owes the {FAILED:?}-scoped rule \
         nothing, got {seen:?}"
    );
    assert!(
        seen.iter()
            .all(|envelope| envelope["detail"]["state"] == json!(FAILED)),
        "case {succeeding:?}: this rule's queue carries only transitions into {FAILED:?}, and \
         never the {CREATED:?} the start call stored, got {seen:?}"
    );
    // And what the control rule received for the FAILED job is that job's terminal transition
    // too, so the two rules saw one publication rather than two.
    assert!(
        job_envelopes(&control_seen, &failed_job)
            .iter()
            .all(|envelope| envelope["detail"]["state"] != json!(CREATED)),
        "case {failing:?}: the state the START call stored is not a move, so it owes the bus \
         nothing, got {control_seen:?}"
    );
}
