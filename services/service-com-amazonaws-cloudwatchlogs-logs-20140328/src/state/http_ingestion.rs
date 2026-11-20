// SPDX-License-Identifier: CC0-1.0

//! The HTTP INGESTION endpoints of CloudWatch Logs: what a client addresses them at, what a
//! request's body carries, and what one answers.
//!
//! These endpoints are not in this service's Smithy model — no operation binds them — so
//! their addressing, their body forms and their answers are stated here once, and read by
//! the service seam that serves them (`crate::service::http_ingestion`). Three forms are
//! served on the one endpoint this emulator answers at:
//!
//! * the HTTP log collector form, which takes a record whose `event` member IS the log
//!   event, as a single object or an array of them ([`Endpoint::HttpLogCollector`]);
//! * the structured JSON form, which takes records spelling a timestamp and a message the
//!   way `PutLogEvents`' own `InputLogEvent` does ([`Endpoint::StructuredJson`]);
//! * the OpenTelemetry form, which takes one OTLP logs export document whose log records
//!   are the events to ingest ([`Endpoint::OpenTelemetry`]).
//!
//! Each names its destination — the log group and the log stream the records are ingested
//! into — in the headers CloudWatch Logs reads it from or in the query string
//! ([`named_destination`], [`destination`]), and each authenticates with a bearer
//! credential rather than with a SigV4 signature: a CloudWatch Logs API key, which is a
//! service-specific credential IAM issues for [`API_KEY_SERVICE_NAME`].
//!
//! What a request ANSWERS is one rule over both forms, because what each carries is a batch
//! of records either way: every record accepted is an empty JSON object
//! ([`ACCEPTED_BODY`]); some accepted and some refused is that batch's partial success,
//! naming how many records were refused and why ([`partial_success_body`]); and a batch no
//! record of which could be ingested is refused, because there was nothing to ingest
//! ([`ALL_EVENTS_INVALID_MESSAGE`]).

use ::serde_json::Value;

/// The path the HTTP log collector form is addressed at.
pub const HTTP_LOG_COLLECTOR_PATH: &str = "/services/collector/event";

/// The path the structured JSON form is addressed at.
pub const STRUCTURED_JSON_PATH: &str = "/ingest/json";

/// The path the OpenTelemetry form is addressed at: the path an OTLP/HTTP exporter ships a
/// LOGS export request to, which is where CloudWatch Logs takes one — the OpenTelemetry
/// ingestion `GetLogObject` names as the path large log objects are made on.
pub const OPEN_TELEMETRY_PATH: &str = "/v1/logs";

/// The query parameter naming the log group a request's records are ingested into.
pub const LOG_GROUP_PARAMETER: &str = "logGroup";

/// The query parameter naming the log stream a request's records are ingested into.
pub const LOG_STREAM_PARAMETER: &str = "logStream";

/// The header naming the log group a request's records are ingested into, which is how a
/// client that ships a body of its own (an OTLP exporter) addresses its destination.
pub const LOG_GROUP_HEADER: &str = "x-aws-log-group";

/// The header naming the log stream a request's records are appended to, the companion of
/// [`LOG_GROUP_HEADER`].
pub const LOG_STREAM_HEADER: &str = "x-aws-log-stream";

/// The service principal a CloudWatch Logs API key is issued for: the `ServiceName` an
/// `iam:CreateServiceSpecificCredential` names to be handed the long-term API key —
/// `ServiceSpecificCredential$ServiceCredentialAlias` documents the member as being "for
/// Bedrock API keys and CloudWatch Logs API keys" — which is the bearer credential these
/// endpoints authenticate.
pub const API_KEY_SERVICE_NAME: &str = "logs.amazonaws.com";

/// The member an HTTP log collector record carries its log event in.
const EVENT_MEMBER: &str = "event";

/// The member an HTTP log collector record may carry its own instant in, in SECONDS since
/// the epoch, the unit a record of this form spells a time in.
const TIME_MEMBER: &str = "time";

/// The member a structured JSON record carries its message in, spelled as
/// `InputLogEvent$message` is.
const MESSAGE_MEMBER: &str = "message";

