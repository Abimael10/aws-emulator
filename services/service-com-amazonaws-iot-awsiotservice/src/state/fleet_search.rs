// SPDX-License-Identifier: CC0-1.0

//! WHICH documents of a fleet index a `queryString` SELECTS, and what an aggregation
//! over one of their fields amounts to.
//!
//! A fleet-indexing read — `GetBucketsAggregation` "Aggregates on indexed data with
//! search queries pertaining to particular fields" — answers a question about the
//! account's own registry: the query selects documents of the index, and the
//! aggregation summarises one field of the documents it selected. So the documents are
//! PROJECTED from the records this store already holds ([`IndexedDocument`]), never
//! kept a second time beside them: a thing's name, id, type, registry version and the
//! groups it belongs to are the index's managed fields of that thing
//! ([`super::fleet_metrics::MANAGED_THING_FIELDS`]), and its attributes are the
//! `attributes.<name>` fields an account declares as custom fields of the index.
//!
//! The query itself is READ by the one reader of a `queryString` this service has
//! ([`super::fleet_query`]), so what an aggregation accepts and what a fleet metric
//! accepts cannot drift; this module only EVALUATES the tokens that reader produced,
//! against one document at a time.
//!
//! WHAT ONE REQUEST MAY COST is capped here, in the two dimensions a request chooses:
//! how many documents the scan examines ([`MAX_SCANNED_DOCUMENTS`]) and how much
//! CHARACTER MATCHING the whole scan performs ([`MAX_MATCH_STEPS`]) — the product of
//! the documents, their values and the length of the query's patterns, which is what a
//! long wildcard term over a large registry multiplies. The budget is spent as the walk
//! runs and the scan STOPS the moment it is exhausted, answering the caller a refusal
//! ([`ScanRefusal`]) rather than spending the host's CPU on it; the caller runs the scan
//! off the async worker and with no lock held, so neither the runtime nor the store is
//! held for its duration.

use std::collections::{BTreeMap, BTreeSet};

use super::fleet_query::{tokenize, Token};

/// One document of a fleet index: the field names the index carries for a record,
/// each with the values that record holds for it.
///
/// A field is multi-valued because some are — the thing groups a thing belongs to are
/// one field with one value per group — and a field the record holds nothing for is
/// ABSENT rather than empty, which is what makes `field:*` select the records that
/// carry it.
pub type IndexedDocument = BTreeMap<String, Vec<String>>;

/// The `thingGroupNames` managed field: the groups a thing belongs to.
const REGISTRY_THING_GROUP_NAMES: &str = "registry.thingGroupNames";

/// The prefix of the fields whose values are a record's own ATTRIBUTES, which an
/// account declares as custom fields of the index before they can be aggregated.
pub const ATTRIBUTE_FIELD_PREFIX: &str = "attributes.";

/// The default `TermsAggregation$maxBuckets`: "The number of buckets to return in the
/// response. Default to 10"
/// (`smithy_query(query="shape",shape="TermsAggregation")`).
pub const DEFAULT_MAX_BUCKETS: usize = 10;

