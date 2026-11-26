// SPDX-License-Identifier: CC0-1.0

//! The per-RESOURCE roll-up of a Region's compliance items: what
//! `ListResourceComplianceSummaries` answers with.
//!
//! "Returns a resource-level summary count. The summary includes information about compliant and
//! non-compliant statuses and detailed compliance-item severity counts, according to the filter
//! criteria you specify" — so where the roll-up beside this one
//! ([`super::compliance_summaries`]) counts the RESOURCES of each compliance type, this one
//! reports ONE summary per resource and compliance type. Both are COMPUTED from the items this
//! service already holds ([`super::compliance_items`], the ones Patch Manager gathered and the
//! ones a `PutComplianceItems` wrote alike) rather than kept beside them: a put that changes a
//! resource's items changes its summary on the next read, and nothing can report a count the
//! items do not support.
//!
//! What each member reports is what the model documents it as:
//!
//! * `CompliantSummary` is "a list of items that are compliant for the resource" and
//!   `NonCompliantSummary` the ones that aren't, so in a RESOURCE-level summary each count is of
//!   the ITEMS that resource holds of that compliance type under that status, and so is each
//!   count of the `SeveritySummary` beside it — "the total number of resources or compliance
//!   items that have a severity level of" that level (`SeveritySummary$CriticalCount`), which
//!   here are the items.
//! * `Status` is "the compliance status for the resource": a resource is
//!   [`STATUS_COMPLIANT`] for a compliance type while every item it holds of that type is, and
//!   [`STATUS_NON_COMPLIANT`] as soon as one is not — the same reading of a resource's own
//!   compliance the type-level roll-up takes.
//! * `OverallSeverity` is "the highest severity item found for the resource", which is the most
//!   severe severity those items carry
//!   ([`super::compliance_summaries::severity_rank`]).
//! * `ExecutionSummary` is "information about the execution", which is the execution that
//!   reported this resource's compliance LAST: the summary carried by the item with the latest
//!   `ExecutionTime`, since "the time the execution ran" is what orders two reports of the same
//!   resource.
//!
//! The summaries are answered in ascending order of the resource and compliance type they are
//! about, which is what lets the read page over them with a cursor naming the one it stopped at.

use std::collections::BTreeMap;

use super::compliance_items::{ComplianceItemRecord, STATUS_COMPLIANT, STATUS_NON_COMPLIANT};
use super::compliance_summaries::{counted_severity, severity_rank, SeverityCounts};
use crate::ops::types::{
    ComplianceExecutionSummary, CompliantSummary, NonCompliantSummary,
    ResourceComplianceSummaryItem,
};

/// Which summary one roll-up is: the resource it is about, that resource's type and the
/// compliance type it was reported under — the three members a `ResourceComplianceSummaryItem`
/// names itself by, in the order the summaries are answered in.
pub type ResourceComplianceKey = (String, String, String);

/// The roll-up of ONE resource's items of ONE compliance type.
#[derive(Debug, Clone, Default)]
pub struct ResourceComplianceSummary {
    /// The severities of the resource's COMPLIANT items of this type, whose total is "a list of
    /// items that are compliant for the resource".
    compliant: SeverityCounts,
    /// The severities of the ones that are NOT compliant.
    non_compliant: SeverityCounts,
    /// The most severe severity any of those items carries, as the modelled vocabulary spells
    /// it ([`counted_severity`]), or `None` while no item has been folded in.
    overall_severity: Option<&'static str>,
    /// The execution summary of the item with the latest `ExecutionTime`.
    latest_execution: Option<ComplianceExecutionSummary>,
}

impl ResourceComplianceSummary {
    /// Fold one item of this resource and compliance type in.
    fn fold(&mut self, item: &ComplianceItemRecord) {
        if item.status == STATUS_COMPLIANT {
            self.compliant.count(&item.severity);
        } else {
            self.non_compliant.count(&item.severity);
        }
        let severity = counted_severity(&item.severity);
        // The HIGHEST severity found, which is the LOWEST rank: the severities are ranked most
        // severe first.
        if self
            .overall_severity
            .is_none_or(|held| severity_rank(severity) < severity_rank(held))
        {
            self.overall_severity = Some(severity);
        }
        // "The time the execution ran": the summary reported last is the one this resource's
        // compliance was last reported under, and the first item folded in is the latest so far.
        if self
            .latest_execution
            .as_ref()
            .is_none_or(|held| instant_of(held) <= instant_of(&item.execution_summary))
        {
            self.latest_execution = Some(item.execution_summary.clone());
        }
    }

