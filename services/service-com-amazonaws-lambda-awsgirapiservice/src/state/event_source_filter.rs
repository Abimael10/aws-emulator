// SPDX-License-Identifier: CC0-1.0

//! Which records of an event source a mapping's `FilterCriteria` admit.
//!
//! `FilterCriteria` is "an object that defines the filter criteria that determine whether
//! Lambda should process an event" and `FilterCriteria$Filters` is "a list of filters", each
//! carrying a `Filter$Pattern`, "a filter pattern" — so the criteria are a decision taken
//! about ONE record at a time, and a record matching ANY of the patterns is one the function
//! is invoked with. This module is that decision, held in one place because BOTH pollers take
//! it: [`super::sqs_poller`] for a mapping on a queue and [`super::event_source_poller`] for
//! one on a shard source, which would otherwise read the same member two ways.
//!
//! What differs per source is only WHAT the patterns are matched against — the record the
//! function receives, with the payload a source carries as opaque text read as the document
//! that text spells where it spells one — and that is each poller's own, because each builds
//! that record.
//!
//! The patterns themselves are the event-pattern language, matched by the ONE reading of it
//! in this tree ([`::service_common::event_pattern`]), so a Lambda filter and an EventBridge
//! rule admit the same documents.

use ::serde_json::Value;

/// How many steps deciding ONE batch may spend matching.
///
/// Matching is work a client's own pattern sizes, so the decisions of one batch are charged
/// against the same ceiling the shared matcher charges one request against: a pattern that
/// would spend more than that admits nothing further rather than holding the poller.
const MAX_MATCH_STEPS_PER_BATCH: u64 = ::service_common::event_pattern::MAX_MATCH_STEPS_PER_REQUEST;

/// The criteria one pass decides with: the parsed patterns, or the answer that every record
/// is admitted.
pub enum Filters {
    /// Every record the pass polled is handed to the function.
    ///
    /// That is what a mapping carrying NO criteria asks for — there is nothing to determine
    /// otherwise — and also what a mapping whose criteria this emulator cannot read gets: a
    /// record rejected on a pattern nothing could evaluate would be withheld from the
    /// function, and consumed from the source, on the strength of a filter that was never
    /// applied, so the reading failure withholds the FILTER rather than the data.
    AdmitEverything,
    /// The patterns a record is matched against, in the order the criteria carry them.
    Patterns(Vec<Value>),
}

impl Filters {
    /// The criteria the mapping `uuid` names decides its records with, from the patterns its
    /// `FilterCriteria` carries ([`patterns_of`]).
    #[must_use]
    pub fn of(uuid: &str, filters: &[String]) -> Self {
        if filters.is_empty() {
            return Self::AdmitEverything;
        }
        match parsed(filters) {
            Some(patterns) => Self::Patterns(patterns),
            None => {
                ::tracing::warn!(
                    mapping = %uuid,
                    "an event source mapping carries filter criteria this emulator cannot \
                     read, so every record it polls is handed to its function"
                );
                Self::AdmitEverything
            }
        }
    }

    /// Whether these criteria admit every record without being matched against one at all —
    /// the mapping that carries none, and the one whose criteria could not be read.
    #[must_use]
    pub fn admits_everything(&self) -> bool {
        matches!(self, Self::AdmitEverything)
    }

    /// Whether these criteria admit `event`, charging the matching to `budget`.
    ///
    /// "The collection of event patterns used to filter events"
    /// (`FilterCriteria$Filters`): a record matching ANY of them is one the function is
    /// invoked with.
    #[must_use]
    pub fn admits(&self, event: &Value, budget: &mut Budget) -> bool {
        match self {
            Self::AdmitEverything => true,
            Self::Patterns(patterns) => patterns.iter().any(|pattern| {
                ::service_common::event_pattern::matches_within(pattern, event, budget)
            }),
        }
    }
}

/// The step budget the decisions of ONE batch are charged to ([`MAX_MATCH_STEPS_PER_BATCH`]).
pub type Budget = ::service_common::event_pattern::MatchBudget;

/// A budget for one batch's decisions.
#[must_use]
pub fn budget() -> Budget {
    Budget::new(MAX_MATCH_STEPS_PER_BATCH)
}