impl super::ComAmazonawsIotAwsiotserviceState {
    /// The documents the fleet index `index_name` holds, projected from this account's
    /// own records.
    ///
    /// A pure read: nothing is written, so asking leaves every later caller seeing what
    /// this one saw. An index name that is no fleet index of this service holds no
    /// documents at all.
    pub fn indexed_documents(&self, index_name: &str) -> Vec<IndexedDocument> {
        match index_name {
            super::THING_INDEX_NAME => self
                .things
                .values()
                .map(|thing| {
                    let mut document = IndexedDocument::new();
                    insert(&mut document, "thingName", vec![thing.thing_name.clone()]);
                    insert(&mut document, "thingId", vec![thing.thing_id.clone()]);
                    insert(
                        &mut document,
                        "registry.version",
                        vec![thing.version.to_string()],
                    );
                    if let Some(thing_type_name) = thing.configuration.thing_type_name.as_ref() {
                        insert(
                            &mut document,
                            "thingTypeName",
                            vec![thing_type_name.clone()],
                        );
                        insert(
                            &mut document,
                            "registry.thingTypeName",
                            vec![thing_type_name.clone()],
                        );
                    }
                    insert(
                        &mut document,
                        REGISTRY_THING_GROUP_NAMES,
                        self.static_thing_groups_of(&thing.thing_name),
                    );
                    for (name, value) in &thing.configuration.attributes {
                        insert(
                            &mut document,
                            &format!("{ATTRIBUTE_FIELD_PREFIX}{name}"),
                            vec![value.clone()],
                        );
                    }
                    document
                })
                .collect(),
            super::THING_GROUP_INDEX_NAME => self
                .thing_groups
                .values()
                .map(|group| {
                    let mut document = IndexedDocument::new();
                    insert(
                        &mut document,
                        "thingGroupName",
                        vec![group.thing_group_name.clone()],
                    );
                    insert(
                        &mut document,
                        "thingGroupId",
                        vec![group.thing_group_id.clone()],
                    );
                    if let Some(parent) = group.configuration.parent_group_name.as_ref() {
                        insert(&mut document, "parentGroupName", vec![parent.clone()]);
                    }
                    for (name, value) in &group.configuration.attributes {
                        insert(
                            &mut document,
                            &format!("{ATTRIBUTE_FIELD_PREFIX}{name}"),
                            vec![value.clone()],
                        );
                    }
                    document
                })
                .collect(),
            _ => Vec::new(),
        }
    }
}

/// Record `values` under `field`, unless the record holds none: a field a record holds
/// no value for is absent from its document.
fn insert(document: &mut IndexedDocument, field: &str, values: Vec<String>) {
    if !values.is_empty() {
        document.insert(field.to_string(), values);
    }
}

/// How many documents ONE aggregation examines.
///
/// A scan decides every document of the index against the query, so the document count
/// is one of the two factors a request multiplies. The ceiling is the one the registry
/// itself is bounded by ([`super::MAX_THINGS`]), so no store this emulator admits is
/// refused — and a scan can never be handed more documents than that, whatever a later
/// store grows to, because past it the aggregation is refused instead of run.
pub const MAX_SCANNED_DOCUMENTS: usize = super::MAX_THINGS;

/// How much CHARACTER MATCHING one aggregation may cost, over the whole scan.
///
/// The other factor a request multiplies: each value a term is compared against costs
/// its own length, and a term carrying a wildcard costs the length of the pattern times
/// the length of the value. A long wildcard term over a large registry multiplies those
/// into work no caller should be able to ask one process for, so the budget is spent as
/// the walk runs and the scan stops the moment it is gone. The figure is this
/// emulator's own, far above what any fleet-indexing query written in practice costs
/// (a plain term over the whole registry costs on the order of its values' bytes).
pub const MAX_MATCH_STEPS: u64 = 20_000_000;

/// How deep the walk of one query may DESCEND.
///
/// The walk is a recursive descent, so each group and each negation the query nests
/// costs a stack frame — and a Rust stack overflow aborts the process rather than
/// answering the request. A query nests as deeply as its text allows (its parentheses
/// are capped by [`super::fleet_query::MAX_QUERY_NESTING_DEPTH`], but a run of `NOT`
/// operators is not), so the descent carries its own ceiling and the scan refuses past
/// it instead of descending. The figure is a small multiple of the parenthesis ceiling,
/// which is what a readable query nests.
pub const MAX_WALK_DEPTH: u32 = 4 * super::fleet_query::MAX_QUERY_NESTING_DEPTH as u32;

