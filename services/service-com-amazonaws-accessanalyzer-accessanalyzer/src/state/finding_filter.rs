// SPDX-License-Identifier: CC0-1.0

//! Which findings a `FilterCriteriaMap` selects: the ONE match this crate applies a filter
//! over findings with.
//!
//! A filter is a map from the name of a finding member to the [`Criterion`] that member's
//! values must satisfy, and the model gives it the same shape wherever it appears — the
//! `filter` a findings listing carries, and the `filter` an archive rule stores. So the
//! match lives here rather than in one operation: the findings an archive rule archives are
//! exactly the findings a listing filtered by the SAME map reports, which they cannot be if
//! each operation reads the criteria its own way.
//!
//! A key names a member of `FindingSummary` ([`FindingMember`]), a map-valued member either
//! as a whole or at one of its keys, and a key naming no member at all is REFUSED rather
//! than silently matching nothing. The operators of one criterion are a conjunction and the
//! values of one operator a disjunction ([`FindingFilterCriterion::holds_for`]).
//!
//! Every comparison is charged to the caller's [`Budget`], so what one call spends on
//! findings-times-criteria work is bounded in the PRODUCT of its factors rather than in each
//! factor alone, exactly as the derivation the findings come from is.

use std::collections::HashMap;

use super::{field_validation_exception, Budget, Exhausted};
use crate::ops::types::{Criterion, FindingSource, FindingSummary, ValidationException};

/// How many criteria one `filter` may carry.
///
/// A criterion names a member of a finding, and the model gives a finding fourteen of them
/// (a map-valued one addressable at each of its keys), so a filter past this ceiling is not
/// a finer query but a multiplier on the match: every criterion is checked against every
/// finding. The emulator's own ceiling, in the same spirit as the model's `@length(max: 20)`
/// on each criterion's `ValueList`.
pub const MAX_FILTER_CRITERIA: usize = 20;

/// How many bytes of string comparison one filter unit buys. A criterion is checked by
/// comparing a finding's value against a value the filter supplied, so what the check costs
/// is their LENGTH, not just the number of pairs — charging per quantum of bytes is what
/// keeps one long value from buying unbounded work.
pub const FILTER_BYTES_PER_UNIT: usize = 64;

/// The request member a filter refusal names.
const FILTER_MEMBER: &str = "filter";

/// The finding members a criterion may read, each with the member name the wire spells it
/// by. Written out so a key naming no member is refused rather than silently matching
/// nothing.
pub const FILTERABLE_FINDING_MEMBERS: &[&str] = &[
    "id",
    "principal",
    "action",
    "resource",
    "isPublic",
    "resourceType",
    "condition",
    "status",
    "resourceOwnerAccount",
    "error",
    "resourceControlPolicyRestriction",
    "sources.type",
    "sources.detail.accessPointArn",
    "sources.detail.accessPointAccount",
];

/// Which member of a finding one filter key reads.
///
/// A criterion's key names a member of `FindingSummary`, so the catalog below is the
/// model's own member list (`smithy_query(query="shape", shape="FindingSummary")`) rather
/// than a list of accepted spellings: a map-valued member is addressed either as a whole or
/// by one of its keys (`principal.AWS`, `condition.aws:PrincipalOrgID`), and the `sources`
/// list by the member of its entries (`sources.type`).
#[derive(Debug)]
pub enum FindingMember {
    /// A member holding at most one value of its own, read as a borrow of the finding.
    Scalar(for<'a> fn(&'a FindingSummary) -> Option<&'a str>),
    /// `action`: the actions the statement allows.
    Action,
    /// `principal`, as a whole or at one principal type.
    Principal(Option<String>),
    /// `condition`, as a whole or at one condition key.
    Condition(Option<String>),
    /// `sources.type`: how the access is granted.
    SourceType,
    /// `sources.detail.accessPointArn`.
    SourceAccessPointArn,
    /// `sources.detail.accessPointAccount`.
    SourceAccessPointAccount,
}

