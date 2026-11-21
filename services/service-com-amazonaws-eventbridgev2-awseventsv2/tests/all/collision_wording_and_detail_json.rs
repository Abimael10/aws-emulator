// SPDX-License-Identifier: CC0-1.0

//! What one `PutEvents` batch leaves BEHIND, and how the batch's layout is
//! answered.
//!
//! The per-entry response of each entry-error class is tabled in
//! `put_events_entry_results.rs`; this suite asserts the other half of the same
//! rule — the store, and the positions.
//!
//! RULE: `PutEventsRequestEntry$Detail` is "The event payload, as a valid JSON
//! string" and a payload is a JSON OBJECT (a subscriber's `DATA`-scoped filter
//! clause names members INSIDE it and a target is handed it as a document), so
//! an entry whose `Detail` is not one is refused IN ITS OWN POSITION: it never
//! reaches the bus's log and consumes no `SequenceNumber`, every other entry of
//! the batch still publishes, `FailedEntryCount` is the number refused, and an
//! accepted payload is stored exactly as the caller wrote it.
//!
//! The rows span that class: the forms a payload takes and the ways a text fails
//! to be one, then the LAYOUT of a mixed batch — which positions are refused,
//! how many, of which refusal class, and in what size of batch.
//!
//! Nothing here waits on the wall clock: the one wait is for a bus's own
//! provisioning transition, polled for the CONDITION (`ACTIVE`) under a generous
//! deadline.

use std::sync::Arc;
use std::time::{Duration, Instant};

use engine::InMemoryEngine;
use smithy_server_runtime::{EmbeddedEmulator, MultiServiceRouter, Operation, OperationContext};

use service_com_amazonaws_eventbridgev2_awseventsv2 as eventsv2;

use eventsv2::ops::create_event_bus::CreateEventBus;
use eventsv2::ops::describe_event_bus::DescribeEventBus;
use eventsv2::ops::put_events::PutEvents;
use eventsv2::ops::types::{
    CreateEventBusInput, DescribeEventBusInput, PutEventsInput, PutEventsOutput,
    PutEventsRequestEntry,
};

use crate::modelled_input_refusal::with_client_token;

/// The region every request below is signed for; it must reach the minted ARNs.
const REGION: &str = "us-west-2";

/// The `ErrorCode` of the entry whose `Detail` is not a payload — the class this
/// suite tells apart from the `Source` refusal when a batch carries both.
const MALFORMED_DETAIL: &str = "MalformedDetail";

/// How long a poll waits for a condition the emulator owes: far above the
/// provisioning span a bus takes, so a loaded host has ample room.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long one pass of a poll sleeps before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// A SigV4 `Authorization` header naming the region and this service's signing
/// name, which is how a request names both.
fn authorization() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTONLY/20260101/{REGION}/events/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=0000"
    )
}

/// The request metadata of a call signed for [`REGION`]: the credential scope is
/// where a request names the region it addresses.
fn parts() -> http::request::Parts {
    let (parts, ()) = http::Request::builder()
        .method("POST")
        .uri("/service/AWSEventsV2/operation/PutEvents")
        .header("authorization", authorization())
        .header("content-type", "application/cbor")
        .body(())
        .expect("the harness request is well formed")
        .into_parts();
    parts
}

/// One emulator over one shared engine, with a request context every typed call
/// reuses.
struct Harness {
    engine: Arc<InMemoryEngine>,
    definition: eventsv2::ServiceDefinition<InMemoryEngine>,
    callback: EmbeddedEmulator,
    parts: http::request::Parts,
}

impl Harness {
    fn new() -> Self {
        let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
        let definition = eventsv2::ServiceDefinition::new(Arc::clone(&engine));
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        let callback = EmbeddedEmulator::from_router(router);
        Self {
            engine,
            definition,
            callback,
            parts: parts(),
        }
    }

