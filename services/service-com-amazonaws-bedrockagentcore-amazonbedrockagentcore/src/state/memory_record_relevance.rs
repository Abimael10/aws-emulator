// SPDX-License-Identifier: CC0-1.0

//! The RELEVANCE side of the long-term memory record plane: which of a memory container's
//! records a SEARCH for a query reports, with what score, and in which order.
//!
//! A plain listing selects records ([`MemoryRecordSelection`]); a search additionally RANKS
//! what it selected, because `RetrieveMemoryRecords` answers with the records "that match
//! the search criteria, ordered by relevance" and reports each one's `score`, "the relevance
//! score of the memory record when returned as part of a search result[;] higher values
//! indicate greater relevance to the search query" (source: smithy_query query="operation"
//! operation="RetrieveMemoryRecords", query="shape" shape="MemoryRecordSummary").
//!
//! The relevance is read off what the container actually stored: a record's remembered
//! CONTENT is text, the query is text, and the score is the share of the query's distinct
//! terms that text carries — so a record carrying everything the query asks about outranks
//! one carrying half of it, and a record carrying none of it is no match for that query at
//! all and is not reported. The two sides are tokenised by the ONE free-text tokeniser this
//! tree already searches stored documents with
//! ([`::service_common::document_text_search::query_terms`]), so a query does not have to
//! reproduce the record's punctuation or letter case to find it.
//!
//! `searchCriteria.topK` then bounds how many of the ranked records exist to be reported —
//! "the maximum number of top-scoring memory records to return" — and the response pages
//! over that ranking in pages of `maxResults`.
//!
//! # What one search is allowed to cost
//!
//! Both factors of the work are bounded, and NEITHER of them is the size of the request.
//!
//! The STORE side is the container's own ceiling: at most
//! [`super::MAX_MEMORY_RECORDS_PER_MEMORY`] records, each holding a modelled content of at
//! most [`MAX_MEMORY_CONTENT_CHARS`] characters, so the records a search reads are capped
//! whatever a caller sends.
//!
//! The QUERY side is reduced to its distinct terms ONCE, into the set the ranking looks
//! records up in ([`relevance_of`]), and is bounded before any work proportional to it runs:
//! [`checked_search_query`] refuses a query past [`MAX_SEARCH_QUERY_CHARS`] characters or
//! [`MAX_SEARCH_QUERY_TERMS`] distinct terms, which a handler calls BEFORE it takes the
//! store. So scoring one record costs that record's own distinct terms — the query is only
//! looked INTO, never walked per record — and one search costs at most
//! `MAX_MEMORY_RECORDS_PER_MEMORY * MAX_MEMORY_CONTENT_CHARS` characters of reading plus one
//! set lookup per term read, with no factor a request can grow.

use ::std::collections::HashSet;

use ::service_common::document_text_search::query_terms;

use super::{
    normalize_memory_id, ComAmazonawsBedrockagentcoreAmazonbedrockagentcoreState,
    MemoryRecordSelection, StoredMemoryRecord,
};
use crate::ops::types::{MemoryContent, MemoryRecordSummary};

/// The most records one search reports from its ranking.
///
/// `SearchCriteria$topK` is modelled `@range(min: 1, max: 100)`, so the request boundary
/// already refuses more; restating it here keeps the bound on what a search materialises
/// LOCAL to this crate rather than inherited from the validator.
pub const MAX_RETRIEVED_TOP_K: usize = 100;

/// The most characters of remembered content one stored record holds.
///
/// `MemoryContent$text` is modelled `@length(min: 1, max: 16000)` (source: smithy_query
/// query="shape" shape="MemoryContent"), so this is the store side of the bound on one
/// search's reading, stated where that bound is stated.
pub const MAX_MEMORY_CONTENT_CHARS: usize = 16_000;

/// The most characters a `searchCriteria.searchQuery` this search reads may carry.
///
/// `SearchCriteria$searchQuery` is modelled `@length(min: 1, max: 10000)` (source:
/// smithy_query query="shape" shape="SearchCriteria"), so the request boundary already
/// refuses a longer one; restating it here is what lets this module bound its OWN work
/// rather than inherit that bound from the validator.
pub const MAX_SEARCH_QUERY_CHARS: usize = 10_000;

/// The most DISTINCT terms one search query names.
///
/// A term is one character at least, and two terms are separated by one more, so a query of
/// [`MAX_SEARCH_QUERY_CHARS`] characters cannot name more than half that many terms: this is
/// the term-count ceiling that length already implies, stated explicitly because it — and
/// not the character count — is the factor the ranking's set lookups are made of.
pub const MAX_SEARCH_QUERY_TERMS: usize = MAX_SEARCH_QUERY_CHARS / 2;

/// Refuse a search query whose size this operation does not read, with the sentence a
/// handler answers `ValidationException` with; `Ok(())` for one it does.
///
/// A handler calls this BEFORE it takes the store, so neither the tokenising nor the
/// ranking ever runs on a query past these bounds, and no request-sized factor reaches the
/// scan (see the module docs).
pub fn checked_search_query(search_query: &str) -> Result<(), String> {
    let characters = search_query.chars().count();
    if characters > MAX_SEARCH_QUERY_CHARS {
        return Err(format!(
            "searchCriteria.searchQuery must be at most {MAX_SEARCH_QUERY_CHARS} characters; \
             got {characters}"
        ));
    }
    let terms = query_terms(search_query).len();
    if terms > MAX_SEARCH_QUERY_TERMS {
        return Err(format!(
            "searchCriteria.searchQuery must name at most {MAX_SEARCH_QUERY_TERMS} distinct \
             terms; got {terms}"
        ));
    }
    Ok(())
}