impl FindingMember {
    /// The member `key` addresses, or `None` when it addresses none.
    pub fn parse(key: &str) -> Option<Self> {
        if let Some(entry) = key.strip_prefix("principal.") {
            return Some(Self::Principal(Some(entry.to_string())));
        }
        if let Some(entry) = key.strip_prefix("condition.") {
            return Some(Self::Condition(Some(entry.to_string())));
        }
        match key {
            "id" => Some(Self::Scalar(|finding| Some(finding.id.as_str()))),
            "resource" => Some(Self::Scalar(|finding| finding.resource.as_deref())),
            // The two spellings a boolean member is filtered by are fixed, so reading it
            // borrows one of them rather than rendering a string per comparison.
            "isPublic" => Some(Self::Scalar(|finding| {
                finding
                    .is_public
                    .map(|public| if public { "true" } else { "false" })
            })),
            "resourceType" => Some(Self::Scalar(|finding| Some(finding.resource_type.as_str()))),
            "status" => Some(Self::Scalar(|finding| Some(finding.status.as_str()))),
            "resourceOwnerAccount" => Some(Self::Scalar(|finding| {
                Some(finding.resource_owner_account.as_str())
            })),
            "error" => Some(Self::Scalar(|finding| finding.error.as_deref())),
            "resourceControlPolicyRestriction" => Some(Self::Scalar(|finding| {
                finding.resource_control_policy_restriction.as_deref()
            })),
            "action" => Some(Self::Action),
            "principal" => Some(Self::Principal(None)),
            "condition" => Some(Self::Condition(None)),
            "sources.type" => Some(Self::SourceType),
            "sources.detail.accessPointArn" => Some(Self::SourceAccessPointArn),
            "sources.detail.accessPointAccount" => Some(Self::SourceAccessPointAccount),
            _ => None,
        }
    }

    /// The values this member holds on `finding`, BORROWED from it: empty when the finding
    /// does not carry the member, which is what `exists` reads.
    ///
    /// Borrowed rather than cloned, so checking a criterion against a finding copies no
    /// string at all — the cost of a check is the bytes it compares, which is what the
    /// [`Budget`] is charged for.
    pub fn values<'a>(&self, finding: &'a FindingSummary) -> Vec<&'a str> {
        /// One entry of a map-valued member, or every entry when no key is named.
        fn entries<'a>(
            map: Option<&'a HashMap<String, String>>,
            at: Option<&String>,
        ) -> Vec<&'a str> {
            let Some(map) = map else {
                return Vec::new();
            };
            match at {
                Some(key) => map
                    .get(key.as_str())
                    .map(String::as_str)
                    .into_iter()
                    .collect(),
                None => map.values().map(String::as_str).collect(),
            }
        }
        /// One member of every finding source.
        fn of_sources<'a>(
            finding: &'a FindingSummary,
            member: impl Fn(&'a FindingSource) -> Option<&'a str>,
        ) -> Vec<&'a str> {
            finding
                .sources
                .iter()
                .flatten()
                .filter_map(member)
                .collect()
        }
        match self {
            Self::Scalar(read) => read(finding).into_iter().collect(),
            Self::Action => finding
                .action
                .iter()
                .flatten()
                .map(String::as_str)
                .collect(),
            Self::Principal(at) => entries(finding.principal.as_ref(), at.as_ref()),
            // `condition` is a REQUIRED member, so it is always there to read: a finding
            // whose statement constrains nothing carries the empty map.
            Self::Condition(at) => match at {
                Some(key) => finding
                    .condition
                    .get(key.as_str())
                    .map(String::as_str)
                    .into_iter()
                    .collect(),
                None => finding.condition.values().map(String::as_str).collect(),
            },
            Self::SourceType => of_sources(finding, |source| Some(source.r#type.as_str())),
            Self::SourceAccessPointArn => of_sources(finding, |source| {
                source
                    .detail
                    .as_ref()
                    .and_then(|detail| detail.access_point_arn.as_deref())
            }),
            Self::SourceAccessPointAccount => of_sources(finding, |source| {
                source
                    .detail
                    .as_ref()
                    .and_then(|detail| detail.access_point_account.as_deref())
            }),
        }
    }
}

/// One criterion of a filter: the finding member it reads, and the comparisons that
/// member's values must satisfy.
#[derive(Debug)]
pub struct FindingFilterCriterion {
    member: FindingMember,
    criterion: Criterion,
}