/// The member a structured JSON record carries its instant in, in MILLISECONDS since the
/// epoch, spelled as `InputLogEvent$timestamp` is — "the number of milliseconds after Jan
/// 1, 1970 00:00:00 UTC" (`PutLogEvents`).
const TIMESTAMP_MEMBER: &str = "timestamp";

/// The member an OTLP logs export document carries its per-resource lists in, the outermost
/// level of the OpenTelemetry logs encoding.
const RESOURCE_LOGS_MEMBER: &str = "resourceLogs";

/// The member one resource's entry carries its per-scope lists in, the middle level of that
/// encoding.
const SCOPE_LOGS_MEMBER: &str = "scopeLogs";

/// The member one scope's entry carries its log records in, the level the events to ingest
/// are at.
const LOG_RECORDS_MEMBER: &str = "logRecords";

/// The member an OTLP log record carries its log event in: a record that carries no body
/// carries nothing to ingest, as a record of either other form that carries no event does.
const BODY_MEMBER: &str = "body";

/// The member an OTLP log record carries its instant in, in NANOSECONDS since the epoch,
/// the unit that encoding counts a time in.
const TIME_UNIX_NANO_MEMBER: &str = "timeUnixNano";

/// The member an OTLP log record carries the instant it was OBSERVED at, in the same unit:
/// the instant a record states for itself when it states no `timeUnixNano`.
const OBSERVED_TIME_UNIX_NANO_MEMBER: &str = "observedTimeUnixNano";

/// Milliseconds in one second, the factor between the two units the two forms spell an
/// instant in.
const MILLIS_PER_SECOND: f64 = 1_000.0;

/// Milliseconds in one nanosecond, the factor between the unit the OpenTelemetry form counts
/// an instant in and the milliseconds a log event's timestamp is counted in.
const MILLIS_PER_NANOSECOND: f64 = 1.0 / 1_000_000.0;

/// The body a request every record of which was ingested is answered with: an empty JSON
/// object, carrying nothing because there is nothing to report.
pub const ACCEPTED_BODY: &str = "{}";

/// The message a request NO record of which could be ingested is refused with — the batch
/// carried nothing to ingest, so there is no partial success to report.
pub const ALL_EVENTS_INVALID_MESSAGE: &str = "All events were invalid";

/// Which of the forms a path addresses, or `None` for a path none of them is served at.
#[must_use]
pub fn endpoint_at(path: &str) -> Option<Endpoint> {
    match path.trim_end_matches('/') {
        HTTP_LOG_COLLECTOR_PATH => Some(Endpoint::HttpLogCollector),
        STRUCTURED_JSON_PATH => Some(Endpoint::StructuredJson),
        OPEN_TELEMETRY_PATH => Some(Endpoint::OpenTelemetry),
        _ => None,
    }
}

/// One of the forms the HTTP ingestion endpoints serve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Endpoint {
    /// `event`-carrying records, one object or an array of them.
    HttpLogCollector,
    /// Records spelling a timestamp and a message.
    StructuredJson,
    /// One OTLP logs export document, whose log records are the events to ingest.
    OpenTelemetry,
}

/// The log group and log stream a request's query string names, or `None` when it names
/// neither or only one: a record is ingested into a stream of a group, so a request that
/// does not say which addresses no destination at all.
///
/// Both values are percent-decoded, because a log group name carries characters a client
/// escapes in a query string (`^[\.\-_/#A-Za-z0-9]+$` admits `/` and `#`).
#[must_use]
pub fn destination(query: Option<&str>) -> Option<Destination> {
    let query = query?;
    let read = |name| {
        ::service_common::query_parameter(query, name)
            .map(::smithy_server_runtime::percent_decode)
            .filter(|value| !value.is_empty())
    };
    named_destination(read(LOG_GROUP_PARAMETER), read(LOG_STREAM_PARAMETER))
}

