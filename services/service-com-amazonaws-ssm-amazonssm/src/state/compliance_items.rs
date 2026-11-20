// SPDX-License-Identifier: CC0-1.0

//! The COMPLIANCE ITEMS this endpoint holds for a managed node — what `ListComplianceItems`
//! reports: "for a specified resource ID, this API operation returns a list of compliance
//! statuses for different resource types".
//!
//! # What an item is, and where it comes from
//!
//! "The compliance type. For example, Association (for a State Manager association), Patch,
//! or Custom:`string` are all valid compliance types" (`ComplianceItem$ComplianceType`), and
//! "`ManagedInstance` is currently the only supported resource type"
//! (`ComplianceItem$ResourceType`). The items this endpoint holds are
//! [`COMPLIANCE_TYPE_PATCH`] ones: Patch Manager reports, patch by patch, what it found on a
//! node ([`super::instance_patches::PatchComplianceRow`]), and each of those rows IS the
//! compliance of one patch on that node — "for a patch resource type, `Items` includes
//! information about the PatchSeverity, Classification, and so on"
//! (`ComplianceItem`). So nothing is stored twice: an item is the projection of the row the
//! patching operation left, and a node no patching operation has reported for holds no item.
//!
//! The compliance a caller WRITES itself (`PutComplianceItems`) and the compliance State
//! Manager derives for its associations are each a store of their own that nothing here
//! holds yet, so a read narrowed to one of those types reports none rather than an item
//! nothing reported.
//!
//! # What a read may ask for
//!
//! `ComplianceStringFilterKey` is a plain string in the model, so the vocabulary is enforced
//! here ([`COMPLIANCE_FILTER_KEYS`]) the way this service's other reads enforce a filter key
//! their model leaves open: a key that names no member of a compliance item narrows a report
//! by nothing and is refused (`InvalidFilter`). Every one of the five modelled comparisons
//! ([`crate::ops::types::COMPLIANCE_QUERY_OPERATOR_TYPE_VALUES`]) is served — "the type of
//! comparison that should be performed for the value: Equal, NotEqual, BeginWith, LessThan,
//! or GreaterThan" — with the two ordering ones comparing the instant an execution ran as an
//! instant and every other member as text.

use std::collections::HashMap;
use std::time::SystemTime;

use crate::ops::types::{
    ComplianceExecutionSummary, ComplianceItem, COMPLIANCE_QUERY_OPERATOR_TYPE_VALUES,
};

use super::instance_patch_states::InstancePatchStateRecord;
use super::instance_patches::PatchComplianceRow;
use super::ComAmazonawsSsmAmazonssmState;

/// The compliance type of an item Patch Manager reports: "Association (for a State Manager
/// association), Patch, or Custom:`string` are all valid compliance types"
/// (`ComplianceItem$ComplianceType`).
pub const COMPLIANCE_TYPE_PATCH: &str = "Patch";

/// "`ManagedInstance` is currently the only supported resource type"
/// (`ComplianceItem$ResourceType`), which is the kind of resource every item here is about.
pub const RESOURCE_TYPE_MANAGED_INSTANCE: &str = "ManagedInstance";

/// The status of an item whose patch is in the state the baseline wants it in, from the
/// modelled `ComplianceStatus` enum (`crate::ops::types::COMPLIANCE_STATUS_VALUES`).
pub const STATUS_COMPLIANT: &str = "COMPLIANT";

/// The status of one that is not (same enum): "an item is either COMPLIANT, NON_COMPLIANT, or
/// an empty string (for Windows patches that aren't applicable)"
/// (`ComplianceItem$Status`).
pub const STATUS_NON_COMPLIANT: &str = "NON_COMPLIANT";

/// The severity of an item whose baseline assigns its patch no compliance level, from the
/// modelled `ComplianceSeverity` enum (`crate::ops::types::COMPLIANCE_SEVERITY_VALUES`),
/// which is the vocabulary a patch baseline's own compliance levels are drawn from.
pub const SEVERITY_UNSPECIFIED: &str = "UNSPECIFIED";

/// "The type of execution. For example, `Command` is a valid execution type"
/// (`ComplianceExecutionSummary$ExecutionType`): a patching operation is performed by the
/// patch-baseline document Patch Manager runs on the node, which is a command.
pub const EXECUTION_TYPE_COMMAND: &str = "Command";

