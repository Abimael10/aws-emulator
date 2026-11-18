// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHO receives the invocation record of a completed asynchronous
//! invocation when the destination the configuration names is an Amazon SNS topic.
//!
//! RULE: the completion of an accepted asynchronous invocation delivers ONE invocation record
//! to the resource its qualifier's `DestinationConfig` slot names for the outcome the event
//! reached — `DestinationConfig$OnFailure` is "the destination configuration for failed
//! invocations" and `OnSuccess` "the destination configuration for successful invocations",
//! and `OnFailure$Destination` is "the Amazon Resource Name (ARN) of the destination resource"
//! (`smithy_query(query="shape", shape="OnFailure")`) — whatever KIND of resource that ARN
//! names; where it names a topic the record is PUBLISHED to it, so every endpoint subscribed
//! to that topic receives it, exactly as a client reading the record off the topic's
//! subscriber expects. The record carries the event itself as its `requestPayload` and what
//! the run came to as its `responseContext`/`responsePayload`, so a failure-handling pipeline
//! reads what was invoked, with what, and what the function answered.
//!
//! The rows differ along what the rule generalises over: the outcome the event reached (a
//! handler that raised, a handler that returned), which slot the configuration states, the
//! kind of resource the destination ARN names (a topic, a queue), and HOW MANY endpoints are
//! subscribed to that topic (one, two) — so a record that only ever reaches the first
//! subscriber, or only a destination that is a queue, fails here.
//!
//! Every row drives real HTTP requests through one router carrying Lambda, Amazon SQS and
//! Amazon SNS on ONE engine, so the record is asserted where a client reads it: off the queue
//! the destination names, or off each queue subscribed to the topic it names. The function's
//! package RUNS, so what the record reports about the response is the handler's own answer.
//!
//! What a record says about the QUALIFIER it belongs to is
//! [`crate::async_destination_record_shape`]'s subject, whose harness this suite shares; what
//! the ladder of ATTEMPTS before a record is [`crate::async_retry_exhaustion_destination`]'s.
//! This suite is about the record reaching every endpoint of the destination it was addressed
//! to.
//!
//! Nothing here waits a span of its own: every wait looks again until what it waits for is
//! there or a generous deadline passes.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::async_destination_record_shape::{
    awaited_message, given_event_invoke_config, given_function, given_queue, given_topic,
    invoke_async, refusal_of, router_with_targets, subscribed_for_raw_delivery,
    CONDITION_RETRIES_EXHAUSTED, CONDITION_SUCCESS, LATEST,
};

/// The `FunctionError` an attempt whose handler raised reports — "if present, indicates that
/// an error occurred during function execution" (`InvocationResponse$FunctionError`) — which
/// is what a failure record's `responseContext` carries.
const FUNCTION_ERROR_UNHANDLED: &str = "Unhandled";

/// The class of the exception the shared handler raises, as the error document of a failed run
/// reports it: what a failure record's `responsePayload` names beside the message.
const HANDLER_ERROR_TYPE: &str = "RuntimeError";

/// One row: what the handler does with the event, which destination slot the configuration
/// states for the outcome that reaches, and how many endpoints the destination fans out to.
struct Row {
    case: &'static str,
    /// The prefix every resource of the row is named from, so no row reads another's record.
    name: &'static str,
    /// Whether the handler raises, which decides both the slot the record is written to and
    /// the condition it reports.
    fails: bool,
    /// How many queues are subscribed to the topic the destination names, or `None` for a row
    /// whose destination is a queue itself.
    subscribers: Option<usize>,
}

/// The catalog: a failure record relayed by a topic to one subscriber and to two, a success
/// record relayed by a topic to two, and — as the other kind of resource the same slot may
/// name — a failure record delivered straight to a queue.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a failure record published to a topic with one subscriber",
            name: "fan-out-failure-one",
            fails: true,
            subscribers: Some(1),
        },
        Row {
            case: "a failure record published to a topic with two subscribers",
            name: "fan-out-failure-two",
            fails: true,
            subscribers: Some(2),
        },
        Row {
            case: "a success record published to a topic with two subscribers",
            name: "fan-out-success-two",
            fails: false,
            subscribers: Some(2),
        },
        Row {
            case: "a failure record delivered straight to a queue",
            name: "fan-out-failure-queue",
            fails: true,
            subscribers: None,
        },
    ]
}

