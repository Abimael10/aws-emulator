// SPDX-License-Identifier: CC0-1.0

//! The X-RAY TRACE CONTEXT one invocation runs with: what the handler reads its own trace
//! context out of, and where that context comes from.
//!
//! The execution environment reports it in `_X_AMZN_TRACE_ID`, one of the variables the
//! Lambda runtime sets itself and will not let a function's configuration set
//! ([`super::super::RESERVED_ENVIRONMENT_KEYS`]) — "the X-Ray tracing header", which
//! "changes with each invocation", so it is a value of the INVOCATION rather than of the
//! function.
//!
//! What it carries is decided by the function's own `TracingConfig` mode and by what the
//! caller supplied, and the three fields are `service_common`'s one propagation
//! ([`::service_common::xray_trace::hop_tracing_header`]) rather than this service's own:
//!
//! * `Active` — "Set Mode to Active to sample and trace a subset of incoming requests with
//!   X-Ray" (`TracingConfig$Mode`): Lambda records a segment for the invocation, so every
//!   invocation of such a function HAS a trace context. The chain is the one the caller
//!   named when it named one in X-Ray's own form and one Lambda starts otherwise, the
//!   `Parent` is the span of the segment Lambda records for this invocation — which is what
//!   the handler's own subsegments hang from — and the sampling decision is the caller's
//!   where it took one, this function's own (it samples) where it did not.
//! * `PassThrough` — the other value of the mode enum, and the one "a function gets when
//!   the caller named none" ([`super::super::DEFAULT_TRACING_MODE`]): Lambda records
//!   nothing of its own, so it has no segment to name as a parent and nothing to decide. A
//!   caller's tracing header is passed on as written, and an invocation that carried none
//!   runs under a chain Lambda started for it, with the decision `0` — so the variable is
//!   always reported, and the chain of an invocation nobody traced is still recognisable as
//!   one chain across the calls it makes ([`super::super::recursion`]).
//!
//! Nothing is kept here: the context is computed per invocation from the request and the
//! resolved configuration, and travels on the [`Invocation`](super::Invocation) the run is
//! performed from.
//!
//! # What an invocation X-Ray records leaves behind
//!
//! A recorded invocation produces TWO segment documents ([`segment_documents`]), which is
//! the shape X-Ray reports a Lambda invocation in: the SERVICE segment, whose `origin` is
//! `AWS::Lambda` and which covers Lambda's own handling of the invocation, and the FUNCTION
//! segment, whose `origin` is `AWS::Lambda::Function` and which covers the run itself. The
//! function segment's parent is the service segment, the service segment's parent is the
//! span the CALLER named (and nothing, for an invocation that started the chain), and the
//! function segment's own id is the `Parent` the run reports — so the subsegments an
//! instrumented handler sends to the daemon ([`super::xray_daemon`]) nest inside it.
//!
//! They are submitted through the one AWS SDK seam a run has ([`super::aws_api`]), which is
//! also the seam the handler's own datagrams are submitted over, so a trace's documents all
//! arrive in the store `BatchGetTraces` answers from.
//!
//! # The traces an invocation LINKS to
//!
//! An invocation Lambda makes for RECORDS it read out of an event source did not continue the
//! chain each record belongs to: one batch may carry records of many chains, so the invocation
//! runs under a chain of its OWN and REFERS to each chain it was asked by. X-Ray carries such a
//! reference as a `links` entry on the segment — the trace it points at, with
//! [`LINK_REFERENCE_TYPE_ATTRIBUTE`] saying how this segment relates to it — so a reader that
//! walks the links of a producer's trace finds the invocation its records were handed to.
//!
//! Those chains travel from the poller that read the records to the invocation it hands them to
//! as [`EventSourceTraceLinks`]: the poller reads each record's own tracing context out of the
//! source ([`crate::state::sqs_poller`]) and hands the batch over through this emulator's own
//! `Invoke` surface, which is the one hop between the two halves of that delivery.

use std::time::SystemTime;

use ::serde_json::json;

/// The request header a caller's tracing header arrives on, as X-Ray spells it. The ONE
/// spelling, shared with the recursive-loop guardrail's reading of the same header
/// ([`crate::state::recursion::TRACE_HEADER`]).
pub use crate::state::recursion::TRACE_HEADER;

/// The environment variable the execution environment reports this invocation's trace
/// context in: "`_X_AMZN_TRACE_ID` – The X-Ray tracing header", which the Lambda
/// Developer Guide lists among the variables the runtime sets and a function may not.
pub const TRACE_ID_VARIABLE: &str = "_X_AMZN_TRACE_ID";