/// Why an aggregation was not run over the account's registry.
///
/// Each variant is a bound ONE request chose to exceed, so the caller is told which,
/// and nothing is computed from a partial scan: a truncated answer would be a count
/// that looks computed over a registry it did not read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanRefusal {
    /// The index holds more documents than one scan examines; carries how many.
    TooManyDocuments(usize),
    /// The query's matching cost more than [`MAX_MATCH_STEPS`] over those documents.
    TooMuchMatching,
    /// The query nests deeper than [`MAX_WALK_DEPTH`], so the walk would descend
    /// further than this emulator evaluates.
    TooDeeplyNested,
}

impl ScanRefusal {
    /// What to tell the caller: the bound the request exceeded, so it can be made
    /// cheaper from the text alone.
    pub fn message(&self) -> String {
        match self {
            Self::TooManyDocuments(documents) => format!(
                "the query would be evaluated over {documents} documents; this emulator \
                 aggregates over at most {MAX_SCANNED_DOCUMENTS}"
            ),
            Self::TooMuchMatching => format!(
                "the query matches more than this emulator evaluates for one aggregation \
                 ({MAX_MATCH_STEPS} character comparisons); narrow the query string"
            ),
            Self::TooDeeplyNested => format!(
                "the query nests more than {MAX_WALK_DEPTH} levels of groups and negations, \
                 which is more than this emulator evaluates"
            ),
        }
    }
}

/// The documents of `documents` the query `query` selects, or the bound the request
/// exceeded.
///
/// A query this service cannot read selects nothing — every caller validates it first
/// ([`super::validate_query_string`]) and is refused before reaching here, so this is
/// the safe answer rather than a second refusal.
///
/// CPU-BOUND: a caller runs this off the async worker
/// (`tokio::task::spawn_blocking`) and with no store lock held, so neither the runtime
/// nor the shared store is held for the scan's duration.
pub fn documents_matching(
    documents: Vec<IndexedDocument>,
    query: &str,
) -> Result<Vec<IndexedDocument>, ScanRefusal> {
    let Ok(tokens) = tokenize(query) else {
        return Ok(Vec::new());
    };
    if tokens.is_empty() {
        return Ok(Vec::new());
    }
    if documents.len() > MAX_SCANNED_DOCUMENTS {
        return Err(ScanRefusal::TooManyDocuments(documents.len()));
    }
    let mut budget = Budget::new();
    let mut selected = Vec::new();
    // How deeply the query nests is a property of the TOKENS, not of the document, so it
    // is read once: a query the walk would descend further than
    // [`MAX_WALK_DEPTH`] into is refused before any document is examined.
    if nesting_depth(&tokens) > MAX_WALK_DEPTH {
        return Err(ScanRefusal::TooDeeplyNested);
    }
    for document in documents {
        let keep = Walk {
            tokens: &tokens,
            at: 0,
            document: &document,
            budget: &mut budget,
            depth: 0,
        }
        .any_of();
        // The budget is read after every document, so the scan stops on the one that
        // spent it rather than after the whole registry.
        if budget.spent() {
            return Err(ScanRefusal::TooMuchMatching);
        }
        if keep {
            selected.push(document);
        }
    }
    Ok(selected)
}

/// How deeply `tokens` nest, which is how many frames the walk of them descends: a
/// group and a negation each open one level, and a group closes one.
///
/// A single pass over the tokens, so reading the depth costs no recursion itself. A
/// negation applies to the one term, group or negation after it, so a run of them
/// nests as deeply as the run is long.
fn nesting_depth(tokens: &[Token]) -> u32 {
    let (mut depth, mut deepest, mut negations) = (0u32, 0u32, 0u32);
    for token in tokens {
        match token {
            Token::GroupOpen => depth = depth.saturating_add(1),
            Token::GroupClose => depth = depth.saturating_sub(1),
            Token::Unary(_) => negations = negations.saturating_add(1),
            // A term closes every negation that was waiting for it.
            Token::Term(_) => negations = 0,
            Token::Binary(_) => {}
        }
        deepest = deepest.max(depth.saturating_add(negations));
    }
    deepest
}

/// What the whole scan has left to spend on character matching.
///
/// One budget for the scan, not one per document, so the bound is on what the REQUEST
/// costs rather than on each comparison inside it.
struct Budget {
    /// How many character comparisons the scan has performed.
    steps: u64,
}