/// RULE: the invocation record of a completed asynchronous invocation reaches the resource the
/// configuration's slot for that outcome names — and where that resource is a topic, every
/// endpoint subscribed to it receives the record, carrying the event itself and what the run
/// came to.
#[tokio::test]
async fn an_invocation_record_reaches_every_endpoint_of_the_destination_it_names() {
    for row in rows() {
        let case = row.case;
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let router = router_with_targets(&engine);

        // The endpoints the record must reach: one queue per subscriber of the topic, or the
        // one queue the destination itself names.
        let wanted = row.subscribers.unwrap_or(1);
        let mut sinks: Vec<(String, String)> = Vec::new();
        for index in 0..wanted {
            sinks.push(given_queue(&router, case, &format!("{}-sink-{index}", row.name)).await);
        }
        let destination = match row.subscribers {
            None => sinks[0].1.clone(),
            Some(_) => {
                let topic = given_topic(&router, case, &format!("{}-topic", row.name)).await;
                for (url, arn) in &sinks {
                    subscribed_for_raw_delivery(&router, case, &topic, url, arn).await;
                }
                topic
            }
        };

        let function_arn = given_function(&router, case, row.name, None).await;
        let (on_success, on_failure) = if row.fails {
            (None, Some(destination.as_str()))
        } else {
            (Some(destination.as_str()), None)
        };
        given_event_invoke_config(&router, case, row.name, None, on_success, on_failure).await;

        let event = json!({ "fail": row.fails, "mark": row.name });
        invoke_async(&router, case, row.name, None, &event).await;

        let expected_condition = if row.fails {
            CONDITION_RETRIES_EXHAUSTED
        } else {
            CONDITION_SUCCESS
        };
        let expected_arn = format!("{function_arn}:{LATEST}");
        for (index, (url, _)) in sinks.iter().enumerate() {
            let what = format!("invocation record of the endpoint {index}");
            let message = awaited_message(&router, case, &what, url).await;
            let body = message["Body"].as_str().unwrap_or_default();
            let record: Value = serde_json::from_str(body).unwrap_or_else(|_| {
                panic!("case {case:?}: the {what} is no JSON document: {body}")
            });

            let answered = record["requestContext"]["condition"].as_str();
            assert_eq!(
                answered,
                Some(expected_condition),
                "case {case:?}: expected the {what} to report the condition \
                 {expected_condition}, got {answered:?} in {record}"
            );
            let answered = record["requestContext"]["functionArn"].as_str();
            assert_eq!(
                answered,
                Some(expected_arn.as_str()),
                "case {case:?}: expected the {what} to name {expected_arn}, got {answered:?} \
                 in {record}"
            );
            let answered = record["requestContext"]["approximateInvokeCount"].as_i64();
            assert_eq!(
                answered,
                Some(1),
                "case {case:?}: expected the {what} to report the one attempt the \
                 configuration allows, got {answered:?} in {record}"
            );
            // The event itself, exactly as the caller supplied it: what makes the record
            // enough to replay the invocation from.
            let answered = &record["requestPayload"];
            assert_eq!(
                answered, &event,
                "case {case:?}: expected the {what} to carry the event {event}, got \
                 {answered} in {record}"
            );

            let answered = record["responseContext"]["functionError"].as_str();
            let expected_error = row.fails.then_some(FUNCTION_ERROR_UNHANDLED);
            assert_eq!(
                answered, expected_error,
                "case {case:?}: expected the {what} to report the function error \
                 {expected_error:?}, got {answered:?} in {record}"
            );
            let answered = &record["responsePayload"];
            let expected_payload = if row.fails {
                json!({
                    "errorType": HANDLER_ERROR_TYPE,
                    "errorMessage": refusal_of(row.name),
                })
            } else {
                json!({ "handled": event })
            };
            for (member, want) in expected_payload
                .as_object()
                .expect("the expected payload is an object")
            {
                let got = answered.get(member).cloned().unwrap_or(Value::Null);
                assert_eq!(
                    &got, want,
                    "case {case:?}: expected the {what} to answer {member} as {want}, got \
                     {got} in {record}"
                );
            }
        }
    }
}
