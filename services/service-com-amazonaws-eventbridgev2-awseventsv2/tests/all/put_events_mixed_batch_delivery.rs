// SPDX-License-Identifier: CC0-1.0

//! A batch publish in which SOME entries are refused still delivers the entries
//! it accepted.
//!
//! RULE: a batch publish decides acceptance ENTRY BY ENTRY, so an entry whose
//! own result carries `SuccessCode` `PUBLISHED` is on the bus and is delivered
//! to every subscriber the bus owes it, exactly as if it had been published
//! alone, whatever any OTHER entry of the same request did; an entry that breaks
//! a constraint its own members document populates only its own result position
//! — an `ErrorCode` and `ErrorMessage` and neither an `EventId` nor a
//! `SuccessCode` — is counted in `FailedEntryCount` and is delivered to nobody.
//! Both publish operations of this service, `PutEvents` and `PutRawEvents`, are
//! held to it.
//!
//! The rows span that class rather than one recorded case: they differ along the
//! REASON an entry is refused (a reserved `Source` prefix, a `Detail` that is
//! not the JSON object the member documents, data that is not the form its own
//! `ContentType` declares, an open-format entry with no schema registry), along
//! the POSITION of the refused entries in the batch (first, last, in the middle,
//! several of two kinds), along the NUMBER of accepted entries beside them, and
//! along whether the subscriber had already been delivered an earlier publish.
//!
//! The harness is [`crate::sns_subscriber_delivery`]'s: the same router with
//! EventBridge V2, Amazon SQS, Amazon SNS and IAM on ONE engine, so a delivery
//! is asserted where a client observes it — in the messages the subscriber's
//! target queue holds, read through the pass that also fails on a message the
//! rule does not allow. Nothing here waits on the wall clock: a publish performs
//! the deliveries it has made due before it answers, and the one wait — for the
//! bus's own provisioning transition — polls for that CONDITION under a generous
//! deadline.

use serde_json::json;

use engine::InMemoryEngine;
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_eventbridgev2_awseventsv2 as eventsv2;

use eventsv2::ops::put_events::PutEvents;
use eventsv2::ops::put_raw_events::PutRawEvents;
use eventsv2::ops::types::{
    PutEventsInput, PutEventsOutput, PutEventsRequestEntry, PutRawEventsInput, PutRawEventsOutput,
    PutRawEventsRequestEntry, PutRawEventsSystemMetadata,
};

use crate::sns_subscriber_delivery::{
    document, emulator, eventsv2_ok, given_active_bus, given_queue, given_role, given_subscriber,
    messages_on, Wanted, SEND_MESSAGE_ACTION, SERVICE_PRINCIPAL,
};

/// A `Source` no entry of this suite is refused for.
const ACCEPTED_SOURCE: &str = "orders.service";

/// A `Source` every entry carrying it is refused for: the `aws.` prefix
/// `PutEventsRequestEntry$Source`'s own documentation reserves ("The `aws.`
/// value prefix is service-reserved and cannot be used as a value").
const RESERVED_SOURCE: &str = "aws.orders";

/// The `ErrorCode` a refusal of a VALUE one member's documentation constrains
/// carries, written out here so this suite pins what a client branches on
/// rather than reading it back off the service.
const VALIDATION_ERROR: &str = "ValidationError";

/// The `ErrorMessage` that goes with the reserved-`Source` refusal: whole fixed
/// text, so nothing of the entry is interpolated into it.
const RESERVED_SOURCE_MESSAGE: &str = "Source must not start with the reserved prefix aws.";

/// The `ErrorCode` of the refusal class that is about the FORM of a payload
/// rather than a value: a `Detail` that is not the JSON object `EventDetail`
/// documents, and a raw entry's `Data` that is not the form its own
/// `ContentType` declares. Each names the MEMBER that is wrong.
///
/// No recording of this suite's corpus pins the SENTENCE of that class, so what
/// a row requires of it is the code a client branches on and that a sentence is
/// there at all — the wording itself is the handler's to state.
const MALFORMED_DETAIL: &str = "MalformedDetail";
/// See [`MALFORMED_DETAIL`].
const MALFORMED_DATA: &str = "MalformedData";

// ---------------------------------------------------------------------------
// What a row says about one entry, and the assertion over the result list
// ---------------------------------------------------------------------------

/// What a row expects of one entry.
#[derive(Clone, Debug)]
enum Expected {
    /// An entry the service accepts onto the bus.
    Published,
    /// An entry the service refuses, with the `ErrorCode` of its class and —
    /// where a recording pins that class's wording — the `ErrorMessage` too.
    Refused {
        error_code: String,
        error_message: Option<String>,
    },
}

