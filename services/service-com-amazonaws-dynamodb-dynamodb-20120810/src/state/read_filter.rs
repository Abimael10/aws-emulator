// SPDX-License-Identifier: CC0-1.0

//! WHICH of the items a read evaluated it answers with: the filter one request states,
//! in either generation of the member that states it.
//!
//! "A filter expression is applied after a Query finishes, but before the results are
//! returned" (`Query$FilterExpression`), and `Scan$FilterExpression` says the same of a
//! scan. The legacy pair the expression replaced — `QueryFilter` / `ScanFilter` with
//! `ConditionalOperator` — states the same filter one generation back ("This is a legacy
//! parameter. Use FilterExpression instead": those members' docs), so both readers meet
//! the same three cases and decide them here ONCE rather than each on its own.
//!
//! A file of its own beside the item store rather than inside it: this is about what one
//! REQUEST narrows its page with, not about what the store holds. It is re-exported up to
//! `crate::state`, so every caller names it `crate::state::…`.

use std::collections::{BTreeSet, HashMap};

use crate::ops::types::Condition;
use crate::state::{
    check_legacy_conditions, expression_conflict, legacy_conditions_match, Item, LegacyMembership,
    ParsedCondition, RequestError, Substitutions,
};

/// The member a `FilterExpression` arrives in, which is what its refusals name.
const FILTER_EXPRESSION: &str = "FilterExpression";

/// How that member is SPELLED in the refusal of a filter that mentions a key attribute,
/// which is the one sentence this service states its name with a space in it.
const FILTER_EXPRESSION_PHRASE: &str = "Filter Expression";

/// The filter one read applies to the attributes each row it evaluated SAW.
#[derive(Debug)]
pub enum ReadFilter<'a> {
    /// No filter at all: every item the page evaluated is answered, so "Count is the
    /// same as ScannedCount".
    Unfiltered,
    /// A `FilterExpression`: the `ConditionExpression` language — one grammar, parsed by
    /// the one parser this crate has for it — judged against each item the read reached
    /// rather than against a key.
    Expression(ParsedCondition),
    /// The legacy filter map, its entries combined by `ConditionalOperator`.
    Legacy {
        /// The member that map arrived in (`ScanFilter`, `QueryFilter`), which is what a
        /// refusal about this filter names ([`ReadFilter::stated_member`]).
        member: &'a str,
        conditions: &'a HashMap<String, Condition>,
        conditional_operator: Option<&'a str>,
        /// The `IN` entries of that map, indexed once here rather than scanned per item
        /// ([`LegacyMembership`]): the length of one entry's operand list is the
        /// request's to choose, and the entry is evaluated for every item the read
        /// evaluates.
        membership: LegacyMembership,
    },
}