    /// "The compliance status for the resource": compliant while every item it holds of this
    /// type is.
    pub fn status(&self) -> &'static str {
        if self.non_compliant.counted() == 0 {
            STATUS_COMPLIANT
        } else {
            STATUS_NON_COMPLIANT
        }
    }

    /// The modelled summary item this roll-up is, for the resource and compliance type it rolled
    /// up ([`ResourceComplianceKey`]).
    pub fn to_resource_compliance_summary_item(
        &self,
        key: &ResourceComplianceKey,
    ) -> ResourceComplianceSummaryItem {
        let (resource_id, resource_type, compliance_type) = key;
        ResourceComplianceSummaryItem {
            compliance_type: Some(compliance_type.clone()),
            resource_type: Some(resource_type.clone()),
            resource_id: Some(resource_id.clone()),
            status: Some(self.status().to_string()),
            overall_severity: self.overall_severity.map(ToString::to_string),
            execution_summary: self.latest_execution.clone(),
            compliant_summary: Some(CompliantSummary {
                compliant_count: self.compliant.counted(),
                severity_summary: Some(self.compliant.to_severity_summary()),
            }),
            non_compliant_summary: Some(NonCompliantSummary {
                non_compliant_count: self.non_compliant.counted(),
                severity_summary: Some(self.non_compliant.to_severity_summary()),
            }),
        }
    }
}

/// Roll `items` up by the resource and compliance type each was reported about, in ascending
/// order of that triple.
///
/// A pure computation over the items handed in — the caller has already narrowed them by the
/// request's own filters — so the same items always answer the same summaries.
pub fn summarize_by_resource<'a>(
    items: impl Iterator<Item = &'a ComplianceItemRecord>,
) -> BTreeMap<ResourceComplianceKey, ResourceComplianceSummary> {
    let mut summaries: BTreeMap<ResourceComplianceKey, ResourceComplianceSummary> = BTreeMap::new();
    for item in items {
        summaries
            .entry((
                item.resource_id.clone(),
                item.resource_type.clone(),
                item.compliance_type.clone(),
            ))
            .or_default()
            .fold(item);
    }
    summaries
}

/// The position in the walk one summary sits at, rendered so that comparing two of them as text
/// orders them the same way the summaries are answered in — what a page's cursor names.
///
/// INJECTIVE over the triple a summary is about, because the separator is a byte no member of it
/// carries (see [`super::parameter_pagination::CURSOR_SEPARATOR`]).
#[must_use]
pub fn summary_position(key: &ResourceComplianceKey) -> String {
    let separator = super::parameter_pagination::CURSOR_SEPARATOR;
    let (resource_id, resource_type, compliance_type) = key;
    format!("{resource_id}{separator}{resource_type}{separator}{compliance_type}")
}

/// Which read a continuation token of this roll-up was minted for, so a token it handed out
/// resumes nothing else.
const CURSOR_SCOPE: &str = "ListResourceComplianceSummaries";

/// The `NextToken` that resumes this read AFTER the summary at `position`.
///
/// Minted through this crate's ONE mint of an authenticated cursor
/// ([`super::parameter_pagination::scoped_page_token`]), under this store's own key, so a
/// position a caller assembled is refused rather than served a page this read never offered.
#[must_use]
pub fn page_token(
    key: &super::parameter_pagination::PageTokenKey,
    position: &str,
) -> String {
    super::parameter_pagination::scoped_page_token(key, CURSOR_SCOPE, position)
}

/// The position a `NextToken` resumes this read at, or `None` for any value THIS store did not
/// mint for THIS read ([`super::parameter_pagination::scoped_page_cursor`]).
#[must_use]
pub fn page_cursor(
    key: &super::parameter_pagination::PageTokenKey,
    token: &str,
) -> Option<String> {
    super::parameter_pagination::scoped_page_cursor(key, CURSOR_SCOPE, token)
}

