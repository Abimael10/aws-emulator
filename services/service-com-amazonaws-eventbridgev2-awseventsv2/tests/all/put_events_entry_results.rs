// SPDX-License-Identifier: CC0-1.0

//! What one `PutEvents` batch answers per entry, and how every bus-addressed
//! operation reports a bus that is not there.
//!
//! Two rules, each driven over a table rather than over one recorded request:
//!
//! * every request entry is read against the constraints its own members
//!   document BEFORE the event is stored, and an entry that breaks one is
//!   refused in its own position — with that entry-error class's fixed
//!   `ErrorCode` and `ErrorMessage`, no `EventId`, no `SequenceNumber` and no
//!   `SuccessCode` — while an accepted entry carries a service-assigned
//!   `EventId` in hyphenated-UUID form, and `FailedEntryCount` is the number of
//!   refused entries;
//! * a bus-addressed operation whose ARN names no stored bus answers
//!   `ResourceNotFoundException` (404) whose `Message` is the fixed sentence
//!   plus ONE variable part: the bus NAME the request addressed.

use std::sync::Arc;
use std::time::{Duration, Instant};

use engine::InMemoryEngine;
use smithy_server_runtime::{
    EmbeddedEmulator, MultiServiceRouter, Operation, OperationContext, OperationError,
};

use eventsv2::ops::create_event_bus::CreateEventBus;
use eventsv2::ops::create_event_source::{CreateEventSource, CreateEventSourceErrorModeled};
use eventsv2::ops::create_subscriber::{CreateSubscriber, CreateSubscriberErrorModeled};
use eventsv2::ops::delete_event_bus::{DeleteEventBus, DeleteEventBusErrorModeled};
use eventsv2::ops::describe_event_bus::{DescribeEventBus, DescribeEventBusErrorModeled};
use eventsv2::ops::put_events::{PutEvents, PutEventsErrorModeled};
use eventsv2::ops::types::{
    AwsServiceEventsSourceConfiguration, CreateEventBusInput, CreateEventSourceInput,
    CreateSubscriberInput, DeleteEventBusInput, DescribeEventBusInput, DescribeEventBusOutput,
    EventSourceConfiguration, InvokeConfiguration, PutEventsInput, PutEventsOutput,
    PutEventsRequestEntry, PutEventsResultEntry,
};
use eventsv2::scaffolded::Scaffolded;
use service_com_amazonaws_eventbridgev2_awseventsv2 as eventsv2;

/// The account segment every emulated ARN carries.
const ACCOUNT: &str = "000000000000";

/// The region the harness signs for, which is the region the minted ARNs carry.
const REGION: &str = "us-west-2";

/// How long a poll for a lifecycle transition keeps looking. Far above the
/// service's own span, so a loaded host does not turn a waiting test into a
/// failing one; the loop stops as soon as the condition holds.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long the poll loop waits before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

// ---------------------------------------------------------------------------
// The harness
// ---------------------------------------------------------------------------

/// A SigV4 `Authorization` header naming `region` and this service's signing
/// name, which is how a request names the region it addresses.
fn authorization(region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTONLY/20260101/{region}/events/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=0000"
    )
}

