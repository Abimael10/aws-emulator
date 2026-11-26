// SPDX-License-Identifier: CC0-1.0

//! The ROLL-UP of a Region's compliance items by the compliance type they were reported
//! under: what `ListComplianceSummaries` answers with.
//!
//! "Returns a summary count of compliant and non-compliant resources for a compliance type"
//! (`ListComplianceSummaries`) — so the answer is COMPUTED from the items this service already
//! holds ([`super::compliance_items`], both the ones Patch Manager gathered and the ones a
//! `PutComplianceItems` wrote) rather than kept beside them: a put that changes a resource's
//! items changes the summary of its type on the next read, and nothing can report a count the
//! items do not support.
//!
//! What each number counts is what the model documents it as:
//!
//! * `CompliantSummary$CompliantCount` is "the total number of resources that are compliant"
//!   and `NonCompliantSummary$NonCompliantCount` the resources that are not, so both count
//!   RESOURCES: one resource is counted once per compliance type, and it is COMPLIANT for that
//!   type when every item it holds of it is ([`super::compliance_items::STATUS_COMPLIANT`]) and
//!   NON-COMPLIANT as soon as one is not — the reading that makes a resource's own compliance
//!   the state a client acts on.
//! * a `SeveritySummary` count is "the total number of resources or compliance items that have
//!   a severity level of" that level (`SeveritySummary$CriticalCount`), and what carries a
//!   severity is the ITEM — "the severity of the compliance status" (`ComplianceItem$Severity`),
//!   which the publisher of the item sets — so the severities of one type's items are counted
//!   there, under the status of the item carrying each.
//!
//! The types are answered in ascending order, which is what lets the read page over them with
//! a cursor naming the type it stopped at.

use std::collections::BTreeMap;

use super::compliance_items::{ComplianceItemRecord, SEVERITY_UNSPECIFIED, STATUS_COMPLIANT};
use crate::ops::types::{ComplianceSummaryItem, CompliantSummary, NonCompliantSummary, SeveritySummary};

/// The severity values a summary has a count for, each with the member of a
/// [`SeveritySummary`] it is counted in: the modelled `ComplianceSeverity` vocabulary
/// (`crate::ops::types::COMPLIANCE_SEVERITY_VALUES`), compared the way the service compares a
/// severity — a value outside it is counted as unspecified, since the summary has no member
/// for it.
///
/// Written MOST SEVERE FIRST, with the unspecified level LAST, so a position in it is both
/// which member counts a severity and how severe that severity is ([`severity_rank`]).
const SEVERITY_MEMBERS: &[(&str, Severity)] = &[
    ("CRITICAL", Severity::Critical),
    ("HIGH", Severity::High),
    ("MEDIUM", Severity::Medium),
    ("LOW", Severity::Low),
    ("INFORMATIONAL", Severity::Informational),
    ("UNSPECIFIED", Severity::Unspecified),
];

/// Which member of a [`SeveritySummary`] one item's severity is counted in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Severity {
    Critical,
    High,
    Medium,
    Low,
    Informational,
    Unspecified,
}

/// The severity a summary counts `severity` under, as the modelled `ComplianceSeverity`
/// vocabulary spells it ([`SEVERITY_MEMBERS`]): a value the vocabulary does not name is counted
/// as unspecified, the one level the summary has for a severity it has no member for.
#[must_use]
pub fn counted_severity(severity: &str) -> &'static str {
    SEVERITY_MEMBERS
        .get(severity_rank(severity))
        .map_or(SEVERITY_UNSPECIFIED, |(value, _)| *value)
}

/// Where `severity` sits in the order the severities are written in, MOST SEVERE FIRST — so the
/// lower rank is the higher severity, which is what "the highest severity item found for the
/// resource" (`ResourceComplianceSummaryItem$OverallSeverity`) picks by. A value the vocabulary
/// does not name ranks as the LAST entry, which is the unspecified level the summary counts it
/// under.
#[must_use]
pub fn severity_rank(severity: &str) -> usize {
    SEVERITY_MEMBERS
        .iter()
        .position(|(value, _)| value.eq_ignore_ascii_case(severity))
        .unwrap_or(SEVERITY_MEMBERS.len() - 1)
}

