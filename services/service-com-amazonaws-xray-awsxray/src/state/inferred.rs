// SPDX-License-Identifier: CC0-1.0

//! The downstream services a trace's own documents only CALLED, and the segments
//! X-Ray synthesises for them.
//!
//! An instrumented service records the calls it makes OUT as entities carrying a
//! `namespace`: `aws` for a call through the AWS SDK, `remote` for any other call
//! that left the process. The service on the far side of such a call may report
//! nothing at all — a managed service does not submit segments for the caller —
//! so X-Ray INFERS a node for it: one top-level segment per call out, hanging
//! from the call's own id, carrying `"inferred": true` and typed by what the call
//! named ([`inferred_service_type`]).
//!
//! Three things decide which nodes a document produces:
//!
//! * WHICH entities are calls out — an entity's `namespace`, whether the entity
//!   is a subsegment carried inline in its segment or a subsegment submitted as
//!   its own document, which is the same entity either way;
//! * WHAT the node is — an AWS SDK call that addressed a RESOURCE (a table, a
//!   bucket, a queue) names that resource's type, and one that addressed none
//!   names the service itself;
//! * WHETHER there is a node to infer at all — a call that reached a service
//!   which reports for ITSELF already has its node in the trace, so nothing is
//!   inferred for it ([`reported_calls`], [`drop_inferred_under`]).

use super::*;

/// The `namespace` that makes an entity a call OUT of the service it sits in,
/// so the work it covers is a downstream service's.
pub const REMOTE_NAMESPACE: &str = "remote";

/// The `namespace` that makes an entity a call out through the AWS SDK, so
/// the work it covers is an AWS service's.
pub const AWS_NAMESPACE: &str = "aws";

/// The members of a call's `aws` block that name the RESOURCE it addressed, and
/// the kind of resource each names.
///
/// Source: the `aws` block the X-Ray segment documents reference gives for a
/// subsegment, whose resource members are `table_name` ("for operations on a
/// DynamoDB table, the name of the table") and `queue_url` ("for operations on
/// an Amazon SQS queue, the queue's URL"), beside the members the X-Ray SDKs
/// record for the other resource-addressed services they instrument. The kind is
/// the last field of the resource type a node of that resource is reported with
/// (`AWS::DynamoDB::Table`, `AWS::SQS::Queue`), whose service field is the name
/// the call itself carries.
const RESOURCE_MEMBERS: [(&str, &str); 5] = [
    ("table_name", "Table"),
    ("bucket_name", "Bucket"),
    ("queue_url", "Queue"),
    ("topic_arn", "Topic"),
    ("stream_name", "Stream"),
];

/// The kind of resource a call's `aws` block names, or `None` for a call that
/// named no resource — which is a call on the service itself.
///
/// A member set to anything but a non-empty string names nothing: the block is a
/// client's own, so a member it left empty is read as absent rather than as a
/// resource with no name.
fn named_resource(aws: Option<&JsonObject>) -> Option<&'static str> {
    let aws = aws?;
    RESOURCE_MEMBERS.iter().find_map(|(member, kind)| {
        aws.get(*member)
            .and_then(serde_json::Value::as_str)
            .is_some_and(|named| !named.is_empty())
            .then_some(*kind)
    })
}

/// Whether `namespace` makes an entity a call OUT, so the work it covers belongs
/// to a service other than the one that recorded it.
fn calls_out(namespace: Option<&str>) -> bool {
    matches!(namespace, Some(REMOTE_NAMESPACE | AWS_NAMESPACE))
}

/// The service type of a node X-Ray INFERRED from a call out of an instrumented
/// service, which the calling entity names, or `None` when the entity names no
/// downstream service at all.
///
/// `Service$Type` documents the vocabulary: [`SERVICE_TYPE_REMOTE`] is "a
/// downstream service of indeterminate type", which is what a
/// [`REMOTE_NAMESPACE`] entity records, and `AWS::<name>` is what
/// "downstream calls to Amazon Web Services ... that didn't target a specific
/// table" report, which is what an [`AWS_NAMESPACE`] entity that addressed no
/// resource records. One that DID address a resource targeted that resource, so
/// it reports the resource's own type, `AWS::<name>::<kind>`. An entity with
/// neither namespace recorded in-process work and infers no service.
///
/// This is the crate's ONE namespace-to-service-type derivation, so every
/// surface that reports the identity of an inferred service — a service graph's
/// nodes, a trace summary's `ServiceIds`, a root cause's services, a
/// `service(name, type)` filter expression — types that service the same way.
pub fn inferred_service_type(
    name: &str,
    namespace: Option<&str>,
    aws: Option<&JsonObject>,
) -> Option<String> {
    match namespace? {
        REMOTE_NAMESPACE => Some(SERVICE_TYPE_REMOTE.to_string()),
        AWS_NAMESPACE => Some(match named_resource(aws) {
            Some(kind) => format!("AWS::{name}::{kind}"),
            None => format!("AWS::{name}"),
        }),
        _ => None,
    }
}

