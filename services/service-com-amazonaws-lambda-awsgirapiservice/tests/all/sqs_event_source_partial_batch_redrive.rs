// SPDX-License-Identifier: CC0-1.0

//! What a client SEES when an event source mapping on an Amazon SQS queue reads its function's
//! report of which messages of a batch failed and that queue carries a `RedrivePolicy`: the
//! messages the report left out are each handed over ONCE and gone, and the one it named keeps
//! coming back until the policy's allowance is spent, at which point Amazon SQS moves exactly
//! that message to the dead-letter queue the policy names.
//!
//! RULE under test, in one sentence: a mapping whose `FunctionResponseTypes` asks for its
//! function's report of which messages of a batch failed deletes exactly the messages that
//! report does NOT name and leaves the named ones to the SOURCE QUEUE's own delivery
//! bookkeeping — so a named message is delivered as many times as that queue's `RedrivePolicy`
//! allows and is then moved to the policy's `deadLetterTargetArn`, while a mapping that asks
//! for no such report takes no notice of the identical response and deletes the whole batch,
//! leaving its dead-letter target empty.
//!
//! What grounds it: `FunctionResponseTypes` is "(Kinesis, DynamoDB Streams, Amazon MSK,
//! self-managed Apache Kafka, and Amazon SQS) A list of current response type enums applied to
//! the event source mapping", whose one value — `smithy_query(query = "shape", shape =
//! "FunctionResponseType")` — is `ReportBatchItemFailures`; a list holding it is what makes the
//! response READ, and a mapping carrying none applies no response type at all. The other half
//! is the queue's: `maxReceiveCount` is "the number of times a message is delivered to the
//! source queue before being moved to the dead-letter queue" (`RedrivePolicy`), and a delivery
//! a mapping's poller makes counts exactly as a client's `ReceiveMessage` does.
//!
//! Every row drives real HTTP requests through the production `MultiServiceRouter` with Lambda,
//! Amazon SQS and CloudWatch Logs on ONE engine, as `core/router` assembles them. Each row is
//! read where a client reads it: the handler's own line per record in the function's log group
//! (so a SIGHTING of a body is a consequence of that record having reached the handler), the
//! bodies the dead-letter queue holds, and the `ApproximateReceiveCount` the dead-lettered
//! message carries — "the number of times a message has been received across all queues but not
//! deleted". A row whose unnamed records were handed over twice, or whose named record never
//! reached the dead-letter queue, fails here.
//!
//! The rows differ along what the rule generalises over: whether the mapping asks for the
//! report at all (the two arms, whose function returns the IDENTICAL response body), how many
//! deliveries the source queue's `RedrivePolicy` allows (one, and the three of the recorded
//! case), how many messages are waiting, how many one batch may hold (the whole batch at once, a
//! ceiling above what is waiting, and one message per batch) and where in the send order the
//! message the handler will not process sits (first, middle, last).
//!
//! What the neighbours pin and this suite does not repeat: `sqs_event_source_handler_run` drives
//! WHICH messages a partial-batch report leaves on the queue across message groups,
//! `event_source_mapping_sqs_delivery` drives the redrive of a batch NOTHING processes, and
//! `sqs_event_source_log_delivery` drives the delivery of a handler's log. What only this one
//! reaches is the two halves TOGETHER: a partial-batch report feeding the source queue's own
//! redrive, and the twin arm in which the same response body is inert.
//!
//! Nothing waits a span of its own choosing: every state a row needs is waited for by looking
//! again until it holds ([`WATCH_BUDGET`] is only the backstop against a chain that never gets
//! there), so no assertion depends on how fast the host runs.
//!
//! The suite RUNS handlers, so it takes this target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]): this emulator serves at most
//! `max_concurrent_runs` invocations at once for the whole process.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde_json::json;
use service_com_amazonaws_lambda_awsgirapiservice::state::execution::{
    HOST_WAIT_CEILING, PERMIT_WAIT,
};
use service_com_amazonaws_lambda_awsgirapiservice::state::REPORT_BATCH_ITEM_FAILURES;

use crate::event_source_mappings::{
    create_queue, create_running_function, given_enabled_mapping, messages_held, queue_held,
    queue_message_counts, send_message, LOOK_AGAIN,
};
// The neighbouring suites own these: the queue reader that reports a message's delivery count
// (`event_source_mapping_sqs_delivery`), and the Lambda + Amazon SQS + CloudWatch Logs harness
// with its reader of a function's delivered log (`sqs_event_source_log_delivery`), so nothing of
// either is written out a second time here.
use crate::event_source_mapping_sqs_delivery::receive;
use crate::sqs_event_source_log_delivery::{emulator, logged_lines};