impl<'a> ReadFilter<'a> {
    /// The filter this request states, refusing a request that states it twice, states a
    /// comparison the service does not serve or with the operands that comparison does
    /// not take, or combines entries it does not have — all facts of the REQUEST, so all
    /// of them settled before anything is read.
    ///
    /// `legacy_member` is the member this reader's legacy filter arrives in
    /// (`QueryFilter` for a query, `ScanFilter` for a scan), which is what the refusals
    /// of a request that used it name.
    pub fn resolve(
        legacy_member: &'a str,
        expression: Option<&str>,
        legacy: Option<&'a HashMap<String, Condition>>,
        conditional_operator: Option<&'a str>,
        substitutions: Substitutions<'_>,
    ) -> Result<Self, RequestError> {
        // A key of either substitution table that is no substitution TOKEN defines nothing
        // this filter could dereference, so that defect is settled before the filter that
        // would dereference one is read ([`Substitutions::check_keys_are_tokens`]).
        substitutions.check_keys_are_tokens(&[FILTER_EXPRESSION])?;
        let legacy = legacy.filter(|map| !map.is_empty());
        match (expression, legacy) {
            // One filter stated in both generations has already been refused
            // ([`crate::state::check_expression_parameter_mix`]); the arm stays total
            // rather than unreachable.
            (Some(_), Some(_)) => Err(expression_conflict(legacy_member, FILTER_EXPRESSION)),
            (Some(expression), None) => Ok(Self::Expression(ParsedCondition::parse(
                FILTER_EXPRESSION,
                expression,
                substitutions,
            )?)),
            (None, Some(conditions)) => {
                // How many entries the map states, whether each names a comparison this
                // service serves with the operands it takes, and whether the operator
                // combining them is one the model declares, are all facts of the
                // request: settled once here rather than per item, so the work the
                // filter does per row is bounded by this service's own constants.
                crate::state::check_conditional_operator_companion(
                    legacy_member,
                    conditional_operator,
                    conditions.len(),
                )?;
                // Every value the filter compares against is held to the documented
                // value rules, before anything is read against them.
                crate::state::check_legacy_condition_values(
                    legacy_member,
                    conditions.iter().flat_map(|(name, condition)| {
                        crate::state::legacy_operands(condition)
                            .iter()
                            .map(move |value| (name.as_str(), value))
                    }),
                )?;
                check_legacy_conditions(conditions, conditional_operator)?;
                // Each entry's operand LIST is held to the rules that bind it as a whole
                // — one `AttributeValue` type across its members, and a `BETWEEN` range
                // the way round the comparator reads it — which are facts of the request
                // like the rules above.
                for condition in conditions.values() {
                    crate::state::check_legacy_operand_list(
                        legacy_member,
                        condition.comparison_operator.as_str(),
                        crate::state::legacy_operands(condition),
                    )?;
                }
                Ok(Self::Legacy {
                    member: legacy_member,
                    conditions,
                    conditional_operator,
                    // Indexed here, where every other fact of this filter is settled, so
                    // what one `IN` entry costs per item is a constant of this service
                    // rather than the number of values the request listed.
                    membership: LegacyMembership::of(conditions),
                })
            }
            (None, None) => {
                // The member combines the entries of the legacy filter map, so a
                // request that sends it with no map states how to combine nothing:
                // the one sentence every surface carrying the member states.
                crate::state::check_conditional_operator_companion(
                    legacy_member,
                    conditional_operator,
                    0,
                )?;
                Ok(Self::Unfiltered)
            }
        }
    }

    /// Whether this filter answers the attributes one read saw.
    pub fn matches(&self, seen: &Item) -> Result<bool, RequestError> {
        match self {
            Self::Unfiltered => Ok(true),
            Self::Expression(condition) => condition.evaluate(Some(seen)),
            Self::Legacy {
                conditions,
                conditional_operator,
                membership,
                ..
            } => legacy_conditions_match(conditions, *conditional_operator, membership, seen),
        }
    }

    /// The member this read states its filter THROUGH, as a refusal about the filter names
    /// it: the legacy map by its own member name, and the expression member as the one
    /// sentence about it spells that name.
    ///
    /// A refusal names the member the REQUEST carried — the rule every other refusal about
    /// this filter follows, `resolve` above being handed that member for exactly that
    /// reason — so a request that filtered the legacy way is never told about an expression
    /// member it did not send. A read that states no filter has no member to name and no
    /// attributes to be refused over either, so it answers the expression member's name
    /// like any read that stated no legacy map.
    pub fn stated_member(&self) -> &str {
        match self {
            Self::Unfiltered | Self::Expression(_) => FILTER_EXPRESSION_PHRASE,
            Self::Legacy { member, .. } => member,
        }
    }

    /// Every top-level attribute this filter READS, which is what a read served from an
    /// index must have in order to judge it — and what a key condition's own attributes
    /// are held against, since "a FilterExpression does not allow key attributes".
    pub fn attributes(&self) -> BTreeSet<String> {
        match self {
            Self::Unfiltered => BTreeSet::new(),
            Self::Expression(condition) => condition.attribute_reads().into_keys().collect(),
            Self::Legacy { conditions, .. } => conditions.keys().cloned().collect(),
        }
    }
}
