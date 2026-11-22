// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for how an Amazon SQS event source mapping's
//! `ScalingConfig$MaximumConcurrency` paces the WHOLE drain of a backlog: not only that the
//! mapping stays under the cap, but that it fills the cap again for every wave the backlog
//! still has left.
//!
//! RULE: `ScalingConfig$MaximumConcurrency` — "limits the number of concurrent instances that
//! the Amazon SQS event source can invoke" (`smithy_query(query="shape",
//! shape="ScalingConfig")`, `@range(min: 2, max: 1000)` on `smithy_query(query="shape",
//! shape="MaximumConcurrency")`) — is a ceiling the mapping SCALES UP TO while a backlog
//! waits, and it keeps scaling up to it until the backlog is gone: a mapping of batch size one
//! runs as many invocations at once as the cap admits, or as many as the backlog still holds
//! when that is fewer, so a backlog of M messages is consumed in ceil(M/N) waves of invocations
//! rather than trickling one at a time after the first wave — and every message is invoked
//! exactly once. The ceiling itself, and what the function's own reservation bounds it to, are
//! [`crate::sqs_event_source_maximum_concurrency_drain`]'s and
//! [`crate::event_source_mapping_concurrency_ceiling`]'s rules; this suite pins the PACE the
//! same cap sets, which is what a client sizing it for a drain rate is asking for.
//!
//! Each row drives real HTTP requests through one router carrying Lambda and Amazon SQS on ONE
//! engine, and observes the invocations the way an independent client would: the handler
//! reports every record it was handed on a side-channel queue the moment it starts, and then
//! stays in its invocation until this suite releases it — on a release queue OF ITS OWN RECORD,
//! so a release reaches the one invocation it is addressed to and no other. So the waves are
//! the suite's to step through: while nothing is released, every invocation the mapping started
//! is still running, and the records waiting on the side channel are exactly those invocations;
//! released as a whole wave, the mapping has to fill the cap again from what is left.
//!
//! The rows differ along what the rule generalises over: the concurrency configured (the
//! published minimum of `MaximumConcurrency` and a value inside its range), how deep the
//! backlog is, whether it divides into whole waves, and whether the cap is deeper than the
//! backlog itself — in which case the backlog bounds the wave.
//!
//! Nothing here waits a span of its own and nothing reads the wall clock: every wait looks
//! again until what it waits for is there or a generous deadline passes, the cap is asserted at
//! every one of those looks, and the one window that asserts something did NOT happen — that no
//! further invocation starts while a whole wave is still running — is a poll loop over that
//! window. A backlog of M messages therefore cannot be consumed in more than ceil(M/N) waves:
//! every wave is seen holding exactly as many invocations as the cap admits, or as many as the
//! backlog still holds, and nothing beyond them.

use std::collections::BTreeMap;
use std::time::Instant;

use serde_json::{json, Map, Value};

use crate::event_source_mappings::{
    create_queue, create_running_function, given_enabled_mapping, queue_held, router_with_queues,
    send_message, taken_bodies, LOOK_AGAIN,
};
use crate::invoke_request_id::one_invocation_stream_at_a_time;
// The spans and the side-channel entry of the suite over the same cap's CEILING, shared rather
// than copied: the function's `Timeout` paired with the source queue's `VisibilityTimeout`, the
// ceiling on patience taken from what this emulator allows one attempt, the environment entry
// the handler reports on, and the body a release carries.
use crate::sqs_event_source_maximum_concurrency_drain::{
    BUDGET, QUIET_WINDOW, RELEASE, STARTED_QUEUE_URL, TIMEOUT_SECONDS,
};

/// The environment entry the handler below reads its per-record release queues from: a JSON
/// object keyed by the record body, which is how each invocation finds the queue its OWN
/// release arrives on.
const RELEASE_QUEUE_URLS: &str = "RELEASE_QUEUE_URLS";