/// An entry the service accepts.
fn accepted() -> Expected {
    Expected::Published
}

/// An entry the service refuses with the code AND the sentence of its class.
fn refused(error_code: &str, error_message: &str) -> Expected {
    Expected::Refused {
        error_code: error_code.to_string(),
        error_message: Some(error_message.to_string()),
    }
}

/// An entry the service refuses with the code of its class, carrying a sentence
/// no recording pins.
fn refused_as(error_code: &str) -> Expected {
    Expected::Refused {
        error_code: error_code.to_string(),
        error_message: None,
    }
}

/// One result position, as both publish operations report it — the members of
/// `PutEventsResultEntry` and `PutRawEventsResultEntry`, which are the same.
#[derive(Debug)]
struct ResultEntry {
    event_id: Option<String>,
    sequence_number: Option<String>,
    success_code: Option<String>,
    error_code: Option<String>,
    error_message: Option<String>,
}

/// Assert `results` reports what `expected` says of each entry, position by
/// position — "the position of each entry in the response list aligns 1:1 with
/// the corresponding request entry".
fn assert_positions(case: &str, expected: &[Expected], results: &[ResultEntry]) {
    assert_eq!(
        results.len(),
        expected.len(),
        "case {case:?}: one result per request entry, got {results:?}"
    );
    for (position, (wanted, result)) in expected.iter().zip(results).enumerate() {
        match wanted {
            Expected::Published => {
                assert_eq!(
                    result.success_code.as_deref(),
                    Some("PUBLISHED"),
                    "case {case:?}: entry {position} must report PUBLISHED, got {result:?}"
                );
                assert!(
                    result.event_id.is_some()
                        && result.sequence_number.is_some()
                        && result.error_code.is_none()
                        && result.error_message.is_none(),
                    "case {case:?}: entry {position} was accepted, so it carries an EventId and \
                     a SequenceNumber and no error, got {result:?}"
                );
            }
            Expected::Refused {
                error_code,
                error_message,
            } => {
                assert_eq!(
                    result.error_code.as_deref(),
                    Some(error_code.as_str()),
                    "case {case:?}: entry {position} must be refused with the code of its class, \
                     got {result:?}"
                );
                match error_message {
                    Some(sentence) => assert_eq!(
                        result.error_message.as_deref(),
                        Some(sentence.as_str()),
                        "case {case:?}: entry {position} must carry the sentence of its class, \
                         got {result:?}"
                    ),
                    None => assert!(
                        result
                            .error_message
                            .as_deref()
                            .is_some_and(|sentence| !sentence.is_empty()),
                        "case {case:?}: entry {position} must say why it was refused, got \
                         {result:?}"
                    ),
                }
                assert!(
                    result.event_id.is_none()
                        && result.sequence_number.is_none()
                        && result.success_code.is_none(),
                    "case {case:?}: entry {position} was refused, so it carries neither an \
                     EventId nor a SequenceNumber nor a SuccessCode, got {result:?}"
                );
            }
        }
    }
}

/// Assert `failed_entry_count` is what the row's refusals come to — "the number
/// of entries in the batch that failed to be published".
fn assert_failed_count(case: &str, expected: &[Expected], failed_entry_count: i32) {
    let refusals = expected
        .iter()
        .filter(|entry| matches!(entry, Expected::Refused { .. }))
        .count();
    assert_eq!(
        usize::try_from(failed_entry_count).unwrap_or(usize::MAX),
        refusals,
        "case {case:?}: FailedEntryCount must count the refused entries, got {failed_entry_count}"
    );
}

// ---------------------------------------------------------------------------
// The resources every row is built from, and the two publishes
// ---------------------------------------------------------------------------

/// A bus with one plain subscriber delivering every event of it to a queue of
/// its own, as `(bus arn, queue url)`.
///
/// The role the subscriber invokes its target with is allowed the one call that
/// delivery makes, so nothing here fails at the identity: what the rows are
/// about is which entries were accepted.
async fn given_bus_and_subscriber(
    router: &MultiServiceRouter,
    case: &str,
    index: usize,
) -> (String, String) {
    let (queue_url, queue_arn) = given_queue(router, case, &format!("mixed-{index}")).await;
    let role_arn = given_role(
        router,
        case,
        &format!("mixed-role-{index}"),
        SERVICE_PRINCIPAL,
        Some(json!([{
            "Effect": "Allow",
            "Action": SEND_MESSAGE_ACTION,
            "Resource": queue_arn.clone(),
        }])),
    )
    .await;
    let bus_arn = given_active_bus(router, case, &format!("mixed-bus-{index}")).await;
    given_subscriber(
        router,
        case,
        Wanted {
            name: &format!("mixed-subscriber-{index}"),
            event_bus_arn: &bus_arn,
            role_arn: &role_arn,
            target_arn: &queue_arn,
            on_failure_arn: None,
            filter: None,
            retry_attempts: None,
        },
    )
    .await;
    (bus_arn, queue_url)
}

