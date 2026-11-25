// SPDX-License-Identifier: CC0-1.0

//! How a request refused BEFORE the handler runs is stated, asserted over tables.
//!
//! Three rules:
//!
//! 0. Every request this service ANSWERS — served or refused, and whichever of its layers
//!    answered it — emits exactly one structured access record, so a request refused
//!    before the handler ran is recorded the same way as one the handler refused.
//! 1. A modelled-constraint refusal is HTTP 400 `ValidationException` carrying the clause
//!    envelope built from the request — `N validation error(s) detected: Value '<what the
//!    request carried>' at '<member path>' failed to satisfy constraint: <requirement>` —
//!    in which a `@pattern` requirement states the expression WITHOUT the whole-value
//!    anchors the model wraps it in, and a COLLECTION member's clause quotes the
//!    collection the request carried rather than a summary of its size. The check itself
//!    is unchanged: the model's pattern is still applied as written.
//! 2. A channel reference that is not a channel reference is answered first, under the
//!    error this service's model declares for that fault, whatever else the same request
//!    broke — the service resolves every call against the channel before it reads the
//!    payload, so that holds for a body the codec cannot read at all as well, which is
//!    also a request this service answers and therefore records.
//!
//! The client is this crate's one test harness, reached from the suite that owns it.

use std::sync::{Arc, Mutex, OnceLock};

use crate::identity_authorization::{given_session, SESSION_KEY_ID};
use crate::put_audit_events::{channel_arn, ev, Ev, Harness, Reply, DEFAULT_ACCESS_KEY_ID};
use service_com_amazonaws_cloudtraildata_cloudtraildataservice::state::Channel;

/// A JSON audit event that passes per-event validation, so nothing but the member under
/// test decides these calls.
pub const GOOD_EVENT: &str = r#"{"eventSource":"custom.app","eventName":"Login"}"#;

/// The largest batch `auditEvents` admits, from the member's own `@length`
/// (`smithy_query(query="shape", shape="AuditEvents")`: min 1, max 100).
pub const MAX_BATCH: usize = 100;
const MIN_BATCH: usize = 1;

/// The expression `AuditEvent$id` is modelled with, and the bounds on it
/// (`smithy_query(query="shape", shape="Uuid")`: `@pattern ^[-_A-Za-z0-9]+$`,
/// `@length 1..128`).
pub const ID_PATTERN: &str = "^[-_A-Za-z0-9]+$";
pub const ID_MAX_LENGTH: usize = 128;

/// The expression `PutAuditEventsRequest$externalId` is modelled with
/// (`smithy_query(query="shape", shape="ExternalId")`).
const EXTERNAL_ID_PATTERN: &str = r"^[\w+=,.@:\/-]*$";

/// `pattern` as a clause states it: the expression without the `^…$` pair the model wraps
/// a whole-value pattern in.
fn unanchored(pattern: &str) -> &str {
    pattern
        .strip_prefix('^')
        .and_then(|body| body.strip_suffix('$'))
        .unwrap_or(pattern)
}