/// The destination two named values address, or `None` when either is absent or blank: a
/// record is ingested into a stream OF a group, so one name alone addresses nothing.
///
/// The one rule both the headers ([`LOG_GROUP_HEADER`], [`LOG_STREAM_HEADER`]) and the
/// query parameters are read through, so a client that addresses its destination either way
/// reaches the same resource and is refused the same way.
#[must_use]
pub fn named_destination<S: Into<String>>(
    log_group: Option<S>,
    log_stream: Option<S>,
) -> Option<Destination> {
    let named = |value: Option<S>| {
        value
            .map(Into::into)
            .map(|value: String| value.trim().to_string())
            .filter(|value| !value.is_empty())
    };
    Some(Destination {
        log_group: named(log_group)?,
        log_stream: named(log_stream)?,
    })
}

/// Where a request's records are ingested.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Destination {
    /// The log group the records are ingested into.
    pub log_group: String,
    /// The log stream of that group they are appended to.
    pub log_stream: String,
}

/// One record a request's body carried: the message to store, and the instant the record
/// states for itself, where it states one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    /// The record's own instant, in milliseconds since the epoch; `None` for a record that
    /// states none, which is ingested at the instant it arrived.
    pub timestamp: Option<i64>,
    /// The log event the record carries.
    pub message: String,
}

/// What a body was read as: the records it carried, and how many of its records carried no
/// log event at all.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReadRecords {
    /// The records that carry a log event, in the order the body listed them.
    pub records: Vec<Record>,
    /// How many of the body's records carry no log event, so there is nothing of them to
    /// ingest. A body that is not readable as JSON at all counts as one such record: it
    /// carried no event either.
    pub unreadable: usize,
}

/// The records `body` carries, read in the form `endpoint` serves.
///
/// A body is a single record or an ARRAY of them in the two record-shaped forms, because a
/// client ships one event or a batch of them over the same endpoint; in the OpenTelemetry
/// form it is ONE export document and the records are the log records nested inside it
/// ([`open_telemetry_log_records`]).
///
/// At most one record PAST [`MAX_RECORDS_PER_REQUEST`] is read: a batch that carries more
/// than the maximum is refused whole, so one record past it is all that has to be seen and
/// a longer body is not walked.
#[must_use]
pub fn read_records(endpoint: Endpoint, body: &[u8]) -> ReadRecords {
    let Ok(document) = ::serde_json::from_slice::<Value>(body) else {
        return ReadRecords {
            records: Vec::new(),
            unreadable: 1,
        };
    };
    let items: Vec<&Value> = match endpoint {
        Endpoint::OpenTelemetry => open_telemetry_log_records(&document),
        Endpoint::HttpLogCollector | Endpoint::StructuredJson => match &document {
            Value::Array(items) => items.iter().take(MAX_RECORDS_PER_REQUEST + 1).collect(),
            other => vec![other],
        },
    };
    let mut read = ReadRecords::default();
    for item in items {
        match record_of(endpoint, item) {
            Some(record) => read.records.push(record),
            None => read.unreadable += 1,
        }
    }
    read
}

/// The log records one OTLP logs export document carries, in the order it lists them.
///
/// The encoding nests them three levels deep — `resourceLogs[].scopeLogs[].logRecords[]` —
/// so one request ships the records of several resources and scopes together; what is
/// ingested is every log record of all of them, because each one is one log event. A
/// document that nests no log record carries nothing to ingest, which is the empty list.
fn open_telemetry_log_records(document: &Value) -> Vec<&Value> {
    nested_items(document, RESOURCE_LOGS_MEMBER)
        .iter()
        .flat_map(|resource| nested_items(resource, SCOPE_LOGS_MEMBER))
        .flat_map(|scope| nested_items(scope, LOG_RECORDS_MEMBER))
        .take(MAX_RECORDS_PER_REQUEST + 1)
        .collect()
}

/// The entries one member of a document lists, or nothing when the member is absent or is
/// not a list — a level of the OpenTelemetry nesting that names none carries no record.
fn nested_items<'a>(value: &'a Value, member: &str) -> &'a [Value] {
    value
        .get(member)
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice)
}

