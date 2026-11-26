// SPDX-License-Identifier: CC0-1.0

//! A long-polling `ReceiveMessage` does not hold the emulator: every other call is
//! answered while that poll is still waiting.
//!
//! The RULE under test:
//!
//! > A receive that waits — "the duration (in seconds) for which the call waits for a
//! > message to arrive in the queue before returning" — holds nothing but its own
//! > request open. Amazon SQS serves every other caller while one consumer long-polls
//! > an empty queue, so a client that watches a queue with the long poll the service
//! > documents for it does not stop the work it is waiting FOR: the publisher that
//! > fills that queue, the service whose own clock owes the event, and the client's
//! > other calls all run while the poll waits.
//!
//! This is the configuration a client watching a queue is in, and it is the one that
//! cannot be seen by a suite whose calls are made one after another: a poll that held
//! the emulator would answer each of them eventually, so only a call made WHILE the
//! poll waits says whether anything was blocked. The leg therefore keeps calling for
//! as long as one poll waits and counts what was answered — nothing is asserted about
//! how LONG anything took, only that the emulator went on serving.
//!
//! Starting the built binary is what makes the answer about the emulator a client
//! runs rather than about a router a test assembles on a runtime of its own choosing.

use std::time::Duration;

use serde_json::{json, Value};

use crate::cfn_deployed_pipeline_on_a_started_emulator::Started;

/// The longest a receive may be asked to wait: "values range: 0 to 20 seconds"
/// (`ReceiveMessageRequest$WaitTimeSeconds`). The poll below asks for the whole of
/// it, which is what a client watching a queue it cannot predict an arrival on asks
/// for.
const MAX_LONG_POLL: u32 = 20;

/// How many calls must be answered while one poll waits.
///
/// Far below what a span of [`MAX_LONG_POLL`] seconds admits — each call below is a
/// local request answered from memory — so a loaded or slow host clears it many times
/// over; an emulator a waiting poll HELD would answer one, when the poll let go.
const ANSWERED_WHILE_POLLING: usize = 10;

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

fn sqs(emulator: &Started, operation: &str, body: &Value) -> (u16, Value) {
    rpc(emulator, "sqs", "AmazonSQS", "1.0", operation, body)
}

/// RULE: the emulator answers every other call while one consumer long-polls an empty
/// queue.
#[test]
fn every_other_call_is_answered_while_a_long_poll_waits() {
    let emulator = Started::start();
    let (status, created) = sqs(
        &emulator,
        "CreateQueue",
        &json!({"QueueName": "long-poll-watched"}),
    );
    assert_eq!(
        status, 200,
        "the watched queue must be created, got {status} {created}"
    );
    let watched = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("no QueueUrl in {created}"))
        .to_string();

    let answered = std::thread::scope(|scope| {
        // One consumer watching an empty queue with the long poll the service
        // documents for it. Nothing is ever sent to that queue, so the poll waits out
        // its whole span.
        let polling = scope.spawn(|| {
            sqs(
                &emulator,
                "ReceiveMessage",
                &json!({
                    "QueueUrl": &watched,
                    "MaxNumberOfMessages": 10,
                    "WaitTimeSeconds": MAX_LONG_POLL,
                }),
            )
        });
        // Everything another caller asks for while that poll waits. The loop ends
        // when the poll does, so every call counted was made with it in flight.
        let mut answered = 0usize;
        while !polling.is_finished() {
            let (status, listed) = sqs(&emulator, "ListQueues", &json!({}));
            assert_eq!(
                status, 200,
                "a call made while a long poll waits must be answered, got {status} {listed}"
            );
            answered += 1;
            // A beat between calls, so the loop reports what the emulator served
            // rather than how fast this thread can ask.
            std::thread::sleep(Duration::from_millis(10));
        }
        let (status, received) = polling.join().expect("the polling thread runs to completion");
        assert_eq!(
            status, 200,
            "the long poll itself must be answered, got {status} {received}"
        );
        assert_eq!(
            received["Messages"],
            Value::Null,
            "nothing was ever sent to the watched queue, so the poll must answer empty, got \
             {received}"
        );
        answered
    });

    assert!(
        answered >= ANSWERED_WHILE_POLLING,
        "expected at least {ANSWERED_WHILE_POLLING} calls to be answered while one consumer \
         long-polled an empty queue for {MAX_LONG_POLL}s, got {answered}\n\nthe emulator's \
         log:\n{}",
        emulator.log()
    );
}