/// The prose member of a refusal.
pub fn message_of(reply: &Reply) -> String {
    reply
        .body
        .get("message")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The envelope one clause travels in.
pub fn one_violation(clause: &str) -> String {
    format!("1 validation error detected: {clause}")
}

/// The requirement half a `@length` MAXIMUM states, for a member of any type.
pub fn at_most_requirement(maximum: usize) -> String {
    format!("Member must have length less than or equal to {maximum}")
}

/// The requirement half a `@length` MINIMUM states.
pub fn at_least_requirement(minimum: usize) -> String {
    format!("Member must have length greater than or equal to {minimum}")
}

/// The requirement half a `@pattern` violation states: the expression as a clause quotes
/// it, without the whole-value anchors the model wraps it in.
pub fn pattern_requirement(pattern: &str) -> String {
    format!(
        "Member must satisfy regular expression pattern: {}",
        unanchored(pattern)
    )
}

/// ONE clause of a refusal: the value the request carried at `member_path`, and the
/// requirement it broke. THE one home for that template, so a change in how AWS words it
/// is edited once; every clause builder below and in the sibling suites is expressed over
/// it.
pub fn clause(value: &str, member_path: &str, requirement: &str) -> String {
    format!("Value '{value}' at '{member_path}' failed to satisfy constraint: {requirement}")
}

/// The clause the `auditEvents` member's own `@length` states under `requirement`: the
/// batch the request carried, quoted whole.
pub fn batch_length_clause(events: &[Ev], requirement: &str) -> String {
    clause(&rendered_batch(events), "auditEvents", requirement)
}

/// The clause one batch element's own `id` violation states: the rejected value verbatim
/// at the element's 1-BASED position under the list member.
pub fn element_id_clause(position: usize, value: &str, requirement: &str) -> String {
    clause(value, &format!("auditEvents.{position}.member.id"), requirement)
}

/// How the rendering of an `AuditEvent` reads inside a clause: the shape's name over every
/// member it declares, in declaration order, with one the request left unset stated as
/// `null` (`smithy_query(query="shape", shape="AuditEvent")`: id, eventData,
/// eventDataChecksum).
pub fn rendered_event(id: &str, event_data: &str, checksum: Option<&str>) -> String {
    format!(
        "AuditEvent(id={id}, eventData={event_data}, eventDataChecksum={})",
        checksum.unwrap_or("null")
    )
}

/// A batch of `count` events, each carrying `GOOD_EVENT`.
fn batch(count: usize) -> Vec<Ev> {
    (0..count)
        .map(|index| ev(format!("evt-{index}"), GOOD_EVENT))
        .collect()
}

/// How that batch reads inside a clause: the elements in order, comma-separated, in
/// brackets.
pub fn rendered_batch(events: &[Ev]) -> String {
    let elements: Vec<String> = events
        .iter()
        .map(|event| rendered_event(event.id(), event.data(), event.checksum()))
        .collect();
    format!("[{}]", elements.join(", "))
}

/// A refusal of the `auditEvents` member's `@length` quotes the BATCH the request carried,
/// not a summary of its size — the value such a clause rejects is the collection itself.
/// Asserted at both ends of the declared bound, and over batch sizes that differ in how
/// much text the clause has to carry.
#[tokio::test]
async fn a_batch_outside_its_declared_length_is_quoted_whole() {
    let harness = Harness::new();
    let arn = channel_arn("8c7f4e0d-93a6-4b25-8d4c-7e9f0a1b2c3d");
    harness.register(Channel::from_arn(&arn, "Custom").expect("a channel ARN"));

    // (case, the batch, the requirement the clause states). The two rows inside the bound
    // are the control: they are served, so the bound is reported only where it is broken.
    let rows: Vec<(&str, Vec<Ev>, Option<String>)> = vec![
        ("an empty batch", batch(0), Some(at_least_requirement(MIN_BATCH))),
        ("one event, the smallest batch admitted", batch(MIN_BATCH), None),
        ("a batch at the declared maximum", batch(MAX_BATCH), None),
        (
            "one event past the declared maximum",
            batch(MAX_BATCH + 1),
            Some(at_most_requirement(MAX_BATCH)),
        ),
        (
            "twenty events past the declared maximum",
            batch(MAX_BATCH + 20),
            Some(at_most_requirement(MAX_BATCH)),
        ),
    ];

    for (case, events, requirement) in &rows {
        let reply = harness.put(&arn, None, events).await;
        match requirement {
            None => assert_eq!(
                reply.status, 200,
                "case {case:?}: a batch inside the bound is served, got {} with body {}",
                reply.status, reply.body
            ),
            Some(requirement) => {
                assert_eq!(
                    (reply.status, reply.error_code().as_str()),
                    (400, "ValidationException"),
                    "case {case:?}: expected 400 ValidationException, got {} with body {}",
                    reply.status,
                    reply.body
                );
                let expected = one_violation(&batch_length_clause(events, requirement));
                let stated = message_of(&reply);
                assert_eq!(
                    stated, expected,
                    "case {case:?}: expected the clause to quote the batch the request \
                     carried ({expected:?}), got {stated:?}"
                );
            }
        }
    }
}

/// A `@pattern` requirement states the expression a value must satisfy WITHOUT the
/// whole-value anchors the model wraps it in, and the clause quotes the value the request
/// carried. Spans the two pattern-constrained members a request reaches — an event id and
/// the external id — and, for the id, the `@length` bound beside the pattern.
#[tokio::test]
async fn a_constraint_clause_states_the_expression_without_its_anchors() {
    let harness = Harness::new();
    let arn = channel_arn("9d8a5f1e-a4b7-4c36-8e5d-8f0a1b2c3d4e");
    harness.register(Channel::from_arn(&arn, "Custom").expect("a channel ARN"));

    let over_long_id = "i".repeat(ID_MAX_LENGTH + 1);

    // (case, the event id, the externalId, the clause expected)
    let rows: Vec<(&str, String, Option<&str>, String)> = vec![
        (
            "an event id carrying a space",
            "event id".to_string(),
            None,
            element_id_clause(1, "event id", &pattern_requirement(ID_PATTERN)),
        ),
        (
            "an event id carrying punctuation",
            "event!".to_string(),
            None,
            element_id_clause(1, "event!", &pattern_requirement(ID_PATTERN)),
        ),
        (
            "an event id carrying a multi-byte character",
            "événement".to_string(),
            None,
            element_id_clause(1, "événement", &pattern_requirement(ID_PATTERN)),
        ),
        (
            "an event id one character past its declared maximum",
            over_long_id.clone(),
            None,
            element_id_clause(1, &over_long_id, &at_most_requirement(ID_MAX_LENGTH)),
        ),
        (
            "an external id outside its expression",
            "evt-1".to_string(),
            Some("external$id"),
            clause(
                "external$id",
                "externalId",
                &pattern_requirement(EXTERNAL_ID_PATTERN),
            ),
        ),
    ];

    for (case, id, external_id, expected) in &rows {
        let reply = harness
            .put(&arn, *external_id, &[ev(id.as_str(), GOOD_EVENT)])
            .await;
        assert_eq!(
            (reply.status, reply.error_code().as_str()),
            (400, "ValidationException"),
            "case {case:?}: expected 400 ValidationException, got {} with body {}",
            reply.status,
            reply.body
        );
        let stated = message_of(&reply);
        assert_eq!(
            stated,
            one_violation(expected),
            "case {case:?}: expected {:?}, got {stated:?}",
            one_violation(expected)
        );
        // The REQUIREMENT half is what loses the anchors; a value the request carried
        // keeps whatever characters it had, a dollar sign included.
        if let Some((_, quoted)) = stated.split_once("regular expression pattern: ") {
            assert!(
                !quoted.starts_with('^') && !quoted.ends_with('$'),
                "case {case:?}: a quoted expression carries no whole-value anchor, got \
                 {quoted:?}"
            );
        }
    }

    // An event id inside both bounds is served, so the clauses above are reported only
    // where the request really broke the member.
    let reply = harness
        .put(&arn, None, &[ev("i".repeat(ID_MAX_LENGTH), GOOD_EVENT)])
        .await;
    assert_eq!(
        reply.status, 200,
        "case \"an event id at its declared maximum\": expected HTTP 200, got {} with body \
         {}",
        reply.status, reply.body
    );
}

/// A channel reference that is not one is answered under this service's own
/// `InvalidChannelARN`, whatever else the same request broke: the channel the request
/// names is what the call is resolved against before the payload is read, so that refusal
/// is payload-independent. Spans requests whose payload is good, whose payload breaks a
/// `@pattern`, and whose payload breaks the batch bound as well.
#[tokio::test]
async fn a_channel_reference_is_answered_before_the_payloads_own_violations() {
    let harness = Harness::new();

    // (case, the payload)
    let payloads: Vec<(&str, Vec<Ev>)> = vec![
        ("a payload that breaks nothing", vec![ev("evt-1", GOOD_EVENT)]),
        (
            "a payload whose event id is outside its expression",
            vec![ev("bad id!", GOOD_EVENT)],
        ),
        (
            "a payload whose event id is past its declared maximum",
            vec![ev("i".repeat(ID_MAX_LENGTH + 1), GOOD_EVENT)],
        ),
        ("a payload past the batch maximum", batch(MAX_BATCH + 1)),
        ("an empty payload", batch(0)),
    ];

    for (case, events) in &payloads {
        let reply = harness.put("not-an-arn", None, events).await;
        assert_eq!(
            (reply.status, reply.error_code().as_str()),
            (400, "InvalidChannelARN"),
            "case {case:?}: a value that is not a channel reference is answered first, got \
             {} with body {}",
            reply.status,
            reply.body
        );
        let stated = message_of(&reply);
        assert_eq!(
            stated, INVALID_CHANNEL_ARN,
            "case {case:?}: expected the sentence the model documents, got {stated:?}"
        );
    }
}

/// The sentence `InvalidChannelARN` carries, as the model documents the shape
/// (`smithy_query(query="shape", shape="InvalidChannelARN")`): a fixed sentence with no
/// slot for a caller value.
const INVALID_CHANNEL_ARN: &str = "The specified channel ARN is not a valid channel ARN.";

/// The operation every access record of this service names, as the model declares it
/// (`smithy_query(query="list_operations")`).
const OPERATION: &str = "PutAuditEvents";

/// One captured access record: every field of the event, flattened to strings.
type Record = std::collections::BTreeMap<String, String>;

/// Collects every field of a `tracing` event into a [`Record`].
#[derive(Default)]
struct RecordFields(Record);

impl tracing::field::Visit for RecordFields {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.0.insert(field.name().to_string(), value.to_string());
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.0.insert(field.name().to_string(), format!("{value:?}"));
    }
}