/// The `origin` of the segment covering LAMBDA's own handling of an invocation.
const SERVICE_ORIGIN: &str = "AWS::Lambda";

/// The `origin` of the segment covering the FUNCTION's own run.
const FUNCTION_ORIGIN: &str = "AWS::Lambda::Function";

/// The member a segment carries its references to OTHER traces in.
const LINKS_MEMBER: &str = "links";

/// The attribute of a link that says how the linked trace relates to this segment.
const LINK_REFERENCE_TYPE_ATTRIBUTE: &str = "aws.xray.reserved.reference_type";

/// The reference type of a link to the trace this work was ASKED for by: the chain of the
/// record the invocation was handed, which the invocation's own chain is not a continuation of.
const LINK_PARENT_REFERENCE: &str = "parent";

/// The chains of the RECORDS an event source mapping's poller is handing over, as they travel
/// from that poller to the `Invoke` it hands them to.
///
/// Lambda's own poll-and-invoke hop is internal to the service; this emulator performs it over
/// its own `Invoke` surface ([`crate::state::sqs_poller`]), so the chains travel as a typed
/// request EXTENSION of this process, the way the framework carries its own in-process facts
/// ([`::smithy_server_runtime::CallbackRecursionDepth`], the same kind of stamp
/// [`crate::state::resource_tags::tag_key_origin`] reads). Nothing a client can send — on any
/// socket, signed with any credential — sets one, so no caller can have a link published on an
/// invocation's trace, which the `Invoke` API gives no way to ask for.
#[derive(Debug, Clone)]
pub struct EventSourceTraceLinks(pub Vec<String>);

/// How many links ONE invocation reports at most.
///
/// A queue source's batch may hold as many records as `BatchSize` allows (`@range(min: 1, max:
/// 10000)`), each of a chain of its own, so that is the most chains one invocation can have
/// been asked by — and the bound on what is read off a header, so no caller-supplied value is
/// unbounded.
const MAX_LINKED_TRACES: usize = 10_000;

/// The X-Ray trace context one invocation runs with.
#[derive(Debug, Clone)]
pub struct InvocationTrace {
    /// What the run reports in [`TRACE_ID_VARIABLE`].
    pub header: String,
    /// The spans of the two segments X-Ray RECORDS for this invocation, or `None` for an
    /// invocation it records nothing for: a function that traces nothing of its own, and a
    /// request whose sampling decision says it is not recorded.
    pub recorded: Option<Recorded>,
}

/// The spans of the segments a recorded invocation produces.
#[derive(Debug, Clone)]
pub struct Recorded {
    /// The chain the segments are filed under.
    pub trace_id: String,
    /// The span of the [`SERVICE_ORIGIN`] segment.
    pub service_span: String,
    /// The span of the [`FUNCTION_ORIGIN`] segment, which is the `Parent` the run reports
    /// and so the span the handler's own subsegments hang from.
    pub function_span: String,
    /// The span the CALLER named as the parent of this invocation, where it named one.
    pub caller_span: Option<String>,
    /// The chains this invocation was ASKED by and did not continue: the trace of each record
    /// an event source mapping's poller handed it, in the order the batch holds them (see the
    /// module docs). Empty for an invocation nothing links to.
    pub linked_traces: Vec<String>,
}

impl InvocationTrace {
    /// The same context, with `upstream` as the chains the segments of a RECORDED invocation
    /// link back to ([`Recorded::linked_traces`]).
    ///
    /// An invocation X-Ray records nothing for has no segment to carry a link, so nothing is
    /// kept for it.
    #[must_use]
    pub fn linking_to(mut self, upstream: Vec<String>) -> Self {
        if let Some(recorded) = self.recorded.as_mut() {
            recorded.linked_traces = upstream;
        }
        self
    }
}