impl Budget {
    /// A fresh budget: [`MAX_MATCH_STEPS`] character comparisons.
    fn new() -> Self {
        Self { steps: 0 }
    }

    /// Charge `cost` comparisons, answering whether the budget is still open.
    fn spend(&mut self, cost: u64) -> bool {
        self.steps = self.steps.saturating_add(cost);
        !self.spent()
    }

    /// Whether the budget is gone, in which case nothing further is matched.
    fn spent(&self) -> bool {
        self.steps >= MAX_MATCH_STEPS
    }
}

/// The RANKED buckets of the values `field` holds across `documents`: "a ranked list of
/// the number of occurrences of an aggregation field value", with at most `max_buckets`
/// of them.
///
/// A document that carries no value for the field is in no bucket — `Bucket$count` is
/// "The number of documents that have the value counted for the particular bucket" —
/// and a document carrying several values for one field (the groups a thing belongs to)
/// counts into the bucket of each. The ranking is by count, highest first, and by value
/// among equal counts, so two reads of one store report the same buckets in the same
/// order.
pub fn terms_buckets(
    documents: &[IndexedDocument],
    field: &str,
    max_buckets: usize,
) -> Vec<(String, usize)> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for document in documents {
        for value in document.get(field).into_iter().flatten() {
            *counts.entry(value.clone()).or_insert(0) += 1;
        }
    }
    let mut ranked: Vec<(String, usize)> = counts.into_iter().collect();
    ranked.sort_by(|(left_value, left_count), (right_value, right_count)| {
        right_count
            .cmp(left_count)
            .then_with(|| left_value.cmp(right_value))
    });
    ranked.truncate(max_buckets);
    ranked
}

/// How many DISTINCT values `field` holds across `documents` — the count of unique
/// values of one field over the documents a query selected.
///
/// Counted over the VALUES themselves, so two documents holding the same value count
/// once and a document holding several values for one field (the groups a thing belongs
/// to) contributes each of them — the same reading of a multi-valued field
/// [`terms_buckets`] gives one bucket per value. A document that carries no value for
/// the field contributes nothing, so a field nothing selected carries has a cardinality
/// of zero.
///
/// CPU-BOUND, like [`documents_matching`] whose answer it counts: a caller runs it off
/// the async worker and with no store lock held.
pub fn distinct_value_count(documents: &[IndexedDocument], field: &str) -> usize {
    let mut distinct: BTreeSet<&str> = BTreeSet::new();
    for document in documents {
        for value in document.get(field).into_iter().flatten() {
            distinct.insert(value.as_str());
        }
    }
    distinct.len()
}

/// The arithmetic summary of the numbers one field holds across the documents a query
/// selected.
///
/// Every member is one of the modelled `Statistics` members that summarises NUMBERS
/// (`smithy_query(query="shape",shape="Statistics")`), so a reader hands back what was
/// computed here member for member and nothing is reported that was not.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NumericSummary {
    /// `Statistics$sum` — "The sum of the aggregated field values".
    pub sum: f64,
    /// `Statistics$average` — "The average of the aggregated field values".
    pub average: f64,
    /// `Statistics$minimum` — "The minimum aggregated field value".
    pub minimum: f64,
    /// `Statistics$maximum` — "The maximum aggregated field value".
    pub maximum: f64,
    /// `Statistics$sumOfSquares` — "The sum of the squares of the aggregated field
    /// values".
    pub sum_of_squares: f64,
    /// `Statistics$variance` — "The variance of the aggregated field values", over the
    /// values themselves (the whole population the query selected), so a field holding
    /// one value, or the same value repeatedly, has variance zero.
    pub variance: f64,
    /// `Statistics$stdDeviation` — "The standard deviation of the aggregated field
    /// values", the root of [`NumericSummary::variance`].
    pub std_deviation: f64,
}