/// A `tracing` subscriber that keeps this service's access records, so a test can assert
/// on the record a request produced.
///
/// Implemented against `tracing`'s own `Subscriber` trait — the crate this service records
/// with — rather than pulling a second tracing crate into the harness. An event is kept
/// when it names this service's operation, which is what every access record states;
/// spans are no part of the record, so the span half of the trait is inert.
struct CaptureAccessRecords {
    records: Arc<Mutex<Vec<Record>>>,
}

impl tracing::Subscriber for CaptureAccessRecords {
    fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        let mut fields = RecordFields::default();
        event.record(&mut fields);
        if fields.0.get("operation").map(String::as_str) != Some(OPERATION) {
            return;
        }
        self.records
            .lock()
            .expect("the capture mutex must not be poisoned")
            .push(fields.0);
    }

    fn enter(&self, _span: &tracing::span::Id) {}

    fn exit(&self, _span: &tracing::span::Id) {}
}

/// The process-wide sink of this service's access records, installing the capturing
/// subscriber the first time it is asked for.
///
/// Global rather than thread-local because `tracing` caches a callsite's interest for the
/// whole process: a thread-local subscriber installed after another case had already
/// reached the record's callsite would see nothing. Records are therefore selected by the
/// channel reference the asserting case named, which no other case of this binary uses.
fn access_records() -> Arc<Mutex<Vec<Record>>> {
    static SINK: OnceLock<Arc<Mutex<Vec<Record>>>> = OnceLock::new();
    Arc::clone(SINK.get_or_init(|| {
        let records = Arc::new(Mutex::new(Vec::new()));
        tracing::subscriber::set_global_default(CaptureAccessRecords {
            records: Arc::clone(&records),
        })
        .expect("this suite installs the only tracing subscriber in the test binary");
        // `tracing` caches each callsite's INTEREST the first time it is hit, and with no
        // subscriber installed yet that cached answer is "never enabled" — so a record
        // some other case of this binary emitted before this install would leave the
        // callsite silent, and which records arrive would depend on the order the cases
        // happened to run in. Rebuilding the cache re-asks this subscriber about every
        // callsite.
        tracing::callsite::rebuild_interest_cache();
        records
    }))
}

