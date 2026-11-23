// SPDX-License-Identifier: CC0-1.0

//! What a subscriber that did NOT ask for raw delivery reads of an asynchronous invocation's
//! completion when the dead-letter target, or the destination, is an Amazon SNS topic — Lambda,
//! Amazon SNS and Amazon SQS on ONE engine, so each publication is read where a client reads
//! it.
//!
//! Two rules are under test, each over a set of inputs rather than one case:
//!
//! 1. **A dead letter carries the original event as the published MESSAGE and exactly three
//!    message attributes beside it** — `RequestID` (`DataType` `String`), `ErrorCode`
//!    (`DataType` `Number`) and `ErrorMessage` (`DataType` `String`, truncated to its first
//!    kilobyte) — taken from the invocation that failed:
//!    `DeadLetterConfig$TargetArn` is "The Amazon Resource Name (ARN) of an Amazon SQS queue or
//!    Amazon SNS topic"
//!    (`smithy_query(query="shape",shape="DeadLetterConfig",member="TargetArn")`), and the
//!    attribute names and their three data types are the fixed contract whatever the failure
//!    was. A publication's attributes reach a subscription that asked for no raw delivery
//!    inside the notification envelope's own `MessageAttributes`
//!    (`{"Type": …, "Value": …}`), and such a delivery sets no Amazon SQS message attribute of
//!    its own — "RawMessageDelivery […] eliminates the need for the endpoints to process JSON
//!    formatting" is what moves them out of the envelope, so a consumer of a plain
//!    subscription finds them there and nowhere else.
//! 2. **An invocation record names the function QUALIFIED by the qualifier the event was
//!    addressed to**, which is the unpublished version for an invocation that names no
//!    qualifier at all: an asynchronous-invocation configuration is held per qualifier
//!    (`PutFunctionEventInvokeConfig$Qualifier`), so `requestContext.functionArn` says which
//!    one the record was written for. Through a plain subscription the record travels as the
//!    envelope's `Message`, so a consumer parses it out of there.
//!
//! The rows of the first table differ along what rule 1 generalises over: the LENGTH of the
//! handler's own failure message (one that fits in the kilobyte a dead letter records, one well
//! past it) and the FORM of the event the dead letter has to relay (a flat object, one carrying
//! a nested object and an array). The rows of the second differ along the SPELLING of the
//! qualifier the event is addressed to (none at all, the unpublished version named explicitly),
//! the OUTCOME the event reached (a handler that returned, a handler that raised) and the FORM
//! of the event the record carries.
//!
//! The sibling suite `async_destination_record_shape.rs` pins both rules through a subscription
//! that asked for RAW delivery, where the attributes arrive as the queue's own and the record is
//! the message body itself; this one pins the plain subscription beside it, so a publication
//! whose attributes never left the Lambda side is an empty attribute set here rather than a
//! value the raw path happened to carry.
//!
//! Every function's package RUNS, so the failure a dead letter reports is the handler's own.
//! Nothing here waits a span of its own: every wait looks again until what it waits for is
//! there or a generous deadline passes.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::async_destination_record_shape::{
    awaited_message, given_event_invoke_config, given_function, given_queue, given_topic,
    invoke_async, refusal_of, router_with_targets, CONDITION_RETRIES_EXHAUSTED,
    CONDITION_SUCCESS, LATEST,
};
use crate::event_source_mapping_failure_destination::sqs;

/// The Region every call below is made in, and the Region every ARN carries.
const REGION: &str = "us-east-1";

/// The API version every Amazon SNS call below states, which that service's `awsQuery` protocol
/// carries as a parameter of the body.
const SNS_API_VERSION: &str = "2010-03-31";

/// The service principal Amazon SNS delivers to a subscribed queue as, which that queue's own
/// policy has to grant `sqs:SendMessage` to.
const SNS_PRINCIPAL: &str = "sns.amazonaws.com";

/// The protocol a queue is subscribed to a topic under (`Subscribe$Protocol`).
const SQS_PROTOCOL: &str = "sqs";