/// The `Details` keys a patch item carries: "for a patch resource type, `Items` includes
/// information about the PatchSeverity, Classification, and so on" (`ComplianceItem`), and
/// the state the patching operation found the patch in, which is what the status above is
/// decided from.
pub const DETAIL_PATCH_SEVERITY: &str = "PatchSeverity";
pub const DETAIL_CLASSIFICATION: &str = "Classification";
pub const DETAIL_PATCH_STATE: &str = "PatchState";
pub const DETAIL_PATCH_BASELINE_ID: &str = "PatchBaselineId";

/// The filter keys a compliance read narrows by: the members of a `ComplianceItem` a value
/// can be compared against, each under its own member name.
///
/// `ComplianceStringFilterKey` is a plain string in the model, so this is the vocabulary and
/// what the refusal of a key outside it names back to the caller.
pub const COMPLIANCE_FILTER_KEYS: &[&str] = &[
    "ComplianceType",
    "ExecutionId",
    "ExecutionTime",
    "ExecutionType",
    "Id",
    "ResourceId",
    "ResourceType",
    "Severity",
    "Status",
    "Title",
];

/// The comparison a filter that names no type is served under: "Equal" is the comparison a
/// value is searched for by, and the modelled `ComplianceQueryOperatorType` spells it
/// `EQUAL`.
pub const OPERATOR_EQUAL: &str = "EQUAL";
/// The comparison that holds when the member differs from the value (same enum).
pub const OPERATOR_NOT_EQUAL: &str = "NOT_EQUAL";
/// The comparison that holds when the member starts with the value (same enum).
pub const OPERATOR_BEGIN_WITH: &str = "BEGIN_WITH";
/// The comparison that holds when the member is ordered before the value (same enum).
pub const OPERATOR_LESS_THAN: &str = "LESS_THAN";
/// The comparison that holds when it is ordered after it (same enum).
pub const OPERATOR_GREATER_THAN: &str = "GREATER_THAN";

/// The lower bound the modelled page size of this read carries (`MaxResults`, range 1..50).
pub const MIN_COMPLIANCE_PAGE_SIZE: i32 = 1;
/// The upper bound of the same member, and the page a request naming no `MaxResults` is
/// served up to (see [`super::page_size`]).
pub const MAX_COMPLIANCE_PAGE_SIZE: i32 = 50;

/// The page size this read serves, or the refusal message for a `MaxResults` outside the
/// modelled range — which the boundary walk already refuses, so this is a backstop rather
/// than the first line of defence.
pub fn compliance_page_size(max_results: Option<i32>) -> Result<usize, String> {
    super::page_size(
        max_results,
        MIN_COMPLIANCE_PAGE_SIZE,
        MAX_COMPLIANCE_PAGE_SIZE,
    )
}

/// How this service states a filter key that names no member of a compliance item.
/// `InvalidFilter`'s own documentation is "the filter name isn't valid. Verify that you
/// entered the correct name and try again", which is the sentence this service's other
/// unknown-filter refusals are worded with ([`super::inventory::INVALID_FILTER`]).
pub const INVALID_COMPLIANCE_FILTER: &str = super::inventory::INVALID_FILTER;

/// Why one `Filters` entry of a request cannot be served, or `None` for a filter this read
/// can narrow by.
///
/// The key has to name a member of a compliance item, the comparison has to be one of the
/// modelled ones, and the filter has to carry a value to search for: "the value for which to
/// search" is what narrows anything, so an entry naming a key and no value narrows nothing.
/// An entry naming NO key at all narrows nothing either — both members of a
/// `ComplianceStringFilter` are optional in the model, so such an entry reaches the handler —
/// and is refused rather than served as an unnarrowed success. Each is a refusal of the
/// REQUEST, taken before an item is read.
pub fn compliance_filter_rejection(
    key: Option<&str>,
    values: &[String],
    operator: Option<&str>,
) -> Option<String> {
    let Some(key) = key.filter(|key| !key.is_empty()) else {
        return Some(INVALID_COMPLIANCE_FILTER.to_string());
    };
    if !COMPLIANCE_FILTER_KEYS.contains(&key) {
        return Some(INVALID_COMPLIANCE_FILTER.to_string());
    }
    if operator
        .filter(|operator| !operator.is_empty())
        .is_some_and(|operator| !COMPLIANCE_QUERY_OPERATOR_TYPE_VALUES.contains(&operator))
    {
        return Some(INVALID_COMPLIANCE_FILTER.to_string());
    }
    values
        .iter()
        .all(|value| value.is_empty())
        .then(|| INVALID_COMPLIANCE_FILTER.to_string())
}