/// The handler every row runs: it reports each record it was handed on the side-channel queue
/// and then stays in its invocation, polling the release queue of the record it is holding
/// until this suite sends a message there, which it deletes so the release frees nothing else.
///
/// Nothing here sleeps for a span: the handler waits on a QUEUE, and the suite decides when
/// that wait ends.
const HANDLER_SOURCE: &str = r#"
import json
import os

import boto3


def lambda_handler(event, context):
    client = boto3.client("sqs")
    started = os.environ["STARTED_QUEUE_URL"]
    releases = json.loads(os.environ["RELEASE_QUEUE_URLS"])
    bodies = [record["body"] for record in event["Records"]]
    for body in bodies:
        client.send_message(QueueUrl=started, MessageBody=body)
    url = releases[bodies[0]]
    while True:
        answered = client.receive_message(
            QueueUrl=url, MaxNumberOfMessages=1, WaitTimeSeconds=1
        )
        held = answered.get("Messages") or []
        if held:
            client.delete_message(QueueUrl=url, ReceiptHandle=held[0]["ReceiptHandle"])
            return {"records": len(bodies)}
"#;

/// One row: what the mapping configures, how deep the backlog is, and how many waves of
/// invocations that pairing comes to.
struct Row {
    case: &'static str,
    /// The prefix every resource of the row is named from.
    name: &'static str,
    /// `ScalingConfig$MaximumConcurrency`.
    maximum_concurrency: i64,
    /// How many messages are waiting before anything polls the queue.
    backlog: usize,
}

impl Row {
    /// The cap as a count of invocations.
    fn cap(&self) -> usize {
        usize::try_from(self.maximum_concurrency).unwrap_or(usize::MAX)
    }
}

/// The catalog: the published minimum of `MaximumConcurrency` over a backlog of three whole
/// waves, a concurrency inside the published range over a backlog whose last wave is a partial
/// one, and a concurrency deeper than the backlog — where the backlog is what bounds the wave.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the published minimum concurrency over three whole waves",
            name: "esm-wave-minimum",
            maximum_concurrency: 2,
            backlog: 6,
        },
        Row {
            case: "a concurrency inside the published range with a partial last wave",
            name: "esm-wave-partial",
            maximum_concurrency: 3,
            backlog: 5,
        },
        Row {
            case: "a concurrency deeper than the backlog",
            name: "esm-wave-shallow",
            maximum_concurrency: 4,
            backlog: 3,
        },
    ]
}