/// What a `Statistics` aggregation over ONE field of the selected documents amounts to.
///
/// The count stands on its own because it is the one statistic a field of any type has
/// — `Statistics$count` is "The count of things that match the query string criteria and
/// contain a valid aggregation field value", which needs no arithmetic — while the
/// arithmetic summary exists only where the field holds numbers to summarise.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FieldStatistics {
    /// How many of the selected documents carry a value the field's type admits.
    pub count: usize,
    /// The arithmetic summary of the numbers they hold, or `None` when they hold none.
    pub summary: Option<NumericSummary>,
}

/// The statistics of the values `field` holds across `documents`, where `numeric` says
/// whether the index carries the field as a NUMBER.
///
/// A document carrying no value for the field is in no statistic at all, exactly as it
/// is in no bucket of [`terms_buckets`]: the count is of the documents that carry a
/// VALID value, which for a numeric field means one that reads as a finite number. A
/// document carrying several values for one field (the groups a thing belongs to)
/// counts once towards the count and contributes each of its values to the arithmetic,
/// the same way it counts into the bucket of each.
///
/// A field the index carries as text or as a flag has no arithmetic summary — there is
/// nothing to add up — so such a field answers with its count alone, whatever its values
/// happen to look like.
///
/// CPU-BOUND, like [`documents_matching`] whose answer it summarises: a caller runs it
/// off the async worker and with no store lock held.
pub fn field_statistics(
    documents: &[IndexedDocument],
    field: &str,
    numeric: bool,
) -> FieldStatistics {
    let mut count = 0usize;
    let mut values: Vec<f64> = Vec::new();
    for document in documents {
        let Some(held) = document.get(field) else {
            continue;
        };
        if !numeric {
            count += 1;
            continue;
        }
        let numbers = held
            .iter()
            .filter_map(|value| value.parse::<f64>().ok())
            .filter(|number| number.is_finite());
        let before = values.len();
        values.extend(numbers);
        if values.len() > before {
            count += 1;
        }
    }
    FieldStatistics {
        count,
        summary: summarise(&values),
    }
}

/// The arithmetic summary of `values`, or `None` when there are none to summarise.
///
/// The variance is read off the deviations from the average rather than off the sum of
/// squares, so values that are all equal summarise to variance zero instead of to the
/// rounding error of a subtraction of two large sums; `sumOfSquares` is still the plain
/// sum of the squares, which is what the member reports.
fn summarise(values: &[f64]) -> Option<NumericSummary> {
    if values.is_empty() {
        return None;
    }
    let population = values.len() as f64;
    let sum: f64 = values.iter().sum();
    let average = sum / population;
    let sum_of_squares: f64 = values.iter().map(|value| value * value).sum();
    let variance: f64 = values
        .iter()
        .map(|value| (value - average) * (value - average))
        .sum::<f64>()
        / population;
    Some(NumericSummary {
        sum,
        average,
        minimum: values.iter().copied().fold(f64::INFINITY, f64::min),
        maximum: values.iter().copied().fold(f64::NEG_INFINITY, f64::max),
        sum_of_squares,
        variance,
        std_deviation: variance.sqrt(),
    })
}

/// The percentile groups an aggregation that names none reports.
///
/// "The default percentile groupings are: 1,5,25,50,75,95,99, although you can specify
/// your own when you call GetPercentiles"
/// (`smithy_query(query="doc",shape="GetPercentiles")`), so a request carrying no
/// `percents` asks for exactly these.
pub const DEFAULT_PERCENTS: &[f64] = &[1.0, 5.0, 25.0, 50.0, 75.0, 95.0, 99.0];

