// SPDX-License-Identifier: CC0-1.0

//! How `CreateEventSourceMapping` refuses a `StartingPosition` the event source it names
//! cannot be read FROM.
//!
//! The rule under test: `StartingPosition` is "The position in a stream from which to start
//! reading", and which of `EventSourcePosition`'s values a source can be read from belongs to
//! the source — the one restriction the member states is the instant, "AT_TIMESTAMP is
//! supported only for Amazon Kinesis streams, Amazon DocumentDB, Amazon MSK, and self-managed
//! Apache Kafka" (`smithy_query(query="doc", shape="CreateEventSourceMappingRequest",
//! member="StartingPosition")`). A request asking for a position the named ARN's type does not
//! support is refused with the modelled `InvalidParameterValueException` (HTTP 400) whose
//! message is worded about the ARN TYPE and not about the position: the fixed sentence
//! `Unsupported starting position for arn type: ` and then the request's own `EventSourceArn`,
//! bare and verbatim — so the message names no position and quotes nothing — and NOTHING is
//! stored for such a create. A position the named type DOES support is accepted and stored,
//! which is what makes the sentence the answer to the position rather than to the operation.
//!
//! [`crate::event_source_mapping_starting_position_rules`] pins the same support rule over the
//! ACCEPTED half (each source's position read back through `GetEventSourceMapping`); what this
//! suite adds is the other half of a REFUSED create — that the sentence is the request's own
//! ARN with no position named and no quoting, over two ARNs of one refused type, and that the
//! refused request left the function with no mapping at all.
//!
//! The message text is recorded behavior: the model states no message for
//! `InvalidParameterValueException`. Source: the answer the corpus unit
//! `lambda-esm-dynamodb-stream-at-timestamp-starting-position-rejected` carries for a table
//! stream asked to be read from an instant.
//!
//! The rows differ along what the rule generalises over: the KIND of event source named (a
//! table stream, a queue, a broker, a data stream, a Kafka cluster, a change stream, and the
//! cluster named by no ARN at all), the ARN VALUE itself (two table streams of different
//! tables, so the ARN in the message is the request's and not one recorded value), and the
//! POSITION asked for (each of the three values the enum declares).
//!
//! Every row drives a real `CreateEventSourceMapping` through the production
//! `MultiServiceRouter` against a function of its own, and reads back what the call left
//! behind.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mappings::{
    create_function, create_mapping, create_request, given_broker, list_mappings, router_on,
    DYNAMODB_SOURCE, KINESIS_SOURCE, MQ_BROKER_NAME, MSK_SOURCE, QUEUE_SOURCE,
};

/// The modelled error a request no mapping can satisfy is refused with
/// (`InvalidParameterValueException`, HTTP 400).
const INVALID_PARAMETER: &str = "InvalidParameterValueException";

/// A second DynamoDB table stream, of another table and another stream label: the ARN the
/// refusal names is the REQUEST's, so one row is not enough to tell a template from a value.
const OTHER_DYNAMODB_SOURCE: &str =
    "arn:aws:dynamodb:us-east-1:000000000000:table/invoices/stream/2026-02-03T04:05:06.789";

/// An Amazon MQ broker, "Amazon MQ – The ARN of the broker", of the broker this suite creates
/// in the service that owns brokers ([`given_broker`]).
const MQ_SOURCE: &str = "arn:aws:mq:us-east-1:000000000000:broker:order-broker:b-1111";

/// A DocumentDB change stream, "Amazon DocumentDB – The ARN of the DocumentDB change stream",
/// which is an Amazon RDS cluster.
const DOCDB_SOURCE: &str = "arn:aws:rds:us-east-1:000000000000:cluster:order-docdb";

/// The three values `EventSourcePosition` declares (`smithy_query(query="shape",
/// shape="EventSourcePosition")`: `TRIM_HORIZON | LATEST | AT_TIMESTAMP`).
const AT_TIMESTAMP: &str = "AT_TIMESTAMP";
/// See [`AT_TIMESTAMP`].
const LATEST: &str = "LATEST";
/// See [`AT_TIMESTAMP`].
const TRIM_HORIZON: &str = "TRIM_HORIZON";

/// An instant in the past, since "StartingPositionTimestamp cannot be in the future": the rows
/// that ask to be read from an instant are about the POSITION, so the instant they state is
/// one every source would accept.
const PAST_INSTANT: i64 = 1_700_000_000;

/// The whole refusal a position the named ARN's type does not support is answered with: AWS's
/// fixed sentence and the `EventSourceArn` the request sent, bare.
fn unsupported_position_refusal(event_source: &str) -> String {
    format!("Unsupported starting position for arn type: {event_source}")
}

/// One row: the source the mapping would poll, the position it asks for, the members that
/// source needs beside them, and whether the type supports that position.
struct PositionRow {
    case: &'static str,
    /// `EventSourceArn`, or `None` for the self-managed Apache Kafka cluster, which carries
    /// none.
    event_source: Option<&'static str>,
    starting_position: &'static str,
    extras: Value,
    /// Whether the create is accepted; a refused row answers the sentence above over its own
    /// `EventSourceArn`.
    accepted: bool,
}

