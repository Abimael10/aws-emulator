// SPDX-License-Identifier: CC0-1.0

//! What a GraphQL API with a `logConfig` WRITES to CloudWatch Logs about each request its
//! `GRAPHQL` endpoint served ([`super::query`]).
//!
//! `LogConfig` is not a label either: it is "The Amazon CloudWatch Logs configuration" of the
//! API, naming "The service role that AppSync assumes to publish to CloudWatch logs in your
//! account" (`LogConfig$cloudWatchLogsRoleArn`) and the level at which the API's own fields
//! are captured (`LogConfig$fieldLogLevel`). So an API that configures one has records in
//! CloudWatch Logs after a request, and every record of one request carries the REQUEST ID
//! the response's `x-amzn-RequestId` field carries, which is what correlates a client's
//! answer with what was logged about producing it.
//!
//! # Which records one request produces
//!
//! Two kinds, exactly as the member that configures them divides them:
//!
//! * REQUEST-level records — [`REQUEST_SUMMARY_LOG_TYPE`] and [`EXECUTION_SUMMARY_LOG_TYPE`]
//!   — are of the request as a whole: the status it was answered with, how long it took and
//!   the span the execution ran over. They are produced by the API having a `logConfig` at
//!   all, because nothing in `fieldLogLevel` is about them: that member states what is
//!   captured "for the fields".
//! * FIELD-level records are what `fieldLogLevel` governs, in its own words
//!   (`smithy_query(query="doc",shape="LogConfig",member="fieldLogLevel")`):
//!   * `NONE` — "No field-level logs are captured";
//!   * `ERROR` — "for the fields that are in error": the field-level errors and "the
//!     generated request/response functions that got resolved for error fields";
//!   * `ALL` — "for all fields in the query": "Field-level tracing information" and "the
//!     generated request/response functions that got resolved for each field".
//!
//!   `FieldLogLevel` carries two further values, `INFO` and `DEBUG`
//!   (`smithy_query(query="shape",shape="FieldLogLevel")`), which the member documents no
//!   rule of its own for; each captures the fields the level above `ERROR` does, so a level
//!   that is not `NONE` and not `ERROR` captures every field.
//!
//! `excludeVerboseContent` is "Set to TRUE to exclude sections that contain information such
//! as headers, context, and evaluated mapping templates, regardless of logging level", so it
//! decides whether a field-level record carries the resolver's own context and the request
//! and response its steps generated — and it holds at every level, the whole point of
//! "regardless of logging level". The context a record carries leaves the request's own
//! fields out whatever the setting ([`super::execution::FieldTrace`] states why), so no
//! record carries the authorization material a caller presented.
//!
//! # Where they go
//!
//! Into the log group AppSync publishes an API's logs under, `/aws/appsync/apis/<apiId>`
//! ([`log_group`]), through the tree's one vended-logs write
//! (`service_common::cloudwatch_logs::delivered_over`): the group is one the delivering
//! service publishes under its own name, so it is created when the account does not hold it
//! yet, and the records are log events a reader then finds with CloudWatch Logs' own
//! readers. The stream is named by the REQUEST, so a client holding the `x-amzn-RequestId`
//! of an answer can reach what was logged about that answer directly.
//!
//! The group itself comes into existence with the FIRST record written into it, not when
//! logging is configured: `logConfig` states the role "that AppSync assumes to publish to
//! CloudWatch logs in your account" and the level the API's fields are captured at, and
//! neither of them is a group of the account's. So an API that has logged nothing is an API
//! CloudWatch Logs holds no group for, and whoever creates a group under that name — a
//! client, or a template that owns it so a metric filter can attach to it — is creating a
//! group the account does not hold yet.
//!
//! Delivery is BEST EFFORT, as every vended-logs delivery is: a write CloudWatch Logs
//! refuses, or an emulator assembled without it, loses the records and never fails the
//! request the endpoint has already answered.

use ::std::time::{Duration, SystemTime};

use ::serde_json::{json, Map as JsonMap, Value as Json};

use super::execution::{Answer, FieldTrace, GeneratedStep};
use super::PlaneContext;
use crate::state::ComAmazonawsAppsyncAwsdeepdishcontrolplaneserviceState as ServiceStore;

/// The log group an API's own logs go to: AppSync publishes them under the API's id, so a
/// reader asking CloudWatch Logs for one API's logs names the group of that API alone.
#[must_use]
pub fn log_group(api_id: &str) -> String {
    format!("/aws/appsync/apis/{api_id}")
}

