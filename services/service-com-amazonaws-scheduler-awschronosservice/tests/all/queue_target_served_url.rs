// SPDX-License-Identifier: CC0-1.0

//! WHICH ADDRESS a queue target's invocation carries: end to end over the wire through
//! the real `MultiServiceRouter`, with Amazon SQS and IAM mounted beside EventBridge
//! Scheduler on ONE engine, while the emulator is SERVED at an address of its own — so
//! the address is asserted where a client reads it, in the dead-letter record and in the
//! queue the invocation reached.
//!
//! RULE: a queue target is "the templated target type for the Amazon SQS SendMessage API
//! operation" (`Target$SqsParameters`), and that request addresses its queue by a
//! `QueueUrl` — "the URL of the Amazon SQS queue", the string `CreateQueue` and
//! `GetQueueUrl` report for it. So an invocation resolves a `Target.Arn` of the form
//! `arn:<partition>:sqs:<region>:<account>:<QueueName>` to the url THAT SERVICE serves
//! for the queue, and the dead-letter record of a failed invocation states the same
//! string: a reader correlates the record with the queue by url, and replays the request
//! against an endpoint Amazon SQS answers at. The host in it is the environment's, never
//! a fixed one composed here.
//!
//! The rows span the class rather than one recorded case: they differ along the ADDRESS
//! this emulator is served at (a loopback literal with a port, a host name with a port,
//! an IPv6 literal with a port — each a different authority, none of them AWS's own
//! published host), and along the FORM of the target queue's name (a plain name, one
//! carrying the underscore and hyphen a queue name admits, the longest name one may be,
//! and a FIFO queue's `.fifo` name, whose target also states a message group).
//!
//! The members such a request carries, over every target kind, are spanned by
//! `tests/all/queue_target_dead_letter.rs` and `tests/all/dead_letter_delivery.rs`; what
//! those suites assert on an UNSERVED emulator, where the url Amazon SQS serves is its
//! own published form, this one asserts on a served one.

use serde_json::{json, Value};
use smithy_server_runtime::{EndpointUrl, MultiServiceRouter};

use crate::dead_letter_delivery::{body_document, messages_once, PATIENCE, QUEUE_ACCESS_DENIED};
use crate::schedule_invocation::{
    at_expression_body, bodies_once, emulator, given_execution_role_for, given_queue, scheduler,
    seconds_from_now, sqs,
};
use crate::wire_harness::account_id;

/// How far ahead of the clock every schedule below states its one occurrence. Every row
/// is written before it passes; the wait for what it then produces is [`PATIENCE`], and
/// nothing here is asserted about how long anything took.
const OCCURRENCE_IN: u64 = 4;

/// The members of Amazon SQS's `SendMessage` request a queue target's invocation makes:
/// the one that ADDRESSES the queue, the one that carries the payload, and the FIFO
/// message group a target may state for it.
const QUEUE_URL_MEMBER: &str = "QueueUrl";
/// See [`QUEUE_URL_MEMBER`].
const MESSAGE_BODY_MEMBER: &str = "MessageBody";
/// See [`QUEUE_URL_MEMBER`].
const MESSAGE_GROUP_ID_MEMBER: &str = "MessageGroupId";

/// The longest a queue name may be: "the name can be up to 80 characters long and can
/// include alphanumeric characters, hyphens (-), and underscores (_)"
/// (`GetQueueUrlRequest$QueueName`, `smithy_query` query="doc" shape="GetQueueUrlRequest"
/// member="QueueName" on Amazon SQS's model).
const MAX_QUEUE_NAME_LENGTH: usize = 80;

/// One row: where this emulator is served, and the queue the row's schedule targets.
struct Row {
    case: &'static str,
    /// The address the emulator is served at, as a data port publishes it.
    served_at: &'static str,
    /// The target queue's name, the `.fifo` suffix included for a FIFO one.
    queue: &'static str,
    /// `Target.SqsParameters.MessageGroupId`, which a FIFO queue's target states.
    message_group_id: Option<&'static str>,
}

