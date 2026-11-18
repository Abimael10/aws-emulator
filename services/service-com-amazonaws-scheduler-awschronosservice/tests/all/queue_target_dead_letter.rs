// SPDX-License-Identifier: CC0-1.0

//! The dead-letter report of an invocation that could not reach a QUEUE target: end to
//! end over the wire through the real `MultiServiceRouter`, with Amazon SQS and IAM
//! mounted beside EventBridge Scheduler on ONE engine — so the report is asserted where
//! a client reads it, in the message the schedule's dead-letter queue holds.
//!
//! RULE: what a schedule's `Target.DeadLetterConfig` queue is handed is the REQUEST the
//! failed attempt made of its target's own API, never the payload document alone. A
//! queue target is "the templated target type for the Amazon SQS SendMessage API
//! operation" (`Target$SqsParameters`), and that request addresses its queue by the url
//! Amazon SQS reports for it — `https://sqs.<region>.amazonaws.com/<accountId>/<name>`,
//! the Region being the one the target ARN labels — and carries the payload the attempt
//! rendered under `MessageBody` (`smithy_query` query="operation" operation="SendMessage"
//! on Amazon SQS's model: `QueueUrl`, `MessageBody`, `MessageGroupId`). So a consumer
//! replays the failed send by resubmitting that document, and reads the undelivered
//! payload under the one member it carries it in. The failure's own detail rides in the
//! message ATTRIBUTES beside it, never in the body.
//!
//! The rows span the class rather than one recorded case: they differ along the FORM of
//! the payload the invocation carried (a flat object, a nested document with quotes and
//! characters beyond ASCII, a top-level array, text that is not JSON, an `Input` naming
//! the schedule's context attributes, and no `Input` at all), along the REGION the target
//! ARN labels, and along WHY the invocation failed (the execution role withholds the
//! send, or the queue the ARN names was never created) — none of which changes the shape
//! of the request the attempt made.
//!
//! The other target kinds' requests are spanned by `tests/all/dead_letter_delivery.rs`
//! and `tests/all/function_target_dead_letter.rs`.

use serde_json::{json, Value};
use service_com_amazonaws_scheduler_awschronosservice::state::invocation;

// The queue-reading, body-parsing and refusal-probing helpers this suite shares with
// the other dead-letter suites (`tests/all/dead_letter_delivery.rs`).
use crate::dead_letter_delivery::{
    body_document, messages_once, refusal_code, PATIENCE, QUEUE_ACCESS_DENIED,
};
use crate::schedule_invocation::{
    at_expression_body, emulator, given_execution_role_for, given_queue, peek, scheduler,
    seconds_from_now, REGION,
};
use crate::wire_harness::account_id;

/// How far ahead of the clock every schedule below states its one occurrence. Every row
/// is written before it passes; the wait for what it then produces is [`PATIENCE`], and
/// nothing here is asserted about how long anything took.
const OCCURRENCE_IN: u64 = 10;

/// The members of Amazon SQS's `SendMessage` request a queue target's failed event is
/// written with: the one that ADDRESSES the queue, and the one that carries the payload.
const QUEUE_URL_MEMBER: &str = "QueueUrl";
/// See [`QUEUE_URL_MEMBER`].
const MESSAGE_BODY_MEMBER: &str = "MessageBody";

/// The context attributes EventBridge Scheduler substitutes into a target's `Input`
/// before handing it over, which the reported request therefore carries SUBSTITUTED:
/// the schedule's ARN, the occurrence's scheduled time, the invocation's execution id
/// and the attempt being made.
const SCHEDULE_ARN_ATTRIBUTE: &str = "<aws.scheduler.schedule-arn>";
/// See [`SCHEDULE_ARN_ATTRIBUTE`].
const SCHEDULED_TIME_ATTRIBUTE: &str = "<aws.scheduler.scheduled-time>";
/// See [`SCHEDULE_ARN_ATTRIBUTE`].
const EXECUTION_ID_ATTRIBUTE: &str = "<aws.scheduler.execution-id>";
/// See [`SCHEDULE_ARN_ATTRIBUTE`].
const ATTEMPT_NUMBER_ATTRIBUTE: &str = "<aws.scheduler.attempt-number>";

