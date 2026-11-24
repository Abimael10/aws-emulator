// SPDX-License-Identifier: CC0-1.0

//! What `CreateEventSourceMapping` tells a caller whose `EventSourceArn` names an Amazon SQS
//! queue that does not exist.
//!
//! RULE: a mapping onto a queue is wired up by RECEIVING from the queue its `EventSourceArn`
//! addresses ("Lambda reads items from the event source and invokes the function",
//! `smithy_query(query="operation", operation="CreateEventSourceMapping")`), so what decides
//! such a create is the answer the queue service gives that read rather than the shape of the
//! ARN: a queue the service that owns queues does not hold is refused with the modelled
//! `InvalidParameterValueException` (HTTP 400) whose message propagates that service's own
//! failure — the call Lambda attempted, then the queue service's error code and message
//! verbatim — and no mapping is stored. A source that is no queue is not touched by it, and
//! neither is an emulator that serves no queue service at all: nothing there can answer for
//! the queue, so nothing about it is shown.
//!
//! The rows below differ along what the rule generalises over: whether the named queue was
//! created, which kind of queue it is (standard and FIFO, whose names differ by the suffix
//! Amazon SQS requires of a FIFO queue), how close the name comes to a held queue's, and
//! whether the source is a queue at all.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mappings::{
    create_function, create_mapping, create_queue, create_request, get_mapping, list_mappings,
    refusal_message, router_on, router_with_queues,
};

/// The modelled error a source this mapping could not read is refused with.
const INVALID_PARAMETER: &str = "InvalidParameterValueException";

/// The Region the harness signs for, which the queue ARNs below are written in.
const REGION: &str = "us-east-1";

/// The account id every emulator ARN carries.
const ACCOUNT: &str = "000000000000";

/// The function every row's mapping would invoke.
const FUNCTION: &str = "esm-absent-queue-fn";

/// The whole refusal a create naming a queue nothing holds answers: the call Lambda
/// attempted, then the error code and the sentence Amazon SQS answers a read of an absent
/// queue with (`AWS.SimpleQueueService.NonExistentQueue` / "The specified queue does not
/// exist.", that service's own fixed text for the condition).
///
/// `pub(crate)` because this sentence has ONE owner here: the suite over WHICH refusal such a
/// create answers when the role is short of the source's reads as well reads it from this
/// constant (`tests/all/event_source_mapping_absent_queue_precedence.rs`), so the two tables
/// cannot drift apart.
pub(crate) const ABSENT_QUEUE_REFUSAL: &str = "Error occurred while ReceiveMessage. SQS Error Code: \
                                    AWS.SimpleQueueService.NonExistentQueue. SQS Error \
                                    Message: The specified queue does not exist.";

/// The ARN of the queue `name` in this account and Region, as Amazon SQS writes it.
fn queue_arn(name: &str) -> String {
    format!("arn:aws:sqs:{REGION}:{ACCOUNT}:{name}")
}

/// One row: the queue to create first (if any), the source the create names, and whether the
/// create must be accepted.
struct Row {
    case: &'static str,
    /// The queue to create before the mapping, with the attributes it takes — `None` for a
    /// row whose source is not a queue this suite creates.
    given_queue: Option<(&'static str, Value)>,
    /// The `EventSourceArn` the create names.
    event_source: String,
    /// Whatever else the create has to state for this source.
    extras: Value,
    /// Whether the create must be accepted; a refused row must carry
    /// [`ABSENT_QUEUE_REFUSAL`].
    accepted: bool,
}

/// The rows: see the module docs.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a standard queue the queue service holds",
            given_queue: Some(("esm-absent-queue-held", json!({}))),
            event_source: queue_arn("esm-absent-queue-held"),
            extras: json!({}),
            accepted: true,
        },
        Row {
            case: "a standard queue name nothing was created under",
            given_queue: None,
            event_source: queue_arn("esm-absent-queue-ghost"),
            extras: json!({}),
            accepted: false,
        },
        Row {
            case: "a name that differs from a held queue's by its last character",
            given_queue: None,
            event_source: queue_arn("esm-absent-queue-helds"),
            extras: json!({}),
            accepted: false,
        },
        Row {
            // A FIFO queue's name must end in `.fifo`, and such a queue gathers no batch
            // over a window, so the row states none.
            case: "a FIFO queue the queue service holds",
            given_queue: Some((
                "esm-absent-queue-held.fifo",
                json!({ "FifoQueue": "true" }),
            )),
            event_source: queue_arn("esm-absent-queue-held.fifo"),
            extras: json!({}),
            accepted: true,
        },
        Row {
            case: "a FIFO queue name nothing was created under",
            given_queue: None,
            event_source: queue_arn("esm-absent-queue-ghost.fifo"),
            extras: json!({}),
            accepted: false,
        },
        Row {
            // The boundary: a source that is no queue at all is decided by the rules of its
            // own service, not by a read of a queue.
            case: "a Kinesis data stream, which is no queue",
            given_queue: None,
            event_source: format!(
                "arn:aws:kinesis:{REGION}:{ACCOUNT}:stream/esm-absent-queue-stream"
            ),
            extras: json!({ "StartingPosition": "LATEST" }),
            accepted: true,
        },
    ]
}

