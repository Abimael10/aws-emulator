// SPDX-License-Identifier: CC0-1.0

//! What a SEND tells the consumer an event source mapping's poller IS: the wiring from Amazon
//! SQS's own `SendMessage` and `SendMessageBatch` to the signal that poller waits on.
//!
//! Amazon SQS hands a waiting consumer a message as soon as the queue has one: "the
//! `WaitTimeSeconds` parameter enables long-poll support ... If a message is available, the call
//! returns sooner than `WaitTimeSeconds`" (`ReceiveMessageRequest$WaitTimeSeconds`). A mapping's
//! poller reads the queue through the shared store rather than over the wire, so it cannot be
//! handed the message by that long poll — the store tells it instead
//! ([`service_com_amazonaws_sqs_amazonsqs::state::message_arrivals`]), and that is what makes a
//! message a client sends be taken by the pass that follows it rather than by whichever pass the
//! poller's own interval happens to make next.
//!
//! RULE: a store no send has reached says nothing, and every call that puts a message on one of
//! its queues wakes the consumer that waits next — whether that consumer was already waiting or
//! arrives after the send, which is what keeps a send that lands while a pass is running from
//! being lost.
//!
//! The cases differ along what the call is: `SendMessage`, and `SendMessageBatch` with more than
//! one entry (one wake-up covers the batch, because the consumer reads the queue itself rather
//! than anything the signal carries). Nothing here waits for a span or reads a clock: the wait
//! is polled ONCE, so an assertion says whether the wake-up is there and never how long it took.

use std::future::Future as _;
use std::task::{Context, Poll, Waker};

use serde_json::json;
use service_com_amazonaws_sqs_amazonsqs::state::{message_arrivals, MessageArrivals};

use crate::event_source_mappings::{call_sqs, create_queue, driven_router_with_queues};

/// Whether the consumer waiting on `arrivals` has its wake-up NOW.
fn woken(arrivals: &MessageArrivals) -> bool {
    let mut waiting = Box::pin(arrivals.notified());
    matches!(
        waiting.as_mut().poll(&mut Context::from_waker(Waker::noop())),
        Poll::Ready(())
    )
}

#[tokio::test]
async fn every_send_wakes_the_consumer_waiting_for_the_next_message() {
    let case = "the queue a mapping's poller would read";
    // The poll claim is taken by the harness, so no background loop competes for the wake-ups
    // this suite reads.
    let (router, driven) = driven_router_with_queues();
    let arrivals =
        message_arrivals(&**driven.engine()).expect("the engine holds the Amazon SQS store");

    // A queue exists and nothing has been sent: there is no message to hand over, so the
    // consumer is not woken.
    let (url, _) = create_queue(&router, case, "arrival-signal-queue", &json!({})).await;
    assert!(
        !woken(&arrivals),
        "case \"no send yet\": a store no send has reached must not wake a consumer"
    );

    // One `SendMessage`: the consumer that waits next is woken — which is the consumer that was
    // mid-pass when the send landed.
    let (status, sent) = call_sqs(
        &router,
        "SendMessage",
        &json!({ "QueueUrl": url, "MessageBody": "one" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"SendMessage\": expected HTTP 200, got {status} {sent}"
    );
    assert!(
        woken(&arrivals),
        "case \"SendMessage\": the send must wake the consumer waiting for the next message"
    );
    assert!(
        !woken(&arrivals),
        "case \"SendMessage\": the wake-up the consumer took must not be handed out twice"
    );

    // And a batch: one wake-up covers every entry it enqueued.
    let (status, batched) = call_sqs(
        &router,
        "SendMessageBatch",
        &json!({
            "QueueUrl": url,
            "Entries": [
                { "Id": "a", "MessageBody": "two" },
                { "Id": "b", "MessageBody": "three" },
            ],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"SendMessageBatch\": expected HTTP 200, got {status} {batched}"
    );
    assert!(
        woken(&arrivals),
        "case \"SendMessageBatch\": the batch must wake the consumer waiting for the next message"
    );
}
