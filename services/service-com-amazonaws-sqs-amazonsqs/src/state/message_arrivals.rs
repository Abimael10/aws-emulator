// SPDX-License-Identifier: CC0-1.0

//! WHEN a consumer that is waiting for this service's next message learns that one is
//! there.
//!
//! Amazon SQS hands a waiting consumer a message as soon as the queue has one: "the
//! `WaitTimeSeconds` parameter enables long-poll support ... If a message is available, the
//! call returns sooner than `WaitTimeSeconds`" (`ReceiveMessageRequest$WaitTimeSeconds`). A
//! consumer inside this process cannot be handed the message by that long poll, because it
//! reads the queue through this store rather than over the wire — the Lambda event source
//! mapping's poller is the one such consumer
//! (`service_com_amazonaws_lambda_awsgirapiservice::state::sqs_poller`) — so it has to be
//! told, and this is what tells it: every send SIGNALS, and a consumer waiting on the signal
//! wakes the instant a message is enqueued instead of waiting out a cadence of its own.
//!
//! What the signal says is only "look again": it carries no queue, no message and no count,
//! so a consumer woken by it reads the store exactly as it would have on its own next look,
//! and a consumer that misses one is no worse off than it was — whatever cadence it keeps
//! beside the signal still finds the message. That is why nothing here is state: it is a
//! wake-up, and the messages themselves are the queues' own.
//!
//! EVERY consumer waiting on it is woken, because one store has as many in-process consumers
//! as there are mappings polling its queues and a send is news to each of them; a consumer
//! whose own queue got nothing finds nothing and waits again. A permit is STORED as well
//! (`Notify::notify_one`), so a send that lands while a consumer is mid-pass wakes the pass
//! after it rather than being lost.

use super::*;

/// The signal a consumer waiting for this store's next message waits on.
///
/// An `Arc`, so a consumer holds it without holding the store's lock: the wait happens
/// outside every lock this service takes.
pub type MessageArrivals = ::std::sync::Arc<::tokio::sync::Notify>;

impl ComAmazonawsSqsAmazonsqsState {
    /// Tell every consumer waiting for this store's next message to look again.
    ///
    /// Called by the two operations that put a message on a queue, which is where a message
    /// ARRIVES: `SendMessage` and `SendMessageBatch`. Everything else that makes a message
    /// available again — a visibility timeout lapsing, a redrive, a move task — is owed by
    /// the CLOCK at an instant no call decides, and is found by the cadence a consumer keeps
    /// anyway.
    ///
    /// Both halves of the wake-up: every consumer waiting NOW is woken, since a store may
    /// have one consumer per mapping polling its queues, and a permit is kept for the one
    /// that was mid-pass and waits next.
    pub fn note_message_arrival(&self) {
        self.arrivals.notify_waiters();
        self.arrivals.notify_one();
    }
}

/// The signal of the store `engine` holds, or `None` for an engine that holds none.
///
/// Read once by a consumer that then waits on it many times: the handle does not change.
#[must_use]
pub fn message_arrivals<Eng: Send + Sync + 'static>(engine: &Eng) -> Option<MessageArrivals> {
    let state = service_state(engine)?;
    let arrivals = ::std::sync::Arc::clone(&state.lock().arrivals);
    Some(arrivals)
}

#[cfg(test)]
mod tests {
    use std::future::Future as _;
    use std::sync::Arc;
    use std::task::{Context, Poll, Waker};

    use super::super::ComAmazonawsSqsAmazonsqsState;
    use super::MessageArrivals;

    /// Whether a consumer waiting on `arrivals` has its wake-up NOW: the future is polled
    /// once, so nothing here waits for a span or reads a clock.
    fn woken(arrivals: &MessageArrivals) -> bool {
        let mut waiting = Box::pin(arrivals.notified());
        matches!(
            waiting.as_mut().poll(&mut Context::from_waker(Waker::noop())),
            Poll::Ready(())
        )
    }

    /// RULE: a store that has had no message says nothing, and one a send signalled hands the
    /// waiting consumer its wake-up — whether that consumer was already waiting or arrives
    /// after the signal, which is what keeps a send that lands mid-pass from being lost.
    #[test]
    fn a_signalled_arrival_wakes_the_next_waiter() {
        let state = ComAmazonawsSqsAmazonsqsState::default();
        let arrivals = Arc::clone(&state.arrivals);
        assert!(
            !woken(&arrivals),
            "a store that has had no message must not wake a consumer"
        );
        state.note_message_arrival();
        // The permit the signal stored is there for the consumer that looks next, which is
        // what a send landing while that consumer was mid-pass needs.
        assert!(
            woken(&arrivals),
            "a signalled arrival must wake the consumer that waits next"
        );
        // One arrival is one wake-up: the consumer that took it waits again.
        assert!(
            !woken(&arrivals),
            "the wake-up a consumer took must not be handed out twice"
        );
    }
}