impl StoredMemoryRecord {
    /// The text of this record's remembered content, which is what a search for a query
    /// reads.
    ///
    /// `MemoryContent` is a union whose only member is `text` (source: smithy_query
    /// query="shape" shape="MemoryContent"), so every stored record carries text to search.
    #[must_use]
    pub fn content_text(&self) -> &str {
        match &self.content {
            MemoryContent::Text(text) => text.as_str(),
        }
    }

    /// The distinct terms this record's content carries, as the shared tokeniser reads
    /// them.
    #[must_use]
    pub fn content_terms(&self) -> HashSet<String> {
        query_terms(self.content_text()).into_iter().collect()
    }
}

/// One record a relevance search selected, with the score it was ranked by.
///
/// The record is BORROWED, so a search materialises only the page a response carries,
/// however many records the ranking holds.
#[derive(Debug, Clone, Copy)]
pub struct ScoredMemoryRecord<'a> {
    /// The share of the query's distinct terms this record's content carries, in `0..=1`.
    pub score: f64,
    /// The record itself, exactly as the container stored it.
    pub record: &'a StoredMemoryRecord,
}

impl ScoredMemoryRecord<'_> {
    /// This result as the modelled `MemoryRecordSummary` a search reports it as: what was
    /// stored, plus the relevance this search ranked it by — the one member a listing of the
    /// same records leaves out, because a listing ranks nothing.
    #[must_use]
    pub fn to_summary(&self) -> MemoryRecordSummary {
        MemoryRecordSummary {
            score: Some(self.score),
            ..self.record.to_summary()
        }
    }
}

/// The search one `RetrieveMemoryRecords` asks for: the scope it narrows the container's
/// records to, the query the remaining records are ranked against, and how many of the
/// top-scoring ones it asks for.
#[derive(Debug, Clone, Copy)]
pub struct MemoryRecordSearch<'a> {
    /// The namespace scope, the strategy association and the metadata expressions a
    /// searched record has to satisfy — the SAME narrowing a plain listing of the records
    /// offers, read exactly once for both calls.
    pub selection: MemoryRecordSelection<'a>,
    /// The free text the selected records are ranked against.
    pub search_query: &'a str,
    /// `searchCriteria.topK`: the most top-scoring records this search reports.
    pub top_k: usize,
}

impl ComAmazonawsBedrockagentcoreAmazonbedrockagentcoreState {
    /// The long-term memory records of the container `memory_id` names that `search`
    /// reports, most relevant first.
    ///
    /// A record is reported when the request's scope selects it AND its content carries at
    /// least one term of the query: a record sharing nothing with the query is no match for
    /// it, so reporting it would be claiming a relevance nothing measured. Of two records
    /// scoring alike the more recently created one is reported first, which makes the order
    /// TOTAL — so two pages of one search cannot disagree about where a record sits.
    ///
    /// A container this plane holds no record for reports none, which is not a statement
    /// about whether the CONTAINER exists — that is resolved against the component that
    /// owns it ([`super::memory_item_container_resolved`]) before this is read.
    #[must_use]
    pub fn retrieved_memory_records(
        &self,
        memory_id: &str,
        search: &MemoryRecordSearch<'_>,
    ) -> Vec<ScoredMemoryRecord<'_>> {
        let Some(memory) = self.memories.get(&normalize_memory_id(memory_id)) else {
            return Vec::new();
        };
        let terms: HashSet<String> = query_terms(search.search_query).into_iter().collect();
        if terms.is_empty() {
            return Vec::new();
        }
        let mut ranked: Vec<ScoredMemoryRecord<'_>> = memory
            .records
            .values()
            .filter(|record| search.selection.holds(record))
            .filter_map(|record| {
                relevance_of(record, &terms).map(|score| ScoredMemoryRecord { score, record })
            })
            .collect();
        ranked.sort_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| right.record.order_key().cmp(&left.record.order_key()))
        });
        ranked.truncate(search.top_k.clamp(1, MAX_RETRIEVED_TOP_K));
        ranked
    }
}

/// How relevant `record` is to a query naming `terms`: the share of those terms its content
/// carries, or `None` for a record carrying none of them.
///
/// The terms of both sides are the shared tokeniser's, so the match is case- and
/// punctuation-insensitive, and a term counts once however often the content repeats it:
/// the score says how much of the QUERY the record answers, not how long the record is.
///
/// The walk is over the RECORD's terms, looking each one up in the query's set, so scoring
/// costs this record's own stored content and nothing proportional to the request; the
/// matched count is the same either way, because a record's terms are distinct.
fn relevance_of(record: &StoredMemoryRecord, terms: &HashSet<String>) -> Option<f64> {
    let matched = record
        .content_terms()
        .iter()
        .filter(|term| terms.contains(*term))
        .count();
    (matched > 0).then(|| matched as f64 / terms.len() as f64)
}
