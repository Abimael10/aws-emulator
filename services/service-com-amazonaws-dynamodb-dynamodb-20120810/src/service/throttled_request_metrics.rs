// SPDX-License-Identifier: CC0-1.0

//! The `AWS/DynamoDB` `ThrottledRequests` series this service publishes for each request a
//! throughput ceiling turned down, dimensioned by the table AND by the API that was called.
//!
//! # What the series is
//!
//! "Requests to DynamoDB that exceed the provisioned throughput limits on a resource (such
//! as a table or an index). `ThrottledRequests` is incremented by one if ANY event within a
//! request exceeds a provisioned throughput limit" (Amazon CloudWatch metrics for DynamoDB),
//! published under the PAIR `{TableName, Operation}` as the latency family beside it is
//! ([`super::request_latency_metrics`]). So the figure is a count of REQUESTS, not of the
//! events inside them: a call that asked for twenty-five writes and had twenty-four of them
//! turned down contributes one, exactly as a call refused outright does.
//!
//! "Any event within a request" is what decides whether a request contributes, so both ways
//! this service can answer a request a ceiling bound count:
//!
//! * the request was REFUSED for throughput — the modelled `ProvisionedThroughputExceededException`
//!   (or the `ThrottlingException` / `RequestLimitExceeded` its ceiling family states) — and
//!   the table it is counted against is the one the refusal names in its
//!   `ThrottlingReasons`, "the ARN of the table or index that experienced the throttling
//!   event" (`ThrottlingReason$resource`), falling back to the tables the request addressed
//!   when the refusal named none;
//! * the request was SERVED and handed part of its work back, which the batched write surface
//!   does for exactly the operations a ceiling turned down ("the failed operations are returned
//!   in the UnprocessedItems response parameter"), or reported the throughput code in one
//!   statement's own slot (`BatchStatementError$Code`). Each table that has such a residue in
//!   an answer had an event of this request throttled, so each contributes one. A batch READ
//!   hands keys back for a second reason — "BatchGetItem returns a partial result if the
//!   response size limit is exceeded" — and a key deferred for THAT is no throttled event, so
//!   such an answer contributes only where the size limit cannot be what deferred it
//!   ([`budget_could_have_deferred`]).
//!
//! A request answered with no throttled event at all — anything served whole, and any other
//! refusal — contributes nothing, which is what lets a client alarm on this series.
//!
//! # Where the datapoints go
//!
//! Into the CloudWatch emulator's own metric store, the same store `GetMetricStatistics`,
//! `GetMetricData` and the alarm evaluation read, reached through the shared engine. Nothing
//! here fails a request: a CloudWatch store this engine does not hold, or one at capacity,
//! drops the observation and the request is answered as it was.

use std::time::{SystemTime, UNIX_EPOCH};

use ::service_com_amazonaws_cloudwatch_graniteserviceversion20100801 as cloudwatch;
use aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use serde_json::Value;

use super::request_latency_metrics::{addressed_tables, METERED_OPERATIONS};

/// The namespace Amazon DynamoDB publishes a table's own metrics under.
const NAMESPACE: &str = "AWS/DynamoDB";

/// The metric that counts the requests a throughput ceiling turned down.
const THROTTLED_REQUESTS: &str = "ThrottledRequests";

/// The unit a count is published in: the `StandardUnit` value for a plain tally.
const COUNT_UNIT: &str = "Count";

/// The storage resolution a standard-resolution metric is published at, in seconds — which is
/// what `PutMetricData` stores for a publisher that names no `StorageResolution`.
const STANDARD_STORAGE_RESOLUTION_SECONDS: i32 = 60;

/// What one request contributes: it was one request, however many of its events were
/// throttled.
const ONE_REQUEST: f64 = 1.0;

/// The lowest status of the success class, and the first status past it: an answer inside the
/// class was SERVED, so whether any event of it was throttled is read off its body.
const SUCCESS_STATUS: u16 = 200;
const PAST_SUCCESS_STATUS: u16 = 300;

/// The error codes that say a THROUGHPUT ceiling turned the whole request down: the modelled
/// `ProvisionedThroughputExceededException` of every data-plane operation, and the two
/// request-rate refusals the same family states (`ThrottlingException`,
/// `RequestLimitExceeded`).
const THROUGHPUT_REFUSALS: [&str; 3] = [
    "ProvisionedThroughputExceededException",
    "RequestLimitExceeded",
    "ThrottlingException",
];

/// The `BatchStatementErrorCodeEnum` value a statement a ceiling turned down carries in its
/// own slot ([`crate::ops::types::BATCH_STATEMENT_ERROR_CODE_ENUM_VALUES`]).
const THROTTLED_SLOT_CODE: &str = "ProvisionedThroughputExceeded";

/// The member a refusal names the resources that experienced the throttling event in.
const THROTTLING_REASONS_MEMBER: &str = "ThrottlingReasons";

/// The member of one such reason that carries the resource's ARN.
const RESOURCE_MEMBER: &str = "resource";

/// The member an error body carries its code in, as this service's protocol renders it.
const ERROR_TYPE_MEMBER: &str = "__type";