/// One record, or `None` when the item carries no log event in the form `endpoint` serves.
///
/// What is STORED is the record itself, as its own JSON text: each form ships one document
/// per log event — the `event` member carries the event in one, the `message` member in the
/// next and the `body` member in the OpenTelemetry one, beside the instant the record states
/// — so what a reader gets back is that document, canonically spelled, and not one member
/// lifted out of it. The member order the record arrived in is part of the document and is
/// kept (this crate reads JSON with `serde_json`'s `preserve_order`).
fn record_of(endpoint: Endpoint, item: &Value) -> Option<Record> {
    let object = item.as_object()?;
    let timestamp = match endpoint {
        // A record that carries no log event at all carries nothing to ingest, whichever
        // member this form spells one in.
        Endpoint::HttpLogCollector => {
            object.get(EVENT_MEMBER)?;
            // "time" is seconds since the epoch in this form, and a fractional second is
            // part of the instant it states, so it is read as such and scaled to the
            // milliseconds a log event's timestamp is counted in.
            object.get(TIME_MEMBER).and_then(seconds_as_millis)
        }
        Endpoint::StructuredJson => {
            object.get(MESSAGE_MEMBER)?;
            // "timestamp" is MILLISECONDS since the epoch in this form, as
            // `InputLogEvent$timestamp` counts them — "the number of milliseconds after Jan
            // 1, 1970 00:00:00 UTC" — and the spellings a client may write it in are the
            // ones the other form's instant admits: a JSON number, or a string spelling
            // one.
            object.get(TIMESTAMP_MEMBER).and_then(millis_of)
        }
        Endpoint::OpenTelemetry => {
            object.get(BODY_MEMBER)?;
            // Both instants this form spells are NANOSECONDS since the epoch, and the
            // observed one is what a record that states no `timeUnixNano` states for
            // itself; a client writes either as a JSON number or, as the encoding spells a
            // 64-bit integer, as a string.
            object
                .get(TIME_UNIX_NANO_MEMBER)
                .and_then(nanos_as_millis)
                .or_else(|| {
                    object
                        .get(OBSERVED_TIME_UNIX_NANO_MEMBER)
                        .and_then(nanos_as_millis)
                })
        }
    };
    Some(Record {
        timestamp,
        message: item.to_string(),
    })
}

/// A number of seconds since the epoch as the milliseconds a log event's timestamp counts,
/// read from a JSON number or from a string spelling one.
fn seconds_as_millis(value: &Value) -> Option<i64> {
    scaled(value, MILLIS_PER_SECOND)
}

/// A number of MILLISECONDS since the epoch, read from the same spellings
/// [`seconds_as_millis`] reads its seconds from, so the two forms differ in the UNIT their
/// member counts and in nothing else.
fn millis_of(value: &Value) -> Option<i64> {
    scaled(value, 1.0)
}

/// A number of NANOSECONDS since the epoch as those same milliseconds, read from the same
/// spellings: the OpenTelemetry form differs from the other two in its UNIT alone.
fn nanos_as_millis(value: &Value) -> Option<i64> {
    scaled(value, MILLIS_PER_NANOSECOND)
}

/// `value`, read as a number or as a string spelling one, multiplied by `factor` and
/// rounded to the millisecond — so a fractional second a record states is part of the
/// instant it states.
fn scaled(value: &Value, factor: f64) -> Option<i64> {
    let stated = match value {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.parse::<f64>().ok(),
        _ => None,
    }?;
    if !stated.is_finite() {
        return None;
    }
    let millis = (stated * factor).round();
    (millis >= i64::MIN as f64 && millis <= i64::MAX as f64).then_some(millis as i64)
}

/// The body a request SOME of whose records were ingested and some refused is answered
/// with: the partial success of that batch, naming how many of its records were refused
/// and why.
///
/// RECORDED behaviour, for the member spelling and for what `errorMessage` carries: the
/// recorded answer to a batch carrying one record outside the ingestion window reports
/// `partialSuccess` with the refused-record count, and its `errorMessage` is read by a
/// client as a JSON DOCUMENT — the recorded check parses that member and reads the count of
/// the class that refused the record out of it ([`window_refusal_document`]). So the reason
/// travels there as the document it is rather than as a sentence about it; neither the
/// embedded-document form nor the member names are this emulator's invention, and the model
/// expresses neither (no shape of it describes these endpoints at all).
#[must_use]
pub fn partial_success_body(rejected: usize, error_message: &str) -> String {
    ::serde_json::json!({
        "partialSuccess": {
            "rejectedLogRecords": rejected,
            "errorMessage": error_message,
        }
    })
    .to_string()
}