/// What a captured record states for `field`, with the quotes a Debug-rendered value
/// carries stripped, so an assertion reads the value and not how `tracing` rendered it.
fn stated(record: &Record, field: &str) -> String {
    record
        .get(field)
        .map(|value| value.trim_matches('"').to_string())
        .unwrap_or_default()
}

/// Every access record naming `channel`, in the order they were emitted.
fn records_naming(records: &Arc<Mutex<Vec<Record>>>, channel: &str) -> Vec<Record> {
    records
        .lock()
        .expect("the capture mutex must not be poisoned")
        .iter()
        .filter(|record| stated(record, "channel") == channel)
        .cloned()
        .collect()
}

/// One answered request and the record it must leave behind.
struct Answered {
    case: &'static str,
    /// The channel reference the request carries — unique to its row, so the record it
    /// emits is the only one that names it.
    channel: String,
    /// Whether a channel is registered under that reference first.
    registered: bool,
    external_id: Option<&'static str>,
    events: Vec<Ev>,
    /// The answer on the wire: the status, and the error code (empty on a success).
    status: u16,
    code: &'static str,
    /// The `outcome` the record states for that answer.
    outcome: &'static str,
}

/// Assert the one record `row` left behind, whatever layer answered the request.
fn assert_one_record(records: &Arc<Mutex<Vec<Record>>>, row: &Answered, akid: &str) {
    let case = row.case;
    let captured = records_naming(records, &row.channel);
    assert_eq!(
        captured.len(),
        1,
        "case {case:?}: expected exactly one access record naming {:?}, got {captured:?}",
        row.channel
    );
    let record = &captured[0];
    assert_eq!(
        stated(record, "operation"),
        OPERATION,
        "case {case:?}: the record must name the operation, got {record:?}"
    );
    assert_eq!(
        stated(record, "outcome"),
        row.outcome,
        "case {case:?}: expected the outcome {:?}, got {record:?}",
        row.outcome
    );
    // The access key id the caller presented is a public identifier, so the record can
    // attribute the call; no secret is recorded.
    assert_eq!(
        stated(record, "akid"),
        akid,
        "case {case:?}: the record must name the access key id presented, got {record:?}"
    );
    assert_eq!(
        stated(record, "account_id"),
        engine::emulator_account_id(),
        "case {case:?}: the record must name the caller account, got {record:?}"
    );
    assert!(
        !stated(record, "detail").is_empty(),
        "case {case:?}: the record must state how the request ended, got {record:?}"
    );
    // The record is a record of ACCESS, so it carries no event payload: the events of
    // this request are nowhere in it.
    for event in &row.events {
        assert!(
            !record.values().any(|value| value.contains(event.data())),
            "case {case:?}: the record must carry no event payload, got {record:?}"
        );
    }
}