/// The counts of one `SeveritySummary`, as the roll-up builds it.
#[derive(Debug, Clone, Default)]
pub struct SeverityCounts {
    critical: i32,
    high: i32,
    medium: i32,
    low: i32,
    informational: i32,
    unspecified: i32,
}

impl SeverityCounts {
    /// Count one item of `severity`.
    pub fn count(&mut self, severity: &str) {
        let member = SEVERITY_MEMBERS
            .get(severity_rank(severity))
            .map_or(Severity::Unspecified, |(_, member)| *member);
        let counted = match member {
            Severity::Critical => &mut self.critical,
            Severity::High => &mut self.high,
            Severity::Medium => &mut self.medium,
            Severity::Low => &mut self.low,
            Severity::Informational => &mut self.informational,
            Severity::Unspecified => &mut self.unspecified,
        };
        *counted = counted.saturating_add(1);
    }

    /// How many items these counts have counted, whatever severity each carried: every item
    /// is counted in exactly one member, so their sum is the item count.
    pub fn counted(&self) -> i32 {
        self.critical
            .saturating_add(self.high)
            .saturating_add(self.medium)
            .saturating_add(self.low)
            .saturating_add(self.informational)
            .saturating_add(self.unspecified)
    }

    /// The modelled summary these counts are.
    pub fn to_severity_summary(&self) -> SeveritySummary {
        SeveritySummary {
            critical_count: self.critical,
            high_count: self.high,
            medium_count: self.medium,
            low_count: self.low,
            informational_count: self.informational,
            unspecified_count: self.unspecified,
        }
    }
}

/// The roll-up of ONE compliance type: which resources hold items of it and whether each is
/// compliant, and the severities its items carry under each status.
#[derive(Debug, Clone, Default)]
pub struct ComplianceTypeSummary {
    /// Whether each resource holding items of this type is compliant — every item of it
    /// compliant — keyed by the resource id so a resource is counted once however many items
    /// it holds.
    resources: BTreeMap<String, bool>,
    /// The severities of its COMPLIANT items.
    compliant: SeverityCounts,
    /// The severities of its NON-COMPLIANT items.
    non_compliant: SeverityCounts,
}

impl ComplianceTypeSummary {
    /// Fold one item of this compliance type in.
    fn fold(&mut self, item: &ComplianceItemRecord) {
        let compliant = item.status == STATUS_COMPLIANT;
        // One resource is compliant for this type only while every item it holds of it is, so
        // an item that is not compliant decides the resource whatever the others said.
        let held = self.resources.entry(item.resource_id.clone()).or_insert(true);
        *held = *held && compliant;
        if compliant {
            self.compliant.count(&item.severity);
        } else {
            self.non_compliant.count(&item.severity);
        }
    }

    /// How many resources of this type are compliant: "the total number of resources that are
    /// compliant" (`CompliantSummary$CompliantCount`).
    pub fn compliant_resources(&self) -> i32 {
        count_of(self.resources.values().filter(|compliant| **compliant))
    }

    /// How many are not ("the total number of resources that aren't compliant",
    /// `NonCompliantSummary$NonCompliantCount`).
    pub fn non_compliant_resources(&self) -> i32 {
        count_of(self.resources.values().filter(|compliant| !**compliant))
    }

    /// The modelled summary item this roll-up is, for the type it rolled up.
    pub fn to_compliance_summary_item(&self, compliance_type: &str) -> ComplianceSummaryItem {
        ComplianceSummaryItem {
            compliance_type: Some(compliance_type.to_string()),
            compliant_summary: Some(CompliantSummary {
                compliant_count: self.compliant_resources(),
                severity_summary: Some(self.compliant.to_severity_summary()),
            }),
            non_compliant_summary: Some(NonCompliantSummary {
                non_compliant_count: self.non_compliant_resources(),
                severity_summary: Some(self.non_compliant.to_severity_summary()),
            }),
        }
    }
}