/// The value each percentile group of `percents` stands at, over the numbers `field`
/// holds across `documents` — one `(percent, value)` pair per group asked for.
///
/// A percentile is a POSITION in the distribution of the selected values: "The
/// percentile group "1" contains the aggregated field value that occurs in approximately
/// one percent of the values that match the query", so the group `p` reports the value
/// `p` percent of the way through those values once they are in order, interpolated
/// between the two neighbouring values when the position falls between them. The groups
/// are reported in ascending order and once each, a group being a position rather than a
/// value of its own, so two reads of one store report the same pairs in the same order.
///
/// A document carrying no value for the field contributes nothing to the distribution,
/// exactly as it is in no bucket of [`terms_buckets`], and a document carrying several
/// values for one field contributes each of them. Values nothing holds have no positions
/// at all, so an empty distribution reports NO groups rather than a figure for each.
///
/// CPU-BOUND, like [`documents_matching`] whose answer it positions: a caller runs it off
/// the async worker and with no store lock held. What it costs is the values the scan
/// already bounded, in order, plus one position per group the request itself carries —
/// nothing is sized from a figure the request names.
pub fn field_percentiles(
    documents: &[IndexedDocument],
    field: &str,
    percents: &[f64],
) -> Vec<(f64, f64)> {
    let mut values: Vec<f64> = documents
        .iter()
        .flat_map(|document| document.get(field).into_iter().flatten())
        .filter_map(|value| value.parse::<f64>().ok())
        .filter(|number| number.is_finite())
        .collect();
    if values.is_empty() {
        return Vec::new();
    }
    values.sort_by(f64::total_cmp);
    let mut asked: Vec<f64> = percents
        .iter()
        .copied()
        .filter(|percent| percent.is_finite())
        .collect();
    asked.sort_by(f64::total_cmp);
    asked.dedup();
    asked
        .into_iter()
        .map(|percent| (percent, percentile_of(&values, percent)))
        .collect()
}

/// Where the group `percent` stands in `sorted`, a non-empty ascending run of numbers.
///
/// The position `percent` percent of the way through the run is `percent / 100` of the
/// way from its first value to its last; where that falls between two values the figure
/// is read off the line between them, so a group no value sits exactly at still reports a
/// figure the distribution supports rather than the nearest value to it.
fn percentile_of(sorted: &[f64], percent: f64) -> f64 {
    let last = sorted.len() - 1;
    let rank = percent.clamp(0.0, 100.0) / 100.0 * last as f64;
    let below = rank.floor();
    let low = sorted[below as usize];
    let high = sorted[rank.ceil() as usize];
    low + (high - low) * (rank - below)
}

/// One bounded walk of a query's tokens against ONE document.
///
/// The grammar is the one [`super::fleet_query`] reads: terms joined by `AND` / `OR`,
/// negated by `NOT`, grouped by parentheses — with two terms side by side joined as
/// `AND`, since a query naming two things is about the records that answer both. Every
/// token is consumed exactly once, so the token consumption is bounded by the query's
/// length; what the walk DESCENDS is bounded by [`MAX_WALK_DEPTH`], read off the tokens
/// before the scan starts and re-checked at every level, so the recursion cannot run
/// the thread's stack out.
struct Walk<'walk> {
    /// The query's tokens.
    tokens: &'walk [Token],
    /// How far the walk has got.
    at: usize,
    /// The document being decided.
    document: &'walk IndexedDocument,
    /// What the whole SCAN has left to spend on character matching, so one document's
    /// walk cannot cost more than the request as a whole is allowed.
    budget: &'walk mut Budget,
    /// How many levels this walk has descended, so a group or a negation past
    /// [`MAX_WALK_DEPTH`] stops rather than taking another stack frame.
    depth: u32,
}