/// RULE: every request this service answers emits exactly one structured access record —
/// served or refused, and whichever of its layers answered it — naming the operation, the
/// access key id presented, the caller account, the channel the request named and how the
/// request ended, with the outcome of a refused request being the error code it was
/// answered under.
///
/// The rows span the layers that answer a request and the classes each of them answers:
/// the pre-handler refusal layer (a channel reference outside its modelled pattern, a
/// batch at either end of its declared length, an element's constraint, the external id's
/// constraint), the handler's own refusals (a value that names no channel, an unheld
/// channel, a repeated event id, an ARN in another account) and a served request.
#[tokio::test]
async fn every_answered_request_emits_one_access_record() {
    let records = access_records();
    let harness = Harness::new();

    let rows: Vec<Answered> = vec![
        Answered {
            case: "a served request",
            channel: channel_arn("aa000001-0000-4000-8000-000000000001"),
            registered: true,
            external_id: None,
            events: vec![ev("evt-1", GOOD_EVENT)],
            status: 200,
            code: "",
            outcome: "Ingested",
        },
        Answered {
            case: "a channel reference outside the member's modelled pattern",
            channel: "access-record-probe-that-is-no-arn".to_string(),
            registered: false,
            external_id: None,
            events: vec![ev("evt-1", GOOD_EVENT)],
            status: 400,
            code: "InvalidChannelARN",
            outcome: "InvalidChannelARN",
        },
        Answered {
            case: "a batch one event past its declared maximum",
            channel: channel_arn("aa000002-0000-4000-8000-000000000002"),
            registered: true,
            external_id: None,
            events: batch(MAX_BATCH + 1),
            status: 400,
            code: "ValidationException",
            outcome: "ValidationException",
        },
        Answered {
            case: "an empty batch, under its declared minimum",
            channel: channel_arn("aa000003-0000-4000-8000-000000000003"),
            registered: true,
            external_id: None,
            events: batch(0),
            status: 400,
            code: "ValidationException",
            outcome: "ValidationException",
        },
        Answered {
            case: "an event id outside its expression",
            channel: channel_arn("aa000004-0000-4000-8000-000000000004"),
            registered: true,
            external_id: None,
            events: vec![ev("bad id!", GOOD_EVENT)],
            status: 400,
            code: "ValidationException",
            outcome: "ValidationException",
        },
        Answered {
            case: "an externalId outside its expression",
            channel: channel_arn("aa000005-0000-4000-8000-000000000005"),
            registered: true,
            external_id: Some("external$id"),
            events: vec![ev("evt-1", GOOD_EVENT)],
            status: 400,
            code: "ValidationException",
            outcome: "ValidationException",
        },
        Answered {
            case: "an arn: value that names no channel",
            channel: "arn:aws:s3:::an-access-record-bucket".to_string(),
            registered: false,
            external_id: None,
            events: vec![ev("evt-1", GOOD_EVENT)],
            status: 400,
            code: "InvalidChannelARN",
            outcome: "InvalidChannelARN",
        },
        Answered {
            case: "a channel ARN this emulator does not hold",
            channel: channel_arn("aa000006-0000-4000-8000-000000000006"),
            registered: false,
            external_id: None,
            events: vec![ev("evt-1", GOOD_EVENT)],
            status: 400,
            code: "ChannelNotFound",
            outcome: "ChannelNotFound",
        },
        Answered {
            case: "a batch repeating an event id",
            channel: channel_arn("aa000007-0000-4000-8000-000000000007"),
            registered: true,
            external_id: None,
            events: vec![ev("same-id", GOOD_EVENT), ev("same-id", GOOD_EVENT)],
            status: 400,
            code: "DuplicatedAuditEventId",
            outcome: "DuplicatedAuditEventId",
        },
        Answered {
            case: "a channel ARN in another account",
            channel: format!(
                "arn:aws:cloudtrail:us-east-1:111122223333:channel/{}",
                "aa000008-0000-4000-8000-000000000008"
            ),
            registered: false,
            external_id: None,
            events: vec![ev("evt-1", GOOD_EVENT)],
            status: 403,
            code: "AccessDeniedException",
            outcome: "AccessDeniedException",
        },
    ];

    for row in &rows {
        if row.registered {
            harness.register(Channel::from_arn(&row.channel, "Custom").expect("a channel ARN"));
        }
        let reply = harness.put(&row.channel, row.external_id, &row.events).await;
        let case = row.case;
        assert_eq!(
            reply.status, row.status,
            "case {case:?}: expected HTTP {}, got {} with body {}",
            row.status, reply.status, reply.body
        );
        assert_eq!(
            reply.error_code(),
            row.code,
            "case {case:?}: expected the error code {:?}, got body {}",
            row.code,
            reply.body
        );
        assert_one_record(&records, row, DEFAULT_ACCESS_KEY_ID);
    }
}