/// One compliance item of a managed node, as this endpoint holds the facts it is made of.
///
/// A projection rather than a store: every member below is read off the patch row the
/// patching operation left and the report that carries it, so what a read reports is what
/// was gathered.
#[derive(Debug, Clone)]
pub struct ComplianceItemRecord {
    /// "The compliance type", which is [`COMPLIANCE_TYPE_PATCH`] for every item here.
    pub compliance_type: String,
    /// "An ID for the resource. For a managed node, this is the node ID."
    pub resource_id: String,
    /// "An ID for the compliance item. For example, if the compliance item is a Windows
    /// patch, the ID could be the number of the KB article" — the operating-system id of the
    /// patch, which is what a patch read names a patch by.
    pub id: String,
    /// "A title for the compliance item", which is the patch's own title.
    pub title: String,
    /// "The status of the compliance item" ([`STATUS_COMPLIANT`],
    /// [`STATUS_NON_COMPLIANT`]).
    pub status: String,
    /// "The severity of the compliance status", which is the compliance level the baseline
    /// assigned the patch, and [`SEVERITY_UNSPECIFIED`] for a patch it assigned none.
    pub severity: String,
    /// "The time the execution ran", which is when the patching operation that reported this
    /// item completed on the node.
    pub execution_time: SystemTime,
    /// "A 'Key': 'Value' tag combination for the compliance item", which for a patch is what
    /// the patch read reports about it beside its state.
    pub details: Vec<(String, String)>,
}

impl ComplianceItemRecord {
    /// The item one patch ROW of `report` is, for the node that report was gathered on.
    pub fn of_patch(report: &InstancePatchStateRecord, row: &PatchComplianceRow) -> Self {
        let mut details = vec![
            (DETAIL_PATCH_STATE.to_string(), row.state.clone()),
            (
                DETAIL_PATCH_BASELINE_ID.to_string(),
                report.baseline_id.clone(),
            ),
        ];
        if !row.classification.is_empty() {
            details.push((
                DETAIL_CLASSIFICATION.to_string(),
                row.classification.clone(),
            ));
        }
        if !row.severity.is_empty() {
            details.push((DETAIL_PATCH_SEVERITY.to_string(), row.severity.clone()));
        }
        details.sort();
        Self {
            compliance_type: COMPLIANCE_TYPE_PATCH.to_string(),
            resource_id: report.instance_id.clone(),
            // "The operating system-specific ID of the patch", which is the id a patch read
            // names the patch by.
            id: row.kb_id.clone(),
            title: row.title.clone(),
            // A patch the baseline wants on the node and is not there is what makes the node
            // non-compliant — the very states the non-compliant counts of the high-level
            // report are counted over
            // ([`super::instance_patch_states::NON_COMPLIANT_STATES`]) — so the status of an
            // item is read from that one set rather than stated a second way.
            status: if super::instance_patch_states::NON_COMPLIANT_STATES
                .contains(&row.state.as_str())
            {
                STATUS_NON_COMPLIANT.to_string()
            } else {
                STATUS_COMPLIANT.to_string()
            },
            severity: row
                .compliance_level
                .clone()
                .unwrap_or_else(|| SEVERITY_UNSPECIFIED.to_string()),
            execution_time: report.operation_end_time,
            details,
        }
    }

    /// The value a filter on `key` is matched against, or `None` when `key` names no member
    /// of a compliance item ([`COMPLIANCE_FILTER_KEYS`]).
    ///
    /// The instant an execution ran is compared as the text the member is rendered as on the
    /// wire for the three comparisons that compare values, and as an instant for the two that
    /// ORDER them ([`Self::matches_filter`]).
    pub fn filter_value(&self, key: &str) -> Option<String> {
        match key {
            "ComplianceType" => Some(self.compliance_type.clone()),
            // An item of this endpoint is reported by the patching operation that gathered
            // it, which uploaded no execution id of its own, so a filter on one matches
            // nothing rather than a value invented here.
            "ExecutionId" => Some(String::new()),
            "ExecutionTime" => Some(rendered_instant(self.execution_time)),
            "ExecutionType" => Some(EXECUTION_TYPE_COMMAND.to_string()),
            "Id" => Some(self.id.clone()),
            "ResourceId" => Some(self.resource_id.clone()),
            "ResourceType" => Some(RESOURCE_TYPE_MANAGED_INSTANCE.to_string()),
            "Severity" => Some(self.severity.clone()),
            "Status" => Some(self.status.clone()),
            "Title" => Some(self.title.clone()),
            _ => None,
        }
    }