impl Walk<'_> {
    /// The token the walk is at, if any.
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.at)
    }

    /// `a OR b OR …`: the lowest-binding operator, so it is read outermost.
    fn any_of(&mut self) -> bool {
        let mut selected = self.all_of();
        while let Some(Token::Binary(operator)) = self.peek() {
            if !is_or(operator) {
                break;
            }
            self.at += 1;
            // Both sides are READ whether or not the answer is already settled: the walk
            // has to consume the tokens of the right-hand side either way.
            let right = self.all_of();
            selected = selected || right;
        }
        selected
    }

    /// `a AND b …`, including two terms side by side.
    fn all_of(&mut self) -> bool {
        let mut selected = self.negation();
        loop {
            match self.peek() {
                Some(Token::Binary(operator)) if !is_or(operator) => {
                    self.at += 1;
                    let right = self.negation();
                    selected = selected && right;
                }
                Some(Token::Term(_) | Token::GroupOpen | Token::Unary(_)) => {
                    let right = self.negation();
                    selected = selected && right;
                }
                _ => break,
            }
        }
        selected
    }

    /// `NOT a`, a group, or one term.
    ///
    /// Both descending forms check the ceiling FIRST: a query that nests deeper than
    /// the walk evaluates is refused before the scan starts
    /// ([`nesting_depth`]), and this guard is what makes that refusal a bound on the
    /// stack rather than a promise about the tokens.
    fn negation(&mut self) -> bool {
        if self.depth >= MAX_WALK_DEPTH {
            // Consume the token so the walk still terminates, and answer that the
            // document is not selected: the caller refuses the request.
            self.at += 1;
            return false;
        }
        match self.peek() {
            Some(Token::Unary(_)) => {
                self.at += 1;
                self.depth += 1;
                let selected = !self.negation();
                self.depth -= 1;
                selected
            }
            Some(Token::GroupOpen) => {
                self.at += 1;
                self.depth += 1;
                let selected = self.any_of();
                self.depth -= 1;
                if matches!(self.peek(), Some(Token::GroupClose)) {
                    self.at += 1;
                }
                selected
            }
            Some(Token::Term(term)) => {
                let term = term.clone();
                self.at += 1;
                self.matches_term(&term)
            }
            // A token in neither position asks for nothing; consuming it keeps the walk
            // moving, so a query that somehow reaches here terminates.
            Some(_) => {
                self.at += 1;
                false
            }
            None => false,
        }
    }

    /// Whether the document answers ONE term: a `field:value` term against the values
    /// that field holds, and a bare term against every value the document holds.
    ///
    /// Every comparison is CHARGED to the scan's budget, and a term reached with the
    /// budget gone matches nothing: the scan is about to stop, and spending more on it
    /// would be the cost the budget exists to bound.
    fn matches_term(&mut self, term: &str) -> bool {
        if self.budget.spent() {
            return false;
        }
        if let Some((field, value)) = term.split_once(':') {
            let held: Vec<String> = self
                .document
                .get(field)
                .into_iter()
                .flatten()
                .cloned()
                .collect();
            return self.matches_any(value, &held);
        }
        let held: Vec<String> = self.document.values().flatten().cloned().collect();
        self.matches_any(term, &held)
    }

    /// Whether any of `held` answers `pattern`, charging each comparison to the budget
    /// and stopping the moment it is gone.
    fn matches_any(&mut self, pattern: &str, held: &[String]) -> bool {
        for value in held {
            if !self.budget.spend(comparison_cost(pattern, value)) {
                return false;
            }
            if value_matches(pattern, value) {
                return true;
            }
        }
        false
    }
}

/// What comparing `held` against the term value `pattern` costs, in character
/// comparisons: the two lengths for a plain or range comparison, and their PRODUCT for
/// a wildcard pattern, which is what [`wildcard_matches`] walks at most.
fn comparison_cost(pattern: &str, held: &str) -> u64 {
    let pattern_length = pattern.chars().count() as u64;
    let held_length = held.chars().count() as u64;
    if pattern.contains('*') || pattern.contains('?') {
        return pattern_length.saturating_mul(held_length).max(1);
    }
    pattern_length.saturating_add(held_length).max(1)
}

/// Whether `operator` joins its two sides as OR (`AND` and `&&` being the other half of
/// [`super::fleet_query`]'s binary operators).
fn is_or(operator: &str) -> bool {
    operator.eq_ignore_ascii_case("OR") || operator == "||"
}