/// RULE: a create whose `EventSourceArn` names a queue the queue service does not hold is
/// refused with the modelled 400 propagating that service's own absent-queue failure, and
/// stores no mapping; a queue it does hold, and a source that is no queue, are accepted and
/// stored.
#[tokio::test]
async fn a_create_naming_a_queue_the_queue_service_does_not_hold_is_refused_with_its_failure() {
    let router = router_with_queues();
    create_function(&router, FUNCTION).await;

    let mut stored = 0usize;
    for row in rows() {
        let case = row.case;
        if let Some((name, attributes)) = &row.given_queue {
            let (_url, arn) = create_queue(&router, case, name, attributes).await;
            assert_eq!(
                arn,
                row.event_source,
                "case {case:?}: the queue must report the ARN this row maps, got {arn}"
            );
        }

        let answer = create_mapping(
            &router,
            &create_request(FUNCTION, Some(&row.event_source), &row.extras),
        )
        .await;

        if row.accepted {
            assert_eq!(
                answer.status,
                202,
                "case {case:?}: {} is a source this mapping can read, so the create must \
                 answer 202, got {} {}",
                row.event_source,
                answer.status,
                answer.text()
            );
            let uuid = answer.string("UUID").unwrap_or_else(|| {
                panic!(
                    "case {case:?}: an accepted create must answer a UUID, got {}",
                    answer.text()
                )
            });
            stored += 1;
            // The round trip: the stored mapping polls the source the request named.
            let read = get_mapping(&router, &uuid).await;
            let reported = read.string("EventSourceArn");
            assert_eq!(
                (read.status, reported.as_deref()),
                (200, Some(row.event_source.as_str())),
                "case {case:?}: the created mapping must read back on {}, got {} {}",
                row.event_source,
                read.status,
                read.text()
            );
        } else {
            assert_eq!(
                answer.status,
                400,
                "case {case:?}: {} names a queue nothing holds, so the create must answer \
                 400, got {} {}",
                row.event_source,
                answer.status,
                answer.text()
            );
            let code = answer.error_code();
            assert!(
                code.contains(INVALID_PARAMETER),
                "case {case:?}: the refusal must be {INVALID_PARAMETER}, got {code:?}"
            );
            let message = refusal_message(&answer).unwrap_or_default();
            assert_eq!(
                message, ABSENT_QUEUE_REFUSAL,
                "case {case:?}: expected the refusal {ABSENT_QUEUE_REFUSAL:?}, got {message:?}"
            );
        }

        let listed = list_mappings(&router, "").await;
        let held = listed.mappings().len();
        assert_eq!(
            held, stored,
            "case {case:?}: expected {stored} stored mapping(s) after this row, got {held} in {}",
            listed.text()
        );
    }
}

/// RULE (the other half): an emulator that serves no queue service cannot be asked about the
/// queue, so nothing is shown about it and the create is accepted on its own terms — the same
/// reading every decision this service takes over another service's store makes when that
/// store is not there.
#[tokio::test]
async fn a_create_is_accepted_where_no_queue_service_can_answer_for_the_queue() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router: MultiServiceRouter = router_on(&engine);
    create_function(&router, FUNCTION).await;

    let event_source = queue_arn("esm-absent-queue-unserved");
    let answer = create_mapping(
        &router,
        &create_request(FUNCTION, Some(&event_source), &json!({})),
    )
    .await;
    assert_eq!(
        answer.status,
        202,
        "no queue service can answer for {event_source}, so the create must answer 202, \
         got {} {}",
        answer.status,
        answer.text()
    );
    let uuid = answer
        .string("UUID")
        .unwrap_or_else(|| panic!("the accepted create must answer a UUID, got {}", answer.text()));
    let read = get_mapping(&router, &uuid).await;
    let reported = read.string("EventSourceArn");
    assert_eq!(
        (read.status, reported.as_deref()),
        (200, Some(event_source.as_str())),
        "the created mapping must read back on {event_source}, got {} {}",
        read.status,
        read.text()
    );
}