/// The instant an execution summary reports, as the pair that orders two of them: "the time the
/// execution ran as a datetime object" (`ComplianceExecutionSummary$ExecutionTime`).
fn instant_of(summary: &ComplianceExecutionSummary) -> (i64, u32) {
    (
        summary.execution_time.secs(),
        summary.execution_time.subsec_nanos(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::compliance_items::ComplianceSource;

    /// One item, as the store holds one.
    fn item(
        compliance_type: &str,
        resource_id: &str,
        item_id: &str,
        status: &str,
        severity: &str,
        execution_time: i64,
    ) -> ComplianceItemRecord {
        ComplianceItemRecord {
            compliance_type: compliance_type.to_string(),
            resource_type: "ManagedInstance".to_string(),
            resource_id: resource_id.to_string(),
            item_id: Some(item_id.to_string()),
            title: None,
            status: status.to_string(),
            severity: severity.to_string(),
            execution_summary: ComplianceExecutionSummary {
                execution_time: ::aws_smithy_types::DateTime::from_secs(execution_time),
                execution_id: Some(format!("run-{execution_time}")),
                execution_type: None,
            },
            details: Vec::new(),
            source: ComplianceSource::Written,
            position: 0,
        }
    }

    /// RULE: one summary per resource and compliance type, whose counts are of that resource's
    /// ITEMS of that type, whose status is compliant only while every one of them is, whose
    /// overall severity is the most severe they carry, and whose execution summary is the latest
    /// they were reported under.
    #[test]
    fn a_resource_summary_counts_its_own_items_of_one_type() {
        let items = [
            item("Custom:Scan", "i-1", "a", STATUS_COMPLIANT, "LOW", 10),
            item("Custom:Scan", "i-1", "b", STATUS_COMPLIANT, "HIGH", 20),
            item("Custom:Scan", "i-2", "a", STATUS_COMPLIANT, "MEDIUM", 30),
            item("Custom:Scan", "i-2", "b", STATUS_NON_COMPLIANT, "CRITICAL", 40),
            item("Patch", "i-1", "p", STATUS_NON_COMPLIANT, "WHATEVER", 50),
        ];
        let summaries = summarize_by_resource(items.iter());
        assert_eq!(
            summaries.keys().cloned().collect::<Vec<ResourceComplianceKey>>(),
            vec![
                ("i-1".to_string(), "ManagedInstance".to_string(), "Custom:Scan".to_string()),
                ("i-1".to_string(), "ManagedInstance".to_string(), "Patch".to_string()),
                ("i-2".to_string(), "ManagedInstance".to_string(), "Custom:Scan".to_string()),
            ],
            "case \"the summaries\": expected one per resource and type in ascending order, got \
             {summaries:?}"
        );

        let key = (
            "i-1".to_string(),
            "ManagedInstance".to_string(),
            "Custom:Scan".to_string(),
        );
        let reported = summaries[&key].to_resource_compliance_summary_item(&key);
        assert_eq!(
            (
                reported.status.as_deref(),
                reported.overall_severity.as_deref(),
                reported.compliant_summary.as_ref().map(|half| half.compliant_count),
                reported
                    .non_compliant_summary
                    .as_ref()
                    .map(|half| half.non_compliant_count),
            ),
            (Some(STATUS_COMPLIANT), Some("HIGH"), Some(2), Some(0)),
            "case \"a resource whose every item is compliant\": got {reported:?}"
        );
        assert_eq!(
            reported
                .execution_summary
                .as_ref()
                .and_then(|summary| summary.execution_id.as_deref()),
            Some("run-20"),
            "case \"the latest execution\": expected the latest item's summary, got {reported:?}"
        );

        let key = (
            "i-2".to_string(),
            "ManagedInstance".to_string(),
            "Custom:Scan".to_string(),
        );
        let reported = summaries[&key].to_resource_compliance_summary_item(&key);
        assert_eq!(
            (
                reported.status.as_deref(),
                reported.overall_severity.as_deref(),
                reported.compliant_summary.as_ref().map(|half| half.compliant_count),
                reported
                    .non_compliant_summary
                    .as_ref()
                    .map(|half| half.non_compliant_count),
            ),
            (Some(STATUS_NON_COMPLIANT), Some("CRITICAL"), Some(1), Some(1)),
            "case \"a resource with an item that is not compliant\": got {reported:?}"
        );

        // A severity the summary has no member for is counted — and ranked — as unspecified.
        let key = (
            "i-1".to_string(),
            "ManagedInstance".to_string(),
            "Patch".to_string(),
        );
        let reported = summaries[&key].to_resource_compliance_summary_item(&key);
        assert_eq!(
            reported.overall_severity.as_deref(),
            Some("UNSPECIFIED"),
            "case \"a severity with no member\": got {reported:?}"
        );
    }

    /// RULE: the position two summaries are walked in is the ascending order of the triple each
    /// is about, so comparing two positions as text orders them the way they are answered.
    #[test]
    fn a_position_orders_the_summaries_the_way_they_are_answered() {
        let keys: Vec<ResourceComplianceKey> = vec![
            ("i-1".to_string(), "ManagedInstance".to_string(), "Custom:Scan".to_string()),
            ("i-1".to_string(), "ManagedInstance".to_string(), "Patch".to_string()),
            ("i-2".to_string(), "ManagedInstance".to_string(), "Custom:Scan".to_string()),
        ];
        let positions: Vec<String> = keys.iter().map(summary_position).collect();
        let mut sorted = positions.clone();
        sorted.sort();
        assert_eq!(
            positions, sorted,
            "case \"the walk order\": expected the positions to sort the way the summaries are \
             answered, got {positions:?}"
        );
    }
}