/// WHY a partially successful batch refused the records it refused: how many records EACH
/// class of the ingestion window refused, counted per class and zero where the batch
/// carried none of that class.
///
/// The ingestion window is what refuses a record of a well-formed batch — "events more than
/// 2 hours in the future are rejected while processing remaining valid events", "events
/// older than 14 days or preceding the log group's retention period are rejected"
/// (`PutLogEvents`) — and the modelled `RejectedLogEventsInfo` that operation answers with
/// reports those three classes as the INDEX runs of the batch they refused. A batch handed
/// over in timestamp order makes each run contiguous and orders the runs — an event older
/// than the 14-day horizon precedes one the retention period alone refuses, which precedes
/// every accepted one, which precedes one ahead of the clock — so the counts are read off
/// those indexes rather than by judging the window a second time here
/// ([`WindowRefusals::of`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WindowRefusals {
    /// How many records were older than the ingestion horizon
    /// (`RejectedLogEventsInfo$tooOldLogEventEndIndex`).
    pub too_old: usize,
    /// How many records preceded the log group's retention period
    /// (`RejectedLogEventsInfo$expiredLogEventEndIndex`).
    pub expired: usize,
    /// How many records were further ahead of the clock than the window admits
    /// (`RejectedLogEventsInfo$tooNewLogEventStartIndex`).
    pub too_new: usize,
}

impl WindowRefusals {
    /// The per-class counts of a batch of `handed_over` records the ingestion answered
    /// `rejected` for, which is `None` for a batch it refused nothing of.
    #[must_use]
    pub fn of(
        handed_over: usize,
        rejected: Option<&crate::ops::types::RejectedLogEventsInfo>,
    ) -> Self {
        let index = |index: Option<i32>| {
            index
                .and_then(|index| usize::try_from(index).ok())
                .map(|index| index.min(handed_over))
        };
        let too_old = index(rejected.and_then(|info| info.too_old_log_event_end_index));
        let expired = index(rejected.and_then(|info| info.expired_log_event_end_index));
        let too_new = index(rejected.and_then(|info| info.too_new_log_event_start_index));
        let too_old_count = too_old.unwrap_or(0);
        Self {
            too_old: too_old_count,
            // The expired run FOLLOWS the too-old one, and the index reported is its
            // exclusive end, so the count is what that end adds past the too-old run.
            expired: expired.unwrap_or(0).saturating_sub(too_old_count),
            // The too-new run runs from its start index to the end of the batch.
            too_new: handed_over.saturating_sub(too_new.unwrap_or(handed_over)),
        }
    }

    /// How many records the window refused in all.
    #[must_use]
    pub fn total(&self) -> usize {
        self.too_old + self.expired + self.too_new
    }
}

/// The per-class counts as the document a partial success reports them in, each class
/// named for the `RejectedLogEventsInfo` member that reports it and carrying the count of
/// the records it refused, zero included: a client reads which classes refused anything by
/// reading which counts are not zero.
///
/// RECORDED behaviour for the three member names and for the zeros: the recorded answer
/// carries this document as the text of `partialSuccess.errorMessage`, and the recorded
/// check reads the count of the class that refused its record out of it after dropping the
/// members whose count is zero — so a class that refused nothing is PRESENT and zero rather
/// than absent. Each name is that of the `RejectedLogEventsInfo` member reporting the same
/// class, which is where the counts come from ([`WindowRefusals::of`]).
#[must_use]
pub fn window_refusal_document(refusals: &WindowRefusals) -> String {
    ::serde_json::json!({
        "tooNewLogEventCount": refusals.too_new,
        "tooOldLogEventCount": refusals.too_old,
        "expiredLogEventCount": refusals.expired,
    })
    .to_string()
}

/// The message a request carrying more records than one batch may hold is refused with.
///
/// No recording covers what THESE endpoints word an oversized batch with, so what they
/// state is the sentence the operation whose batch bound it is documents for the same bound:
/// `PutLogEvents`' "the maximum number of log events in a batch is 10,000" — including the
/// thousands separator that sentence writes it with ([`MAX_RECORDS_SPELLED`]).
#[must_use]
pub fn too_many_records_message() -> String {
    format!("The maximum number of log events in a batch is {MAX_RECORDS_SPELLED}.")
}