/// The rows.
fn position_rows() -> Vec<PositionRow> {
    vec![
        PositionRow {
            case: "a table stream read from an instant, which its type does not support",
            event_source: Some(DYNAMODB_SOURCE),
            starting_position: AT_TIMESTAMP,
            extras: json!({ "StartingPositionTimestamp": PAST_INSTANT }),
            accepted: false,
        },
        PositionRow {
            case: "another table's stream read from an instant, so the ARN is the request's",
            event_source: Some(OTHER_DYNAMODB_SOURCE),
            starting_position: AT_TIMESTAMP,
            extras: json!({ "StartingPositionTimestamp": PAST_INSTANT }),
            accepted: false,
        },
        PositionRow {
            case: "a queue read from an instant, a source with no stream to position in",
            event_source: Some(QUEUE_SOURCE),
            starting_position: AT_TIMESTAMP,
            extras: json!({ "StartingPositionTimestamp": PAST_INSTANT }),
            accepted: false,
        },
        PositionRow {
            case: "a broker read from an instant",
            event_source: Some(MQ_SOURCE),
            starting_position: AT_TIMESTAMP,
            extras: json!({
                "Queues": ["order-jobs"],
                "StartingPositionTimestamp": PAST_INSTANT,
            }),
            accepted: false,
        },
        PositionRow {
            case: "a table stream read from the oldest retained records, which it supports",
            event_source: Some(DYNAMODB_SOURCE),
            starting_position: TRIM_HORIZON,
            extras: json!({}),
            accepted: true,
        },
        PositionRow {
            case: "a table stream read from the newest records, which it supports",
            event_source: Some(DYNAMODB_SOURCE),
            starting_position: LATEST,
            extras: json!({}),
            accepted: true,
        },
        PositionRow {
            case: "a data stream read from an instant, one of the sources that supports it",
            event_source: Some(KINESIS_SOURCE),
            starting_position: AT_TIMESTAMP,
            extras: json!({ "StartingPositionTimestamp": PAST_INSTANT }),
            accepted: true,
        },
        PositionRow {
            case: "a kafka cluster read from an instant",
            event_source: Some(MSK_SOURCE),
            starting_position: AT_TIMESTAMP,
            extras: json!({
                "Topics": ["order-events"],
                "StartingPositionTimestamp": PAST_INSTANT,
            }),
            accepted: true,
        },
        PositionRow {
            case: "a change stream read from an instant",
            event_source: Some(DOCDB_SOURCE),
            starting_position: AT_TIMESTAMP,
            extras: json!({
                "DocumentDBEventSourceConfig": { "DatabaseName": "orders" },
                "StartingPositionTimestamp": PAST_INSTANT,
            }),
            accepted: true,
        },
        PositionRow {
            case: "a self-managed kafka cluster read from an instant, named by no ARN at all",
            event_source: None,
            starting_position: AT_TIMESTAMP,
            extras: json!({
                "SelfManagedEventSource": {
                    "Endpoints": { "KAFKA_BOOTSTRAP_SERVERS": ["broker-1.example.com:9092"] },
                },
                "Topics": ["order-events"],
                "StartingPositionTimestamp": PAST_INSTANT,
            }),
            accepted: true,
        },
    ]
}

/// The members one row's request carries beside its source: the position it asks for and
/// whatever that source needs.
fn extras(row: &PositionRow) -> Value {
    let mut extras = row.extras.clone();
    extras["StartingPosition"] = json!(row.starting_position);
    extras
}

/// Lambda and the service that owns brokers, which is where the broker one row maps lives.
fn emulator() -> MultiServiceRouter {
    router_on(&Arc::new(engine::InMemoryEngine::new()))
}

#[tokio::test]
async fn a_position_the_sources_arn_type_does_not_support_is_refused_over_that_arn() {
    let router = emulator();
    // The broker the Amazon MQ row names: a mapping onto a broker is accepted only against one
    // Amazon MQ itself holds, so that row is refused on the POSITION and on nothing else.
    given_broker(&router, MQ_BROKER_NAME).await;

    for (index, row) in position_rows().iter().enumerate() {
        let case = row.case;
        // One function per row: a function reads a given source through at most one mapping,
        // so two rows naming the same source must not be two mappings of one function.
        let function = format!("esm-position-support-{index}");
        create_function(&router, &function).await;

        let answer = create_mapping(
            &router,
            &create_request(&function, row.event_source, &extras(row)),
        )
        .await;
        let listed = list_mappings(&router, &format!("FunctionName={function}")).await;
        let held = listed.mappings().len();

        if row.accepted {
            assert_eq!(
                answer.status,
                202,
                "case {case:?}: this source supports the position asked for, so the create \
                 must be accepted, got {} {}",
                answer.status,
                answer.text()
            );
            assert_eq!(
                held,
                1,
                "case {case:?}: an accepted create stores its mapping, got {} in {}",
                held,
                listed.text()
            );
            continue;
        }

        assert_eq!(
            answer.status,
            400,
            "case {case:?}: this source does not support the position asked for, so the \
             create must answer 400, got {} {}",
            answer.status,
            answer.text()
        );
        let code = answer.error_code();
        assert!(
            code.contains(INVALID_PARAMETER),
            "case {case:?}: the refusal must be {INVALID_PARAMETER}, got {code:?}"
        );
        let expected = unsupported_position_refusal(row.event_source.unwrap_or_default());
        let message = answer.string("message").unwrap_or_default();
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
        assert!(
            !message.contains(row.starting_position) && !message.contains('\''),
            "case {case:?}: the refusal is worded about the arn type, so it names no position \
             and quotes nothing, got {message:?}"
        );
        assert_eq!(
            held,
            0,
            "case {case:?}: a refused create stores no mapping, got {held} in {}",
            listed.text()
        );
    }
}