/// One structured request entry: the source it is published under and the
/// `Detail` it carries.
fn entry(source: &str, detail: &str) -> PutEventsRequestEntry {
    PutEventsRequestEntry {
        source: source.to_string(),
        detail_type: "order.created".to_string(),
        detail: Some(detail.to_string()),
        ..Default::default()
    }
}

/// Publish `entries` onto `event_bus_arn`, whatever the service makes of them.
async fn put_events(
    router: &MultiServiceRouter,
    case: &str,
    event_bus_arn: &str,
    entries: Vec<PutEventsRequestEntry>,
) -> PutEventsOutput {
    eventsv2_ok::<PutEvents<InMemoryEngine>>(
        router,
        &PutEventsInput {
            event_bus_arn: event_bus_arn.to_string(),
            entries,
            deduplication_configuration: None,
        },
        case,
    )
    .await
}

/// The results of a `PutEvents` answer, as the positions this suite asserts on.
fn structured_results(answer: &PutEventsOutput) -> Vec<ResultEntry> {
    answer
        .entries
        .clone()
        .unwrap_or_default()
        .into_iter()
        .map(|result| ResultEntry {
            event_id: result.event_id,
            sequence_number: result.sequence_number,
            success_code: result.success_code,
            error_code: result.error_code,
            error_message: result.error_message,
        })
        .collect()
}

/// The results of a `PutRawEvents` answer, likewise.
fn raw_results(answer: &PutRawEventsOutput) -> Vec<ResultEntry> {
    answer
        .entries
        .clone()
        .unwrap_or_default()
        .into_iter()
        .map(|result| ResultEntry {
            event_id: result.event_id,
            sequence_number: result.sequence_number,
            success_code: result.success_code,
            error_code: result.error_code,
            error_message: result.error_message,
        })
        .collect()
}

/// The markers of EXACTLY `expected` messages the queue at `queue_url` holds,
/// read off the payload each delivered envelope carries under `detail`, sorted
/// so a comparison is about WHAT arrived.
async fn delivered_markers(
    router: &MultiServiceRouter,
    case: &str,
    queue_url: &str,
    expected: usize,
) -> Vec<String> {
    let mut markers: Vec<String> = delivered_bodies(router, case, queue_url, expected)
        .await
        .into_iter()
        .map(|text| {
            let body = document(case, &text);
            body["detail"]["marker"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: a delivery must carry its payload under detail, got {body}"
                    )
                })
                .to_string()
        })
        .collect();
    markers.sort();
    markers
}