/// The patterns a mapping's `FilterCriteria` carries, in order, and none for a mapping
/// carrying no criteria.
#[must_use]
pub fn patterns_of(criteria: Option<&crate::ops::types::FilterCriteria>) -> Vec<String> {
    criteria
        .and_then(|criteria| criteria.filters.as_ref())
        .map(|filters| {
            filters
                .iter()
                .filter_map(|filter| filter.pattern.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// All of `filters` parsed, or `None` when any of them is not a pattern this emulator can
/// match: a filter that was only partly read would admit or reject on criteria the mapping
/// does not carry (see [`Filters::AdmitEverything`]).
fn parsed(filters: &[String]) -> Option<Vec<Value>> {
    let mut patterns: Vec<Value> = Vec::with_capacity(filters.len());
    for pattern in filters {
        patterns.push(::service_common::event_pattern::parse(pattern).ok()?);
    }
    Some(patterns)
}

#[cfg(test)]
mod tests {
    use ::serde_json::json;

    use super::{budget, patterns_of, Filters};
    use crate::ops::types::{Filter, FilterCriteria};

    /// RULE: a record ONE of the mapping's patterns matches is admitted, and one none of them
    /// matches is not; criteria that carry no pattern, and patterns this emulator cannot
    /// read, admit every record.
    ///
    /// The rows differ along what the rule generalises over: how many patterns the criteria
    /// carry, whether the one that matches is the first or a later one, whether the clause
    /// names a member at the top of the record or one nested inside it, and what the criteria
    /// hold when they can decide nothing at all.
    #[test]
    fn exactly_the_records_a_pattern_matches_are_admitted() {
        let record = json!({
            "eventName": "INSERT",
            "dynamodb": { "Keys": { "pk": { "S": "row-1" } } },
        });
        // (case, the patterns, whether the record above is admitted)
        let rows: Vec<(&str, Vec<&str>, bool)> = vec![
            ("no-pattern-at-all", Vec::new(), true),
            (
                "one-pattern-that-matches",
                vec![r#"{"eventName": ["INSERT"]}"#],
                true,
            ),
            (
                "one-pattern-that-does-not",
                vec![r#"{"eventName": ["MODIFY"]}"#],
                false,
            ),
            (
                "a-clause-nested-in-the-record",
                vec![r#"{"dynamodb": {"Keys": {"pk": {"S": ["row-1"]}}}}"#],
                true,
            ),
            (
                "a-nested-clause-that-does-not-match",
                vec![r#"{"dynamodb": {"Keys": {"pk": {"S": ["row-2"]}}}}"#],
                false,
            ),
            (
                "the-second-of-two-patterns-matches",
                vec![
                    r#"{"eventName": ["REMOVE"]}"#,
                    r#"{"eventName": ["INSERT"]}"#,
                ],
                true,
            ),
            (
                "neither-of-two-patterns-matches",
                vec![
                    r#"{"eventName": ["REMOVE"]}"#,
                    r#"{"eventName": ["MODIFY"]}"#,
                ],
                false,
            ),
            (
                "a-pattern-this-emulator-cannot-read",
                vec!["not an event pattern at all"],
                true,
            ),
        ];
        for (case, patterns, expected) in rows {
            let filters: Vec<String> = patterns.iter().map(|text| (*text).to_string()).collect();
            let criteria = Filters::of(case, &filters);
            let mut budget = budget();
            let admitted = criteria.admits(&record, &mut budget);
            assert_eq!(
                admitted, expected,
                "case {case:?}: expected the record to be admitted: {expected}, got {admitted}"
            );
        }
    }

    /// RULE: criteria that decide nothing admit every record without being matched against
    /// one — a mapping carrying no pattern, and one whose patterns cannot be read.
    ///
    /// The rows differ along what the rule generalises over: no pattern, an unreadable one,
    /// an unreadable one beside a readable one, and patterns that are all readable.
    #[test]
    fn criteria_that_decide_nothing_admit_every_record() {
        // (case, the patterns, whether every record is admitted without being matched)
        let rows: Vec<(&str, Vec<&str>, bool)> = vec![
            ("no-pattern-at-all", Vec::new(), true),
            (
                "a-pattern-that-is-not-a-pattern",
                vec!["not an event pattern at all"],
                true,
            ),
            (
                "one-unreadable-pattern-beside-a-readable-one",
                vec![r#"{"eventName": ["INSERT"]}"#, "{"],
                true,
            ),
            (
                "every-pattern-readable",
                vec![r#"{"eventName": ["INSERT"]}"#],
                false,
            ),
        ];
        for (case, patterns, expected) in rows {
            let filters: Vec<String> = patterns.iter().map(|text| (*text).to_string()).collect();
            let decides_nothing = Filters::of(case, &filters).admits_everything();
            assert_eq!(
                decides_nothing, expected,
                "case {case:?}: expected criteria that decide nothing: {expected}, got \
                 {decides_nothing}"
            );
        }
    }

    /// RULE: the patterns a pass decides with are the ones `FilterCriteria$Filters` carries,
    /// in order, and a mapping carrying no criteria carries no pattern.
    #[test]
    fn the_patterns_are_the_ones_the_criteria_carry() {
        let pattern = |text: &str| Filter {
            pattern: Some(text.to_string()),
        };
        // (case, the criteria as stored, the patterns one pass decides with)
        let rows: Vec<(&str, Option<FilterCriteria>, Vec<&str>)> = vec![
            ("no-criteria", None, Vec::new()),
            (
                "criteria-carrying-no-filters",
                Some(FilterCriteria { filters: None }),
                Vec::new(),
            ),
            (
                "an-empty-list-of-filters",
                Some(FilterCriteria {
                    filters: Some(Vec::new()),
                }),
                Vec::new(),
            ),
            (
                "one-filter",
                Some(FilterCriteria {
                    filters: Some(vec![pattern(r#"{"eventName": ["INSERT"]}"#)]),
                }),
                vec![r#"{"eventName": ["INSERT"]}"#],
            ),
            (
                "two-filters-in-order",
                Some(FilterCriteria {
                    filters: Some(vec![
                        pattern(r#"{"eventName": ["INSERT"]}"#),
                        pattern(r#"{"eventName": ["REMOVE"]}"#),
                    ]),
                }),
                vec![
                    r#"{"eventName": ["INSERT"]}"#,
                    r#"{"eventName": ["REMOVE"]}"#,
                ],
            ),
            (
                "a-filter-carrying-no-pattern",
                Some(FilterCriteria {
                    filters: Some(vec![Filter { pattern: None }]),
                }),
                Vec::new(),
            ),
        ];
        for (case, criteria, expected) in rows {
            let read = patterns_of(criteria.as_ref());
            assert_eq!(
                read, expected,
                "case {case:?}: expected the patterns {expected:?}, got {read:?}"
            );
        }
    }
}