/// The `logType` of each record, which is what a reader selects one kind of record by.
pub const REQUEST_SUMMARY_LOG_TYPE: &str = "RequestSummary";
pub const EXECUTION_SUMMARY_LOG_TYPE: &str = "ExecutionSummary";
pub const TRACING_LOG_TYPE: &str = "Tracing";
pub const REQUEST_MAPPING_LOG_TYPE: &str = "RequestMapping";
pub const RESPONSE_MAPPING_LOG_TYPE: &str = "ResponseMapping";

/// The `FieldLogLevel` values this module reads by name; the rest of them capture the fields
/// `ERROR` does not ([`FieldLevel`]).
const NONE_LEVEL: &str = "NONE";
const ERROR_LEVEL: &str = "ERROR";

/// Which fields a level captures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldLevel {
    /// "No field-level logs are captured."
    None,
    /// The fields "that are in error".
    ErrorFields,
    /// "All fields in the query."
    EveryField,
}

impl FieldLevel {
    /// The level `field_log_level` names.
    #[must_use]
    pub fn of(field_log_level: &str) -> Self {
        match field_log_level {
            NONE_LEVEL => Self::None,
            ERROR_LEVEL => Self::ErrorFields,
            _ => Self::EveryField,
        }
    }

    /// Whether a field whose resolution ended in `error` is captured at this level.
    fn captures(self, error: Option<&String>) -> bool {
        match self {
            Self::None => false,
            Self::ErrorFields => error.is_some(),
            Self::EveryField => true,
        }
    }
}

/// How one API logs: the level its fields are captured at and whether a record carries the
/// verbose sections, or `None` for an API that configures no logging at all.
#[derive(Debug, Clone, Copy)]
pub struct Logging {
    /// The level `fieldLogLevel` names.
    pub level: FieldLevel,
    /// `excludeVerboseContent`, as the member spells the exclusion.
    pub excludes_verbose_content: bool,
}

/// How `api_id` logs, read from the API's own record, or `None` when it carries no
/// `logConfig` — which is an API nothing is logged for.
#[must_use]
pub fn logging(store: &ServiceStore, api_id: &str) -> Option<Logging> {
    let config = store.graphql_apis.get(api_id)?.log_config.as_ref()?;
    Some(Logging {
        level: FieldLevel::of(&config.field_log_level),
        excludes_verbose_content: config.exclude_verbose_content,
    })
}

/// Whether `api_id` captures FIELD-level logs, which is what the execution asks before it
/// collects a trace of each field it resolves ([`super::execution`]).
#[must_use]
pub fn captures_field_logs(store: &ServiceStore, api_id: &str) -> bool {
    logging(store, api_id).is_some_and(|logging| logging.level != FieldLevel::None)
}

/// One request this endpoint answered, as the API's logs report it.
#[derive(Debug)]
pub struct Served<'a> {
    /// The request id the answer carries in its `x-amzn-RequestId` field, which every record
    /// of this request is stamped with.
    pub request_id: &'a str,
    /// The status the request was answered with.
    pub status: u16,
    /// When the endpoint began answering it.
    pub started: SystemTime,
    /// How long answering it took.
    pub latency: Duration,
    /// What executing the document came to, or `None` for a request that was refused before
    /// any of it was executed.
    pub answer: Option<&'a Answer>,
}

/// Write what the API logs about one request it served into that API's log group.
///
/// Reads the API's `logConfig` as it stands NOW, so a client that changed the level with
/// `UpdateGraphqlApi` is logged at the level in force when the request was served. Call it
/// with NO store guard held — it awaits.
pub async fn delivered(context: &PlaneContext, served: &Served<'_>) {
    let Some(logging) = logging(&context.state.lock(), &context.api_id) else {
        return;
    };
    let records = records(&context.api_id, logging, served);
    if records.is_empty() {
        return;
    }
    let group = log_group(&context.api_id);
    if !::service_common::cloudwatch_logs::delivered_over(
        &context.callback,
        &context.region,
        &group,
        served.request_id,
        &records,
    )
    .await
    {
        ::tracing::debug!(
            api_id = %context.api_id,
            log_group = %group,
            "what this API logs about a request could not be delivered to CloudWatch Logs"
        );
    }
}