/// How the documented sentence SPELLS that maximum: `PutLogEvents`' own "the maximum number
/// of log events in a batch is 10,000" writes it with a thousands separator, so the message
/// reproduces that spelling rather than the digits of the constant. The two are the same
/// number, which [`tests::the_spelled_maximum_is_the_bound_it_states`] pins.
const MAX_RECORDS_SPELLED: &str = "10,000";

/// The most records one request may carry, which is what one batch of the ingestion may
/// hold: `PutLogEvents`' documented "the maximum number of log events in a batch is
/// 10,000", which is also the modelled `@length 1..10000` of its `logEvents`
/// (`smithy_query(query="shape", shape="InputLogEvents")`). A request carrying more is
/// refused whole rather than ingested in part, and nothing past it is read.
pub const MAX_RECORDS_PER_REQUEST: usize = 10_000;

/// The message a request whose query names no destination is refused with: a record is
/// ingested into a log stream of a log group, so a request that names neither addresses
/// nothing — the refusal names the two parameters that address one rather than reporting
/// the batch as empty, which it need not be.
#[must_use]
pub fn no_destination_message() -> String {
    format!(
        "The query string must name the destination of the log events: \
         {LOG_GROUP_PARAMETER} and {LOG_STREAM_PARAMETER}."
    )
}