    /// Whether this item satisfies one filter: any of its `values` under `operator`.
    ///
    /// One filter compares ONE member of the item against the values it carries, and two
    /// filters narrow one another, which is what "use a filter to return a more specific list
    /// of results" means. A key naming no member of an item admits it under no value, which
    /// is a case a caller cannot reach because such a key is refused before any item is read
    /// ([`compliance_filter_rejection`]).
    pub fn matches_filter(&self, key: &str, values: &[String], operator: &str) -> bool {
        let Some(held) = self.filter_value(key) else {
            return false;
        };
        let compares = |value: &String| -> bool {
            match operator {
                OPERATOR_NOT_EQUAL => &held != value,
                OPERATOR_BEGIN_WITH => held.starts_with(value.as_str()),
                OPERATOR_LESS_THAN => ordered_before(key, &held, value),
                OPERATOR_GREATER_THAN => ordered_before(key, value, &held),
                _ => &held == value,
            }
        };
        // "NotEqual" narrows to the items that differ from EVERY value it carries: a filter
        // naming two values asks for the items that are neither of them, where the other
        // comparisons ask for the items that satisfy any of them.
        if operator == OPERATOR_NOT_EQUAL {
            return values.iter().all(compares);
        }
        values.iter().any(compares)
    }

    /// The modelled `ComplianceItem` this record is reported as.
    pub fn to_compliance_item(&self) -> ComplianceItem {
        ComplianceItem {
            compliance_type: Some(self.compliance_type.clone()),
            resource_type: Some(RESOURCE_TYPE_MANAGED_INSTANCE.to_string()),
            resource_id: Some(self.resource_id.clone()),
            id: Some(self.id.clone()),
            title: Some(self.title.clone()),
            status: Some(self.status.clone()),
            severity: Some(self.severity.clone()),
            execution_summary: Some(ComplianceExecutionSummary {
                execution_time: ::aws_smithy_types::DateTime::from(self.execution_time),
                // "An ID created by the system when `PutComplianceItems` was called" — no
                // call wrote this item, so it carries no such id rather than one invented
                // here.
                execution_id: None,
                execution_type: Some(EXECUTION_TYPE_COMMAND.to_string()),
            }),
            details: Some(
                self.details
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect::<HashMap<String, String>>(),
            ),
        }
    }
}

/// Whether `left` is ordered before `right` for a filter on `key`: as INSTANTS for the member
/// that carries one, and as text for every other member.
fn ordered_before(key: &str, left: &str, right: &str) -> bool {
    if key == "ExecutionTime" {
        return match (super::request_instant(left), super::request_instant(right)) {
            (Some(left), Some(right)) => left < right,
            // A value that names no instant orders against nothing, so the comparison admits
            // no item rather than falling back on comparing the text of two timestamps.
            _ => false,
        };
    }
    left < right
}

/// The instant `at` as the text it is compared against: the `yyyy-MM-dd'T'HH:mm:ss'Z'` form
/// `ComplianceExecutionSummary$ExecutionTime` documents, which is what a filter naming an
/// instant carries.
fn rendered_instant(at: SystemTime) -> String {
    ::aws_smithy_types::DateTime::from(at)
        .fmt(::aws_smithy_types::date_time::Format::DateTime)
        .unwrap_or_default()
}

impl ComAmazonawsSsmAmazonssmState {
    /// The compliance items `region` holds for the nodes `resource_ids` names — every node of
    /// the Region when it names none — in ascending `(node id, item id)` order.
    ///
    /// A READ: the items are the projection of the patch rows the patching operations of those
    /// nodes left ([`ComplianceItemRecord::of_patch`]), so nothing is written and two callers
    /// see the same items. The order is total and taken from the items themselves, which is
    /// what lets a page resume at the item the last one stopped at rather than at an offset:
    /// the items a node holds are its patches, of one compliance type and one id each, so the
    /// pair is the whole of an item's identity.
    pub fn compliance_items(
        &self,
        region: &str,
        resource_ids: &[String],
    ) -> Vec<ComplianceItemRecord> {
        let mut items: Vec<ComplianceItemRecord> = self
            .instance_patch_states
            .iter()
            .filter(|((held_region, _), _)| held_region == region)
            .filter(|((_, node), _)| {
                resource_ids.is_empty() || resource_ids.iter().any(|named| named == node)
            })
            .flat_map(|(_, report)| {
                report
                    .patches
                    .iter()
                    .map(|row| ComplianceItemRecord::of_patch(report, row))
            })
            .collect();
        items.sort_by(|left, right| {
            (&left.resource_id, &left.id).cmp(&(&right.resource_id, &right.id))
        });
        items
    }
}