/// The attempt every row's invocation failed on: each row's policy admits no retry, and
/// a refusal of the request is the invocation's last attempt in any case, so the payload
/// the report carries is the first attempt's.
const FIRST_ATTEMPT: &str = "1";

/// What a target stating no `Input` of its own is handed, and therefore what the
/// reported request carries for it.
const DEFAULT_PAYLOAD: &str = "{}";

/// The action a queue target's invocation is authorized by, as Amazon SQS names it in a
/// refusal of its own: that service names an action case-folded throughout.
const REPORTED_SEND_MESSAGE: &str = "sqs:sendmessage";

/// Why a row's invocation of its queue target fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Failing {
    /// The queue exists and the execution role's permissions withhold the send to it.
    Withheld,
    /// The role allows the send, and the queue the target ARN names was never created,
    /// so Amazon SQS itself refuses the request.
    Absent,
}

/// One row: the payload the schedule hands its queue target, where that queue is, and
/// why the invocation of it fails.
struct Row {
    case: &'static str,
    /// `Target.Input`, or `None` for a schedule stating none at all.
    input: Option<&'static str>,
    /// The Region the target ARN labels, or `None` for the schedule's own.
    target_region: Option<&'static str>,
    failing: Failing,
}

impl Row {
    /// The Region the row's target ARN labels.
    fn region(&self) -> &str {
        self.target_region.unwrap_or(REGION)
    }

    /// Whether the row's target queue is CREATED: a row whose queue is absent states no
    /// queue to create, and neither does a row naming one in another Region, which this
    /// emulator's Amazon SQS is reached in the schedule's own.
    fn creates_target(&self) -> bool {
        self.failing == Failing::Withheld && self.target_region.is_none()
    }

    /// The payload the attempt rendered from the row's `Input`, which is what the
    /// reported request carries: the context attributes substituted, and nothing else
    /// about the text changed.
    fn payload(&self, schedule_arn: &str, scheduled_time: &str, execution_id: &str) -> String {
        match self.input {
            None => DEFAULT_PAYLOAD.to_string(),
            Some(input) => input
                .replace(SCHEDULE_ARN_ATTRIBUTE, schedule_arn)
                .replace(SCHEDULED_TIME_ATTRIBUTE, scheduled_time)
                .replace(EXECUTION_ID_ATTRIBUTE, execution_id)
                .replace(ATTEMPT_NUMBER_ATTRIBUTE, FIRST_ATTEMPT),
        }
    }
}