/// The id of the segment synthesised for the call `call_id` of `trace_id`: 16
/// hexadecimal digits, as a segment id is.
///
/// Service-assigned and derived only from what it identifies, so it is the same
/// on every read and distinct per call — re-submitting the same document
/// replaces the inferred segment it already derived instead of adding a second
/// one, and the node a reporting service supersedes can be found by the call it
/// was inferred for. The mixing is [`service_common::stable_hex_id`], the one
/// deterministic id helper the service crates share: this id travels on the
/// wire, so it has to be reproducible across builds as well as across reads,
/// which `std`'s default hasher does not promise. It is still an opaque id, not
/// a value to depend on.
pub fn inferred_segment_id(trace_id: &str, call_id: &str) -> String {
    ::service_common::stable_hex_id(&["inferred", trace_id, call_id])
}

/// The segments X-Ray synthesises for the downstream services `segment` CALLED:
/// one per call out it records — the document itself where it is a subsegment
/// that is a call out, and every subsegment in its tree that is one — that
/// carries an id to parent the synthesised segment on.
///
/// Each is a top-level segment of the same trace whose `parent_id` is the call's
/// id and whose document carries `"inferred": true`, so a reader building a
/// service map sees the downstream node the caller recorded. Its times, `http`
/// block, `aws` block, outcome flags and `cause` are the call's: the call is the
/// only record of what the downstream service did.
///
/// The tree is walked with an explicit worklist rather than by recursion, so the
/// nesting a submitted document carries costs heap and not call stack; the order
/// is the recursion's, a node then its children left to right.
pub fn inferred_segments(trace_id: &str, segment: &SegmentRecord) -> Vec<SegmentRecord> {
    let mut out = Vec::new();
    if segment.inferred {
        // A synthesised segment never derives further segments from itself. It
        // carries no subsegments either, so this is belt and braces: only
        // `inferred_segment` sets the flag, never a submitted document.
        return out;
    }
    // A subsegment SUBMITTED as its own document is the same entity as one
    // carried inline, so a call out it records names the same downstream node —
    // hanging from the document's own id, which is the call.
    if segment.is_subsegment() && calls_out(segment.namespace.as_deref()) {
        out.push(inferred_segment(
            trace_id,
            &segment.id,
            &call_out_of(segment),
            segment.start_time,
        ));
    }
    let mut pending: Vec<&SubsegmentRecord> = segment.subsegments.iter().rev().collect();
    while let Some(subsegment) = pending.pop() {
        if let (Some(call_id), true) = (
            subsegment.id.as_deref(),
            calls_out(subsegment.namespace.as_deref()),
        ) {
            out.push(inferred_segment(
                trace_id,
                call_id,
                subsegment,
                segment.start_time,
            ));
        }
        pending.extend(subsegment.subsegments.iter().rev());
    }
    out
}

/// A submitted SUBSEGMENT document as the call-out entity it is, so one
/// derivation serves both the inline and the standalone form of one entity.
fn call_out_of(segment: &SegmentRecord) -> SubsegmentRecord {
    SubsegmentRecord {
        id: Some(segment.id.clone()),
        name: segment.name.clone(),
        namespace: segment.namespace.clone(),
        start_time: Some(segment.start_time),
        end_time: segment.end_time,
        in_progress: segment.in_progress,
        error: segment.error,
        fault: segment.fault,
        throttle: segment.throttle,
        http: segment.http.clone(),
        annotations: segment.annotations.clone(),
        aws: segment.aws.clone(),
        exceptions: segment.exceptions.clone(),
        // The nodes its own subsegments name are derived from the stored
        // document itself, not from this view of it.
        subsegments: Vec::new(),
    }
}

/// Every call of this trace a service REPORTED its own SEGMENT for: the
/// `parent_id` of each submitted segment it holds.
///
/// Such a call reached a service that records for itself, so the trace already
/// holds that node and X-Ray infers none for the call. A submitted SUBSEGMENT is
/// no such report: it records work inside the service that recorded it — an
/// instrumented client's own child of its call out is exactly that shape — so
/// the node for the call it sits under is still the downstream service's, and
/// still inferred.
///
/// Read once per ingested document, so one submitted document costs one scan of
/// the trace however many calls it records.
pub fn reported_calls(trace: &TraceRecord) -> BTreeSet<String> {
    trace
        .segments
        .iter()
        .filter(|segment| !segment.inferred && !segment.is_subsegment())
        .filter_map(|segment| segment.parent_id.clone())
        .collect()
}