impl FindingFilterCriterion {
    /// Whether `finding` satisfies this criterion, charging the comparison to `budget`.
    ///
    /// The operators of one `Criterion` are a CONJUNCTION — a criterion carrying `eq` and
    /// `contains` demands both — and the values of one operator are a disjunction, which is
    /// how the model documents them ("an \"equals\" operator to match"). A member holding
    /// several values (`action`, a whole `principal` map) satisfies a positive operator
    /// when ANY of its values does, and a negative one only when NONE does.
    ///
    /// `Err` once the budget is spent: the match is refused rather than run to the end.
    pub fn holds_for(
        &self,
        finding: &FindingSummary,
        budget: &mut Budget,
    ) -> Result<bool, Exhausted> {
        let held = self.member.values(finding);
        // Reading the member's values is charged for too, so a member holding many of them
        // costs more than one holding none even before a comparison happens.
        if !budget.spend(1 + held.len() as u64) {
            return Err(filter_exhausted(budget.ceiling()));
        }
        if let Some(expected) = &self.criterion.eq {
            if !any_pair(&held, expected, budget, |value, expected| value == expected)? {
                return Ok(false);
            }
        }
        if let Some(rejected) = &self.criterion.neq {
            if any_pair(&held, rejected, budget, |value, rejected| value == rejected)? {
                return Ok(false);
            }
        }
        if let Some(needles) = &self.criterion.contains {
            if !any_pair(&held, needles, budget, |value, needle| value.contains(needle))? {
                return Ok(false);
            }
        }
        if let Some(exists) = self.criterion.exists {
            if exists == held.is_empty() {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

/// Matching a filter's criteria against the account's findings takes more work than one
/// call spends — so it is the `filter` the caller has to narrow.
fn filter_exhausted(ceiling: u64) -> Exhausted {
    Exhausted {
        member: FILTER_MEMBER,
        detail: format!(
            "matching this filter against the findings of this analyzer takes more than the \
             {ceiling} steps this emulator spends on one call"
        ),
    }
}

/// Whether `accept` holds for any (held value, criterion value) pair, charged to `budget`
/// by the BYTES each comparison may touch — so one long value buys no more work than its
/// length, and the match as a whole is bounded in the product of its factors.
fn any_pair(
    held: &[&str],
    values: &[String],
    budget: &mut Budget,
    accept: impl Fn(&str, &str) -> bool,
) -> Result<bool, Exhausted> {
    for value in held {
        for candidate in values {
            let bytes = value.len().saturating_add(candidate.len());
            if !budget.spend(1 + (bytes / FILTER_BYTES_PER_UNIT) as u64) {
                return Err(filter_exhausted(budget.ceiling()));
            }
            if accept(value, candidate.as_str()) {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// The findings every criterion holds for, in the order they were derived.
///
/// Runs under the SAME [`Budget`] as the derivation the findings came from, so one call's
/// findings-times-criteria work is capped rather than each factor alone.
pub fn findings_matching_filter(
    findings: Vec<(String, FindingSummary)>,
    criteria: &[FindingFilterCriterion],
    budget: &mut Budget,
) -> Result<Vec<(String, FindingSummary)>, Exhausted> {
    if criteria.is_empty() {
        return Ok(findings);
    }
    let mut matching = Vec::new();
    for (key, finding) in findings {
        let mut holds = true;
        for criterion in criteria {
            if !criterion.holds_for(&finding, budget)? {
                holds = false;
                break;
            }
        }
        if holds {
            matching.push((key, finding));
        }
    }
    Ok(matching)
}

/// Read a `filter` map into the criteria a selected finding must satisfy.
///
/// A key naming no member of a finding, and a filter carrying more than
/// [`MAX_FILTER_CRITERIA`] criteria, are refused with the modelled `ValidationException` the
/// calling operation lifts into its own error channel: it is a field validation, so the
/// message names the member at fault and the constraint it violated, which is what a client
/// has to fix. The criteria are ordered by key so a refusal does not depend on the map's
/// layout.
pub fn finding_filter_criteria(
    filter: Option<&HashMap<String, Criterion>>,
) -> Result<Vec<FindingFilterCriterion>, ValidationException> {
    let refuse =
        |message: String| -> ValidationException { field_validation_exception(FILTER_MEMBER, message) };
    let mut keys: Vec<&String> = filter.into_iter().flatten().map(|(key, _)| key).collect();
    if keys.len() > MAX_FILTER_CRITERIA {
        // Every criterion is checked against every finding, so the count is a multiplier on
        // the match and not just a finer query.
        return Err(refuse(format!(
            "filter carries {} criteria, more than the {MAX_FILTER_CRITERIA} this emulator \
             matches against one analyzer's findings",
            keys.len()
        )));
    }
    keys.sort();
    let mut criteria = Vec::with_capacity(keys.len());
    for key in keys {
        let member = FindingMember::parse(key).ok_or_else(|| {
            refuse(format!(
                "filter key {key:?} names no member of a finding; the filterable members \
                 are {}, and a map-valued member may also be addressed at one of its keys \
                 (principal.<principal type>, condition.<condition key>)",
                FILTERABLE_FINDING_MEMBERS.join(", ")
            ))
        })?;
        criteria.push(FindingFilterCriterion {
            member,
            criterion: filter
                .and_then(|filter| filter.get(key))
                .cloned()
                .unwrap_or_default(),
        });
    }
    Ok(criteria)
}
