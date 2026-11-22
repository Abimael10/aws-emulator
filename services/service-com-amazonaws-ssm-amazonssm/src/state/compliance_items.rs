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
//! (`ComplianceItem$ResourceType`). The items this endpoint holds come from the two places
//! compliance comes from, and a read reports both:
//!
//! * the [`COMPLIANCE_TYPE_PATCH`] ones Patch Manager gathered — patch by patch, what it
//!   found on a node ([`super::instance_patches::PatchComplianceRow`]), where each row IS the
//!   compliance of one patch on that node ("for a patch resource type, `Items` includes
//!   information about the PatchSeverity, Classification, and so on", `ComplianceItem`). So
//!   nothing is stored twice: such an item is the projection of the row the patching operation
//!   left ([`ComplianceItemRecord::of_patch`]), and a node no patching operation has reported
//!   for holds none;
//! * the ones a caller WROTE itself: "registers a compliance type and other compliance
//!   details on a designated resource. This operation lets you register custom compliance
//!   details with a resource" (`PutComplianceItems`), which is the store below
//!   ([`super::ComAmazonawsSsmAmazonssmState::written_compliance_items`]).
//!
//! # What one put addresses
//!
//! A put states the resource (`ResourceId` of `ResourceType`) and ONE `ComplianceType`, and
//! "this call overwrites existing compliance information on the resource, so you must provide
//! a full list of compliance items each time that you send the request" — so the items a put
//! carries REPLACE the ones that resource held of that compliance type, and the items it holds
//! of any OTHER compliance type are untouched: the types are independent reports about the
//! same resource (a patch report does not erase an association's). The store is therefore
//! keyed by `(region, resource type, resource id, compliance type, position)`, with the
//! position the item's place in the list its put carried — so one put's items keep the order
//! they were submitted in, and an item carrying no `Id` is held beside one that does rather
//! than collapsing into it.
//!
//! The `UploadType` picks between the two readings of "overwrites": in `COMPLETE` mode, the
//! default, "the system overwrites all existing compliance information for the resource. You
//! must provide a full list of compliance items each time you send the request", while in
//! `PARTIAL` mode "the system overwrites compliance information for a specific association" —
//! the association an item names through its own `Id` ("Id: The patch, association, or custom
//! compliance ID", `PutComplianceItems`) — so a partial put replaces the items whose ids it
//! names and leaves the rest of that compliance type standing.
//!
//! The `ItemContentHash` is the put's own shortcut: "the content hash is used to determine if
//! existing information should be overwritten or ignored. If the content hashes match, the
//! request to put compliance information is ignored"
//! ([`ComplianceItemRecord::content_hash`]).
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

use std::collections::{BTreeSet, HashMap};

use ::service_common::page_tokens::PageTokenKey;

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

/// The compliance resource types this service supports: `ManagedInstance` and nothing else
/// (see [`RESOURCE_TYPE_MANAGED_INSTANCE`]).
///
/// The one place that set is written: the put, the read and the refusal below all ask it.
pub const SUPPORTED_RESOURCE_TYPES: &[&str] = &[RESOURCE_TYPE_MANAGED_INSTANCE];

/// Whether `resource_type` is one compliance may be reported about
/// ([`SUPPORTED_RESOURCE_TYPES`]).
#[must_use]
pub fn is_supported_resource_type(resource_type: &str) -> bool {
    SUPPORTED_RESOURCE_TYPES.contains(&resource_type)
}

/// How this service refuses a `ResourceType` compliance cannot be reported about, through the
/// modelled `InvalidResourceType` — whose own account of its class is "the resource type
/// isn't valid".
///
/// RECORDED BEHAVIOUR (corpus `simparity-behavioral-aws-v1`, unit
/// `ssm.compliance-items-invalid-resource-type-rejected`): fixed text apart from the rejected
/// type, which is echoed from the request, and the bracketed list of the types the SERVICE
/// supports ([`SUPPORTED_RESOURCE_TYPES`]).
#[must_use]
pub fn unsupported_resource_type_message(resource_type: &str) -> String {
    format!(
        "Resource type {resource_type} is not supported, example resource type are [{}]",
        SUPPORTED_RESOURCE_TYPES.join(", ")
    )
}