    fn ctx(&self) -> OperationContext<'_> {
        OperationContext::new(&self.parts, self.definition.identity(), self.callback.clone())
    }

    /// This service's store, which is what this suite is about: what a publish
    /// left behind rather than what it answered.
    fn store(&self) -> engine::ServiceState<eventsv2::ComAmazonawsEventbridgev2Awseventsv2State> {
        eventsv2::state::service_state(&*self.engine)
            .expect("the in-memory engine always has this service's store")
    }

    /// A bus called `name`, polled until it REPORTS `ACTIVE` — the state a
    /// publish requires of it. Polls the condition rather than sleeping a fixed
    /// time, because provisioning is asynchronous.
    async fn active_bus(&self, case: &str, name: &str) -> String {
        let arn = CreateEventBus::new(Arc::clone(&self.engine))
            .handle(
                // Every create of this service states the idempotency token it requires;
                // the harness supplies a fresh one, as a code-generated client does
                // (`crate::modelled_input_refusal::with_client_token`).
                with_client_token(CreateEventBusInput {
                    name: name.to_string(),
                    ..Default::default()
                }),
                &self.ctx(),
            )
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: CreateEventBus must succeed, got {e:?}"))
            .event_bus_arn
            .unwrap_or_else(|| panic!("case {case:?}: CreateEventBus must report an EventBusArn"));

        let deadline = Instant::now() + POLL_DEADLINE;
        loop {
            let reported = DescribeEventBus::new(Arc::clone(&self.engine))
                .handle(
                    DescribeEventBusInput {
                        event_bus_arn: arn.clone(),
                    },
                    &self.ctx(),
                )
                .await
                .unwrap_or_else(|e| {
                    panic!("case {case:?}: DescribeEventBus must succeed, got {e:?}")
                })
                .state;
            if reported.as_deref() == Some("ACTIVE") {
                return arn;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: the bus {arn} never reported ACTIVE, last reported {reported:?}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    async fn put_events(
        &self,
        case: &str,
        arn: &str,
        entries: Vec<PutEventsRequestEntry>,
    ) -> PutEventsOutput {
        PutEvents::new(Arc::clone(&self.engine))
            .handle(
                PutEventsInput {
                    event_bus_arn: arn.to_string(),
                    entries,
                    ..Default::default()
                },
                &self.ctx(),
            )
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: PutEvents must answer, got {e:?}"))
    }

    /// The `(detail_type, detail, sequence_number)` of every event the bus at
    /// `arn` holds, in log order.
    fn stored_log(&self, arn: &str) -> Vec<(String, Option<String>, u64)> {
        let store = self.store();
        let state = store.lock();
        state
            .event_buses
            .get(arn)
            .map(|bus| {
                bus.events
                    .iter()
                    .map(|stored| {
                        let structured = stored.structured().unwrap_or_else(|| {
                            panic!("every event this suite publishes is a PutEvents entry")
                        });
                        (
                            structured.detail_type.clone(),
                            structured.detail.clone(),
                            stored.sequence_number,
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// An entry with the given source, detail type and detail.
fn entry(source: &str, detail_type: &str, detail: Option<&str>) -> PutEventsRequestEntry {
    PutEventsRequestEntry {
        source: source.to_string(),
        detail_type: detail_type.to_string(),
        detail: detail.map(str::to_string),
        ..Default::default()
    }
}

/// Payloads a `Detail` may carry, and whether each is one.
///
/// The accepted rows differ along the JSON object's own shape (empty, flat,
/// nested, escaped, surrounded by whitespace) and include the member omitted
/// altogether, which carries no payload constraint at all. The refused rows
/// differ along HOW the text fails: no JSON document (truncated, unquoted key,
/// trailing text, empty, a bare word) and a JSON document that is not an object
/// (a list, and each scalar).
const PAYLOADS: &[(&str, Option<&str>, bool)] = &[
    ("absent", None, true),
    ("empty object", Some("{}"), true),
    ("flat object", Some(r#"{"a":1}"#), true),
    (
        "nested object",
        Some(r#"{"a":{"b":[1,2,null]},"c":"text"}"#),
        true,
    ),
    ("object with escapes", Some(r#"{"a":"\u00e9\n"}"#), true),
    ("object inside whitespace", Some("  {\"a\":1}\n"), true),
    ("not json at all", Some("not-json"), false),
    ("empty text", Some(""), false),
    ("whitespace only", Some("   "), false),
    ("truncated object", Some("{"), false),
    ("object missing a value", Some(r#"{"a":}"#), false),
    ("single-quoted key", Some("{'a':1}"), false),
    (
        "trailing text after a document",
        Some(r#"{"a":1}trailing"#),
        false,
    ),
    ("unterminated list", Some("[1,2"), false),
    ("a bare word", Some("undefined"), false),
    ("list", Some("[1,2,3]"), false),
    ("string", Some(r#""text""#), false),
    ("number", Some("7"), false),
    ("boolean", Some("true"), false),
    ("null", Some("null"), false),
];

/// RULE, on the store: a refused payload reaches neither the log nor a sequence
/// number, and an accepted one is stored exactly as the caller wrote it — for
/// every form a payload takes and every way a text fails to be one.
#[tokio::test]
async fn a_detail_that_is_not_a_payload_is_never_stored() {
    let harness = Harness::new();
    let arn = harness.active_bus("payloads", "payload-bus").await;

    let mut published = 0_u64;
    for (case, detail, is_payload) in PAYLOADS {
        let result = harness
            .put_events(case, &arn, vec![entry("demo.app", case, *detail)])
            .await;
        let refused = if *is_payload { 0 } else { 1 };
        if *is_payload {
            published += 1;
        }
        assert_eq!(
            result.failed_entry_count, refused,
            "case {case:?}: expected {refused} failed entr(y|ies), got {result:?}"
        );

        // Only the accepted payloads reach the log, and a refusal consumes no
        // sequence number: the numbers stay unbroken from 1.
        let log = harness.stored_log(&arn);
        assert_eq!(
            log.len() as u64,
            published,
            "case {case:?}: the bus must hold exactly the accepted entries, got {log:?}"
        );
        let numbers: Vec<u64> = log.iter().map(|(_, _, number)| *number).collect();
        let expected: Vec<u64> = (1..=published).collect();
        assert_eq!(
            numbers, expected,
            "case {case:?}: stored sequence numbers must run unbroken from 1, got {numbers:?}"
        );
    }

    // Every accepted payload is stored as the caller wrote it, not as a
    // re-rendering of it.
    let log = harness.stored_log(&arn);
    for (case, detail, _) in PAYLOADS.iter().filter(|(_, _, is_payload)| *is_payload) {
        let stored = log
            .iter()
            .find(|(detail_type, _, _)| detail_type == case)
            .unwrap_or_else(|| panic!("case {case:?}: the accepted entry must be stored"));
        assert_eq!(
            stored.1.as_deref(),
            *detail,
            "case {case:?}: the stored Detail must be the text sent, got {:?}",
            stored.1
        );
    }
}

/// What one entry of a mixed batch is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Publishes.
    Good,
    /// Refused: its `Detail` is not a payload.
    BadDetail,
    /// Refused: its `Source` carries the service-reserved prefix.
    ReservedSource,
}

/// RULE, on the positions: the result list aligns 1:1 with the request entries
/// whatever the layout of the batch — each refused entry is reported in its own
/// position and in its own class, every other entry publishes,
/// `FailedEntryCount` is the number refused, and the log holds exactly the
/// published entries in request order.
#[tokio::test]
async fn a_mixed_batch_publishes_every_other_entry_and_counts_the_refusals() {
    let harness = Harness::new();

    // Rows differing along WHERE the refusals sit, how many there are, which
    // refusal class they belong to, and the size of the batch.
    let every_third: Vec<Kind> = (0..12)
        .map(|i| if i % 3 == 0 { Kind::BadDetail } else { Kind::Good })
        .collect();
    let layouts: Vec<(&str, Vec<Kind>)> = vec![
        ("a bad payload alone", vec![Kind::BadDetail]),
        ("a bad payload first", vec![Kind::BadDetail, Kind::Good]),
        ("a bad payload last", vec![Kind::Good, Kind::BadDetail]),
        (
            "a bad payload in the middle",
            vec![Kind::Good, Kind::BadDetail, Kind::Good],
        ),
        ("every payload bad", vec![Kind::BadDetail, Kind::BadDetail]),
        ("no payload bad", vec![Kind::Good, Kind::Good]),
        (
            "both refusal classes at once",
            vec![
                Kind::Good,
                Kind::BadDetail,
                Kind::ReservedSource,
                Kind::Good,
            ],
        ),
        ("every third of twelve bad", every_third),
    ];

    for (row, (case, layout)) in layouts.iter().enumerate() {
        let arn = harness
            .active_bus(case, &format!("mixed-batch-bus-{row}"))
            .await;
        let entries: Vec<PutEventsRequestEntry> = layout
            .iter()
            .enumerate()
            .map(|(position, kind)| match kind {
                Kind::Good => entry(
                    "demo.app",
                    &format!("Entry{position}"),
                    Some(&format!(r#"{{"position":{position}}}"#)),
                ),
                Kind::BadDetail => {
                    entry("demo.app", &format!("Entry{position}"), Some("not-json"))
                }
                Kind::ReservedSource => entry(
                    "aws.ec2",
                    &format!("Entry{position}"),
                    Some(r#"{"position":0}"#),
                ),
            })
            .collect();

        let result = harness.put_events(case, &arn, entries).await;
        let results = result.entries.clone().unwrap_or_default();
        assert_eq!(
            results.len(),
            layout.len(),
            "case {case:?}: one result per request entry, got {results:?}"
        );
        let expected_failures = layout.iter().filter(|kind| **kind != Kind::Good).count();
        assert_eq!(
            usize::try_from(result.failed_entry_count).unwrap_or(usize::MAX),
            expected_failures,
            "case {case:?}: FailedEntryCount must count the refused entries, got {}",
            result.failed_entry_count
        );

        for (position, kind) in layout.iter().enumerate() {
            let answered = &results[position];
            match kind {
                Kind::Good => assert!(
                    answered.event_id.is_some()
                        && answered.success_code.as_deref() == Some("PUBLISHED")
                        && answered.error_code.is_none(),
                    "case {case:?}: entry {position} must publish, got {answered:?}"
                ),
                Kind::BadDetail => assert_eq!(
                    answered.error_code.as_deref(),
                    Some(MALFORMED_DETAIL),
                    "case {case:?}: entry {position} carries no payload, so it must be refused \
                     as that class, got {answered:?}"
                ),
                Kind::ReservedSource => {
                    assert!(
                        answered.error_code.is_some() && answered.event_id.is_none(),
                        "case {case:?}: entry {position} has a reserved Source, so it must be \
                         refused, got {answered:?}"
                    );
                    assert_ne!(
                        answered.error_code.as_deref(),
                        Some(MALFORMED_DETAIL),
                        "case {case:?}: entry {position} fails on its Source, not on its \
                         payload, got {answered:?}"
                    );
                }
            }
        }

        let stored: Vec<String> = harness
            .stored_log(&arn)
            .into_iter()
            .map(|(detail_type, _, _)| detail_type)
            .collect();
        let expected: Vec<String> = layout
            .iter()
            .enumerate()
            .filter(|(_, kind)| **kind == Kind::Good)
            .map(|(position, _)| format!("Entry{position}"))
            .collect();
        assert_eq!(
            stored, expected,
            "case {case:?}: the bus must hold exactly the published entries in order, \
             got {stored:?}"
        );
    }
}