/// Drop the node this service inferred for the call `call_id`, where it holds
/// one — the downstream service has since reported for itself, which supersedes
/// it.
///
/// Only an inferred segment is ever dropped: a submitted document is never
/// removed by the arrival of another.
pub fn drop_inferred_under(trace: &mut TraceRecord, trace_id: &str, call_id: &str) {
    let id = inferred_segment_id(trace_id, call_id);
    let Some(at) = trace.segment_index.get(&id).copied() else {
        return;
    };
    if !trace.segments[at].inferred {
        return;
    }
    trace.segments.remove(at);
    // Every index past the removed one has shifted, so the whole index is
    // rebuilt: a trace holds at most `MAX_SEGMENTS_PER_TRACE` segments, and this
    // runs only when a node really was superseded.
    trace.segment_index = trace
        .segments
        .iter()
        .enumerate()
        .map(|(index, segment)| (segment.id.clone(), index))
        .collect();
    trace.recompute_bounds();
}

/// One inferred segment: the record, and the document a reader returns for it.
///
/// `timed_from` is the calling segment's own `start_time`, which is when a call
/// that carries no `start_time` of its own is timed from: it is the only instant
/// known for it.
fn inferred_segment(
    trace_id: &str,
    call_id: &str,
    call: &SubsegmentRecord,
    timed_from: f64,
) -> SegmentRecord {
    let start_time = call.start_time.unwrap_or(timed_from);
    let id = inferred_segment_id(trace_id, call_id);
    let mut document = serde_json::Map::new();
    document.insert("id".to_string(), serde_json::json!(id));
    document.insert("name".to_string(), serde_json::json!(call.name));
    document.insert("trace_id".to_string(), serde_json::json!(trace_id));
    document.insert("parent_id".to_string(), serde_json::json!(call_id));
    document.insert("start_time".to_string(), serde_json::json!(start_time));
    match call.end_time {
        Some(end_time) => {
            document.insert("end_time".to_string(), serde_json::json!(end_time));
        }
        None => {
            document.insert("in_progress".to_string(), serde_json::json!(true));
        }
    }
    document.insert("inferred".to_string(), serde_json::json!(true));
    for (member, set) in [
        ("fault", call.fault),
        ("error", call.error),
        ("throttle", call.throttle),
    ] {
        if set {
            document.insert(member.to_string(), serde_json::json!(true));
        }
    }
    if let Some(http) = &call.http {
        document.insert("http".to_string(), http_document(http));
    }
    if let Some(aws) = &call.aws {
        // The call's own `aws` block: it names the service and the resource this
        // node stands for, which is the only record of them the node has.
        document.insert("aws".to_string(), serde_json::Value::Object(aws.clone()));
    }
    if !call.exceptions.is_empty() {
        document.insert("cause".to_string(), cause_document(&call.exceptions));
    }
    let inferred_type =
        inferred_service_type(&call.name, call.namespace.as_deref(), call.aws.as_ref());
    // The RESOURCE TYPE of the node an AWS SDK call inferred is what a segment
    // carries in its `origin`, so a reader tells the node's kind off the
    // document the same way it does for a service that reported one. A `remote`
    // call names "a downstream service of indeterminate type" — no resource type
    // at all — so it leaves the member absent.
    let origin = (call.namespace.as_deref() == Some(AWS_NAMESPACE))
        .then(|| inferred_type.clone())
        .flatten();
    if let Some(origin) = &origin {
        document.insert("origin".to_string(), serde_json::json!(origin));
    }

    SegmentRecord {
        id,
        parent_id: Some(call_id.to_string()),
        name: call.name.clone(),
        document: serde_json::Value::Object(document).to_string(),
        start_time,
        end_time: call.end_time,
        in_progress: call.in_progress,
        error: call.error,
        fault: call.fault,
        throttle: call.throttle,
        http: call.http.clone(),
        annotations: BTreeMap::new(),
        aws: call.aws.clone(),
        origin,
        account_id: None,
        user: None,
        resource_arn: None,
        // An inferred segment stands for a downstream SERVICE, not for work
        // inside one, so it is no `type: "subsegment"` document; and it carries
        // no `namespace` of its own — the namespace that produced it was the
        // call's.
        segment_type: None,
        namespace: None,
        exceptions: call.exceptions.clone(),
        subsegments: Vec::new(),
        inferred: true,
        inferred_type,
    }
}