/// How long every source queue below hides a delivered message for, and the `Timeout` of the
/// function it maps onto: a queue only maps onto a function it outlasts, so the two are one
/// span, and a message no attempt processed comes back one such span after the attempt that
/// left it. It is the span the neighbouring suite over the same redrive uses
/// (`sqs_event_source_revoked_downstream_grant`): well above what a handler printing a line per
/// record needs even on a host running every other suite of the workspace beside it, so an
/// attempt is never cut short by it, and short enough that a row waits seconds rather than
/// minutes for the redeliveries its `RedrivePolicy` allows.
const VISIBILITY_TIMEOUT: i64 = 10;

/// The marker a message body carries to be one the handler reports as unprocessed. Nothing in
/// the emulator reads it: the handler reads it out of the record's own body.
const POISON: &str = "poison";

/// What the handler puts around the body of each record it was handed, so one line of the
/// function's log is one SIGHTING of that body and a body that is a prefix of another cannot be
/// counted for it. ONE spelling of it, bound into the handler's source and into the counting
/// below.
const FENCE: &str = "|record|";

/// How long a row watches for the state the clock owes it — the mapping's passes, the
/// invocations, the handler's log reaching the group, the redeliveries the source queue's
/// `RedrivePolicy` allows and the move to the dead-letter queue — before it is a failure.
///
/// PATIENCE, not a weakened expectation: every assertion below is unchanged. The bound is taken
/// from what the EMULATOR itself allows ONE attempt rather than picked here — the wait for a
/// share of the run capacity ([`PERMIT_WAIT`]) and the wait for the run's own answer
/// ([`HOST_WAIT_CEILING`]) — with room for the several deliveries a row waits for, because a
/// whole-workspace run shares its cores with every other suite.
const WATCH_BUDGET: Duration = PERMIT_WAIT
    .saturating_add(HOST_WAIT_CEILING)
    .saturating_add(Duration::from_secs(90));

/// How long a message this suite takes off a dead-letter queue stays hidden for, so one read
/// cannot be handed the same message twice.
const READER_HOLD_SECONDS: i64 = 300;

/// The handler every row runs: it prints a fenced line per record of the batch it was handed,
/// taken from that record's own body, and reports every record whose body carries the marker as
/// one it did not process — naming it by the `messageId` of the record it was handed, which is
/// the identifier a batch item failure report names a message by.
///
/// The response body is the SAME whichever arm the row is: what differs is whether the mapping
/// asked for it to be read.
fn reporting_handler() -> String {
    format!(
        r#"
FENCE = "{FENCE}"
POISON = "{POISON}"


def lambda_handler(event, context):
    records = event["Records"]
    if not records:
        raise ValueError("a batch was handed over holding no record")
    failures = []
    for record in records:
        if record["eventSource"] != "aws:sqs":
            raise ValueError("eventSource is " + repr(record["eventSource"]))
        if not record["messageId"]:
            raise ValueError("a record carries no messageId")
        print(FENCE + record["body"] + FENCE)
        if POISON in record["body"]:
            failures.append({{"itemIdentifier": record["messageId"]}})
    return {{"batchItemFailures": failures}}
"#
    )
}

/// The line the handler writes when it is handed the message whose body is `body`.
fn sighting(body: &str) -> String {
    format!("{FENCE}{body}{FENCE}")
}

/// How many times each body of `bodies` was handed to the handler, counted from the function's
/// own log lines.
fn sightings(lines: &[String], bodies: &[&str]) -> BTreeMap<String, usize> {
    bodies
        .iter()
        .map(|body| {
            let wanted = sighting(body);
            let seen = lines
                .iter()
                .filter(|line| line.contains(&wanted))
                .count();
            ((*body).to_string(), seen)
        })
        .collect()
}