/// RULE: a mapping fills its `ScalingConfig$MaximumConcurrency` again for every wave its
/// backlog still has left — so an M-message backlog is consumed in ceil(M/N) waves, each of as
/// many invocations as the cap admits or as the backlog still holds, and every message is
/// invoked exactly once.
#[tokio::test]
async fn a_capped_mapping_fills_its_concurrency_again_for_every_wave_of_the_backlog() {
    // This emulator serves at most `state::execution::MAX_CONCURRENT_RUNS` invocations at once
    // for the whole process, and the rows below keep several of those slots held while they
    // observe them: one stream of invocations, as every suite of this target that runs handlers
    // for these rules takes.
    let _stream = one_invocation_stream_at_a_time().await;

    for row in rows() {
        let case = row.case;
        let cap = row.cap();
        let router = router_with_queues();
        let (started_url, _) =
            create_queue(&router, case, &format!("{}-started", row.name), &json!({})).await;
        // The backlog, and one release queue per message of it: a release addressed to a record
        // frees the one invocation holding that record, so a wave is released as a whole and
        // never partly by a message another invocation happened to read.
        let expected: Vec<String> = (0..row.backlog)
            .map(|index| format!("record-{index}"))
            .collect();
        let mut release_urls: BTreeMap<String, String> = BTreeMap::new();
        for body in &expected {
            let (url, _) = create_queue(
                &router,
                case,
                &format!("{}-release-{body}", row.name),
                &json!({}),
            )
            .await;
            release_urls.insert(body.clone(), url);
        }
        let announced: Map<String, Value> = release_urls
            .iter()
            .map(|(body, url)| (body.clone(), json!(url)))
            .collect();
        let (source_url, source_arn) = create_queue(
            &router,
            case,
            &format!("{}-source", row.name),
            &json!({ "VisibilityTimeout": TIMEOUT_SECONDS.to_string() }),
        )
        .await;
        let function = format!("{}-fn", row.name);
        create_running_function(
            &router,
            case,
            &function,
            HANDLER_SOURCE,
            TIMEOUT_SECONDS,
            &json!({
                STARTED_QUEUE_URL: started_url,
                RELEASE_QUEUE_URLS: Value::Object(announced).to_string(),
            }),
        )
        .await;

        // The whole backlog is waiting BEFORE anything polls it, which is what makes how many
        // invocations each wave runs the mapping's own decision rather than a question of how
        // fast the messages arrived.
        for body in &expected {
            send_message(&router, case, &source_url, body, None).await;
        }
        given_enabled_mapping(
            &router,
            case,
            &json!({
                "FunctionName": function,
                "EventSourceArn": source_arn,
                "BatchSize": 1,
                "Enabled": true,
                "ScalingConfig": { "MaximumConcurrency": row.maximum_concurrency },
            }),
        )
        .await;

        // The drain, wave by wave: every invocation that has reported is still running, so the
        // records that have arrived past the ones this suite released are the invocations in
        // flight. A wave is done when the cap is full — or when what is left of the backlog is.
        let mut invoked: Vec<String> = Vec::new();
        let mut released = 0usize;
        let mut waves = 0usize;
        while released < row.backlog {
            let wave = cap.min(row.backlog - released);
            let deadline = Instant::now() + BUDGET;
            loop {
                invoked.extend(taken_bodies(&router, case, &started_url).await);
                let running = invoked.len() - released;
                assert!(
                    running <= cap,
                    "case {case:?}: at most {cap} invocation(s) may run at once, and wave \
                     {waves} was seen running {running} ({invoked:?})"
                );
                if running == wave {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "case {case:?}: wave {waves} of the backlog of {} must run {wave} \
                     invocation(s) at once, got {running} ({invoked:?})",
                    row.backlog
                );
                tokio::time::sleep(LOOK_AGAIN).await;
            }
            // The wave is FULL and every one of its invocations is still running, so nothing
            // the mapping may start is left to start: a further invocation inside this window
            // would be one past what the cap admits of a wave.
            let quiet_until = Instant::now() + QUIET_WINDOW;
            while Instant::now() < quiet_until {
                invoked.extend(taken_bodies(&router, case, &started_url).await);
                let running = invoked.len() - released;
                assert_eq!(
                    running, wave,
                    "case {case:?}: wave {waves} must hold {wave} invocation(s) while every one \
                     of them is still running, got {running} ({invoked:?})"
                );
                tokio::time::sleep(LOOK_AGAIN).await;
            }
            // This wave, released as a whole: each of its invocations on its own queue.
            for body in &invoked[released..] {
                let url = release_urls
                    .get(body)
                    .unwrap_or_else(|| panic!("case {case:?}: an unexpected record {body:?}"));
                send_message(&router, case, url, RELEASE, None).await;
            }
            released = invoked.len();
            waves = waves.saturating_add(1);
        }

        // And the backlog really is gone: every message was invoked, each of them exactly once,
        // and the source queue holds nothing.
        let deadline = Instant::now() + BUDGET;
        loop {
            let held = queue_held(&router, case, &source_url).await;
            if held == 0 {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: the drained backlog must leave the source queue empty, got \
                 {held} message(s) still held"
            );
            tokio::time::sleep(LOOK_AGAIN).await;
        }
        invoked.sort();
        assert_eq!(
            invoked, expected,
            "case {case:?}: expected every message of the backlog to be invoked exactly once, \
             got {invoked:?}"
        );
    }
}