/// How many CUSTOM compliance types one resource may hold reports of: "You specified too many
/// custom compliance types. You can specify a maximum of 10 different types"
/// (`ComplianceTypeCountLimitExceededException`), the quota that error class publishes.
pub const MAX_CUSTOM_COMPLIANCE_TYPES: usize = 10;

/// The namespace a caller's own compliance type is named in: "specify `Association` (for a
/// State Manager association), `Patch`, or `Custom:string`"
/// (`PutComplianceItemsRequest$ComplianceType`), so a type outside it is one Systems Manager
/// itself reports under and no custom type at all.
pub const CUSTOM_COMPLIANCE_PREFIX: &str = "Custom:";

/// Whether `compliance_type` is one of the caller's OWN compliance types, which is what
/// [`MAX_CUSTOM_COMPLIANCE_TYPES`] counts.
#[must_use]
pub fn is_custom_compliance_type(compliance_type: &str) -> bool {
    compliance_type.starts_with(CUSTOM_COMPLIANCE_PREFIX)
}

/// What a put refused by [`MAX_CUSTOM_COMPLIANCE_TYPES`] reports: the quota that error class
/// publishes, with the figure it stands at.
#[must_use]
pub fn compliance_type_ceiling_message() -> String {
    format!(
        "You specified too many custom compliance types. You can specify a maximum of \
         {MAX_CUSTOM_COMPLIANCE_TYPES} different types."
    )
}

/// How many written compliance items this store holds at once, across every Region and
/// resource.
///
/// The EMULATOR's own ceiling rather than a quota Systems Manager publishes: a put is the
/// only thing that admits an item, callers may write as many as they like, and the store is
/// held in memory for as long as the process runs. A put that would admit one past it is
/// refused through the modelled `TotalSizeLimitExceededException` and writes nothing.
pub const MAX_COMPLIANCE_ITEMS: usize = 10_000;

/// What a put refused by [`MAX_COMPLIANCE_ITEMS`] reports: the emulator's own ceiling, named
/// as the emulator's, with the figure it stands at.
#[must_use]
pub fn item_ceiling_message() -> String {
    format!(
        "This emulator holds at most {MAX_COMPLIANCE_ITEMS} compliance items across every \
         Region"
    )
}

/// The `UploadType` that replaces every item the resource holds of the put's compliance
/// type: "In `COMPLETE` mode, the system overwrites all existing compliance information for
/// the resource", which is also the mode a put naming none uses ("By default, all requests
/// use `COMPLETE` mode").
pub const UPLOAD_TYPE_COMPLETE: &str = "COMPLETE";

/// The `UploadType` that replaces only the items the put names: "In `PARTIAL` mode, the
/// system overwrites compliance information for a specific association."
pub const UPLOAD_TYPE_PARTIAL: &str = "PARTIAL";

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

/// Which of the two places an item came from (see the module docs), which is part of what
/// orders the items of one resource and compliance type: the two sources are reported
/// together, so the order has to tell an item of one from an item of the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComplianceSource {
    /// Patch Manager gathered it, patch by patch, on the node
    /// ([`ComplianceItemRecord::of_patch`]).
    Gathered,
    /// A `PutComplianceItems` call wrote it.
    Written,
}

impl ComplianceSource {
    /// How this source is ordered against the other: the gathered items of one resource and
    /// compliance type before the written ones.
    const fn rank(self) -> &'static str {
        match self {
            Self::Gathered => "0",
            Self::Written => "1",
        }
    }
}