/// What the JSON notification SNS wraps a published message in reports itself as, and the
/// members it carries the topic, the message and the publication's attributes under — the
/// envelope a subscriber that has NOT asked for raw delivery receives (`RawMessageDelivery`).
const NOTIFICATION_TYPE: &str = "Notification";
/// See [`NOTIFICATION_TYPE`].
const TYPE_MEMBER: &str = "Type";
/// See [`NOTIFICATION_TYPE`].
const TOPIC_MEMBER: &str = "TopicArn";
/// See [`NOTIFICATION_TYPE`].
const MESSAGE_MEMBER: &str = "Message";
/// See [`NOTIFICATION_TYPE`].
const MESSAGE_ATTRIBUTES_MEMBER: &str = "MessageAttributes";
/// The members one attribute of that envelope is carried under, which are spelled differently
/// from the `DataType`/`StringValue` pair a publication states them with.
const ENVELOPE_ATTRIBUTE_TYPE: &str = "Type";
/// See [`ENVELOPE_ATTRIBUTE_TYPE`].
const ENVELOPE_ATTRIBUTE_VALUE: &str = "Value";

/// The message attributes a dead letter carries, and the `DataType` of each
/// (`MessageAttributeValue$DataType`: the status of the failed invocation is a `Number`, the
/// request id and the message are `String`).
const REQUEST_ID_ATTRIBUTE: &str = "RequestID";
/// See [`REQUEST_ID_ATTRIBUTE`].
const ERROR_CODE_ATTRIBUTE: &str = "ErrorCode";
/// See [`REQUEST_ID_ATTRIBUTE`].
const ERROR_MESSAGE_ATTRIBUTE: &str = "ErrorMessage";
/// See [`REQUEST_ID_ATTRIBUTE`].
const STRING_TYPE: &str = "String";
/// See [`REQUEST_ID_ATTRIBUTE`].
const NUMBER_TYPE: &str = "Number";

/// The status an invocation that executed answers, which is what a dead letter's `ErrorCode`
/// records.
const INVOKED_STATUS: &str = "200";

/// How much of the handler's own message a dead letter records: AWS truncates the error message
/// of a dead-lettered event to its first kilobyte, so a handler that raised with a long message
/// does not put an unbounded value on the target's attributes.
const MAX_ERROR_MESSAGE_BYTES: usize = 1_024;

/// The schema version an invocation record carries.
const RECORD_SCHEMA_VERSION: &str = "1.0";

/// The authorization header every call below carries. Request authentication is off on the
/// router these suites stand up, so the header only has to name the signing name the request is
/// claimed by.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=emulator-test-key/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=not-verified"
    )
}