/// The bodies of EXACTLY `expected` messages the queue at `queue_url` holds, as
/// the TEXT they were delivered as.
async fn delivered_bodies(
    router: &MultiServiceRouter,
    case: &str,
    queue_url: &str,
    expected: usize,
) -> Vec<String> {
    messages_on(router, case, queue_url, expected)
        .await
        .iter()
        .map(|message| {
            message["Body"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: a received message carries no Body: {message}")
                })
                .to_string()
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The rule over PutEvents
// ---------------------------------------------------------------------------

/// One entry of a structured row: the request entry, the marker to expect a
/// delivery of (`None` for an entry nothing is owed for), and what the service
/// makes of it.
struct Sent {
    requested: PutEventsRequestEntry,
    marker: Option<&'static str>,
    expected: Expected,
}

/// An entry the service accepts, carrying `marker` in its payload.
fn publishable(marker: &'static str) -> Sent {
    Sent {
        requested: entry(ACCEPTED_SOURCE, &format!(r#"{{"marker":"{marker}"}}"#)),
        marker: Some(marker),
        expected: accepted(),
    }
}

/// An entry the service refuses for its reserved `Source` prefix. Its payload is
/// well formed all the same, which is what makes the refusal about that one
/// member rather than about the entry's shape.
fn reserved_source() -> Sent {
    Sent {
        requested: entry(RESERVED_SOURCE, r#"{"marker":"m-refused"}"#),
        marker: None,
        expected: refused(VALIDATION_ERROR, RESERVED_SOURCE_MESSAGE),
    }
}

/// An entry the service refuses because `detail` is not the JSON object
/// `EventDetail` documents.
fn malformed_detail(detail: &'static str) -> Sent {
    Sent {
        requested: entry(ACCEPTED_SOURCE, detail),
        marker: None,
        expected: refused_as(MALFORMED_DETAIL),
    }
}

/// One row of the structured table: the batch, and whether the subscriber had
/// already been delivered an earlier publish when it was sent.
struct Row {
    case: &'static str,
    entries: Vec<Sent>,
    warmed_up: bool,
}

/// A batch's accepted entries are published and delivered whatever its refused
/// entries did.
#[tokio::test]
async fn an_accepted_entry_is_delivered_beside_a_refused_sibling() {
    let rows = vec![
        Row {
            case: "a reserved Source first, one well-formed entry after it",
            entries: vec![reserved_source(), publishable("m-kept")],
            warmed_up: true,
        },
        Row {
            case: "a reserved Source last, one well-formed entry before it",
            entries: vec![publishable("m-first"), reserved_source()],
            warmed_up: false,
        },
        Row {
            case: "a reserved Source in the middle of a batch of three",
            entries: vec![
                publishable("m-one"),
                reserved_source(),
                publishable("m-two"),
            ],
            warmed_up: false,
        },
        Row {
            case: "two refusals of two kinds around three well-formed entries",
            entries: vec![
                malformed_detail("not json at all"),
                publishable("m-a"),
                reserved_source(),
                publishable("m-b"),
                publishable("m-c"),
            ],
            warmed_up: true,
        },
        Row {
            case: "a Detail that is a JSON scalar rather than the object the member documents, \
                   beside one well-formed entry",
            entries: vec![malformed_detail("\"just text\""), publishable("m-only")],
            warmed_up: false,
        },
        Row {
            case: "every entry refused, so the subscriber is owed nothing",
            entries: vec![reserved_source(), malformed_detail("[1,2,3]")],
            warmed_up: true,
        },
        Row {
            case: "no refusal at all: the control the rule generalises from",
            entries: vec![publishable("m-x"), publishable("m-y")],
            warmed_up: true,
        },
    ];

    for (index, row) in rows.into_iter().enumerate() {
        let case = row.case;
        let router = emulator();
        let (bus_arn, queue_url) = given_bus_and_subscriber(&router, case, index).await;

        // A row that warms up publishes a single-entry batch first and reads it
        // off the queue, so the subscriber's delivery position has already
        // advanced when the batch under test is sent: that is the state a
        // producer that has been publishing for a while is in.
        if row.warmed_up {
            put_events(
                &router,
                case,
                &bus_arn,
                vec![entry(ACCEPTED_SOURCE, r#"{"marker":"m-warm-up"}"#)],
            )
            .await;
            let warm = delivered_markers(&router, case, &queue_url, 1).await;
            assert_eq!(
                warm,
                vec!["m-warm-up".to_string()],
                "case {case:?}: the warm-up publish must reach the subscriber, got {warm:?}"
            );
        }

        let answer = put_events(
            &router,
            case,
            &bus_arn,
            row.entries
                .iter()
                .map(|sent| sent.requested.clone())
                .collect(),
        )
        .await;
        let expected: Vec<Expected> = row
            .entries
            .iter()
            .map(|sent| sent.expected.clone())
            .collect();
        assert_positions(case, &expected, &structured_results(&answer));
        assert_failed_count(case, &expected, answer.failed_entry_count);

        // The rule itself: every entry the response reported PUBLISHED reached
        // the subscriber, and no refused entry did — the read fails on a message
        // beside those too.
        let mut owed: Vec<String> = row
            .entries
            .iter()
            .filter_map(|sent| sent.marker.map(str::to_string))
            .collect();
        owed.sort();
        let delivered = delivered_markers(&router, case, &queue_url, owed.len()).await;
        assert_eq!(
            delivered, owed,
            "case {case:?}: expected the subscriber to be delivered exactly {owed:?}, got \
             {delivered:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// The same rule over the other publish operation: PutRawEvents
// ---------------------------------------------------------------------------

/// One entry of a raw row: the request entry, and what the service makes of it.
struct SentRaw {
    requested: PutRawEventsRequestEntry,
    /// The text a delivery of this entry carries, for an entry the service
    /// accepts — a raw delivery is the payload as it was published.
    delivered: Option<String>,
    expected: Expected,
}

/// One raw request entry carrying `data` under the content type `content_type`.
fn raw_entry(content_type: &str, data: &str) -> PutRawEventsRequestEntry {
    PutRawEventsRequestEntry {
        data: data.as_bytes().to_vec(),
        metadata: None,
        system_metadata: PutRawEventsSystemMetadata {
            content_type: content_type.to_string(),
            deduplication_id: None,
            event_group_id: None,
        },
    }
}

/// A raw entry the service accepts: a JSON content type and the document that
/// content type declares.
fn raw_publishable(data: &str) -> SentRaw {
    SentRaw {
        requested: raw_entry("application/json", data),
        delivered: Some(data.to_string()),
        expected: accepted(),
    }
}

/// A raw entry the service refuses because its bytes are not the form its own
/// `ContentType` declares.
fn raw_malformed(data: &str) -> SentRaw {
    SentRaw {
        requested: raw_entry("application/json", data),
        delivered: None,
        expected: refused_as(MALFORMED_DATA),
    }
}

/// A raw entry the service refuses because it declares an open format and the
/// request names no `SchemaRegistryConfiguration` to encode it against —
/// "Schema registry settings used to encode open-format (Avro/Protobuf)
/// events", "required for open-format entries; ignored for JSON entries".
///
/// `content_type` is one of the registered names of those formats. No recording
/// pins the sentence of this refusal either, so what is required of it is the
/// code and that it says why.
fn raw_open_format(content_type: &str) -> SentRaw {
    SentRaw {
        requested: raw_entry(content_type, "not a document"),
        delivered: None,
        expected: refused_as(VALIDATION_ERROR),
    }
}

/// Publish `entries` onto `event_bus_arn` through `PutRawEvents`.
async fn put_raw_events(
    router: &MultiServiceRouter,
    case: &str,
    event_bus_arn: &str,
    entries: &[SentRaw],
) -> PutRawEventsOutput {
    eventsv2_ok::<PutRawEvents<InMemoryEngine>>(
        router,
        &PutRawEventsInput {
            event_bus_arn: event_bus_arn.to_string(),
            entries: entries
                .iter()
                .map(|sent| sent.requested.clone())
                .collect(),
            ..Default::default()
        },
        case,
    )
    .await
}

/// `PutRawEvents` decides acceptance entry by entry too, so the rule holds
/// through it as well.
#[tokio::test]
async fn an_accepted_raw_entry_is_delivered_beside_a_refused_sibling() {
    let rows: Vec<(&str, Vec<SentRaw>)> = vec![
        (
            "a JSON entry whose data is no document, first",
            vec![
                raw_malformed("not json at all"),
                raw_publishable(r#"{"orderId":"A-1"}"#),
            ],
        ),
        (
            "an open-format entry with no schema registry, last",
            vec![
                raw_publishable(r#"{"orderId":"B-2"}"#),
                raw_open_format("application/vnd.apache.avro+binary"),
            ],
        ),
        (
            "two refusals of two kinds around two accepted entries",
            vec![
                raw_open_format("application/x-protobuf"),
                raw_publishable(r#"{"orderId":"C-3"}"#),
                raw_malformed("{\"unterminated\": "),
                raw_publishable(r#"{"orderId":"D-4"}"#),
            ],
        ),
        (
            "no refusal at all: the control the rule generalises from",
            vec![
                raw_publishable(r#"{"orderId":"E-5"}"#),
                raw_publishable(r#"{"orderId":"F-6"}"#),
            ],
        ),
    ];

    for (index, (case, entries)) in rows.into_iter().enumerate() {
        let router = emulator();
        let (bus_arn, queue_url) = given_bus_and_subscriber(&router, case, 100 + index).await;

        let answer = put_raw_events(&router, case, &bus_arn, &entries).await;
        let expected: Vec<Expected> = entries
            .iter()
            .map(|sent| sent.expected.clone())
            .collect();
        assert_positions(case, &expected, &raw_results(&answer));
        assert_failed_count(case, &expected, answer.failed_entry_count);

        let mut owed: Vec<String> = entries
            .iter()
            .filter_map(|sent| sent.delivered.clone())
            .collect();
        owed.sort();
        let mut delivered = delivered_bodies(&router, case, &queue_url, owed.len()).await;
        delivered.sort();
        assert_eq!(
            delivered, owed,
            "case {case:?}: expected the subscriber to be delivered exactly {owed:?}, got \
             {delivered:?}"
        );
    }
}