/// The tracing header the caller of this invocation supplied, or `None` where it supplied
/// none.
#[must_use]
pub fn supplied_trace_header(ctx: &smithy_server_runtime::OperationContext<'_>) -> Option<String> {
    ctx.parts()
        .headers
        .get(TRACE_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

/// The chains the records of THIS invocation's batch belong to, as the poller that read them
/// stamped them ([`EventSourceTraceLinks`]) — empty for an invocation that was handed no record
/// of a traced chain, and for every request a client made, which carries no such stamp.
#[must_use]
pub fn supplied_event_source_links(
    ctx: &smithy_server_runtime::OperationContext<'_>,
) -> Vec<String> {
    ctx.parts()
        .extensions
        .get::<EventSourceTraceLinks>()
        .map(|stamped| event_source_links(&stamped.0))
        .unwrap_or_default()
}

/// The chains `supplied` names: each value that IS a trace id, each named once, at most
/// [`MAX_LINKED_TRACES`] of them.
///
/// A value that is not a trace id names no chain X-Ray could index, so it is left out rather
/// than reported as a link to nothing; the order is the one the stamp was written in, which is
/// the order the batch holds its records in.
#[must_use]
pub fn event_source_links(supplied: &[String]) -> Vec<String> {
    let mut links: Vec<String> = Vec::new();
    for named in supplied {
        let named = named.trim();
        if !::service_common::xray_trace::is_trace_id(named) {
            continue;
        }
        if links.iter().any(|held| held == named) {
            continue;
        }
        links.push(named.to_string());
        if links.len() == MAX_LINKED_TRACES {
            break;
        }
    }
    links
}

/// The trace context ONE invocation runs with — see the module docs for the rule each
/// `TracingConfig` mode follows.
///
/// Every invocation HAS one: `_X_AMZN_TRACE_ID` is a variable the runtime reports for each
/// invocation, so a function that records nothing of its own and a caller that named no chain
/// still run inside one, unsampled and unrecorded.
#[must_use]
pub fn invocation_trace_context(
    supplied: Option<&str>,
    tracing_mode: &str,
    at: SystemTime,
) -> InvocationTrace {
    let records = tracing_mode == crate::state::TRACING_MODE_ACTIVE;
    // A function that records nothing of its own has no span to name as a parent, so it
    // passes on what it was given; one that records has a span for the run itself.
    let function_span = records.then(::service_common::xray_trace::mint_span_id);
    let header = ::service_common::xray_trace::onward_tracing_header(
        supplied,
        function_span.as_deref(),
        at,
    )
    // A function that records nothing of its own, called by a client that named no chain —
    // or named one in no form X-Ray reads — still RUNS inside a chain: Lambda reports
    // `_X_AMZN_TRACE_ID` for every invocation, so the chain is Lambda's own where the caller
    // named none, with the span of its own handling as the parent and the decision `0`
    // because nothing of this invocation is recorded. It is what makes the hops of a chain
    // this emulator itself started recognisable as one chain, which is what the
    // recursive-loop guardrail counts ([`crate::state::recursion`]).
    .unwrap_or_else(|| {
        ::service_common::xray_trace::stamped_tracing_header(
            &::service_common::xray_trace::mint_trace_id(at),
            &::service_common::xray_trace::mint_span_id(),
            false,
        )
    });
    // A chain is RECORDED only where the decision says so: `Sampled=0` is an invocation
    // whose work still happens and whose trace is not recorded.
    let sampled = ::service_common::xray_trace::sampling_decision(&header).unwrap_or(false);
    let trace_id = ::service_common::xray_trace::continued_root(&header).map(str::to_string);
    let recorded = match (sampled.then_some(function_span).flatten(), trace_id) {
        (Some(function_span), Some(trace_id)) => Some(Recorded {
            trace_id,
            service_span: ::service_common::xray_trace::mint_span_id(),
            function_span,
            caller_span: supplied
                .and_then(::service_common::xray_trace::parent_span)
                .map(str::to_string),
            // The chains an invocation was asked by are the ones the hop that handed it its
            // records named ([`InvocationTrace::linking_to`]), and no invocation is asked by
            // one until it is told so.
            linked_traces: Vec::new(),
        }),
        _ => None,
    };
    InvocationTrace { header, recorded }
}

/// The two segment documents a recorded invocation produces, in the order a reader sees
/// them: Lambda's own handling of the invocation, then the function's run inside it.
///
/// Every value is read off the invocation: the chain its own header named, the function it
/// ran, the instants the run began and ended, and whether it faulted.
#[must_use]
pub fn segment_documents(
    recorded: &Recorded,
    function_name: &str,
    started: SystemTime,
    ended: SystemTime,
    faulted: bool,
) -> Vec<String> {
    let from = ::service_common::xray_segments::epoch_seconds(started);
    let to = ::service_common::xray_segments::epoch_seconds(ended);
    let mut service = json!({
        "trace_id": recorded.trace_id,
        "id": recorded.service_span,
        "name": function_name,
        "start_time": from,
        "end_time": to,
        "origin": SERVICE_ORIGIN,
    });
    if let Some(object) = service.as_object_mut() {
        if let Some(caller_span) = recorded.caller_span.as_deref() {
            object.insert("parent_id".to_string(), json!(caller_span));
        }
        // The chains this invocation was ASKED by, each as one `links` entry of the segment
        // covering Lambda's own handling of the invocation: the invocation runs under a chain
        // of its own, and each record it was handed belongs to a chain this one refers back to
        // (see the module docs). An invocation nothing links to carries no member at all.
        let links: Vec<::serde_json::Value> = recorded
            .linked_traces
            .iter()
            .map(|trace_id| {
                json!({
                    "trace_id": trace_id,
                    "attributes": { LINK_REFERENCE_TYPE_ATTRIBUTE: LINK_PARENT_REFERENCE },
                })
            })
            .collect();
        if !links.is_empty() {
            object.insert(LINKS_MEMBER.to_string(), json!(links));
        }
    }
    let function = json!({
        "trace_id": recorded.trace_id,
        "id": recorded.function_span,
        "parent_id": recorded.service_span,
        "name": function_name,
        "start_time": from,
        "end_time": to,
        "origin": FUNCTION_ORIGIN,
        // "fault" is what X-Ray reads a server-side failure of the recorded work off, which
        // for a run is the handler having raised.
        "fault": faulted,
    });
    vec![service.to_string(), function.to_string()]
}

/// The service token X-Ray is reached under through a run's own SDK seam
/// ([`super::aws_api`]), which is that service's SDK id as a client names it.
const XRAY_NAMESPACE: &str = "xray";

/// The operation a producer submits its segments with: "Uploads segment documents to Amazon
/// Web Services X-Ray" (`PutTraceSegments`).
const PUT_TRACE_SEGMENTS: &str = "PutTraceSegments";

/// How long the submission of a run's own segments is given, as the delivery of its log is:
/// a bound on an in-process hop made off the answer, so a service that does not answer cannot
/// hold an invocation open.
const CALL_CEILING: std::time::Duration = std::time::Duration::from_secs(5);

/// Submit `documents` as the segments X-Ray recorded for this invocation.
///
/// Made over the run's own SDK seam ([`super::aws_api`]), which is the seam the handler's own
/// datagrams are submitted over, so every document of one trace arrives in the store
/// `BatchGetTraces` answers from. Reporting a trace is never part of answering the
/// invocation: a submission nothing answered, or one X-Ray refused, is traced where an
/// undelivered log is and the caller is answered either way.
pub async fn report_segments(
    aws: &super::aws_api::AwsAccess,
    region: &str,
    documents: Vec<String>,
) {
    if documents.is_empty() {
        return;
    }
    let body = json!({ "TraceSegmentDocuments": documents }).to_string();
    match aws
        .call_within(XRAY_NAMESPACE, PUT_TRACE_SEGMENTS, region, &body, CALL_CEILING)
        .await
    {
        Ok(answer) if answer.status < 300 => {}
        Ok(answer) => ::tracing::debug!(
            status = answer.status,
            error_type = answer.error_type.as_deref().unwrap_or_default(),
            "the segments this invocation produced were not accepted"
        ),
        Err(refusal) => ::tracing::debug!(
            refusal = ?refusal,
            "the segments this invocation produced were not submitted"
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use super::{
        event_source_links, invocation_trace_context, segment_documents, LINK_PARENT_REFERENCE,
        LINK_REFERENCE_TYPE_ATTRIBUTE, LINKS_MEMBER,
    };

    /// RULE: an invocation handed records of OTHER chains runs under a chain of its own and
    /// REFERS back to each of theirs — one `links` entry per chain on the segment covering
    /// Lambda's own handling of the invocation, each naming that chain and
    /// [`LINK_PARENT_REFERENCE`] as how it relates to this one — and an invocation nothing
    /// links to carries no such member.
    ///
    /// The rows differ along what the rule generalises over: how many chains the hop stamped, a
    /// chain it stamped twice, a value that is not a chain X-Ray reads, and no chain at all. The
    /// mode and the decision are the other test's business: these rows all run an invocation
    /// X-Ray records.
    #[test]
    fn a_recorded_invocation_links_back_to_the_chains_its_records_belong_to() {
        let first = "1-5759e988-bd862e3fe1be46a994272793";
        let second = "1-00000000-000000000000000000000000";
        let at = UNIX_EPOCH + Duration::from_secs(0x5759_e988);
        // (case, the chains the hop stamped, the chains the segment links to)
        let rows: Vec<(&str, Vec<String>, Vec<&str>)> = vec![
            ("no chain at all", Vec::new(), Vec::new()),
            ("one chain", vec![first.to_string()], vec![first]),
            (
                "two chains",
                vec![first.to_string(), second.to_string()],
                vec![first, second],
            ),
            (
                "one chain stamped twice",
                vec![first.to_string(), first.to_string()],
                vec![first],
            ),
            (
                "a value that is no chain, beside one that is",
                vec!["not-a-trace-id".to_string(), second.to_string()],
                vec![second],
            ),
            (
                "a value that is no chain at all",
                vec!["whatever".to_string()],
                Vec::new(),
            ),
            (
                "a chain written with spaces around it",
                vec![format!(" {first} ")],
                vec![first],
            ),
        ];
        for (case, stamped, expected) in rows {
            let links = event_source_links(&stamped);
            let context = invocation_trace_context(None, "Active", at).linking_to(links.clone());
            let recorded = context
                .recorded
                .as_ref()
                .unwrap_or_else(|| panic!("case {case:?}: an active function records a segment"));
            assert_eq!(
                recorded.linked_traces, expected,
                "case {case:?}: expected the invocation to link to {expected:?}, got {:?}",
                recorded.linked_traces
            );
            // The invocation runs under a chain of its OWN: a record's chain is referred to,
            // never continued.
            for chain in &expected {
                assert_ne!(
                    recorded.trace_id, *chain,
                    "case {case:?}: the invocation must run under a chain of its own, it runs \
                     under {chain:?}"
                );
            }
            let documents = segment_documents(
                recorded,
                "probe",
                at,
                at + Duration::from_millis(25),
                false,
            );
            let service: ::serde_json::Value = ::serde_json::from_str(
                documents
                    .first()
                    .unwrap_or_else(|| panic!("case {case:?}: two documents are produced")),
            )
            .unwrap_or_else(|error| panic!("case {case:?}: the document is JSON ({error})"));
            let reported: Vec<(String, String)> = service[LINKS_MEMBER]
                .as_array()
                .map(|links| {
                    links
                        .iter()
                        .map(|link| {
                            (
                                link["trace_id"].as_str().unwrap_or_default().to_string(),
                                link["attributes"][LINK_REFERENCE_TYPE_ATTRIBUTE]
                                    .as_str()
                                    .unwrap_or_default()
                                    .to_string(),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            let wanted: Vec<(String, String)> = expected
                .iter()
                .map(|chain| ((*chain).to_string(), LINK_PARENT_REFERENCE.to_string()))
                .collect();
            assert_eq!(
                reported, wanted,
                "case {case:?}: expected the segment to carry the links {wanted:?}, got \
                 {service}"
            );
            // The segment of the FUNCTION's own run carries none: the reference is Lambda's
            // own handling of the invocation.
            let function: ::serde_json::Value = ::serde_json::from_str(
                documents
                    .get(1)
                    .unwrap_or_else(|| panic!("case {case:?}: two documents are produced")),
            )
            .unwrap_or_else(|error| panic!("case {case:?}: the document is JSON ({error})"));
            assert!(
                function.get(LINKS_MEMBER).is_none(),
                "case {case:?}: the function segment carries no link, got {function}"
            );
        }
    }

    /// Which trace context an invocation runs with — over a TABLE whose rows differ along
    /// the function's `TracingConfig` mode and what the caller supplied (a sampled chain, an
    /// unsampled one, a chain with no decision, a malformed root, and no header at all).
    #[test]
    fn an_invocations_trace_context_follows_the_mode_and_the_caller() {
        let root = "1-5759e988-bd862e3fe1be46a994272793";
        let at = UNIX_EPOCH + Duration::from_secs(0x5759_e988);
        // What the invocation's `_X_AMZN_TRACE_ID` must be.
        enum Expected {
            /// Lambda STAMPS its own: this chain (`None` for one Lambda started), its own
            /// span as the parent, and this decision.
            Stamped(Option<&'static str>, &'static str),
            /// The caller's header, passed on as written.
            Verbatim,
        }
        let rows: Vec<(&str, &str, Option<String>, Expected)> = vec![
            (
                "an active function called with a sampled chain",
                "Active",
                Some(format!("Root={root};Sampled=1")),
                Expected::Stamped(Some(root), "1"),
            ),
            (
                "an active function called with an unsampled chain",
                "Active",
                Some(format!("Root={root};Sampled=0")),
                Expected::Stamped(Some(root), "0"),
            ),
            (
                "an active function called with a chain and no decision",
                "Active",
                Some(format!("Root={root}")),
                Expected::Stamped(Some(root), "1"),
            ),
            (
                "an active function called with a root that is not a trace id",
                "Active",
                Some("Root=not-a-trace-id;Sampled=1".to_string()),
                Expected::Stamped(None, "1"),
            ),
            (
                "an active function called with no header at all",
                "Active",
                None,
                Expected::Stamped(None, "1"),
            ),
            (
                "a pass-through function called with a sampled chain",
                "PassThrough",
                Some(format!("Root={root};Sampled=1")),
                Expected::Verbatim,
            ),
            (
                "a pass-through function called with no header at all",
                "PassThrough",
                None,
                Expected::Stamped(None, "0"),
            ),
            (
                "a pass-through function called with a root that is not a trace id",
                "PassThrough",
                Some("Root=not-a-trace-id;Sampled=1".to_string()),
                Expected::Stamped(None, "0"),
            ),
        ];
        for (case, mode, supplied, expected) in rows {
            let context = invocation_trace_context(supplied.as_deref(), mode, at);
            // The segments an invocation produces follow the same decision: an invocation
            // Lambda records produces two, and one it does not produces none.
            let recorded = context.recorded.as_ref();
            let header = context.header.clone();
            match expected {
                Expected::Verbatim => {
                    assert_eq!(
                        Some(header.as_str()),
                        supplied.as_deref(),
                        "case {case:?}: expected the caller's header passed on as written, got \
                         {header:?}"
                    );
                    assert!(
                        recorded.is_none(),
                        "case {case:?}: a function that traces nothing records no segment"
                    );
                    continue;
                }
                Expected::Stamped(chain, decision) => {
                    let context = header;
                    let named = ::service_common::xray_trace::trace_field(&context, "Root")
                        .unwrap_or_default();
                    assert!(
                        ::service_common::xray_trace::is_trace_id(named),
                        "case {case:?}: expected a trace id as the Root of {context:?}, got \
                         {named:?}"
                    );
                    if let Some(continued) = chain {
                        assert_eq!(
                            named, continued,
                            "case {case:?}: expected {context:?} to continue {continued:?}, got \
                             {named:?}"
                        );
                    }
                    let parent = ::service_common::xray_trace::trace_field(&context, "Parent")
                        .unwrap_or_default();
                    assert!(
                        ::service_common::xray_trace::is_span_id(parent),
                        "case {case:?}: expected a span id as the Parent of {context:?}, got \
                         {parent:?}"
                    );
                    assert_eq!(
                        ::service_common::xray_trace::trace_field(&context, "Sampled"),
                        Some(decision),
                        "case {case:?}: expected the decision {decision:?} in {context:?}"
                    );
                    // Only a SAMPLED invocation is recorded, and the function segment's own
                    // span is the `Parent` the run reports, so the handler's subsegments
                    // nest inside it.
                    assert_eq!(
                        recorded.is_some(),
                        decision == "1",
                        "case {case:?}: expected the invocation to be recorded={}, got \
                         {recorded:?}",
                        decision == "1"
                    );
                    if let Some(recorded) = recorded {
                        assert_eq!(
                            recorded.function_span, parent,
                            "case {case:?}: expected the function segment to be the span the \
                             run reports, got {recorded:?}"
                        );
                        assert_eq!(
                            recorded.trace_id, named,
                            "case {case:?}: expected the segments filed under the chain the \
                             header named, got {recorded:?}"
                        );
                        let documents = segment_documents(
                            recorded,
                            "probe",
                            at,
                            at + Duration::from_millis(25),
                            false,
                        );
                        let origins: Vec<String> = documents
                            .iter()
                            .filter_map(|text| {
                                ::serde_json::from_str::<::serde_json::Value>(text).ok()
                            })
                            .filter_map(|document| {
                                document["origin"].as_str().map(str::to_string)
                            })
                            .collect();
                        assert_eq!(
                            origins,
                            vec!["AWS::Lambda".to_string(), "AWS::Lambda::Function".to_string()],
                            "case {case:?}: expected Lambda's two segments, got {documents:?}"
                        );
                    }
                }
            }
        }
    }
}