const ROWS: &[Row] = &[
    Row {
        case: "a loopback literal with a port",
        served_at: "https://127.0.0.1:9297",
        queue: "served-target",
        message_group_id: None,
    },
    Row {
        case: "a host name with a port",
        served_at: "https://emulator.test:8443",
        queue: "served_target-2",
        message_group_id: None,
    },
    Row {
        case: "an IPv6 literal with a port",
        served_at: "https://[::1]:36181",
        // [`MAX_QUEUE_NAME_LENGTH`] characters exactly — the longest name a queue may
        // carry — so the address the record states is not the shortest one either.
        queue: "served-target-whose-name-is-the-longest-a-queue-name-may-be_01234567890123456789",
        message_group_id: None,
    },
    Row {
        case: "a FIFO queue, whose target states a message group",
        served_at: "https://127.0.0.1:19297",
        queue: "served-target.fifo",
        message_group_id: Some("orders"),
    },
];

/// Every row: the dead-letter record of an invocation that never reached its queue
/// target addresses that queue by the url Amazon SQS serves for it, which is an address
/// of the endpoint this emulator is served at.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_queue_targets_request_addresses_it_by_the_url_amazon_sqs_serves() {
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        assert!(
            row.queue.len() <= MAX_QUEUE_NAME_LENGTH,
            "case {case:?}: a queue name may carry at most {MAX_QUEUE_NAME_LENGTH} characters, \
             and {:?} carries {}",
            row.queue,
            row.queue.len()
        );
        let emulator = served_emulator(case, row.served_at);
        let (dead_letter_url, dead_letter_arn) =
            given_queue(&emulator, case, &format!("served-dlq-{index}")).await;
        let target_arn = given_target_queue(&emulator, case, row.queue).await;
        // The url that service SERVES for the target queue, read through its own reader:
        // the string the record must carry. It is an address of this emulator's own
        // endpoint, which the row states independently of what the service answered.
        let served = served_queue_url(&emulator, case, row.queue).await;
        let expected_url = format!("{}/{}/{}", row.served_at, account_id(), row.queue);
        assert_eq!(
            served, expected_url,
            "case {case:?}: expected Amazon SQS to serve {expected_url:?}, got {served:?}"
        );

        // The execution role may write to the dead-letter queue alone, so the invocation
        // is withheld and its request is reported there.
        let role = given_execution_role_for(
            &emulator,
            case,
            &format!("served-role-{index}"),
            &[&dead_letter_arn],
        )
        .await;
        let occurrence = seconds_from_now(OCCURRENCE_IN);
        let payload = format!("{{\"marker\":\"{index}\"}}");
        let mut target = json!({
            "Arn": target_arn,
            "RoleArn": role,
            "Input": payload,
            // A refusal of the request is the invocation's last attempt in any case; the
            // policy is stated so the report is of an invocation that retried nothing.
            "RetryPolicy": { "MaximumRetryAttempts": 0 },
            "DeadLetterConfig": { "Arn": dead_letter_arn },
        });
        if let Some(group) = row.message_group_id {
            target["SqsParameters"] = json!({ "MessageGroupId": group });
        }
        let (status, created) = scheduler(
            &emulator,
            "POST",
            &format!("/schedules/served-address-{index}"),
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

        let reported = messages_once(&emulator, &dead_letter_url, 1, PATIENCE).await;
        let message = reported
            .first()
            .unwrap_or_else(|| panic!("case {case:?}: nothing was dead-lettered"));
        let document = body_document(message);
        let mut expected = serde_json::Map::new();
        expected.insert(QUEUE_URL_MEMBER.to_string(), Value::String(served.clone()));
        expected.insert(
            MESSAGE_BODY_MEMBER.to_string(),
            Value::String(payload.clone()),
        );
        if let Some(group) = row.message_group_id {
            expected.insert(
                MESSAGE_GROUP_ID_MEMBER.to_string(),
                Value::String(group.to_string()),
            );
        }
        let expected = Value::Object(expected);
        assert_eq!(
            document, expected,
            "case {case:?}: expected the dead-lettered request {expected}, got {message}"
        );
        // The failure reported beside it is the withheld call, so the record is of the
        // invocation this row's role refused rather than of some other attempt.
        assert_eq!(
            message["MessageAttributes"]["ERROR_CODE"]["StringValue"].as_str(),
            Some(QUEUE_ACCESS_DENIED),
            "case {case:?}: expected {QUEUE_ACCESS_DENIED:?} to be reported, got {message}"
        );
        assert_eq!(
            message["MessageAttributes"]["TARGET_ARN"]["StringValue"].as_str(),
            Some(target_arn.as_str()),
            "case {case:?}: expected the target ARN {target_arn:?} to be reported, got {message}"
        );
    }
}

