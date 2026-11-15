// SPDX-License-Identifier: CC0-1.0

//! The ONE reading of a `TraceIds` list the reads that take one make, and the
//! sentences the faults it can carry are refused with.
//!
//! `BatchGetTraces` (POST /Traces) and `GetTraceGraph` (POST /TraceGraph) both
//! take the SAME required `TraceIdList` (`smithy_query(query="shape",
//! shape="TraceIdList")`: member `TraceId`, `@length` 1..=35, with no list-level
//! bound and no `@uniqueItems`), so the faults a list can carry beyond what the
//! model declares are found ONCE here — [`trace_ids_fault`], in the order a read
//! checks them: the shape of the list, then the ids in it.
//!
//! Which faults a read REFUSES, and the sentence it states for one, belong to
//! the read: the two are different queries over the same ids and X-Ray's own
//! answers differ, so each handler decides, and this module only holds the
//! reading and the sentences so that two reads cannot word one fault two ways.
//! [`TraceIdsFault::stated_with`] is where a read turns a fault into its
//! message, and the one fault the two word differently — an id that is not of
//! X-Ray's form — is the argument it takes.

use std::collections::BTreeSet;

/// What a read whose `TraceIds` carries no id at all refuses that list with.
pub const ZERO_TRACE_IDS_MESSAGE: &str = "Zero trace ids provided";

/// What a read whose `TraceIds` names one id more than once refuses that list
/// with: entirely fixed text, because WHICH id was repeated is not what is wrong
/// with the request.
pub const DUPLICATE_TRACE_IDS_MESSAGE: &str = "Containing duplicate trace ids";

/// What the trace GRAPH read refuses an id that is not of
/// [`super::is_trace_id`]'s form with: the sentence naming the rule the value
/// broke and the error code a caller branches on, and nothing of the value
/// itself — the shape the ingest's own per-document rejection has (`Invalid
/// segment. ErrorCode: InvalidTraceId`).
///
/// The batch read states the SAME fault in its own words
/// ([`super::malformed_trace_id_message`]): the message is the only
/// discriminator among a read's 400s, and the two reads' are not the same text.
pub const INVALID_TRACE_ID_MESSAGE: &str = "Invalid traceId. ErrorCode: InvalidId";

/// What a `TraceIds` list broke, found before any lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceIdsFault<'ids> {
    /// The list names no id at all, so it names no trace to answer for.
    NoIds,
    /// The list names more ids than one call retrieves
    /// ([`super::MAX_TRACE_IDS_PER_CALL`]).
    TooManyIds,
    /// The id carried is not of X-Ray's trace-id form, so it names no trace this
    /// service could ever hold.
    MalformedId(&'ids str),
    /// The list names one id more than once, which leaves what the answer
    /// reports for that id undefined.
    DuplicateIds,
}

impl TraceIdsFault<'_> {
    /// The message a read refuses this fault with.
    ///
    /// Three of the four are one fixed sentence each, stated here so the reads
    /// cannot word one fault two ways; the fourth — an id of the wrong form — is
    /// worded by `malformed_id`, because X-Ray's two readers of a trace-id list
    /// state that one differently.
    #[must_use]
    pub fn stated_with(&self, malformed_id: impl Fn(&str) -> String) -> String {
        match self {
            Self::NoIds => ZERO_TRACE_IDS_MESSAGE.to_string(),
            Self::TooManyIds => super::exceeding_maximum_query_size_message(),
            Self::MalformedId(trace_id) => malformed_id(trace_id),
            Self::DuplicateIds => DUPLICATE_TRACE_IDS_MESSAGE.to_string(),
        }
    }
}

/// The first fault `trace_ids` carries, or `None` for a list that carries none.
///
/// The order is the shape of the list first — that it names an id at all, and no
/// more ids than one call retrieves — and then the ids in it: that each is of
/// X-Ray's form, and that none is named twice.
#[must_use]
pub fn trace_ids_fault(trace_ids: &[String]) -> Option<TraceIdsFault<'_>> {
    if trace_ids.is_empty() {
        return Some(TraceIdsFault::NoIds);
    }
    if trace_ids.len() > super::MAX_TRACE_IDS_PER_CALL {
        return Some(TraceIdsFault::TooManyIds);
    }
    if let Some(malformed) = trace_ids
        .iter()
        .find(|trace_id| !super::is_trace_id(trace_id))
    {
        return Some(TraceIdsFault::MalformedId(malformed));
    }
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    if trace_ids
        .iter()
        .any(|trace_id| !seen.insert(trace_id.as_str()))
    {
        return Some(TraceIdsFault::DuplicateIds);
    }
    None
}