/// The request metadata of a call signed for [`REGION`].
fn parts() -> http::request::Parts {
    let (parts, ()) = http::Request::builder()
        .method("POST")
        .uri("/service/AWSEventsV2/operation/PutEvents")
        .header("authorization", authorization(REGION))
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

    /// This service's store, for asserting what a publish actually left behind.
    fn store(&self) -> engine::ServiceState<eventsv2::ComAmazonawsEventbridgev2Awseventsv2State> {
        eventsv2::state::service_state(&*self.engine).expect("the store is on this engine")
    }

    async fn put_events(
        &self,
        input: PutEventsInput,
    ) -> Result<PutEventsOutput, eventsv2::ops::put_events::PutEventsError> {
        PutEvents::new(Arc::clone(&self.engine))
            .handle(input, &self.ctx())
            .await
    }

    async fn describe(
        &self,
        arn: &str,
    ) -> Result<DescribeEventBusOutput, eventsv2::ops::describe_event_bus::DescribeEventBusError>
    {
        DescribeEventBus::new(Arc::clone(&self.engine))
            .handle(
                DescribeEventBusInput {
                    event_bus_arn: arn.to_string(),
                },
                &self.ctx(),
            )
            .await
    }

    /// A bus that has finished provisioning, so it accepts events.
    ///
    /// Waits for the CONDITION — the reported `State` — rather than for a
    /// duration: the transition is asynchronous, so the loop looks again until
    /// the bus is ACTIVE or [`POLL_DEADLINE`] passes.
    async fn active_bus(&self, name: &str) -> String {
        let created = CreateEventBus::new(Arc::clone(&self.engine))
            .handle(
                CreateEventBusInput {
                    name: name.to_string(),
                    ..Default::default()
                },
                &self.ctx(),
            )
            .await
            .unwrap_or_else(|e| panic!("case {name:?}: CreateEventBus must succeed, got {e:?}"));
        let arn = created
            .event_bus_arn
            .unwrap_or_else(|| panic!("case {name:?}: CreateEventBus must report an EventBusArn"));
        let deadline = Instant::now() + POLL_DEADLINE;
        loop {
            let state = self
                .describe(&arn)
                .await
                .unwrap_or_else(|e| panic!("case {name:?}: DescribeEventBus must answer, got {e:?}"))
                .state;
            if state.as_deref() == Some("ACTIVE") {
                return arn;
            }
            assert!(
                Instant::now() < deadline,
                "case {name:?}: the bus never reached ACTIVE, last reported {state:?}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
}

// ---------------------------------------------------------------------------
// Rule 1: per-entry results
// ---------------------------------------------------------------------------

/// What the service owes for one request entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expected {
    /// The entry is admitted: it reports an `EventId`, a `SequenceNumber` and a
    /// `SuccessCode`, and nothing of the error half.
    Published,
    /// The entry is refused in its own position, with this `ErrorCode` and
    /// `ErrorMessage` and nothing of the success half.
    Refused {
        error_code: &'static str,
        error_message: &'static str,
    },
}

/// The refusal of an entry whose `Source` uses the prefix its own member
/// documents as service-reserved.
const RESERVED_SOURCE: Expected = Expected::Refused {
    error_code: "ValidationError",
    error_message: "Source must not start with the reserved prefix aws.",
};

/// The refusal of an entry whose `Detail` is not the payload the `EventDetail`
/// shape documents: text that is no JSON document, or a JSON document that is
/// not an object.
const MALFORMED_DETAIL: Expected = Expected::Refused {
    error_code: "MalformedDetail",
    error_message: "Detail is not valid JSON",
};

/// One row per entry the class spans: the `Source` and `Detail` an entry
/// carries, and what the service owes for it.
///
/// The `Source` rows come from that member's own documentation ("The `aws.`
/// value prefix is service-reserved and cannot be used as a value") and the
/// `Detail` rows from `EventDetail`'s ("The event payload of a PutEvents entry,
/// as a valid JSON string"): payload documents on one side, and on the other
/// both text that is no JSON document at all and JSON that is not a payload.
fn rows() -> Vec<(&'static str, &'static str, Option<&'static str>, Expected)> {
    vec![
        // A payload is a JSON object, whatever it holds.
        (
            "object detail",
            "demo.app",
            Some(r#"{"id":1}"#),
            Expected::Published,
        ),
        ("empty object", "demo.app", Some("{}"), Expected::Published),
        (
            "nested object",
            "demo.app",
            Some(r#"{"a":{"b":[true,null,1.5]}}"#),
            Expected::Published,
        ),
        (
            "object surrounded by whitespace",
            "demo.app",
            Some(" {\n  \"a\": 1\n}\n"),
            Expected::Published,
        ),
        // No payload at all is not a payload constraint.
        ("absent detail", "demo.app", None, Expected::Published),
        // The reserved value is a PREFIX, so a source that merely starts with
        // the same letters is a source like any other.
        (
            "source starting with aws but not the prefix",
            "awsome.app",
            Some("{}"),
            Expected::Published,
        ),
        (
            "source carrying the prefix later on",
            "demo.aws.app",
            Some("{}"),
            Expected::Published,
        ),
        // The documented prefix is the lowercase one.
        (
            "upper-case AWS prefix",
            "AWS.ec2",
            Some("{}"),
            Expected::Published,
        ),
        // The reserved prefix, in each form a caller writes it.
        ("reserved service", "aws.ec2", Some("{}"), RESERVED_SOURCE),
        ("reserved, no detail", "aws.s3", None, RESERVED_SOURCE),
        ("reserved bare prefix", "aws.", Some("{}"), RESERVED_SOURCE),
        (
            "reserved partner source",
            "aws.partner/example",
            Some(r#"{"ok":true}"#),
            RESERVED_SOURCE,
        ),
        // Text that is no JSON document at all.
        (
            "not JSON at all",
            "demo.app",
            Some("not json"),
            MALFORMED_DETAIL,
        ),
        ("empty payload", "demo.app", Some(""), MALFORMED_DETAIL),
        (
            "truncated object",
            "demo.app",
            Some(r#"{"a":1"#),
            MALFORMED_DETAIL,
        ),
        (
            "trailing comma",
            "demo.app",
            Some(r#"{"a":1,}"#),
            MALFORMED_DETAIL,
        ),
        ("unquoted key", "demo.app", Some("{a:1}"), MALFORMED_DETAIL),
        (
            "single-quoted key",
            "demo.app",
            Some("{'a':1}"),
            MALFORMED_DETAIL,
        ),
        (
            "two documents in one payload",
            "demo.app",
            Some("{} {}"),
            MALFORMED_DETAIL,
        ),
        ("bare word", "demo.app", Some("NaN"), MALFORMED_DETAIL),
        // JSON documents that are not payloads: a payload's members are what a
        // DATA-scoped filter and a target read, so a scalar or a list is none.
        ("array", "demo.app", Some("[1,2,3]"), MALFORMED_DETAIL),
        ("string", "demo.app", Some("\"text\""), MALFORMED_DETAIL),
        ("number", "demo.app", Some("42"), MALFORMED_DETAIL),
        ("boolean", "demo.app", Some("true"), MALFORMED_DETAIL),
        ("null", "demo.app", Some("null"), MALFORMED_DETAIL),
        // An entry that breaks two constraints is refused once, for the first
        // of them.
        (
            "reserved source and malformed detail",
            "aws.ec2",
            Some("nope"),
            RESERVED_SOURCE,
        ),
    ]
}

/// One request entry carrying `source` and `detail`.
fn entry(source: &str, detail_type: &str, detail: Option<&str>) -> PutEventsRequestEntry {
    PutEventsRequestEntry {
        source: source.to_string(),
        detail_type: detail_type.to_string(),
        detail: detail.map(str::to_string),
        ..Default::default()
    }
}

/// Whether `value` is rendered as a hyphenated lowercase UUID — five groups of
/// 8, 4, 4, 4 and 12 hexadecimal digits — which is the form a service-assigned
/// event id takes, and so the form a client that parses or correlates one
/// accepts.
fn is_hyphenated_uuid(value: &str) -> bool {
    let groups: Vec<&str> = value.split('-').collect();
    let widths: Vec<usize> = groups.iter().map(|group| group.len()).collect();
    widths == vec![8, 4, 4, 4, 12]
        && groups
            .iter()
            .all(|group| group.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')))
}

/// Assert one answered result against what its row owes, whatever position of
/// whatever batch it came back in.
fn assert_result(case: &str, expected: Expected, answered: &PutEventsResultEntry) {
    match expected {
        Expected::Published => {
            let event_id = answered.event_id.as_deref().unwrap_or_else(|| {
                panic!("case {case:?}: an accepted entry must carry an EventId, got {answered:?}")
            });
            assert!(
                is_hyphenated_uuid(event_id),
                "case {case:?}: expected a hyphenated lowercase UUID EventId, got {event_id:?}"
            );
            assert!(
                answered.sequence_number.is_some(),
                "case {case:?}: an accepted entry must carry a SequenceNumber, got {answered:?}"
            );
            assert_eq!(
                answered.success_code.as_deref(),
                Some("PUBLISHED"),
                "case {case:?}: expected SuccessCode PUBLISHED, got {:?}",
                answered.success_code
            );
            assert!(
                answered.error_code.is_none() && answered.error_message.is_none(),
                "case {case:?}: an accepted entry carries no error half, got {answered:?}"
            );
        }
        Expected::Refused {
            error_code,
            error_message,
        } => {
            assert_eq!(
                answered.error_code.as_deref(),
                Some(error_code),
                "case {case:?}: expected ErrorCode {error_code:?}, got {:?}",
                answered.error_code
            );
            assert_eq!(
                answered.error_message.as_deref(),
                Some(error_message),
                "case {case:?}: expected ErrorMessage {error_message:?}, got {:?}",
                answered.error_message
            );
            assert!(
                answered.event_id.is_none()
                    && answered.sequence_number.is_none()
                    && answered.success_code.is_none(),
                "case {case:?}: a refused entry is not published, so it carries none of the \
                 success half, got {answered:?}"
            );
        }
    }
}

/// RULE: each `PutEvents` entry is read against the constraints its own members
/// document before the event is stored; an entry that breaks one is refused in
/// its own position with that entry-error class's `ErrorCode` and
/// `ErrorMessage` and none of the success half, an accepted entry carries a
/// hyphenated-UUID `EventId`, and `FailedEntryCount` is the number of refused
/// entries — whatever the mix of entries in the batch.
#[tokio::test]
async fn each_entry_is_answered_in_its_own_position_and_counted() {
    let harness = Harness::new();
    let rows = rows();

    // Every row at once, so the positional alignment and the count are read off
    // ONE mixed batch.
    let batched = harness.active_bus("batched-bus").await;
    let answer = harness
        .put_events(PutEventsInput {
            event_bus_arn: batched.clone(),
            entries: rows
                .iter()
                .map(|(case, source, detail, _)| entry(source, case, *detail))
                .collect(),
            ..Default::default()
        })
        .await
        .expect("case \"mixed batch\": PutEvents answers 200 whatever its entries carry");
    let answered = answer.entries.clone().unwrap_or_default();
    assert_eq!(
        answered.len(),
        rows.len(),
        "case \"mixed batch\": one result per request entry, got {}",
        answered.len()
    );
    for (position, (case, _, _, expected)) in rows.iter().enumerate() {
        assert_result(case, *expected, &answered[position]);
    }
    let refused = rows
        .iter()
        .filter(|(_, _, _, expected)| *expected != Expected::Published)
        .count();
    assert_eq!(
        usize::try_from(answer.failed_entry_count).unwrap_or(usize::MAX),
        refused,
        "case \"mixed batch\": FailedEntryCount must count the refused entries, got {}",
        answer.failed_entry_count
    );
    let ids: std::collections::BTreeSet<&str> = answered
        .iter()
        .filter_map(|result| result.event_id.as_deref())
        .collect();
    assert_eq!(
        ids.len(),
        rows.len() - refused,
        "case \"mixed batch\": one distinct EventId per accepted entry, got {ids:?}"
    );

    // A refused entry is not stored either: the log holds exactly the accepted
    // entries, in request order.
    let published: Vec<(String, Option<String>)> = rows
        .iter()
        .filter(|(_, _, _, expected)| *expected == Expected::Published)
        .map(|(_, source, detail, _)| ((*source).to_string(), detail.map(str::to_string)))
        .collect();
    {
        let store = harness.store();
        let state = store.lock();
        let bus = state
            .event_buses
            .get(&batched)
            .expect("the bus is in the store");
        let stored: Vec<(String, Option<String>)> = bus
            .events
            .iter()
            .map(|event| (event.source.clone(), event.detail.clone()))
            .collect();
        assert_eq!(
            stored, published,
            "case \"mixed batch\": the log must hold exactly the accepted entries, got {stored:?}"
        );
    }

    // The same rows one entry at a time, so neither the position nor the count
    // depends on what else the batch carried.
    let single = harness.active_bus("single-entry-bus").await;
    for (case, source, detail, expected) in rows {
        let answer = harness
            .put_events(PutEventsInput {
                event_bus_arn: single.clone(),
                entries: vec![entry(source, case, detail)],
                ..Default::default()
            })
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: PutEvents must answer 200, got {e:?}"));
        let answered = answer.entries.clone().unwrap_or_default();
        assert_eq!(
            answered.len(),
            1,
            "case {case:?}: one result per request entry, got {answered:?}"
        );
        assert_result(case, expected, &answered[0]);
        assert_eq!(
            answer.failed_entry_count,
            i32::from(expected != Expected::Published),
            "case {case:?}: FailedEntryCount must agree with the single result, got {answer:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Rule 2: the bus-not-found refusal
// ---------------------------------------------------------------------------

/// A well-formed `EventBusArn` naming `name` with the id segment `bus_id`, for a
/// bus the store does not hold.
fn missing_bus_arn(name: &str, bus_id: &str) -> String {
    assert_eq!(
        bus_id.len(),
        eventsv2::state::BUS_ID_LEN,
        "the modelled EventBusArn pattern pins the id segment at {} characters",
        eventsv2::state::BUS_ID_LEN
    );
    format!("arn:aws:events:{REGION}:{ACCOUNT}:event-busv2/{name}/{bus_id}")
}

/// The `Message` of the `ResourceNotFoundException` each bus-addressed
/// operation answers for `arn`, having first asserted the error class and its
/// HTTP status.
async fn not_found_messages(harness: &Harness, case: &str, arn: &str) -> Vec<(&'static str, String)> {
    let mut messages = Vec::new();

    let error = harness
        .put_events(PutEventsInput {
            event_bus_arn: arn.to_string(),
            entries: vec![entry("demo.app", "Demo", Some("{}"))],
            ..Default::default()
        })
        .await
        .err()
        .unwrap_or_else(|| panic!("case {case:?}: PutEvents must be refused"));
    assert_not_found(case, "PutEvents", &error);
    messages.push((
        "PutEvents",
        match error {
            Scaffolded::Modeled(PutEventsErrorModeled::ResourceNotFoundException(not_found)) => {
                not_found.message.unwrap_or_default()
            }
            other => panic!("case {case:?}: PutEvents answered {other:?}"),
        },
    ));

    let error = harness
        .describe(arn)
        .await
        .err()
        .unwrap_or_else(|| panic!("case {case:?}: DescribeEventBus must be refused"));
    assert_not_found(case, "DescribeEventBus", &error);
    messages.push((
        "DescribeEventBus",
        match error {
            Scaffolded::Modeled(DescribeEventBusErrorModeled::ResourceNotFoundException(
                not_found,
            )) => not_found.message.unwrap_or_default(),
            other => panic!("case {case:?}: DescribeEventBus answered {other:?}"),
        },
    ));

    let error = DeleteEventBus::new(Arc::clone(&harness.engine))
        .handle(
            DeleteEventBusInput {
                event_bus_arn: arn.to_string(),
            },
            &harness.ctx(),
        )
        .await
        .err()
        .unwrap_or_else(|| panic!("case {case:?}: DeleteEventBus must be refused"));
    assert_not_found(case, "DeleteEventBus", &error);
    messages.push((
        "DeleteEventBus",
        match error {
            Scaffolded::Modeled(DeleteEventBusErrorModeled::ResourceNotFoundException(
                not_found,
            )) => not_found.message.unwrap_or_default(),
            other => panic!("case {case:?}: DeleteEventBus answered {other:?}"),
        },
    ));

    let error = CreateEventSource::new(Arc::clone(&harness.engine))
        .handle(
            CreateEventSourceInput {
                name: "orphan-source".to_string(),
                event_bus_arn: arn.to_string(),
                configuration: EventSourceConfiguration::AwsServiceEventsConfiguration(
                    AwsServiceEventsSourceConfiguration {
                        aws_service: "aws.s3".to_string(),
                        pattern: None,
                        on_failure_configuration: None,
                    },
                ),
                description: None,
                tags: None,
                client_token: None,
            },
            &harness.ctx(),
        )
        .await
        .err()
        .unwrap_or_else(|| panic!("case {case:?}: CreateEventSource must be refused"));
    assert_not_found(case, "CreateEventSource", &error);
    messages.push((
        "CreateEventSource",
        match error {
            Scaffolded::Modeled(CreateEventSourceErrorModeled::ResourceNotFoundException(
                not_found,
            )) => not_found.message.unwrap_or_default(),
            other => panic!("case {case:?}: CreateEventSource answered {other:?}"),
        },
    ));

    let error = CreateSubscriber::new(Arc::clone(&harness.engine))
        .handle(
            CreateSubscriberInput {
                name: "orphan-subscriber".to_string(),
                event_bus_arn: arn.to_string(),
                invoke_configuration: InvokeConfiguration {
                    role_arn: format!("arn:aws:iam::{ACCOUNT}:role/events-delivery"),
                    target_arn: format!("arn:aws:sqs:{REGION}:{ACCOUNT}:orphan-queue"),
                    ..Default::default()
                },
                ..Default::default()
            },
            &harness.ctx(),
        )
        .await
        .err()
        .unwrap_or_else(|| panic!("case {case:?}: CreateSubscriber must be refused"));
    assert_not_found(case, "CreateSubscriber", &error);
    messages.push((
        "CreateSubscriber",
        match error {
            Scaffolded::Modeled(CreateSubscriberErrorModeled::ResourceNotFoundException(
                not_found,
            )) => not_found.message.unwrap_or_default(),
            other => panic!("case {case:?}: CreateSubscriber answered {other:?}"),
        },
    ));

    messages
}

/// Every one of these refusals is the modelled 404 error class.
fn assert_not_found<E: OperationError>(case: &str, operation: &str, error: &E) {
    assert_eq!(
        error.shape_name(),
        "ResourceNotFoundException",
        "case {case:?}: {operation} must answer ResourceNotFoundException, got {:?}",
        error.shape_name()
    );
    assert_eq!(
        error.http_status(),
        404,
        "case {case:?}: {operation} must answer HTTP 404, got {}",
        error.http_status()
    );
}

/// RULE: an operation that resolves an event bus BY NAME and finds none there
/// answers `ResourceNotFoundException` (404) whose `Message` is the fixed
/// sentence plus ONE variable part — the NAME of the bus the request addressed,
/// and nothing else of the ARN.
///
/// The one operation that resolves the WHOLE ARN instead, `DescribeEventBus`,
/// answers the fixed text of the class for an identifier this service never
/// minted; that rule is asserted in `tests/all/modelled_input_refusal.rs`, and
/// the rows below assert it too, so one of these two answers is given for every
/// bus-addressed operation and never the other.
#[tokio::test]
async fn a_bus_that_is_not_there_is_reported_by_name() {
    let harness = Harness::new();

    // Names spanning the modelled `EventBusName` grammar
    // (`^[A-Za-z0-9][\.\-_A-Za-z0-9]*$`, 1..256 characters): the shortest one,
    // each punctuation character it admits, mixed case, and the longest one.
    // Each row carries its OWN id segment, so an id can be no part of the
    // message.
    let long_name = format!("L{}", "o".repeat(255 - 1));
    let names: Vec<(&str, &str)> = vec![
        ("A", "aaaaaaaaaaaaaaaaaaaaaaaaa"),
        ("ghost-bus", "bbbbbbbbbbbbbbbbbbbbbbbbb"),
        ("A.b-c_9", "ccccccccccccccccccccccccc"),
        ("MixedCaseBus", "ddddddddddddddddddddddddd"),
        ("bus.with.dots", "eeeeeeeeeeeeeeeeeeeeeeeee"),
        (long_name.as_str(), "fffffffffffffffffffffffff"),
    ];

    for (name, bus_id) in names {
        let arn = missing_bus_arn(name, bus_id);
        let named = format!("Event bus not found: {name}");
        for (operation, message) in not_found_messages(&harness, name, &arn).await {
            // `DescribeEventBus` resolves the whole ARN, identifier segment
            // included: no bus of this identifier was ever minted here, so the
            // ARN addresses no resource of this service and the refusal is the
            // fixed text of the class.
            let expected = if operation == "DescribeEventBus" {
                eventsv2::state::RESOURCE_NOT_FOUND_MESSAGE.to_string()
            } else {
                named.clone()
            };
            assert_eq!(
                message, expected,
                "case {name:?}: {operation} must report {expected:?}, got {message:?}"
            );
            assert!(
                !message.contains("arn:aws:"),
                "case {name:?}: {operation} must name the bus, not the whole ARN, got {message:?}"
            );
            assert!(
                !message.contains(bus_id),
                "case {name:?}: {operation} must not report the generated id segment, \
                 got {message:?}"
            );
        }
    }

    // The same name with a different id segment is the same message, so the
    // id is no part of it; and a bus that IS there is not refused at all.
    let one = missing_bus_arn("twin-bus", "11111111111111111111nnnnn");
    let other = missing_bus_arn("twin-bus", "22222222222222222222nnnnn");
    assert_eq!(
        not_found_messages(&harness, "twin, first id", &one).await,
        not_found_messages(&harness, "twin, second id", &other).await,
        "case \"twin\": two ARNs naming the same bus must be reported identically"
    );

    let live = harness.active_bus("live-bus").await;
    let answer = harness
        .put_events(PutEventsInput {
            event_bus_arn: live,
            entries: vec![entry("demo.app", "Demo", Some("{}"))],
            ..Default::default()
        })
        .await
        .expect("case \"live\": a bus that is there must accept the publish");
    assert_eq!(
        answer.failed_entry_count, 0,
        "case \"live\": a well-formed entry on a stored bus must publish, got {answer:?}"
    );
}