const ROWS: &[Row] = &[
    Row {
        case: "a flat JSON object the execution role may not send",
        input: Some(r#"{"marker":"flat"}"#),
        target_region: None,
        failing: Failing::Withheld,
    },
    Row {
        case: "a nested document carrying quotes and characters beyond ASCII",
        input: Some(
            r#"{"marker":"nested","list":[1,2.5,null,true],"unicode":"café ✓","quote":"she said \"hi\""}"#,
        ),
        target_region: None,
        failing: Failing::Withheld,
    },
    Row {
        case: "a top-level array",
        input: Some(r#"[1,"two",{"three":3}]"#),
        target_region: None,
        failing: Failing::Withheld,
    },
    Row {
        case: "text that is not JSON",
        input: Some("a line of plain text, not JSON"),
        target_region: None,
        failing: Failing::Withheld,
    },
    Row {
        case: "no Input at all",
        input: None,
        target_region: None,
        failing: Failing::Withheld,
    },
    Row {
        case: "an Input naming the schedule's context attributes",
        input: Some(
            r#"{"schedule":"<aws.scheduler.schedule-arn>","at":"<aws.scheduler.scheduled-time>","id":"<aws.scheduler.execution-id>","attempt":<aws.scheduler.attempt-number>}"#,
        ),
        target_region: None,
        failing: Failing::Withheld,
    },
    Row {
        // The request addresses the queue in the Region its own ARN labels, which is not
        // always the Region the schedule lives in.
        case: "a target queue in a Region other than the schedule's",
        input: Some(r#"{"marker":"other-region"}"#),
        target_region: Some("us-east-1"),
        failing: Failing::Withheld,
    },
    Row {
        case: "a target queue this account never held",
        input: Some(r#"{"marker":"absent"}"#),
        target_region: None,
        failing: Failing::Absent,
    },
];

/// What one row left behind, read once its occurrence has passed.
struct Prepared<'row> {
    row: &'row Row,
    dead_letter_url: String,
    /// The url of the row's target queue, for a row that created one.
    target_url: Option<String>,
    target_arn: String,
    schedule_arn: String,
    role_name: String,
}

/// Every row: the one message the schedule's dead-letter queue holds carries the whole
/// `SendMessage` request the attempt made — the url the target queue is addressed by in
/// the Region its ARN labels, and the payload the attempt rendered under `MessageBody` —
/// and nothing else, with the failure's detail in the message attributes beside it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_queue_targets_dead_letter_report_states_the_whole_send_message_request() {
    let emulator = emulator();
    // The url form below is the one Amazon SQS itself reports for a queue of this
    // account, pinned against that service rather than restated here.
    let (reported_url, reported_arn) =
        given_queue(&emulator, "the url Amazon SQS reports", "dlq-url-form").await;
    assert_eq!(
        queue_url_of(&reported_arn),
        reported_url,
        "expected the queue url Amazon SQS reports for {reported_arn}"
    );

    let occurrence = seconds_from_now(OCCURRENCE_IN);
    let scheduled_time = invocation::scheduled_time_text(occurrence);
    // Every row's schedule is written on ONE emulator, each with its own queues, so the
    // suite waits for one occurrence rather than one per row.
    let mut prepared: Vec<Prepared<'_>> = Vec::new();
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let (dead_letter_url, dead_letter_arn) =
            given_queue(&emulator, case, &format!("queue-dlq-{index}")).await;
        let name = format!("queue-dlq-target-{index}");
        let (target_url, target_arn) = if row.creates_target() {
            let (url, arn) = given_queue(&emulator, case, &name).await;
            (Some(url), arn)
        } else {
            // The ARN a queue of that name would carry in the Region the row names, and
            // nothing holds it.
            (
                None,
                format!("arn:aws:sqs:{}:{}:{name}", row.region(), account_id()),
            )
        };
        // The role may always write to the row's dead-letter queue, so the report is
        // delivered whatever ended the invocation itself; it may write to the TARGET
        // only where the row's failure is the target's own refusal.
        let mut granted: Vec<&str> = vec![dead_letter_arn.as_str()];
        if row.failing == Failing::Absent {
            granted.push(target_arn.as_str());
        }
        let role_name = format!("queue-dlq-role-{index}");
        let role = given_execution_role_for(&emulator, case, &role_name, &granted).await;
        let mut target = json!({
            "Arn": target_arn,
            "RoleArn": role,
            // Both failures below are refusals of the request itself, which no further
            // attempt would be answered differently; the policy is stated all the same,
            // so the report is of an invocation that retried nothing.
            "RetryPolicy": { "MaximumRetryAttempts": 0 },
            "DeadLetterConfig": { "Arn": dead_letter_arn },
        });
        if let Some(input) = row.input {
            target["Input"] = json!(input);
        }
        let (status, created) = scheduler(
            &emulator,
            "POST",
            &format!("/schedules/queue-dead-letter-{index}"),
            Some(json!({
                "ScheduleExpression": format!("at({})", at_expression_body(occurrence)),
                "ScheduleExpressionTimezone": "UTC",
                "FlexibleTimeWindow": { "Mode": "OFF" },
                "State": "ENABLED",
                "Target": target,
            })),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateSchedule answered {status} {created}"
        );
        let schedule_arn = created["ScheduleArn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no ScheduleArn in {created}"))
            .to_string();
        prepared.push(Prepared {
            row,
            dead_letter_url,
            target_url,
            target_arn,
            schedule_arn,
            role_name,
        });
    }

    for entry in &prepared {
        let case = entry.row.case;
        let reported = messages_once(&emulator, &entry.dead_letter_url, 1, PATIENCE).await;
        let message = reported
            .first()
            .unwrap_or_else(|| panic!("case {case:?}: nothing was dead-lettered"));
        let document = body_document(message);
        // The body is the REQUEST, so its members are that request's and no others: a
        // consumer reads the undelivered payload under `MessageBody` and the queue the
        // occurrence was destined for under `QueueUrl`.
        let mut members: Vec<&str> = document
            .as_object()
            .map(|map| map.keys().map(String::as_str).collect())
            .unwrap_or_default();
        members.sort_unstable();
        assert_eq!(
            members,
            vec![MESSAGE_BODY_MEMBER, QUEUE_URL_MEMBER],
            "case {case:?}: expected the SendMessage request's own members, got {document}"
        );
        let execution = attribute(message, "EXECUTION_ID").unwrap_or_default();
        let payload = entry
            .row
            .payload(&entry.schedule_arn, &scheduled_time, &execution);
        let expected = json!({
            QUEUE_URL_MEMBER: queue_url_of(&entry.target_arn),
            MESSAGE_BODY_MEMBER: payload,
        });
        assert_eq!(
            document, expected,
            "case {case:?}: expected the dead-lettered request {expected}, got {message}"
        );
        // The failure's detail is reported BESIDE the request, in the attributes.
        let expected_code = match entry.row.failing {
            Failing::Withheld => QUEUE_ACCESS_DENIED.to_string(),
            // The code a report of a refused request carries is the one the target's own
            // API answers that request with, so the row asks that service rather than
            // restating its vocabulary here.
            Failing::Absent => {
                refusal_code(&emulator, case, &queue_url_of(&entry.target_arn)).await
            }
        };
        for (name, expected) in [
            ("ERROR_CODE", expected_code),
            ("SCHEDULE_ARN", entry.schedule_arn.clone()),
            ("TARGET_ARN", entry.target_arn.clone()),
            ("SCHEDULED_TIME", scheduled_time.clone()),
            ("RETRY_ATTEMPTS", "0".to_string()),
        ] {
            assert_eq!(
                attribute(message, name),
                Some(expected.clone()),
                "case {case:?}: expected {name} to carry {expected:?}, got {message}"
            );
        }
        let detail = attribute(message, "ERROR_MESSAGE").unwrap_or_default();
        match entry.row.failing {
            // AWS's authorization refusal: the principal the call was made by — the
            // session this invocation assumed the execution role under, named by a token
            // of its own execution id — the action as Amazon SQS names one in a refusal
            // of its own, and the resource the call was on.
            Failing::Withheld => {
                let session = execution.replace('-', "");
                let expected = format!(
                    "User: arn:aws:sts::{account}:assumed-role/{role}/{session} is not authorized \
                     to perform: {REPORTED_SEND_MESSAGE} on resource: {resource} because no \
                     identity-based policy allows the {REPORTED_SEND_MESSAGE} action",
                    account = account_id(),
                    role = entry.role_name,
                    resource = entry.target_arn,
                );
                assert_eq!(
                    detail, expected,
                    "case {case:?}: expected the refusal {expected:?}, got {detail:?}"
                );
            }
            Failing::Absent => assert!(
                !detail.is_empty(),
                "case {case:?}: expected the refusal to be reported, got {message}"
            ),
        }
        // Nothing reached the target: the report is of an invocation that did not
        // arrive, so the payload is recoverable from the report alone.
        if let Some(url) = &entry.target_url {
            let held = peek(&emulator, url).await;
            assert!(
                held.is_empty(),
                "case {case:?}: expected the target queue to hold nothing, got {held:?}"
            );
        }
    }
}

/// One message attribute's string value, as a client that asked for every attribute
/// reads it.
fn attribute(message: &Value, name: &str) -> Option<String> {
    message["MessageAttributes"][name]["StringValue"]
        .as_str()
        .map(ToString::to_string)
}

/// The url Amazon SQS addresses the queue an ARN names by:
/// `https://sqs.<region>.amazonaws.com/<accountId>/<QueueName>`, over the Region and the
/// account the ARN itself labels — `arn:<partition>:sqs:<region>:<account>:<QueueName>`.
/// Pinned against the url that service reports for a queue of this account above.
fn queue_url_of(queue_arn: &str) -> String {
    let fields: Vec<&str> = queue_arn.split(':').collect();
    let region = fields.get(3).copied().unwrap_or_default();
    let account = fields.get(4).copied().unwrap_or_default();
    let name = fields.get(5).copied().unwrap_or_default();
    format!("https://sqs.{region}.amazonaws.com/{account}/{name}")
}