/// The same rule for the layer in FRONT of the operation: a request no identity-based
/// policy allows is refused there, and that layer records it too — so the record is
/// emitted once per answered request whichever layer answered, and the channel it names is
/// the reference the request carried.
#[tokio::test]
async fn a_request_the_authorization_layer_refuses_is_recorded_too() {
    let records = access_records();
    let harness = Harness::new();
    // A session whose role carries no policy document at all: IAM's default deny, so the
    // layer in front of the operation answers this request.
    given_session(harness.engine());

    let row = Answered {
        case: "a session no statement allows",
        channel: channel_arn("aa000009-0000-4000-8000-000000000009"),
        registered: true,
        external_id: None,
        events: vec![ev("evt-1", GOOD_EVENT)],
        status: 403,
        code: "AccessDeniedException",
        outcome: "AccessDeniedException",
    };
    harness.register(Channel::from_arn(&row.channel, "Custom").expect("a channel ARN"));

    let reply = harness
        .put_as(SESSION_KEY_ID, &row.channel, None, &row.events)
        .await;
    let case = row.case;
    assert_eq!(
        (reply.status, reply.error_code().as_str()),
        (row.status, row.code),
        "case {case:?}: expected the authorization refusal, got {} with body {}",
        reply.status,
        reply.body
    );
    assert_one_record(&records, &row, SESSION_KEY_ID);
}