/// One row: whether the mapping asks for the report, what the source queue's `RedrivePolicy`
/// allows, how many messages one batch may hold and the bodies waiting on the queue — exactly
/// one of which carries [`POISON`].
struct Row {
    case: &'static str,
    /// The suffix every resource of this row is named with, so the rows share no queue,
    /// function or log group.
    slug: &'static str,
    /// Whether the mapping carries `FunctionResponseTypes: [ReportBatchItemFailures]`.
    reports_failures: bool,
    /// "The number of times a message is delivered to the source queue before being moved to
    /// the dead-letter queue" (`RedrivePolicy`).
    max_receive_count: i64,
    /// `BatchSize`: "the maximum number of records in each batch that Lambda pulls from your
    /// stream or queue".
    batch_size: i64,
    /// The bodies sent to the source queue, in order.
    sent: &'static [&'static str],
}

impl Row {
    /// The one body of the row the handler will report as unprocessed.
    fn poison(&self) -> &'static str {
        self.sent
            .iter()
            .copied()
            .find(|body| body.contains(POISON))
            .unwrap_or_else(|| panic!("case {:?}: a row sends exactly one poison body", self.case))
    }

    /// How many messages the dead-letter queue has to end up holding: the one message the
    /// report named when the mapping asked for the report, and none at all when it did not.
    fn dead_lettered(&self) -> u32 {
        u32::from(self.reports_failures)
    }

    /// How many times each body has to be handed to the handler: once for a message the report
    /// leaves out, and — only in the arm that asks for the report — as many times as the source
    /// queue's `RedrivePolicy` allows for the one it names, each of those deliveries being a
    /// delivery the policy counts.
    fn expected_sightings(&self) -> BTreeMap<String, usize> {
        let deliveries = usize::try_from(self.max_receive_count).unwrap_or(usize::MAX);
        self.sent
            .iter()
            .map(|body| {
                let seen = if self.reports_failures && body.contains(POISON) {
                    deliveries
                } else {
                    1
                };
                ((*body).to_string(), seen)
            })
            .collect()
    }
}

/// The catalog: the recorded shape (four messages processed, one not) and its twin arm without
/// the response type, a row whose named message is redelivered until the recorded allowance of
/// three deliveries is spent, and a row read one message per batch. Every row runs a real
/// interpreter and waits out the redeliveries its policy allows, so the table spans the class
/// with as few of them as the class takes.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the report read, one message of five not processed, one delivery allowed",
            slug: "read-one",
            reports_failures: true,
            max_receive_count: 1,
            batch_size: 5,
            sent: &[
                "alpha-ok",
                "bravo-ok",
                "charlie-ok",
                "delta-ok",
                "echo-poison",
            ],
        },
        Row {
            case: "the identical response inert with no response type, three deliveries allowed",
            slug: "inert-three",
            reports_failures: false,
            max_receive_count: 3,
            batch_size: 5,
            sent: &[
                "alpha-ok",
                "bravo-ok",
                "charlie-ok",
                "delta-ok",
                "echo-poison",
            ],
        },
        Row {
            case: "the report read, the named message first in the batch, three deliveries \
                   allowed",
            slug: "read-three",
            reports_failures: true,
            max_receive_count: 3,
            batch_size: 10,
            sent: &["foxtrot-poison", "golf-ok"],
        },
        Row {
            case: "the report read, one message per batch, one delivery allowed",
            slug: "read-one-per-batch",
            reports_failures: true,
            max_receive_count: 1,
            batch_size: 1,
            sent: &["hotel-ok", "india-poison", "juliett-ok"],
        },
    ]
}