/// Roll `items` up by the compliance type each was reported under, in ascending type order.
///
/// A pure computation over the items handed in — the caller has already narrowed them by the
/// request's own filters — so the same items always answer the same summaries.
pub fn summarize<'a>(
    items: impl Iterator<Item = &'a ComplianceItemRecord>,
) -> BTreeMap<String, ComplianceTypeSummary> {
    let mut summaries: BTreeMap<String, ComplianceTypeSummary> = BTreeMap::new();
    for item in items {
        summaries
            .entry(item.compliance_type.clone())
            .or_default()
            .fold(item);
    }
    summaries
}

/// How many entries an iterator yields, as the modelled count member carries it.
fn count_of<T>(entries: impl Iterator<Item = T>) -> i32 {
    i32::try_from(entries.count()).unwrap_or(i32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::types::ComplianceExecutionSummary;
    use crate::state::compliance_items::{ComplianceSource, STATUS_NON_COMPLIANT};

    /// One written item, as the store holds one.
    fn item(
        compliance_type: &str,
        resource_id: &str,
        item_id: &str,
        status: &str,
        severity: &str,
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
                execution_time: ::aws_smithy_types::DateTime::from_secs(0),
                execution_id: None,
                execution_type: None,
            },
            details: Vec::new(),
            source: ComplianceSource::Written,
            position: 0,
        }
    }

    /// RULE: the counts are of RESOURCES — one resource is compliant for a type only while
    /// every item it holds of it is — and the severity counts are of the ITEMS under each
    /// status, per compliance type.
    #[test]
    fn a_type_counts_its_resources_and_the_severities_of_their_items() {
        let items = [
            // A resource whose every item is compliant.
            item("Custom:Scan", "i-1", "a", STATUS_COMPLIANT, "LOW"),
            item("Custom:Scan", "i-1", "b", STATUS_COMPLIANT, "HIGH"),
            // One with an item that is not: the resource is not compliant.
            item("Custom:Scan", "i-2", "a", STATUS_COMPLIANT, "MEDIUM"),
            item("Custom:Scan", "i-2", "b", STATUS_NON_COMPLIANT, "CRITICAL"),
            // Another type, over one of the same resources.
            item("Patch", "i-1", "p", STATUS_NON_COMPLIANT, "INFORMATIONAL"),
            // A severity the summary has no member for is counted as unspecified.
            item("Patch", "i-2", "p", STATUS_NON_COMPLIANT, "WHATEVER"),
        ];
        let summaries = summarize(items.iter());
        assert_eq!(
            summaries.keys().collect::<Vec<&String>>(),
            vec!["Custom:Scan", "Patch"],
            "case \"the types\": expected the types in ascending order, got {summaries:?}"
        );

        let scan = &summaries["Custom:Scan"];
        assert_eq!(
            (scan.compliant_resources(), scan.non_compliant_resources()),
            (1, 1),
            "case \"the resource counts\": expected one resource of each, got {scan:?}"
        );
        let reported = scan.to_compliance_summary_item("Custom:Scan");
        let compliant = reported
            .compliant_summary
            .and_then(|summary| summary.severity_summary)
            .unwrap_or_default();
        assert_eq!(
            (compliant.low_count, compliant.high_count, compliant.medium_count),
            (1, 1, 1),
            "case \"the compliant severities\": expected the three compliant items counted, \
             got {compliant:?}"
        );
        let non_compliant = reported
            .non_compliant_summary
            .and_then(|summary| summary.severity_summary)
            .unwrap_or_default();
        assert_eq!(
            (non_compliant.critical_count, non_compliant.low_count),
            (1, 0),
            "case \"the non-compliant severities\": expected the one critical item counted, \
             got {non_compliant:?}"
        );

        let patch = &summaries["Patch"];
        assert_eq!(
            (patch.compliant_resources(), patch.non_compliant_resources()),
            (0, 2),
            "case \"the other type\": expected both resources non-compliant, got {patch:?}"
        );
        let severities = patch
            .to_compliance_summary_item("Patch")
            .non_compliant_summary
            .and_then(|summary| summary.severity_summary)
            .unwrap_or_default();
        assert_eq!(
            (severities.informational_count, severities.unspecified_count),
            (1, 1),
            "case \"a severity with no member\": expected it counted as unspecified, got \
             {severities:?}"
        );
    }
}