/// The member `BatchWriteItem` hands its unadmitted operations back in, a map KEYED by the
/// table whose writes were turned down: "if any requested operations fail because the table's
/// provisioned throughput is exceeded ... the failed operations are returned in the
/// UnprocessedItems response parameter", so a residue there IS a ceiling's work.
const WRITE_RESIDUE_MEMBER: &str = "UnprocessedItems";

/// The member `BatchGetItem` hands its unread keys back in, the same way. A residue there has
/// TWO possible reasons, and only one of them is a throttled event: "BatchGetItem returns a
/// partial result if the response size limit is exceeded ... If a partial result is returned,
/// the operation returns a value for UnprocessedKeys" (the operation's own doc) is the other,
/// and a key deferred for that reason was neither charged nor refused
/// ([`budget_could_have_deferred`] is what tells the two apart).
const READ_RESIDUE_MEMBER: &str = "UnprocessedKeys";

/// The member a batch of statements reports one slot's outcome in, and the members of a slot
/// that carry its failure and the table it was against.
const RESPONSES_MEMBER: &str = "Responses";
const ERROR_MEMBER: &str = "Error";
const CODE_MEMBER: &str = "Code";
const TABLE_NAME_MEMBER: &str = "TableName";

/// Publish one observation of `ThrottledRequests` for each table an event of this request was
/// throttled on, under the operation it was made with.
///
/// Call it with NO store lock held: it takes CloudWatch's store to append the observations.
/// It is called from the one seam every response of this service passes through
/// ([`super::OperationScopedClauseForm`]), with the answer already produced, so what a
/// ceiling turned down is published as part of serving the request.
pub fn publish_throttled_request<Eng: Send + Sync + 'static>(
    engine: &Eng,
    operation: &str,
    req: &RtRequest,
    parts: &::http::request::Parts,
    response: &RtResponse,
) {
    if METERED_OPERATIONS.binary_search(&operation).is_err() {
        return;
    }
    let Some(body) = response.body().bytes() else {
        return;
    };
    let served = (SUCCESS_STATUS..PAST_SUCCESS_STATUS).contains(&response.status().as_u16());
    // The answer is read only where it could carry a throttled event at all: an answer this
    // service SERVED says so by handing work back or by a slot's own code, each of which names
    // itself in the body, so a served answer naming none of them is past without being parsed.
    if served && !mentions_a_throttled_event(body) {
        return;
    }
    let Some(answered) = document_of(Some(body)) else {
        return;
    };
    // A table name is unique within one region, and the region is the one this request's
    // endpoint names — the same resolution every handler of this service reads a table by.
    let region = crate::state::request_region(parts);
    let tables = if served {
        throttled_in_answer(&answered, body.len(), &region)
    } else {
        refused_tables(&answered, req, &region)
    };
    if tables.is_empty() {
        return;
    }
    publish(engine, operation, &tables, SystemTime::now());
}

/// Whether a SERVED answer mentions any of the three things that carry a throttled event, as
/// raw bytes: the two residue members and the per-slot error a statement's own failure is
/// reported under.
///
/// What this spares is the second whole parse of an answer that cannot be carrying one — a
/// page of items is the common answer of this data plane, and the common answer is the one
/// worth not parsing twice.
fn mentions_a_throttled_event(body: &[u8]) -> bool {
    [WRITE_RESIDUE_MEMBER, READ_RESIDUE_MEMBER, ERROR_MEMBER]
        .iter()
        .any(|member| contains(body, member.as_bytes()))
}

/// Whether `haystack` holds `needle` anywhere.
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// The JSON document a body carries, or `None` for a body that is absent or is not one.
fn document_of(body: Option<&[u8]>) -> Option<Value> {
    ::serde_json::from_slice::<Value>(body?).ok()
}

/// The tables a REFUSED request is counted against: the ones the refusal named as having
/// experienced the throttling event, or — when it named none — every table the request
/// addressed, since the ceiling bound somewhere among them.
///
/// Empty for any refusal that is not a throughput refusal at all: a request refused for its
/// own shape, or for a resource it named, had no event of it throttled.
fn refused_tables(answered: &Value, req: &RtRequest, region: &str) -> Vec<String> {
    if !is_throughput_refusal(answered) {
        return Vec::new();
    }
    let mut tables: Vec<String> = Vec::new();
    for reason in answered
        .get(THROTTLING_REASONS_MEMBER)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(resource) = reason.get(RESOURCE_MEMBER).and_then(Value::as_str) else {
            continue;
        };
        add_resolved(&mut tables, resource, region);
    }
    if !tables.is_empty() {
        return tables;
    }
    document_of(req.body().bytes())
        .map(|document| addressed_tables(&document, region))
        .unwrap_or_default()
}

/// Whether a refusal is one a THROUGHPUT ceiling stated ([`THROUGHPUT_REFUSALS`]).
///
/// The code is read off the rendered error the same way a client reads it: the last field of
/// the `__type` this protocol reports, which carries the shape's namespace before it.
fn is_throughput_refusal(answered: &Value) -> bool {
    answered
        .get(ERROR_TYPE_MEMBER)
        .and_then(Value::as_str)
        .and_then(|reported| reported.rsplit(['#', '/']).next())
        .is_some_and(|code| THROUGHPUT_REFUSALS.contains(&code))
}