/// The url the invocation resolves is one Amazon SQS ANSWERS at: a schedule whose
/// execution role allows the send reaches the queue through it, and the message is read
/// back at the url that service serves.
///
/// The rows differ along the address the emulator is served at, which is the authority
/// the invocation's own request carried.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_invocation_reaches_its_queue_through_the_url_amazon_sqs_serves() {
    for (index, row) in ROWS.iter().enumerate() {
        // A FIFO queue takes no message without a deduplication id of its own, which a
        // schedule's target states nothing for, so the delivery rows are the standard
        // queues'.
        if row.message_group_id.is_some() {
            continue;
        }
        let case = row.case;
        let emulator = served_emulator(case, row.served_at);
        let target_arn = given_target_queue(&emulator, case, row.queue).await;
        let served = served_queue_url(&emulator, case, row.queue).await;
        let role = given_execution_role_for(
            &emulator,
            case,
            &format!("delivering-role-{index}"),
            &[&target_arn],
        )
        .await;
        let payload = format!("{{\"delivered\":\"{index}\"}}");
        let (status, created) = scheduler(
            &emulator,
            "POST",
            &format!("/schedules/served-delivery-{index}"),
            Some(json!({
                "ScheduleExpression": format!(
                    "at({})",
                    at_expression_body(seconds_from_now(OCCURRENCE_IN))
                ),
                "ScheduleExpressionTimezone": "UTC",
                "FlexibleTimeWindow": { "Mode": "OFF" },
                "State": "ENABLED",
                "Target": {
                    "Arn": target_arn,
                    "RoleArn": role,
                    "Input": payload,
                    "RetryPolicy": { "MaximumRetryAttempts": 0 },
                },
            })),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateSchedule answered {status} {created}"
        );
        let held = bodies_once(&emulator, &served, 1, PATIENCE).await;
        assert_eq!(
            held,
            vec![payload.clone()],
            "case {case:?}: expected the queue at {served:?} to hold {payload:?}, got {held:?}"
        );
    }
}

/// The emulator of `crate::schedule_invocation`, SERVED at `endpoint` — the state
/// starting its data port there leaves it in, which is what makes the address it
/// announces for a queue its own rather than AWS's published host.
///
/// The base is shared by every clone of one router (`MultiServiceRouter::set_url_base`:
/// "Shared by every clone"), so the router returned answers at it.
fn served_emulator(case: &str, endpoint: &str) -> MultiServiceRouter {
    let base = EndpointUrl::parse(endpoint)
        .unwrap_or_else(|| panic!("case {case:?}: a data port publishes a bare https url"));
    let mut router = emulator();
    router.set_url_base(base);
    router
}

/// The row's target queue, created, and the ARN a schedule names it by. A name ending in
/// `.fifo` is a FIFO queue, which is created as one.
async fn given_target_queue(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    if !name.ends_with(".fifo") {
        let (_url, arn) = given_queue(router, case, name).await;
        return arn;
    }
    let (status, created) = sqs(
        router,
        "CreateQueue",
        &json!({ "QueueName": name, "Attributes": { "FifoQueue": "true" } }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateQueue answered {status} {created}"
    );
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no QueueUrl in {created}"))
        .to_string();
    let (status, attributes) = sqs(
        router,
        "GetQueueAttributes",
        &json!({ "QueueUrl": url, "AttributeNames": ["QueueArn"] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetQueueAttributes answered {status} {attributes}"
    );
    attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no QueueArn in {attributes}"))
        .to_string()
}

/// The url Amazon SQS SERVES for the queue called `name`, read through that service's
/// own reader — the string a client holds for the queue, and the one the invocation's
/// request must carry.
async fn served_queue_url(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let (status, answered) = sqs(router, "GetQueueUrl", &json!({ "QueueName": name })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetQueueUrl answered {status} {answered}"
    );
    answered["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no QueueUrl in {answered}"))
        .to_string()
}