/// The records one served request produces, in the order they are written: the request-level
/// pair, then each captured field's own.
#[must_use]
pub fn records(api_id: &str, logging: Logging, served: &Served<'_>) -> Vec<String> {
    let mut written: Vec<Json> = vec![
        request_summary(api_id, served),
        execution_summary(api_id, served),
    ];
    let traces = served
        .answer
        .map(|answer| answer.traces.as_slice())
        .unwrap_or_default();
    for trace in traces {
        if !logging.level.captures(trace.error.as_ref()) {
            continue;
        }
        written.extend(field_records(api_id, logging, served, trace));
    }
    // The records as the events they travel as, under what one request may DELIVER: the
    // request-level pair is always written, and the field-level records are written while
    // there is room for them, so what one request puts in the group is bounded by this rather
    // than by how many fields it resolved or how large the values they carried were.
    let mut delivered: Vec<String> = Vec::with_capacity(written.len());
    let mut bytes: usize = 0;
    for record in &written {
        let text = record.to_string();
        bytes = bytes.saturating_add(text.len());
        if bytes > MAX_DELIVERED_BYTES && delivered.len() >= REQUEST_LEVEL_RECORDS {
            ::tracing::debug!(
                api_id = %api_id,
                request_id = %served.request_id,
                "this request logged more than its API delivers in one batch, so the records \
                 past that are not delivered"
            );
            break;
        }
        delivered.push(text);
    }
    delivered
}

/// How many records one request DELIVERS, in bytes, and how many of them are the
/// request-level pair that is written whatever the rest comes to.
///
/// An emulator bound, well inside what one `PutLogEvents` batch carries: a record holds
/// request-derived values, so what a request may put in the group is bounded here rather than
/// refused downstream once the memory has been taken.
const MAX_DELIVERED_BYTES: usize = 512 * 1024;
const REQUEST_LEVEL_RECORDS: usize = 2;

/// The request-level record of the request as a whole: what it was answered with and how
/// long that took.
fn request_summary(api_id: &str, served: &Served<'_>) -> Json {
    json!({
        "logType": REQUEST_SUMMARY_LOG_TYPE,
        "requestId": served.request_id,
        "graphQLAPIId": api_id,
        "statusCode": served.status,
        "latency": nanos(served.latency),
    })
}

/// The request-level record of the EXECUTION: the span it ran over, and how many errors the
/// result carries.
fn execution_summary(api_id: &str, served: &Served<'_>) -> Json {
    let errors = served
        .answer
        .map(|answer| answer.errors.len())
        .unwrap_or_default();
    json!({
        "logType": EXECUTION_SUMMARY_LOG_TYPE,
        "requestId": served.request_id,
        "graphQLAPIId": api_id,
        "startTime": timestamp(served.started),
        "endTime": timestamp(served.started + served.latency),
        "duration": nanos(served.latency),
        "errorCount": errors,
    })
}

/// The records ONE captured field produces: its tracing information at a level that captures
/// every field, and the request and response each of its steps generated.
fn field_records(
    api_id: &str,
    logging: Logging,
    served: &Served<'_>,
    trace: &FieldTrace,
) -> Vec<Json> {
    let mut records: Vec<Json> = Vec::new();
    // "Field-level tracing information" is what the ALL level adds over ERROR, which logs
    // the error fields' own errors and generated functions.
    if logging.level == FieldLevel::EveryField {
        let mut tracing = field_envelope(api_id, served, trace);
        insert(&mut tracing, "logType", json!(TRACING_LOG_TYPE));
        insert(&mut tracing, "returnType", json!(trace.return_type));
        insert(&mut tracing, "startOffset", json!(nanos(trace.start_offset)));
        insert(&mut tracing, "duration", json!(nanos(trace.duration)));
        records.push(Json::Object(tracing));
    }
    for step in &trace.steps {
        records.push(step_record(
            api_id,
            logging,
            served,
            trace,
            step,
            REQUEST_MAPPING_LOG_TYPE,
        ));
        records.push(step_record(
            api_id,
            logging,
            served,
            trace,
            step,
            RESPONSE_MAPPING_LOG_TYPE,
        ));
    }
    records
}