/// RULE: the messages a partial-batch report does not name are deleted after ONE delivery, and
/// the one it names is delivered as many times as the source queue's `RedrivePolicy` allows and
/// then moved to the dead-letter queue that policy names — while a mapping asking for no such
/// report takes no notice of the same response and deletes the whole batch.
#[tokio::test]
async fn a_reported_message_is_redelivered_until_its_queue_dead_letters_it() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    for row in rows() {
        let case = row.case;
        let router = emulator();
        let function = format!("esm-partial-redrive-{}", row.slug);
        // The dead-letter queue first: the source queue's policy names it, so it has to exist
        // before that policy is accepted.
        let (dlq_url, dlq_arn) = create_queue(
            &router,
            case,
            &format!("esm-partial-redrive-{}-dlq", row.slug),
            &json!({}),
        )
        .await;
        let (source_url, source_arn) = create_queue(
            &router,
            case,
            &format!("esm-partial-redrive-{}-source", row.slug),
            &json!({
                "VisibilityTimeout": VISIBILITY_TIMEOUT.to_string(),
                "RedrivePolicy": json!({
                    "deadLetterTargetArn": dlq_arn,
                    "maxReceiveCount": row.max_receive_count,
                })
                .to_string(),
            }),
        )
        .await;
        create_running_function(
            &router,
            case,
            &function,
            &reporting_handler(),
            VISIBILITY_TIMEOUT,
            &json!({}),
        )
        .await;

        let mut request = json!({
            "FunctionName": function,
            "EventSourceArn": source_arn,
            "BatchSize": row.batch_size,
            "Enabled": true,
        });
        if row.reports_failures {
            request["FunctionResponseTypes"] = json!([REPORT_BATCH_ITEM_FAILURES]);
        }
        let _uuid = given_enabled_mapping(&router, case, &request).await;

        for body in row.sent {
            send_message(&router, case, &source_url, body, None).await;
        }

        // The whole chain, waited for as ONE state: the source queue lets every message go —
        // the ones the report left out because the mapping deleted them, the one it named
        // because its queue moved it — the dead-letter queue holds what the policy owed it, and
        // the handler's own line for every delivery has reached the function's log group.
        let group = format!("/aws/lambda/{function}");
        let expected_sightings = row.expected_sightings();
        let deadline = Instant::now() + WATCH_BUDGET;
        let (source_counts, lines) = loop {
            let lines = logged_lines(&router, case, &group).await;
            let source_counts = queue_message_counts(&router, case, &source_url).await;
            let dead_lettered = queue_held(&router, case, &dlq_url).await;
            let seen = sightings(&lines, row.sent);
            let settled = messages_held(source_counts) == 0
                && dead_lettered == row.dead_lettered()
                && expected_sightings
                    .iter()
                    .all(|(body, wanted)| seen.get(body).is_some_and(|count| count >= wanted));
            if settled {
                // Both queues have settled, so no further delivery can be made and the log is
                // final: it is read once MORE here rather than kept from the look above, which
                // was made before the counts this break rests on.
                break (source_counts, logged_lines(&router, case, &group).await);
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: expected the source queue to let every message go, the \
                 dead-letter queue to hold {} and the handler to have been handed \
                 {expected_sightings:?}; the source holds {source_counts:?} as (visible, in \
                 flight, delayed), the dead-letter queue holds {dead_lettered} and the log holds \
                 {lines:?}",
                row.dead_lettered()
            );
            tokio::time::sleep(LOOK_AGAIN).await;
        };

        // Every body was handed over exactly as many times as the rule owes it — once for a
        // message the report left out, and the policy's whole allowance for the one it named.
        let seen = sightings(&lines, row.sent);
        assert_eq!(
            seen, expected_sightings,
            "case {case:?}: each message must reach the handler exactly as many times as the \
             rule owes it, got {seen:?} from {lines:?}"
        );
        assert_eq!(
            messages_held(source_counts),
            0,
            "case {case:?}: the source queue must hold nothing once the report's messages were \
             deleted and the named one moved, got {source_counts:?} as (visible, in flight, \
             delayed)"
        );

        // And what the dead-letter queue holds is the one message the report named, body for
        // body, with the deliveries its source queue made counted on it — or nothing at all in
        // the arm whose mapping never asked for the report.
        let dead_letters = receive(&router, case, &dlq_url, READER_HOLD_SECONDS).await;
        let bodies: Vec<&str> = dead_letters
            .iter()
            .map(|(body, _)| body.as_str())
            .collect();
        if row.reports_failures {
            assert_eq!(
                bodies,
                vec![row.poison()],
                "case {case:?}: the dead-letter queue must hold exactly the message the report \
                 named, got {dead_letters:?}"
            );
            // "The number of times a message is delivered to the source queue before being
            // moved to the dead-letter queue": the source queue made exactly that many
            // deliveries, and the receive above is one more —
            // `ApproximateReceiveCount` is "the number of times a message has been received
            // across all queues but not deleted".
            let expected_count =
                u32::try_from(row.max_receive_count).unwrap_or(u32::MAX).saturating_add(1);
            for (body, count) in &dead_letters {
                assert_eq!(
                    *count, expected_count,
                    "case {case:?}: the message {body:?} must have been delivered {} time(s) by \
                     the source queue before the move, got a receive count of {count} on the \
                     dead-letter queue",
                    row.max_receive_count
                );
            }
        } else {
            assert!(
                dead_letters.is_empty(),
                "case {case:?}: a mapping that asked for no response type deletes the whole \
                 batch, so nothing can be dead-lettered, got {dead_letters:?}"
            );
        }
    }
}