/// One compliance item of a managed node: either the projection of a patch row a patching
/// operation left ([`Self::of_patch`]) or an item a `PutComplianceItems` call wrote.
///
/// Every member a read reports is here, so a read projects this record rather than inventing
/// anything.
#[derive(Debug, Clone)]
pub struct ComplianceItemRecord {
    /// "The compliance type", which is [`COMPLIANCE_TYPE_PATCH`] for an item Patch Manager
    /// gathered and the type its put named for one a caller wrote.
    pub compliance_type: String,
    /// "The type of resource. `ManagedInstance` is currently the only supported resource
    /// type."
    pub resource_type: String,
    /// "An ID for the resource. For a managed node, this is the node ID."
    pub resource_id: String,
    /// "An ID for the compliance item. For example, if the compliance item is a Windows
    /// patch, the ID could be the number of the KB article" — the operating-system id of the
    /// patch for an item Patch Manager gathered, and `None` for a written item that carried
    /// none.
    pub item_id: Option<String>,
    /// "A title for the compliance item", which is the patch's own title for a gathered item,
    /// and `None` for a written one that carried none.
    pub title: Option<String>,
    /// "The status of the compliance item" ([`STATUS_COMPLIANT`], [`STATUS_NON_COMPLIANT`]).
    pub status: String,
    /// "The severity of the compliance status", which is the compliance level the baseline
    /// assigned the patch, and [`SEVERITY_UNSPECIFIED`] for a patch it assigned none.
    pub severity: String,
    /// "A summary for the compliance item. The summary includes an execution ID, the
    /// execution type ... and the execution time": when the patching operation that reported
    /// the item completed on the node, or the summary the put supplied as it was received.
    pub execution_summary: ComplianceExecutionSummary,
    /// "A 'Key': 'Value' tag combination for the compliance item", which for a patch is what
    /// the patch read reports about it beside its state, in ascending key order.
    ///
    /// A map is what the member IS, so an item carrying no pair holds the EMPTY map rather
    /// than no map at all — and a read reports the member for every item
    /// ([`Self::to_compliance_item`]).
    pub details: Vec<(String, String)>,
    /// Where this item sat in the list its put carried, which is the order a read walks the
    /// WRITTEN items of one resource and compliance type in. Zero for a gathered item, which
    /// its patch id orders instead ([`item_position`]).
    pub position: usize,
    /// Which of the two places this item came from, which the order of the items of one
    /// resource and compliance type reads ([`item_position`]).
    pub source: ComplianceSource,
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
            resource_type: RESOURCE_TYPE_MANAGED_INSTANCE.to_string(),
            resource_id: report.instance_id.clone(),
            // "The operating system-specific ID of the patch", which is the id a patch read
            // names the patch by.
            item_id: Some(row.kb_id.clone()),
            title: Some(row.title.clone()),
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
            execution_summary: ComplianceExecutionSummary {
                execution_time: ::aws_smithy_types::DateTime::from(report.operation_end_time),
                // "An ID created by the system when `PutComplianceItems` was called" — no
                // call wrote this item, so it carries no such id rather than one invented
                // here.
                execution_id: None,
                execution_type: Some(EXECUTION_TYPE_COMMAND.to_string()),
            },
            details,
            position: 0,
            source: ComplianceSource::Gathered,
        }
    }

    /// The value a filter on `key` is matched against, or `None` when `key` names no member
    /// of a compliance item ([`COMPLIANCE_FILTER_KEYS`]).
    ///
    /// A member this item holds nothing under compares as the empty value rather than as no
    /// value at all: the key is a member of an item either way, so a filter on it narrows the
    /// report to the items that carry what it asks for instead of being unserveable.
    ///
    /// The instant an execution ran is compared as the text the member is rendered as on the
    /// wire for the three comparisons that compare values, and as an instant for the two that
    /// ORDER them ([`Self::matches_filter`]).
    pub fn filter_value(&self, key: &str) -> Option<String> {
        match key {
            "ComplianceType" => Some(self.compliance_type.clone()),
            "ExecutionId" => Some(
                self.execution_summary
                    .execution_id
                    .clone()
                    .unwrap_or_default(),
            ),
            "ExecutionTime" => Some(rendered_instant(&self.execution_summary.execution_time)),
            "ExecutionType" => Some(
                self.execution_summary
                    .execution_type
                    .clone()
                    .unwrap_or_default(),
            ),
            "Id" => Some(self.item_id.clone().unwrap_or_default()),
            "ResourceId" => Some(self.resource_id.clone()),
            "ResourceType" => Some(self.resource_type.clone()),
            "Severity" => Some(self.severity.clone()),
            "Status" => Some(self.status.clone()),
            "Title" => Some(self.title.clone().unwrap_or_default()),
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
    ///
    /// NOT `super::operator_matches`, which answers the same question for a managed-node
    /// filter: that vocabulary is the node filters' own (`Equal`, `NotEqual`, `BeginWith`)
    /// and its ordering comparisons read a dotted agent version as numbers, while this one is
    /// the `ComplianceQueryOperatorType` the model declares for compliance over a list of
    /// values. The two readings are of two different filter languages and are deliberately
    /// apart.
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
            resource_type: Some(self.resource_type.clone()),
            resource_id: Some(self.resource_id.clone()),
            id: self.item_id.clone(),
            title: self.title.clone(),
            status: Some(self.status.clone()),
            severity: Some(self.severity.clone()),
            execution_summary: Some(self.execution_summary.clone()),
            // "A 'Key': 'Value' tag combination for the compliance item": the member is a
            // map, so an item that holds no pair is reported as the EMPTY map and the member
            // is reported for every item — a read never leaves it out and so never makes a
            // caller tell an item with no details from one whose details it cannot see.
            details: Some(
                self.details
                    .iter()
                    .cloned()
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
fn rendered_instant(at: &::aws_smithy_types::DateTime) -> String {
    at.fmt(::aws_smithy_types::date_time::Format::DateTime)
        .unwrap_or_default()
}

/// Which read a compliance continuation token was minted for, so a token this read handed out
/// resumes nothing else.
const CURSOR_SCOPE: &str = "ListComplianceItems";

/// How wide the position's own ordinal is rendered, so the cursor's lexicographic order is
/// the walk's: the item count this store can hold ([`MAX_COMPLIANCE_ITEMS`]) never reaches
/// eight digits.
const CURSOR_ORDINAL_DIGITS: usize = 8;

/// The position in the walk `record` sits at — the total order a read walks the items of one
/// Region in, rendered so that comparing two of them as text orders them the same way.
///
/// The order is taken from the items themselves, which is what lets a page resume at the item
/// the last one stopped at rather than at an offset. It is INJECTIVE over the two sources a
/// read reports together: the resource and the compliance type an item belongs to, then the
/// source it came from ([`ComplianceSource::rank`]), and then the one thing that tells two
/// items of that source apart — the patch's own id for a gathered item, which is unique among
/// the patches one report holds, and the item's place in the list its put carried for a
/// written one, which is why a put's items are reported in the order they were submitted
/// whatever ids they carry.
#[must_use]
pub fn item_position(record: &ComplianceItemRecord) -> String {
    let separator = super::parameter_pagination::CURSOR_SEPARATOR;
    let within_source = match record.source {
        ComplianceSource::Gathered => record.item_id.clone().unwrap_or_default(),
        ComplianceSource::Written => {
            format!("{:0width$}", record.position, width = CURSOR_ORDINAL_DIGITS)
        }
    };
    format!(
        "{}{separator}{}{separator}{}{separator}{within_source}",
        record.resource_id,
        record.compliance_type,
        record.source.rank(),
    )
}

/// The `NextToken` that resumes a compliance read AFTER the item at `position`.
///
/// Minted through this crate's ONE mint of an authenticated cursor
/// ([`super::parameter_pagination::scoped_page_token`]), under this store's own key, so a
/// position a caller assembled is refused rather than served a page this read never offered.
#[must_use]
pub fn page_token(key: &PageTokenKey, position: &str) -> String {
    super::parameter_pagination::scoped_page_token(key, CURSOR_SCOPE, position)
}

/// The position a `NextToken` resumes a compliance read at, or `None` for any value THIS
/// store did not mint for THIS read ([`super::parameter_pagination::scoped_page_cursor`]).
#[must_use]
pub fn page_cursor(key: &PageTokenKey, token: &str) -> Option<String> {
    super::parameter_pagination::scoped_page_cursor(key, CURSOR_SCOPE, token)
}

/// The key one WRITTEN item is held under: its Region, the resource it is about, the
/// compliance type it was reported under, and its place in the list its put carried.
pub type ComplianceItemKey = (String, String, String, String, usize);

/// The key the content hash of the last put is held under: its Region, the resource it was
/// about and the compliance type it reported — the pair a put addresses, which is what the
/// hash is about ([`ComAmazonawsSsmAmazonssmState::compliance_content_hash`]).
///
/// A hash is held only for a pair that HOLDS items, so the hashes are a subset of the item
/// store and the one ceiling on that store ([`MAX_COMPLIANCE_ITEMS`]) bounds both.
pub type ComplianceContentHashKey = (String, String, String, String);

/// What one put of compliance addresses: the resource it is about, the compliance type it
/// reports, the mode it was sent in and the content hash it carried — everything a write needs
/// beside the items themselves ([`ComAmazonawsSsmAmazonssmState::put_compliance_items`]).
#[derive(Debug, Clone, Copy)]
pub struct CompliancePut<'a> {
    /// The Region whose endpoint the put was sent to.
    pub region: &'a str,
    /// "The type of resource", which is the one supported type
    /// ([`SUPPORTED_RESOURCE_TYPES`]).
    pub resource_type: &'a str,
    /// "An ID for this resource. For a managed node, this is the node ID."
    pub resource_id: &'a str,
    /// "The compliance type" the put reports, which is the one it overwrites.
    pub compliance_type: &'a str,
    /// Whether the put was sent in `PARTIAL` mode ([`UPLOAD_TYPE_PARTIAL`]) rather than the
    /// default `COMPLETE` one.
    pub partial: bool,
    /// "MD5 or SHA-256 content hash" the put carried, or `None` for one that carried none.
    pub content_hash: Option<&'a str>,
}

impl ComAmazonawsSsmAmazonssmState {
    /// The compliance items `region` holds for the nodes `resource_ids` names — every node of
    /// the Region when it names none — in the total order of [`item_position`].
    ///
    /// A READ: the gathered items are the projection of the patch rows the patching operations
    /// of those nodes left ([`ComplianceItemRecord::of_patch`]) and the written ones are what
    /// the puts persisted, so nothing is written here and two callers see the same items.
    pub fn compliance_items(
        &self,
        region: &str,
        resource_ids: &[String],
    ) -> Vec<ComplianceItemRecord> {
        let named = |resource_id: &String| {
            resource_ids.is_empty() || resource_ids.iter().any(|asked| asked == resource_id)
        };
        let mut items: Vec<ComplianceItemRecord> = self
            .instance_patch_states
            .iter()
            .filter(|((held_region, _), _)| held_region == region)
            .filter(|((_, node), _)| named(node))
            .flat_map(|(_, report)| {
                report
                    .patches
                    .iter()
                    .map(|row| ComplianceItemRecord::of_patch(report, row))
            })
            .collect();
        items.extend(
            self.written_compliance_items
                .iter()
                .filter(|((held_region, _, _, _, _), _)| held_region == region)
                .map(|(_, record)| record)
                .filter(|record| named(&record.resource_id))
                .cloned(),
        );
        items.sort_by_key(item_position);
        items
    }

    /// The compliance items `region` holds about one resource under `compliance_type` because
    /// a put wrote them, in the order that put carried them.
    pub fn written_compliance_items_of<'a>(
        &'a self,
        region: &'a str,
        resource_type: &'a str,
        resource_id: &'a str,
        compliance_type: &'a str,
    ) -> impl Iterator<Item = &'a ComplianceItemRecord> + 'a {
        self.written_compliance_items
            .iter()
            .filter(move |((held_region, held_type, held_id, held_compliance, _), _)| {
                held_region == region
                    && held_type == resource_type
                    && held_id == resource_id
                    && held_compliance == compliance_type
            })
            .map(|(_, record)| record)
    }

    /// The CUSTOM compliance types one resource of `region` holds written reports of
    /// ([`is_custom_compliance_type`]) — the set [`MAX_CUSTOM_COMPLIANCE_TYPES`] bounds.
    pub fn custom_compliance_types_of(
        &self,
        region: &str,
        resource_type: &str,
        resource_id: &str,
    ) -> BTreeSet<&str> {
        self.written_compliance_items
            .keys()
            .filter(|(held_region, held_type, held_id, _, _)| {
                held_region == region && held_type == resource_type && held_id == resource_id
            })
            .map(|(_, _, _, compliance_type, _)| compliance_type.as_str())
            .filter(|compliance_type| is_custom_compliance_type(compliance_type))
            .collect()
    }

    /// The content hash the LAST put of `compliance_type` about one resource carried, or
    /// `None` where that put carried none or no put has been served: "the content hash is used
    /// to determine if existing information should be overwritten or ignored. If the content
    /// hashes match, the request to put compliance information is ignored"
    /// (`PutComplianceItemsRequest$ItemContentHash`).
    ///
    /// Read from what the put recorded rather than inferred from the items it left, so a
    /// PARTIAL put — whose kept items are the older ones and come first — is compared against
    /// its own hash and not the previous put's.
    pub fn compliance_content_hash(
        &self,
        region: &str,
        resource_type: &str,
        resource_id: &str,
        compliance_type: &str,
    ) -> Option<String> {
        self.compliance_content_hashes
            .get(&(
                region.to_string(),
                resource_type.to_string(),
                resource_id.to_string(),
                compliance_type.to_string(),
            ))
            .cloned()
    }

    /// Write `items` as the compliance `region` holds about one resource under
    /// `compliance_type`, the way the put's `UploadType` says: a COMPLETE put replaces every
    /// item held of that compliance type, a PARTIAL one replaces only the items whose `Id` it
    /// names and leaves the rest standing (see the module docs).
    ///
    /// The items are re-positioned from the start, so the order a read walks them in is the
    /// order they are held in here: the items a partial put kept, then the ones it carried.
    /// The put's own content hash is recorded as the hash of the `(resource, compliance type)`
    /// pair — a put that carried none leaves the pair with none.
    pub fn put_compliance_items(
        &mut self,
        put: &CompliancePut<'_>,
        items: Vec<ComplianceItemRecord>,
    ) {
        let CompliancePut {
            region,
            resource_type,
            resource_id,
            compliance_type,
            partial,
            content_hash,
        } = *put;
        let mut kept: Vec<ComplianceItemRecord> = Vec::new();
        if partial {
            let replaced: BTreeSet<&str> = items
                .iter()
                .filter_map(|item| item.item_id.as_deref())
                .collect();
            kept = self
                .written_compliance_items_of(region, resource_type, resource_id, compliance_type)
                .filter(|held| {
                    held.item_id
                        .as_deref()
                        .is_none_or(|id| !replaced.contains(id))
                })
                .cloned()
                .collect();
        }
        self.remove_compliance_items_of(region, resource_type, resource_id, compliance_type);
        let mut held = 0_usize;
        for (position, mut record) in kept.into_iter().chain(items).enumerate() {
            record.position = position;
            let key: ComplianceItemKey = (
                region.to_string(),
                resource_type.to_string(),
                resource_id.to_string(),
                compliance_type.to_string(),
                position,
            );
            self.written_compliance_items.insert(key, record);
            held = position + 1;
        }
        let hash_key: ComplianceContentHashKey = (
            region.to_string(),
            resource_type.to_string(),
            resource_id.to_string(),
            compliance_type.to_string(),
        );
        // A hash is held only for a pair that HOLDS items, so this store is a subset of the
        // item store and is bounded by the same ceiling: a put that leaves the pair empty —
        // one carrying no item at all — takes its hash with it, and so does a put that
        // carried no hash.
        match content_hash.filter(|_| held > 0) {
            Some(hash) => {
                self.compliance_content_hashes
                    .insert(hash_key, hash.to_string());
            }
            None => {
                self.compliance_content_hashes.remove(&hash_key);
            }
        }
    }

    /// Drop every written compliance item `region` holds about one resource under
    /// `compliance_type`, answering how many went.
    pub fn remove_compliance_items_of(
        &mut self,
        region: &str,
        resource_type: &str,
        resource_id: &str,
        compliance_type: &str,
    ) -> usize {
        let before = self.written_compliance_items.len();
        self.written_compliance_items.retain(
            |(held_region, held_type, held_id, held_compliance, _), _| {
                held_region != region
                    || held_type != resource_type
                    || held_id != resource_id
                    || held_compliance != compliance_type
            },
        );
        before - self.written_compliance_items.len()
    }

    /// How many written compliance items this store would hold once a put of `incoming` items
    /// about one resource under `compliance_type` landed — what [`MAX_COMPLIANCE_ITEMS`] is
    /// asked about BEFORE anything is written, so a put past the ceiling writes nothing.
    ///
    /// A COMPLETE put drops every item held of that compliance type, so those do not count
    /// against the ceiling; a PARTIAL one drops only the items it names, which this counts as
    /// none — the conservative reading, since the ceiling is the emulator's own.
    pub fn compliance_items_after_put(
        &self,
        region: &str,
        resource_type: &str,
        resource_id: &str,
        compliance_type: &str,
        incoming: usize,
        partial: bool,
    ) -> usize {
        let dropped = if partial {
            0
        } else {
            self.written_compliance_items_of(region, resource_type, resource_id, compliance_type)
                .count()
        };
        self.written_compliance_items.len() - dropped + incoming
    }
}