/// Whether the value `held` answers the term value `pattern`.
///
/// `pattern` is one of the forms the query syntax admits for a value: a `[low TO high]`
/// or `{low TO high}` range, a quoted phrase, a value carrying the `*` and `?`
/// wildcards, or a plain value — in which case the two have to be equal.
fn value_matches(pattern: &str, held: &str) -> bool {
    if let Some(range) = read_range(pattern) {
        return range.contains(held);
    }
    let pattern = pattern
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .unwrap_or(pattern);
    if pattern.contains('*') || pattern.contains('?') {
        return wildcard_matches(pattern, held);
    }
    pattern == held
}

/// A `[low TO high]` / `{low TO high}` range of a query term.
struct Range<'range> {
    /// The lower bound, or `None` for the `*` that leaves it open.
    low: Option<&'range str>,
    /// The upper bound, or `None` for the `*` that leaves it open.
    high: Option<&'range str>,
    /// Whether the bounds themselves are in the range: `[` and `]` include them, `{`
    /// and `}` exclude them.
    inclusive: bool,
}

impl Range<'_> {
    /// Whether `held` is in the range.
    ///
    /// Two numbers are compared as NUMBERS and anything else as text, so a range over a
    /// numeric field (`registry.version:[1 TO 10]`) orders 9 before 10 rather than
    /// after it.
    fn contains(&self, held: &str) -> bool {
        self.low.is_none_or(|low| self.at_least(held, low))
            && self.high.is_none_or(|high| self.at_most(held, high))
    }

    /// Whether `held` is above `bound`, or equal to it in an inclusive range.
    fn at_least(&self, held: &str, bound: &str) -> bool {
        match compare(held, bound) {
            ::std::cmp::Ordering::Less => false,
            ::std::cmp::Ordering::Equal => self.inclusive,
            ::std::cmp::Ordering::Greater => true,
        }
    }

    /// Whether `held` is below `bound`, or equal to it in an inclusive range.
    fn at_most(&self, held: &str, bound: &str) -> bool {
        match compare(held, bound) {
            ::std::cmp::Ordering::Greater => false,
            ::std::cmp::Ordering::Equal => self.inclusive,
            ::std::cmp::Ordering::Less => true,
        }
    }
}

/// `left` against `right`, as numbers when both are numbers and as text otherwise.
fn compare(left: &str, right: &str) -> ::std::cmp::Ordering {
    match (left.parse::<f64>(), right.parse::<f64>()) {
        (Ok(left), Ok(right)) => left.total_cmp(&right),
        _ => left.cmp(right),
    }
}

/// `pattern` read as a range, or `None` when it is no range at all.
fn read_range(pattern: &str) -> Option<Range<'_>> {
    let (body, inclusive) = match (pattern.chars().next()?, pattern.chars().next_back()?) {
        ('[', ']') => (&pattern[1..pattern.len() - 1], true),
        ('{', '}') => (&pattern[1..pattern.len() - 1], false),
        _ => return None,
    };
    let (low, high) = body.split_once(" TO ")?;
    Some(Range {
        low: bound(low),
        high: bound(high),
        inclusive,
    })
}

/// One bound of a range: the value, or `None` for the `*` that leaves it open.
fn bound(raw: &str) -> Option<&str> {
    let raw = raw.trim();
    if raw == "*" || raw.is_empty() {
        None
    } else {
        Some(raw)
    }
}

/// Whether `held` matches the wildcard `pattern`, where `*` stands for any run of
/// characters and `?` for exactly one.
///
/// The match itself is the ONE wildcard matcher this tree has
/// ([`::service_common::iam_authorization::wildcard_match`], a single pass with one
/// backtrack point per `*`, so its cost is bounded by the product of the two lengths and
/// it never recurses) rather than a second copy of it here. A fleet-indexing value is
/// matched case-SENSITIVELY: a thing's name, type and attributes are compared as the
/// registry holds them.
fn wildcard_matches(pattern: &str, held: &str) -> bool {
    ::service_common::iam_authorization::wildcard_match(pattern, held, true)
}