/// The forms a body this service's codec cannot build the operation's input from takes:
/// one position of that input carrying a value of a type the model does not declare there,
/// and a document the JSON grammar cannot hold at all.
///
/// Read off the model's own input (`smithy_query(query="operation",
/// operation="PutAuditEvents")`: `auditEvents` is a list of `AuditEvent`, whose `id` and
/// `eventData` are strings), so the rows span the POSITIONS of that input rather than one
/// body a caller happened to send.
fn unreadable_bodies() -> Vec<(&'static str, &'static str)> {
    vec![
        ("the batch sent as a string", r#"{"auditEvents":"evt-1"}"#),
        ("the batch sent as a number", r#"{"auditEvents":7}"#),
        (
            "the batch sent as one object",
            r#"{"auditEvents":{"id":"evt-1","eventData":"{}"}}"#,
        ),
        ("a batch element sent as a string", r#"{"auditEvents":["evt-1"]}"#),
        (
            "an element id sent as a number",
            r#"{"auditEvents":[{"id":7,"eventData":"{}"}]}"#,
        ),
        (
            "an element id sent as a list",
            r#"{"auditEvents":[{"id":["evt-1"],"eventData":"{}"}]}"#,
        ),
        (
            "an element eventData sent as an object",
            r#"{"auditEvents":[{"id":"evt-1","eventData":{"eventName":"Login"}}]}"#,
        ),
        (
            "an element eventData sent as a boolean",
            r#"{"auditEvents":[{"id":"evt-1","eventData":true}]}"#,
        ),
        (
            "a document the JSON grammar cannot hold",
            r#"{"auditEvents":[{"id":"evt-1","#,
        ),
        ("a body that is no JSON at all", "events, please"),
        (
            "a body that is a JSON list",
            r#"[{"id":"evt-1","eventData":"{}"}]"#,
        ),
    ]
}

/// RULE: a channel reference outside the member's modelled expression is answered under the
/// error this service's model declares for that fault — HTTP 400 `InvalidChannelARN` with
/// the sentence the model documents — whatever else the request carried, whether or not its
/// body is one the codec can build the input from: the call is resolved against the channel
/// the request names before the payload is read at all.
///
/// The table above spans payloads the codec CAN read; these rows are every form of a body it
/// cannot (`unreadable_bodies`) against every form of a reference the expression does not
/// admit — a word, a bare channel id, a mis-cased prefix, a URL, the prefix alone and one
/// bound with no value — so neither the body's shape nor the reference's spelling decides
/// the answer.
#[tokio::test]
async fn a_channel_reference_is_answered_first_even_when_the_body_cannot_be_read() {
    let harness = Harness::new();

    // (case, the value bound to `channelArn`)
    let references: Vec<(&str, &str)> = vec![
        ("a bare word", "not-an-arn"),
        (
            "a bare channel id (the ARN suffix the doc also names)",
            "1a2b3c4d-5e6f-7081-92a3-b4c5d6e7f809",
        ),
        (
            "an upper-case ARN prefix",
            "ARN:aws:cloudtrail:us-east-1:000000000000:channel/abc",
        ),
        ("a URL", "https://cloudtrail.us-east-1.amazonaws.com/channel"),
        ("the prefix without its colon", "arn"),
        ("a reference bound with no value", ""),
    ];

    for (body_case, body) in unreadable_bodies() {
        for (reference_case, reference) in &references {
            let case = format!("{body_case}, with {reference_case}");
            let reply = harness.put_body(reference, body).await;
            assert_eq!(
                (reply.status, reply.error_code().as_str()),
                (400, "InvalidChannelARN"),
                "case {case:?}: the channel reference is answered first, got {} with body {}",
                reply.status,
                reply.body
            );
            let stated = message_of(&reply);
            assert_eq!(
                stated, INVALID_CHANNEL_ARN,
                "case {case:?}: expected the sentence the model documents, got {stated:?}"
            );
        }
    }
}

/// RULE: every request this service ANSWERS emits exactly one structured access record
/// naming the code the caller was answered under — including a request whose body no codec
/// can build the input from, which the layers behind this service's pre-handler refusal
/// layer can neither classify nor record.
///
/// Both halves of the table are the same bodies (`unreadable_bodies`): under a REGISTERED
/// channel ARN of this account, where the answer is the one the framework words for a body
/// its codec cannot read, and under a reference outside the member's expression, where it is
/// this service's own channel fault. A request whose body was never read persists nothing
/// either way.
#[tokio::test]
async fn a_request_whose_body_cannot_be_read_is_answered_and_recorded_once() {
    let records = access_records();
    let harness = Harness::new();

    for (index, (body_case, body)) in unreadable_bodies().into_iter().enumerate() {
        // A channel of this account, registered first, so nothing but the body decides the
        // answer — and named uniquely, so the records this case left are its own.
        let arn = channel_arn(&format!("cc0000{index:02}-0000-4000-8000-0000000000{index:02}"));
        harness.register(Channel::from_arn(&arn, "Custom").expect("a channel ARN"));

        let reply = harness.put_body(&arn, body).await;
        assert_eq!(
            reply.status, 400,
            "case {body_case:?}: expected HTTP 400, got {} with body {}",
            reply.status, reply.body
        );
        let code = reply.error_code();
        assert_ne!(
            code, "InvalidChannelARN",
            "case {body_case:?}: this request bound a channel ARN the member's expression \
             admits, so the channel fault is not its answer, got body {}",
            reply.body
        );
        assert!(
            !code.is_empty() && !message_of(&reply).is_empty(),
            "case {body_case:?}: a refusal states a code and a sentence, got body {}",
            reply.body
        );
        assert_recorded_once(&records, body_case, &arn, &code);
        let log = harness.ingest_log(&arn);
        assert!(
            log.is_empty(),
            "case {body_case:?}: a request whose body was never read persists nothing, got \
             {log:?}"
        );
    }

    for (index, (body_case, body)) in unreadable_bodies().into_iter().enumerate() {
        let reference = format!("unreadable-body-probe-{index}");
        let reply = harness.put_body(&reference, body).await;
        assert_eq!(
            (reply.status, reply.error_code().as_str()),
            (400, "InvalidChannelARN"),
            "case {body_case:?} with a reference outside the member's expression: expected \
             the channel fault, got {} with body {}",
            reply.status,
            reply.body
        );
        assert_recorded_once(&records, body_case, &reference, "InvalidChannelARN");
    }
}

/// Assert the ONE access record a request naming `channel` left behind: the operation, the
/// access key id presented, the caller account, how the request ended, and the outcome being
/// the code the caller was answered under.
fn assert_recorded_once(
    records: &Arc<Mutex<Vec<Record>>>,
    case: &str,
    channel: &str,
    code: &str,
) {
    let captured = records_naming(records, channel);
    assert_eq!(
        captured.len(),
        1,
        "case {case:?}: expected exactly one access record naming {channel:?}, got {captured:?}"
    );
    let record = &captured[0];
    assert_eq!(
        stated(record, "operation"),
        OPERATION,
        "case {case:?}: the record must name the operation, got {record:?}"
    );
    assert_eq!(
        stated(record, "outcome"),
        code,
        "case {case:?}: expected the outcome {code:?} the caller was answered under, got \
         {record:?}"
    );
    assert_eq!(
        stated(record, "akid"),
        DEFAULT_ACCESS_KEY_ID,
        "case {case:?}: the record must name the access key id presented, got {record:?}"
    );
    assert_eq!(
        stated(record, "account_id"),
        engine::emulator_account_id(),
        "case {case:?}: the record must name the caller account, got {record:?}"
    );
    assert!(
        !stated(record, "detail").is_empty(),
        "case {case:?}: the record must state how the request ended, got {record:?}"
    );
}