/// The tables a SERVED answer reports a throttled event of: the ones it handed work back for
/// because a ceiling turned that work down, and the ones a statement's own slot reports the
/// throughput code against. `answered_bytes` is the size of the answer, which is what tells a
/// batch READ's two reasons for handing keys back apart ([`budget_could_have_deferred`]).
fn throttled_in_answer(answered: &Value, answered_bytes: usize, region: &str) -> Vec<String> {
    let mut tables: Vec<String> = Vec::new();
    let read_residue =
        (!budget_could_have_deferred(answered_bytes)).then_some(READ_RESIDUE_MEMBER);
    for member in [Some(WRITE_RESIDUE_MEMBER), read_residue].into_iter().flatten() {
        for name in answered
            .get(member)
            .and_then(Value::as_object)
            .into_iter()
            .flat_map(::serde_json::Map::keys)
        {
            add_resolved(&mut tables, name, region);
        }
    }
    for slot in answered
        .get(RESPONSES_MEMBER)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let throttled = slot
            .get(ERROR_MEMBER)
            .and_then(|error| error.get(CODE_MEMBER))
            .and_then(Value::as_str)
            == Some(THROTTLED_SLOT_CODE);
        if !throttled {
            continue;
        }
        if let Some(name) = slot.get(TABLE_NAME_MEMBER).and_then(Value::as_str) {
            add_resolved(&mut tables, name, region);
        }
    }
    tables
}

/// Whether an answer of `answered_bytes` could have deferred a key for its RESPONSE's size
/// budget rather than for capacity.
///
/// "BatchGetItem returns a partial result if the response size limit is exceeded ... If a
/// partial result is returned, the operation returns a value for UnprocessedKeys" (the
/// operation's own doc), and that deferral stops before the first whole item that would carry
/// the answer past the limit ([`crate::state::MAX_BATCH_GET_RESPONSE_BYTES`]) — so an answer
/// more than one item's worth ([`crate::state::MAX_ITEM_SIZE_BYTES`]) short of that limit
/// cannot have deferred anything for it, and a residue in such an answer is capacity's work
/// alone. Nearer the limit the two reasons are indistinguishable on the wire, and the answer
/// is left uncounted: a series that counts a plain partial result as a throttle would fire a
/// client's alarm on a request no ceiling turned anything down on.
fn budget_could_have_deferred(answered_bytes: usize) -> bool {
    let answered = i64::try_from(answered_bytes).unwrap_or(i64::MAX);
    let clear_of_the_limit = crate::state::MAX_BATCH_GET_RESPONSE_BYTES
        .saturating_sub(crate::state::MAX_ITEM_SIZE_BYTES);
    answered >= clear_of_the_limit
}

/// Add the table `name` addresses to `tables`, once: a name spelled as the table's ARN
/// resolves to the table's own name, because the series is the TABLE's and a table is one
/// resource however a request addressed it.
fn add_resolved(tables: &mut Vec<String>, name: &str, region: &str) {
    // A name this resolution refuses addresses no table of this endpoint, so it is declined
    // rather than guessed at.
    if let Ok(resolved) = crate::state::resolve_table_name_in(name, region) {
        if !tables.contains(&resolved) {
            tables.push(resolved);
        }
    }
}

/// Append one observation to each table's series, under `operation`.
fn publish<Eng: Send + Sync + 'static>(
    engine: &Eng,
    operation: &str,
    tables: &[String],
    at: SystemTime,
) {
    let Some(metrics) = cloudwatch::state::service_state(engine) else {
        return;
    };
    let timestamp_secs = at
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |since| since.as_secs_f64());
    let mut store = metrics.lock();
    for table in tables {
        // The one spelling of the `{TableName, Operation}` pair, shared with every other
        // family published under it ([`crate::state::operation_dimensions`]).
        let key = cloudwatch::state::MetricKey {
            namespace: NAMESPACE.to_string(),
            metric_name: THROTTLED_REQUESTS.to_string(),
            dimensions: crate::state::operation_dimensions(table, operation),
        };
        let datum = cloudwatch::state::StoredDatum {
            // CloudWatch holds a datapoint's instant as epoch SECONDS, which is the unit its
            // own readers window by.
            timestamp_secs,
            // One observation: this request, one of whose events a ceiling turned down.
            sample_count: ONE_REQUEST,
            sum: ONE_REQUEST,
            minimum: ONE_REQUEST,
            maximum: ONE_REQUEST,
            unit: Some(COUNT_UNIT.to_string()),
            storage_resolution: STANDARD_STORAGE_RESOLUTION_SECONDS,
        };
        if store.append_datapoints(key, vec![datum]).is_err() {
            // The metric store holds no more series. The request itself was already answered,
            // so nothing is failed for it; the dropped observation is reported where a
            // developer can see it.
            ::tracing::warn!(
                table = %table,
                operation = %operation,
                "the CloudWatch metric store is at capacity, so the throttled event of this \
                 request was not published"
            );
        }
    }
}