/// A one-member JSON object carrying `message`, which is the envelope these endpoints
/// report a refusal in.
#[must_use]
pub fn message_body(message: &str) -> String {
    ::serde_json::json!({ "message": message }).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the spelling the refusal states the maximum with is that maximum.
    #[test]
    fn the_spelled_maximum_is_the_bound_it_states() {
        let spelled: String = MAX_RECORDS_SPELLED.chars().filter(|c| *c != ',').collect();
        let read = spelled.parse::<usize>();
        assert_eq!(
            read,
            Ok(MAX_RECORDS_PER_REQUEST),
            "case the documented spelling: expected {MAX_RECORDS_PER_REQUEST}, got {read:?}"
        );
        let message = too_many_records_message();
        assert!(
            message.contains(MAX_RECORDS_SPELLED),
            "case the refusal: expected the documented spelling, got {message:?}"
        );
    }

    /// One row of the refusal-count table: the case, how many records were handed over,
    /// the indexes the ingestion reported, and the per-class counts they stand for.
    type RefusalRow = (
        &'static str,
        usize,
        (Option<i32>, Option<i32>, Option<i32>),
        WindowRefusals,
    );

    /// RULE: each class of the ingestion window refused the records its index run covers —
    /// the too-old and expired runs end at their exclusive end index, the expired run
    /// following the too-old one, and the too-new run reaches the end of the batch.
    #[test]
    fn each_window_class_counts_the_records_its_run_covers() {
        let rows: &[RefusalRow] = &[
            (
                "nothing refused",
                3,
                (None, None, None),
                WindowRefusals::default(),
            ),
            (
                "one too new of two",
                2,
                (Some(1), None, None),
                WindowRefusals {
                    too_new: 1,
                    ..WindowRefusals::default()
                },
            ),
            (
                "two too old of five",
                5,
                (None, Some(2), None),
                WindowRefusals {
                    too_old: 2,
                    ..WindowRefusals::default()
                },
            ),
            (
                "one expired of four",
                4,
                (None, None, Some(1)),
                WindowRefusals {
                    expired: 1,
                    ..WindowRefusals::default()
                },
            ),
            (
                "two too old then one expired, and one too new, of six",
                6,
                (Some(5), Some(2), Some(3)),
                WindowRefusals {
                    too_old: 2,
                    expired: 1,
                    too_new: 1,
                },
            ),
            (
                "every record refused",
                2,
                (None, Some(2), None),
                WindowRefusals {
                    too_old: 2,
                    ..WindowRefusals::default()
                },
            ),
        ];
        for (case, handed_over, (too_new, too_old, expired), expected) in rows {
            let reported = crate::ops::types::RejectedLogEventsInfo {
                too_new_log_event_start_index: *too_new,
                too_old_log_event_end_index: *too_old,
                expired_log_event_end_index: *expired,
            };
            let actual = WindowRefusals::of(*handed_over, Some(&reported));
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
            assert_eq!(
                actual.total(),
                expected.too_old + expected.expired + expected.too_new,
                "case {case:?}: the total counts every class, got {actual:?}"
            );
        }
        let nothing = WindowRefusals::of(2, None);
        assert_eq!(
            nothing,
            WindowRefusals::default(),
            "case no report at all: expected no refusal, got {nothing:?}"
        );
    }

    /// RULE: only the paths the endpoints are served at name an endpoint, and a
    /// trailing slash addresses the same one.
    #[test]
    fn only_the_served_paths_name_an_endpoint() {
        let rows: &[(&str, Option<Endpoint>)] = &[
            (HTTP_LOG_COLLECTOR_PATH, Some(Endpoint::HttpLogCollector)),
            ("/services/collector/event/", Some(Endpoint::HttpLogCollector)),
            (STRUCTURED_JSON_PATH, Some(Endpoint::StructuredJson)),
            ("/ingest/json/", Some(Endpoint::StructuredJson)),
            (OPEN_TELEMETRY_PATH, Some(Endpoint::OpenTelemetry)),
            ("/v1/logs/", Some(Endpoint::OpenTelemetry)),
            ("/services/collector", None),
            ("/services/collector/raw", None),
            ("/ingest", None),
            ("/v1", None),
            ("/v1/metrics", None),
            ("/", None),
        ];
        for (path, expected) in rows {
            let actual = endpoint_at(path);
            assert_eq!(
                actual, *expected,
                "case {path:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: a destination is the log group AND the log stream the query names, each
    /// percent-decoded; a query naming only one of them addresses nothing.
    #[test]
    fn a_destination_is_both_names_the_query_carries() {
        let both = destination(Some("logGroup=a%2Fb&logStream=s%23one"));
        assert_eq!(
            both,
            Some(Destination {
                log_group: "a/b".to_string(),
                log_stream: "s#one".to_string(),
            }),
            "case both: expected the decoded pair, got {both:?}"
        );
        for query in [
            Some("logGroup=a"),
            Some("logStream=s"),
            Some("logGroup=&logStream=s"),
            Some(""),
            None,
        ] {
            let actual = destination(query);
            assert!(
                actual.is_none(),
                "case {query:?}: expected no destination, got {actual:?}"
            );
        }
    }

    /// RULE: a body is one record or an array of them, and a record with no log event is
    /// counted as carrying nothing to ingest.
    #[test]
    fn a_body_is_one_record_or_an_array_of_them() {
        let single = read_records(Endpoint::HttpLogCollector, br#"{"event": "one"}"#);
        assert_eq!(
            single,
            ReadRecords {
                records: vec![Record {
                    timestamp: None,
                    message: r#"{"event":"one"}"#.to_string()
                }],
                unreadable: 0,
            },
            "case single object: got {single:?}"
        );
        let array = read_records(
            Endpoint::HttpLogCollector,
            br#"[{"event": "one", "time": 1.5}, {"host": "h"}, {"event": {"a": 1}}]"#,
        );
        assert_eq!(
            array.records,
            vec![
                Record {
                    timestamp: Some(1_500),
                    message: r#"{"event":"one","time":1.5}"#.to_string()
                },
                Record {
                    timestamp: None,
                    message: r#"{"event":{"a":1}}"#.to_string()
                },
            ],
            "case array: got {array:?}"
        );
        assert_eq!(array.unreadable, 1, "case array: got {array:?}");
        let structured = read_records(
            Endpoint::StructuredJson,
            br#"[{"timestamp": 7, "message": "m"}, {"timestamp": 8}]"#,
        );
        assert_eq!(
            structured,
            ReadRecords {
                records: vec![Record {
                    timestamp: Some(7),
                    message: r#"{"timestamp":7,"message":"m"}"#.to_string()
                }],
                unreadable: 1,
            },
            "case structured: got {structured:?}"
        );
        // The instant is read from the same spellings in both forms, in each form's own
        // unit: a JSON number, a fractional one, or a string spelling one.
        let spellings = read_records(
            Endpoint::StructuredJson,
            br#"[{"timestamp": "9", "message": "a"}, {"timestamp": 10.4, "message": "b"}]"#,
        );
        assert_eq!(
            spellings
                .records
                .iter()
                .map(|record| record.timestamp)
                .collect::<Vec<_>>(),
            vec![Some(9), Some(10)],
            "case structured spellings: got {spellings:?}"
        );
        let unreadable = read_records(Endpoint::StructuredJson, b"not json");
        assert_eq!(
            unreadable,
            ReadRecords {
                records: Vec::new(),
                unreadable: 1,
            },
            "case unreadable: got {unreadable:?}"
        );
    }

    /// RULE: an OpenTelemetry body is ONE export document, whose records are every log
    /// record nested under its resources and scopes; a record with no body carries nothing
    /// to ingest, and a document nesting no log record carries nothing at all.
    #[test]
    fn an_open_telemetry_body_is_every_log_record_it_nests() {
        let nested = read_records(
            Endpoint::OpenTelemetry,
            br#"{"resourceLogs": [
                   {"scopeLogs": [
                      {"logRecords": [
                         {"timeUnixNano": "1700000000000000000", "body": {"stringValue": "a"}},
                         {"observedTimeUnixNano": 1700000000500000000, "body": {"stringValue": "b"}},
                         {"timeUnixNano": "1700000001000000000"}
                      ]},
                      {"logRecords": [{"body": {"stringValue": "c"}}]}
                   ]},
                   {"scopeLogs": [{"logRecords": [{"body": {"stringValue": "d"}}]}]}
                 ]}"#,
        );
        assert_eq!(
            nested
                .records
                .iter()
                .map(|record| record.timestamp)
                .collect::<Vec<_>>(),
            vec![
                Some(1_700_000_000_000),
                Some(1_700_000_000_500),
                None,
                None
            ],
            "case nested records: expected each record's own instant in milliseconds, \
             got {nested:?}"
        );
        assert_eq!(
            nested
                .records
                .iter()
                .map(|record| record.message.as_str())
                .collect::<Vec<_>>(),
            vec![
                r#"{"timeUnixNano":"1700000000000000000","body":{"stringValue":"a"}}"#,
                r#"{"observedTimeUnixNano":1700000000500000000,"body":{"stringValue":"b"}}"#,
                r#"{"body":{"stringValue":"c"}}"#,
                r#"{"body":{"stringValue":"d"}}"#,
            ],
            "case nested records: expected the documents they arrived as, got {nested:?}"
        );
        assert_eq!(
            nested.unreadable, 1,
            "case nested records: the record with no body carries nothing, got {nested:?}"
        );
        for (case, body) in [
            ("no resourceLogs at all", &br#"{}"#[..]),
            ("an empty resourceLogs", &br#"{"resourceLogs": []}"#[..]),
            (
                "a scope nesting no log record",
                &br#"{"resourceLogs": [{"scopeLogs": [{"logRecords": []}]}]}"#[..],
            ),
        ] {
            let read = read_records(Endpoint::OpenTelemetry, body);
            assert_eq!(
                read,
                ReadRecords::default(),
                "case {case}: expected nothing to ingest, got {read:?}"
            );
        }
    }

    /// RULE: a destination is the log group AND the log stream a request names, whether it
    /// names them in the headers or in the query string; one name alone addresses nothing.
    #[test]
    fn a_destination_is_both_names_however_they_are_given() {
        let named = named_destination(Some("group"), Some("stream"));
        assert_eq!(
            named,
            Some(Destination {
                log_group: "group".to_string(),
                log_stream: "stream".to_string(),
            }),
            "case both headers: expected the named pair, got {named:?}"
        );
        for (case, group, stream) in [
            ("no group", None, Some("stream")),
            ("no stream", Some("group"), None),
            ("neither", None, None),
            ("a blank group", Some("  "), Some("stream")),
            ("a blank stream", Some("group"), Some("")),
        ] {
            let actual = named_destination(group, stream);
            assert!(
                actual.is_none(),
                "case {case}: expected no destination, got {actual:?}"
            );
        }
    }
}