/// One step's generated request or response, as a field-level record.
fn step_record(
    api_id: &str,
    logging: Logging,
    served: &Served<'_>,
    trace: &FieldTrace,
    step: &GeneratedStep,
    log_type: &str,
) -> Json {
    let mut record = field_envelope(api_id, served, trace);
    insert(&mut record, "logType", json!(log_type));
    insert(&mut record, "functionName", json!(step.name));
    if !logging.excludes_verbose_content {
        insert(&mut record, "context", trace.context.clone());
        let generated = if log_type == REQUEST_MAPPING_LOG_TYPE {
            step.request.clone()
        } else {
            step.response.clone()
        };
        insert(
            &mut record,
            "transformedTemplate",
            json!(generated.map(|value| value.to_string())),
        );
    }
    Json::Object(record)
}

/// The members every field-level record of one field carries: which field it is of, what
/// resolved it, the request it belongs to and what went wrong with it.
fn field_envelope(api_id: &str, served: &Served<'_>, trace: &FieldTrace) -> JsonMap<String, Json> {
    let mut envelope = JsonMap::new();
    insert(&mut envelope, "requestId", json!(served.request_id));
    insert(&mut envelope, "graphQLAPIId", json!(api_id));
    insert(&mut envelope, "path", json!(trace.path));
    insert(&mut envelope, "parentType", json!(trace.parent_type));
    insert(&mut envelope, "fieldName", json!(trace.field_name));
    insert(&mut envelope, "resolverArn", json!(trace.resolver_arn));
    insert(&mut envelope, "fieldInError", json!(trace.error.is_some()));
    insert(
        &mut envelope,
        "errors",
        json!(trace
            .error
            .as_ref()
            .map(|message| vec![message.clone()])
            .unwrap_or_default()),
    );
    envelope
}

/// One member of a record, so a member name is written once per record.
fn insert(record: &mut JsonMap<String, Json>, member: &str, value: Json) {
    record.insert(member.to_string(), value);
}

/// `span` in NANOSECONDS, which is the unit a record states a latency, an offset and a
/// duration in. A span longer than the widest number a record can carry saturates rather
/// than wrapping.
fn nanos(span: Duration) -> u64 {
    u64::try_from(span.as_nanos()).unwrap_or(u64::MAX)
}

/// `at` as a record writes an instant: the `yyyy-MM-dd'T'HH:mm:ss'Z'` form every timestamp
/// this service reports is rendered in, through the same renderer the control plane's own
/// `created` members use (`::aws_smithy_types::DateTime`).
fn timestamp(at: SystemTime) -> String {
    ::aws_smithy_types::DateTime::from(at)
        .fmt(::aws_smithy_types::date_time::Format::DateTime)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: `fieldLogLevel` decides which FIELDS are captured — none, the ones in error, or
    /// every one — and every value the model carries beyond `NONE` and `ERROR` captures every
    /// field. Source: `FieldLogLevel`'s own values
    /// (`smithy_query(query="shape",shape="FieldLogLevel")`).
    #[test]
    fn each_level_captures_the_fields_its_own_rule_names() {
        let failed = Some("boom".to_string());
        for (level, resolved_captured, failed_captured) in [
            ("NONE", false, false),
            ("ERROR", false, true),
            ("ALL", true, true),
            ("INFO", true, true),
            ("DEBUG", true, true),
            // A level no `FieldLogLevel` names is not `NONE` and not `ERROR`, so it captures
            // every field, as every level above `ERROR` does.
            ("SOMETHING_ELSE", true, true),
        ] {
            let read = FieldLevel::of(level);
            assert_eq!(
                read.captures(None),
                resolved_captured,
                "case {level:?}: expected a resolved field to be captured={resolved_captured}, \
                 got {:?}",
                read.captures(None)
            );
            assert_eq!(
                read.captures(failed.as_ref()),
                failed_captured,
                "case {level:?}: expected a failed field to be captured={failed_captured}, got \
                 {:?}",
                read.captures(failed.as_ref())
            );
        }
    }

    /// RULE: the log group an API's logs go to names that API, so one API's records are
    /// readable on their own.
    #[test]
    fn the_log_group_names_the_api() {
        for api_id in ["abcd1234", "zzzz", "a1b2c3d4e5f6g7h8"] {
            let group = log_group(api_id);
            assert_eq!(
                group,
                format!("/aws/appsync/apis/{api_id}"),
                "case {api_id:?}: expected the API's own group, got {group:?}"
            );
            assert!(
                ::service_common::cloudwatch_logs::is_log_group_name(&group),
                "case {api_id:?}: expected a name CloudWatch Logs holds, got {group:?}"
            );
        }
    }
}