/// One Amazon SNS call, in the `awsQuery` form that service's protocol binds.
async fn sns(
    router: &MultiServiceRouter,
    action: &str,
    parameters: &[(&str, &str)],
) -> (u16, String) {
    let mut form = form_urlencoded::Serializer::new(String::new());
    form.append_pair("Action", action);
    form.append_pair("Version", SNS_API_VERSION);
    for (name, value) in parameters {
        form.append_pair(name, value);
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("sns.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("sns"))
        .body(bytes::Bytes::from(form.finish()))
        .expect("the test request must build");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (
        status,
        String::from_utf8_lossy(&response.into_body()).to_string(),
    )
}

/// A topic of this account with the queue `queue_arn` names subscribed to it in the PLAIN form —
/// no `RawMessageDelivery`, so every delivery is the JSON notification SNS wraps the published
/// message in — and that queue's own `Policy` written to admit the topic's deliveries. Answers
/// the topic's ARN.
async fn given_topic_relaying_envelopes(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    queue_url: &str,
    queue_arn: &str,
) -> String {
    let topic_arn = given_topic(router, case, name).await;
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "AllowDeliveryFromTheTopic",
            "Effect": "Allow",
            "Principal": { "Service": SNS_PRINCIPAL },
            "Action": "sqs:SendMessage",
            "Resource": queue_arn,
            "Condition": { "ArnEquals": { "aws:SourceArn": topic_arn } },
        }],
    })
    .to_string();
    let (status, written) = sqs(
        router,
        "SetQueueAttributes",
        &json!({ "QueueUrl": queue_url, "Attributes": { "Policy": policy } }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: SetQueueAttributes answered {status} {written}"
    );
    let (status, subscribed) = sns(
        router,
        "Subscribe",
        &[
            ("TopicArn", &topic_arn),
            ("Protocol", SQS_PROTOCOL),
            ("Endpoint", queue_arn),
            ("ReturnSubscriptionArn", "true"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: Subscribe answered {status} {subscribed}"
    );
    topic_arn
}

/// The notification one message of a plainly subscribed queue carries, checked to be the
/// envelope of `topic_arn`'s own publication — and checked to carry no Amazon SQS message
/// attribute of its own, because a plain delivery leaves the publication's attributes INSIDE
/// the envelope.
fn envelope_of(case: &str, what: &str, topic_arn: &str, message: &Value) -> Value {
    let body = message["Body"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the {what} carries no body: {message}"));
    let envelope: Value = serde_json::from_str(body).unwrap_or_else(|_| {
        panic!("case {case:?}: the {what} is no JSON notification: {body}")
    });
    let answered = envelope[TYPE_MEMBER].as_str();
    assert_eq!(
        answered,
        Some(NOTIFICATION_TYPE),
        "case {case:?}: expected the {what} to be a {NOTIFICATION_TYPE}, got {answered:?} in \
         {envelope}"
    );
    let answered = envelope[TOPIC_MEMBER].as_str();
    assert_eq!(
        answered,
        Some(topic_arn),
        "case {case:?}: expected the {what} to name the topic {topic_arn}, got {answered:?} in \
         {envelope}"
    );
    let own = message[MESSAGE_ATTRIBUTES_MEMBER]
        .as_object()
        .map_or(0, |held| held.len());
    assert_eq!(
        own,
        0,
        "case {case:?}: expected the {what} to carry no queue attributes of its own, got {}",
        message[MESSAGE_ATTRIBUTES_MEMBER]
    );
    envelope
}

/// One attribute of the envelope, as the envelope spells it.
fn envelope_attribute(data_type: &str, value: &str) -> Value {
    json!({ ENVELOPE_ATTRIBUTE_TYPE: data_type, ENVELOPE_ATTRIBUTE_VALUE: value })
}

/// What `ErrorMessage` must carry for the refusal `message`: the whole of it when it fits in
/// [`MAX_ERROR_MESSAGE_BYTES`], and its first kilobyte otherwise. Every message below is ASCII,
/// so that bound is a character boundary.
fn recorded_message(message: &str) -> String {
    message
        .get(..MAX_ERROR_MESSAGE_BYTES)
        .unwrap_or(message)
        .to_string()
}

/// The request id the invocation record on `url` reports, which is the id the dead letter's
/// `RequestID` has to link back to.
async fn awaited_request_id(router: &MultiServiceRouter, case: &str, url: &str) -> String {
    let message = awaited_message(router, case, "invocation record", url).await;
    let body = message["Body"].as_str().unwrap_or_default();
    let record: Value = serde_json::from_str(body)
        .unwrap_or_else(|_| panic!("case {case:?}: the record is no JSON document: {body}"));
    let request_id = record["requestContext"]["requestId"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        !request_id.is_empty(),
        "case {case:?}: the record must name the request the event was accepted as, got {record}"
    );
    request_id
}

/// One row of the dead-letter table: the event the caller submits, and the mark the handler's
/// own refusal of it names.
struct DeadLetterRow {
    case: &'static str,
    /// A name no other row uses, for every resource the row creates.
    name: &'static str,
    /// What the event marks itself with, which is what the handler raises with.
    mark: String,
    /// The event the caller submits, carrying that mark.
    event: Value,
}

/// The rows: a refusal that fits in the kilobyte a dead letter records and one well past it, a
/// flat event and one carrying a nested object and an array.
fn dead_letter_rows() -> Vec<DeadLetterRow> {
    let short = "dead-letter-short".to_string();
    // Past [`MAX_ERROR_MESSAGE_BYTES`] once the handler's own sentence is around it, so the
    // recorded value is the first kilobyte of the message rather than the whole of it.
    let long = "over-a-kilobyte-".repeat(80);
    vec![
        DeadLetterRow {
            case: "a refusal inside the recorded kilobyte, relayed with a flat event",
            name: "enveloped-dlq-short",
            mark: short.clone(),
            event: json!({ "fail": true, "mark": short }),
        },
        DeadLetterRow {
            case: "a refusal past the recorded kilobyte, relayed with a nested event",
            name: "enveloped-dlq-long",
            mark: long.clone(),
            event: json!({
                "fail": true,
                "mark": long,
                "detail": { "items": [1, 2, 3], "note": "nested" },
            }),
        },
    ]
}

/// RULE: a dead letter published to a topic carries the original event as the message and
/// exactly `RequestID` (String), `ErrorCode` (Number) and `ErrorMessage` (String, truncated to
/// its first kilobyte) beside it — which a subscription that asked for no raw delivery reads out
/// of the notification envelope.
#[tokio::test]
async fn a_dead_letter_topic_relays_the_event_with_the_failures_bookkeeping() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = router_with_targets(&engine);
    for row in dead_letter_rows() {
        let case = row.case;
        // The topic the dead letter is published to, read through a plainly subscribed queue,
        // and the queue the invocation record goes to — the record is what says which request
        // the dead letter belongs to.
        let (relayed_url, relayed_arn) =
            given_queue(&router, case, &format!("{}-relayed", row.name)).await;
        let topic_arn = given_topic_relaying_envelopes(
            &router,
            case,
            &format!("{}-topic", row.name),
            &relayed_url,
            &relayed_arn,
        )
        .await;
        let (record_url, record_arn) =
            given_queue(&router, case, &format!("{}-records", row.name)).await;
        given_function(&router, case, row.name, Some(&topic_arn)).await;
        given_event_invoke_config(&router, case, row.name, None, None, Some(&record_arn)).await;

        invoke_async(&router, case, row.name, None, &row.event).await;

        let request_id = awaited_request_id(&router, case, &record_url).await;
        let relayed = awaited_message(&router, case, "dead-lettered event", &relayed_url).await;
        let envelope = envelope_of(case, "dead-lettered event", &topic_arn, &relayed);

        // The message is the event payload EXACTLY as the caller supplied it, which is what
        // makes the dropped event replayable.
        let answered = envelope[MESSAGE_MEMBER].as_str();
        let expected = row.event.to_string();
        assert_eq!(
            answered,
            Some(expected.as_str()),
            "case {case:?}: expected the relayed message to be the event itself, got {answered:?}"
        );
        let expected = json!({
            REQUEST_ID_ATTRIBUTE: envelope_attribute(STRING_TYPE, &request_id),
            ERROR_CODE_ATTRIBUTE: envelope_attribute(NUMBER_TYPE, INVOKED_STATUS),
            ERROR_MESSAGE_ATTRIBUTE: envelope_attribute(
                STRING_TYPE,
                &recorded_message(&refusal_of(&row.mark)),
            ),
        });
        let answered = &envelope[MESSAGE_ATTRIBUTES_MEMBER];
        assert_eq!(
            answered, &expected,
            "case {case:?}: expected the relayed notification to carry {expected}, got {answered}"
        );
    }
}

/// One row of the invocation-record table: the qualifier the event is addressed to, what the
/// handler does with it, and the event itself.
struct RecordRow {
    case: &'static str,
    /// A name no other row uses, for every resource the row creates.
    name: &'static str,
    /// The `Qualifier` the configuration is held against and the invocation names, and `None`
    /// for the pair of requests that name none at all.
    qualifier: Option<&'static str>,
    /// Whether the handler raises, which decides the slot the record is written to and the
    /// condition it reports.
    fails: bool,
    /// The event the caller submits.
    event: Value,
}

impl RecordRow {
    /// The condition the record reports for this row's outcome.
    fn condition(&self) -> &'static str {
        if self.fails {
            CONDITION_RETRIES_EXHAUSTED
        } else {
            CONDITION_SUCCESS
        }
    }
}

/// The rows: the unpublished version addressed by naming no qualifier and by naming it, a
/// handler that returned and one that raised, a flat event and an event that is an array.
fn record_rows() -> Vec<RecordRow> {
    vec![
        RecordRow {
            case: "a success record for an invocation naming no qualifier",
            name: "enveloped-record-unqualified",
            qualifier: None,
            fails: false,
            event: json!({ "mark": "enveloped-record-unqualified" }),
        },
        RecordRow {
            case: "a success record for an invocation naming the unpublished version, with an \
                   event that is an array",
            name: "enveloped-record-latest",
            qualifier: Some(LATEST),
            fails: false,
            event: json!([1, "two", { "three": true }]),
        },
        RecordRow {
            case: "a failure record for an invocation naming no qualifier",
            name: "enveloped-record-failure",
            qualifier: None,
            fails: true,
            event: json!({ "fail": true, "mark": "enveloped-record-failure" }),
        },
    ]
}

/// RULE: the invocation record a topic destination relays names the function QUALIFIED by the
/// qualifier the event was addressed to — the unpublished version for an invocation that named
/// none — and reports the condition that ended the event; a plain subscription reads it out of
/// the notification envelope's message.
#[tokio::test]
async fn an_enveloped_invocation_record_names_the_qualifier_the_event_was_addressed_to() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = router_with_targets(&engine);
    for row in record_rows() {
        let case = row.case;
        let (relayed_url, relayed_arn) =
            given_queue(&router, case, &format!("{}-relayed", row.name)).await;
        let topic_arn = given_topic_relaying_envelopes(
            &router,
            case,
            &format!("{}-topic", row.name),
            &relayed_url,
            &relayed_arn,
        )
        .await;
        let arn = given_function(&router, case, row.name, None).await;
        let (on_success, on_failure) = if row.fails {
            (None, Some(topic_arn.as_str()))
        } else {
            (Some(topic_arn.as_str()), None)
        };
        given_event_invoke_config(
            &router,
            case,
            row.name,
            row.qualifier,
            on_success,
            on_failure,
        )
        .await;

        invoke_async(&router, case, row.name, row.qualifier, &row.event).await;

        let relayed = awaited_message(&router, case, "invocation record", &relayed_url).await;
        let envelope = envelope_of(case, "invocation record", &topic_arn, &relayed);
        let message = envelope[MESSAGE_MEMBER].as_str().unwrap_or_default();
        let record: Value = serde_json::from_str(message).unwrap_or_else(|_| {
            panic!("case {case:?}: the relayed record is no JSON document: {message}")
        });

        let answered = record["version"].as_str();
        assert_eq!(
            answered,
            Some(RECORD_SCHEMA_VERSION),
            "case {case:?}: expected the record schema version {RECORD_SCHEMA_VERSION}, got \
             {answered:?} in {record}"
        );
        // The qualifier is the one the configuration is held against, whichever way the request
        // spelled it: an invocation naming none addresses the unpublished version.
        let expected = format!("{arn}:{LATEST}");
        let answered = record["requestContext"]["functionArn"].as_str();
        assert_eq!(
            answered,
            Some(expected.as_str()),
            "case {case:?}: expected the record to name {expected}, got {answered:?} in {record}"
        );
        let expected = row.condition();
        let answered = record["requestContext"]["condition"].as_str();
        assert_eq!(
            answered,
            Some(expected),
            "case {case:?}: expected the condition {expected}, got {answered:?} in {record}"
        );
        let answered = record["requestContext"]["requestId"].as_str();
        assert!(
            answered.is_some_and(|request_id| !request_id.is_empty()),
            "case {case:?}: the record must name the request the event was accepted as, got \
             {answered:?} in {record}"
        );
        let answered = record["requestContext"]["approximateInvokeCount"].as_i64();
        assert_eq!(
            answered,
            Some(1),
            "case {case:?}: expected the one attempt the configuration allows to be reported, got \
             {answered:?} in {record}"
        );
        let answered = &record["requestPayload"];
        assert_eq!(
            answered, &row.event,
            "case {case:?}: expected the record to carry the event {}, got {answered}",
            row.event
        );
        if !row.fails {
            // What the handler answered for the event, which is the record's response payload.
            let expected = json!({ "handled": row.event.clone() });
            let answered = &record["responsePayload"];
            assert_eq!(
                answered, &expected,
                "case {case:?}: expected the record to carry the answer {expected}, got \
                 {answered}"
            );
        }
    }
}
