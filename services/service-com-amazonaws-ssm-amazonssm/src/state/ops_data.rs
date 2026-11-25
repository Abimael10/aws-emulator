// SPDX-License-Identifier: CC0-1.0

//! The operations metadata (OpsData) this service holds, and how a summary of it is
//! filtered, aggregated and projected.
//!
//! "OpsData can include information about Amazon Web Services Systems Manager OpsCenter
//! operational workitems (OpsItems) as well as information about any Amazon Web Services
//! resource or service configured to report OpsData to Amazon Web Services Systems Manager
//! Explorer" (`GetOpsSummary`), so OpsData is not a store of its own: it is a PROJECTION of
//! the resources this service already holds, under the data type each of them reports as.
//! Nothing here writes anything and nothing is invented at read time — an account that has
//! opened no OpsItem and registered no node holds no OpsData.
//!
//! The data types are the ones the model publishes: "Valid value: `AWS:OpsItem`,
//! `AWS:EC2InstanceInformation`, `AWS:OpsItemTrendline`, or `AWS:ComplianceSummary`"
//! (`OpsResultAttribute$TypeName`) — [`OPS_DATA_TYPES`]. Two of them are reported from what
//! this service persists:
//!
//! * `AWS:OpsItem` — one item per OpsItem OpsCenter holds, its attributes the members the
//!   model's own summary of an OpsItem carries ([`OPS_ITEM_ATTRIBUTES`], projected by
//!   [`ops_item_attribute`]);
//! * `AWS:EC2InstanceInformation` — one item per managed node the Region holds, its
//!   attributes the instance information Inventory already reports for that node
//!   ([`super::inventory::INSTANCE_INFORMATION_ATTRIBUTES`]).
//!
//! The other two are data types nothing in this emulator has reported OpsData for, so a
//! summary of one reports NOTHING rather than refusing: they are published types that have
//! collected nothing. A type name outside the published list is refused, which is what
//! `InvalidTypeNameException` ("The parameter type name isn't valid") reports.
//!
//! A filter key is `<TypeName>.<AttributeName>` over one of those types
//! ([`filter_key_rejection`]), the same dotted form Inventory's filters carry
//! ([`super::inventory::split_qualified_attribute`]) and refused with the same wording when
//! it is not that form ([`super::inventory::invalid_filter_key_message`]), and the operators
//! are the modelled
//! `OpsFilterOperatorType` values — `Exists` asking only whether the attribute is reported
//! at all and every other one the comparison this service filters by everywhere
//! ([`super::operator_matches`]).

use std::collections::BTreeMap;
use std::time::SystemTime;

use super::inventory::{
    instance_information_attribute, split_qualified_attribute, INSTANCE_INFORMATION_ATTRIBUTES,
};
use super::ops_items::OpsItemRecord;
use super::{ComAmazonawsSsmAmazonssmState, ManagedNodeRecord};
use crate::ops::types::OpsFilter;

/// The OpsData type whose items are the OpsItems OpsCenter holds, and the data type a
/// summary that names none reports: `GetOpsSummary` is the read of "a summary of operations
/// metadata (OpsData)" whose first named source is "OpsCenter operational workitems
/// (OpsItems)", and this is the first of the published values of
/// `OpsResultAttribute$TypeName`.
pub const OPS_DATA_TYPE_OPS_ITEM: &str = "AWS:OpsItem";

/// The OpsData type whose items are the managed nodes of a Region, as published by the same
/// doc.
pub const OPS_DATA_TYPE_EC2_INSTANCE_INFORMATION: &str = "AWS:EC2InstanceInformation";

/// The OpsData type carrying OpsCenter's trendline, as published by the same doc. Nothing in
/// this emulator computes a trendline, so no item of it is reported.
pub const OPS_DATA_TYPE_OPS_ITEM_TRENDLINE: &str = "AWS:OpsItemTrendline";

/// The OpsData type carrying the compliance summary, as published by the same doc. Nothing
/// in this emulator reports compliance OpsData, so no item of it is reported.
pub const OPS_DATA_TYPE_COMPLIANCE_SUMMARY: &str = "AWS:ComplianceSummary";

/// Every OpsData type name a request may name, as `OpsResultAttribute$TypeName` publishes
/// them: "Valid value: `AWS:OpsItem`, `AWS:EC2InstanceInformation`, `AWS:OpsItemTrendline`,
/// or `AWS:ComplianceSummary`"
/// (`smithy_query(query="doc",shape="OpsResultAttribute",member="TypeName")`).
pub const OPS_DATA_TYPES: &[&str] = &[
    OPS_DATA_TYPE_OPS_ITEM,
    OPS_DATA_TYPE_EC2_INSTANCE_INFORMATION,
    OPS_DATA_TYPE_OPS_ITEM_TRENDLINE,
    OPS_DATA_TYPE_COMPLIANCE_SUMMARY,
];

/// The attributes an `AWS:OpsItem` item carries: the members the model's summary of an
/// OpsItem carries (`smithy_query(query="shape",shape="OpsItemSummary")`), which is what
/// Explorer reports OpsData about an OpsItem as. Listed in the model's own member order.
pub const OPS_ITEM_ATTRIBUTES: &[&str] = &[
    "CreatedBy",
    "CreatedTime",
    "LastModifiedBy",
    "LastModifiedTime",
    "Priority",
    "Source",
    "Status",
    "OpsItemId",
    "Title",
    "OperationalData",
    "Category",
    "Severity",
    "OpsItemType",
    "ActualStartTime",
    "ActualEndTime",
    "PlannedStartTime",
    "PlannedEndTime",
];

/// The aggregation a summary of OpsData computes: a COUNT of the OpsData per distinct value
/// of the attribute an aggregator names — "one or more aggregators for viewing counts of
/// OpsData using different dimensions" (`OpsAggregator`), "the name of an OpsData attribute
/// on which to limit the count of OpsData" (`$AttributeName`). It is one of the spellings the
/// modelled `OpsAggregatorType` pattern `^(range|count|sum)$` admits, and the only
/// aggregation this service computes: an aggregator naming another one asks for an
/// aggregation that is not supported here, which is what `InvalidAggregatorException`
/// reports.
pub const AGGREGATOR_TYPE_COUNT: &str = "count";

/// The modelled `OpsFilterOperatorType` value that asks only whether the attribute is
/// reported, rather than comparing its value (the one value the comparison
/// [`super::operator_matches`] makes has no relation for).
pub const OPERATOR_EXISTS: &str = "Exists";

/// The most characters an OpsData attribute value may carry (`OpsEntityItemEntry`'s value
/// targets `AttributeValue`, `len 0..4096`), so an attribute whose rendering is longer than
/// this is one the item cannot carry and is left out of its content rather than truncated.
pub const MAX_ATTRIBUTE_VALUE: usize = 4096;

/// How many counted entries one aggregated summary may report, and so how many a request
/// may ask it to count: the most an item's content may carry (`OpsEntityItem$Content`
/// targets `OpsEntityItemEntryList`, `len 0..10000`), since a count is reported as an entry
/// of that content.
pub const MAX_AGGREGATED_ENTRIES: usize = 10_000;

/// The lower bound the modelled `MaxResults` of a summary carries
/// (`GetOpsSummaryRequest$MaxResults`, range 1..50).
pub const MIN_OPS_SUMMARY_PAGE_SIZE: i32 = 1;

/// The upper bound of the same member, and the page size a request naming no `MaxResults`
/// is served up to (see [`super::page_size`]).
pub const MAX_OPS_SUMMARY_PAGE_SIZE: i32 = 50;

/// How this service states a type name that names no OpsData type, which is what
/// `InvalidTypeNameException`'s own documentation words: "The parameter type name isn't
/// valid."
pub const INVALID_TYPE_NAME: &str = "The parameter type name isn't valid.";

/// The page size a summary serves, or the refusal message for a `MaxResults` outside the
/// modelled range — which the protocol's boundary validator already refuses, so this is a
/// backstop rather than the first line of defence.
pub fn ops_summary_page_size(max_results: Option<i32>) -> Result<usize, String> {
    super::page_size(
        max_results,
        MIN_OPS_SUMMARY_PAGE_SIZE,
        MAX_OPS_SUMMARY_PAGE_SIZE,
    )
}

/// Whether `type_name` names an OpsData type this service publishes ([`OPS_DATA_TYPES`]).
/// The comparison is exact: a data type is a name rather than a label, so neither another
/// spelling nor another case names one.
pub fn is_ops_data_type(type_name: &str) -> bool {
    OPS_DATA_TYPES.contains(&type_name)
}

/// The Amazon Web Services Regions whose OpsData a resource data sync aggregates: "the
/// `SyncSource` Amazon Web Services Regions included in the resource data sync"
/// (`ResourceDataSyncSource$SourceRegions`), so a summary read THROUGH a sync reports the
/// OpsData of the Regions its source names rather than of the one endpoint the request
/// reached.
pub fn sync_source_regions(record: &super::resource_data_syncs::ResourceDataSyncRecord) -> Vec<String> {
    record
        .sync_source
        .as_ref()
        .map(|source| source.source_regions.clone())
        .unwrap_or_default()
}

/// The attributes an item of `type_name` carries, or `None` for a published type this
/// service holds no OpsData of — which has no attributes because it has reported nothing.
pub fn ops_data_attributes(type_name: &str) -> Option<&'static [&'static str]> {
    match type_name {
        OPS_DATA_TYPE_OPS_ITEM => Some(OPS_ITEM_ATTRIBUTES),
        OPS_DATA_TYPE_EC2_INSTANCE_INFORMATION => Some(INSTANCE_INFORMATION_ATTRIBUTES),
        _ => None,
    }
}

/// Whether `attribute` is one of the attributes an item of `type_name` carries. A type this
/// service holds no OpsData of publishes attributes this emulator does not know, so any
/// attribute name is one it may report and none is refused for it.
pub fn names_attribute(type_name: &str, attribute: &str) -> bool {
    ops_data_attributes(type_name).is_none_or(|attributes| attributes.contains(&attribute))
}

/// Why an OpsData filter key cannot be served, or `None` when it can.
///
/// A key names an attribute of an OpsData type: the dotted `<TypeName>.<AttributeName>`
/// form, over a type this service publishes, naming an attribute of that type. A key naming
/// a published type whose OpsData this service holds none of is served — against the nothing
/// that type has reported — rather than refused.
///
/// A key that is not that FORM names no attribute of any type, so the refusal states the
/// rule and the key the request carried, through the one wording this service has for a key
/// of the wrong form ([`super::inventory::invalid_filter_key_message`], which the sibling
/// Inventory query's filter keys are refused with too). A key that IS the form but names a
/// type or an attribute no published type carries is refused with this error class's own
/// sentence.
pub fn filter_key_rejection(key: &str) -> Option<String> {
    let Some((type_name, attribute)) = split_qualified_attribute(key) else {
        return Some(super::inventory::invalid_filter_key_message(key));
    };
    if !is_ops_data_type(type_name) || !names_attribute(type_name, attribute) {
        return Some(super::inventory::INVALID_FILTER.to_string());
    }
    None
}

/// An instant as an OpsData attribute value and as an item's capture time spell it: the
/// modelled `OpsEntityItemCaptureTime` pattern
/// (`^(20)[0-9][0-9]-(0[1-9]|1[012])-([12][0-9]|3[01]|0[1-9])(T)…(Z)$`) is whole seconds, so
/// the instant is truncated to the second rather than carrying the fraction a stored
/// timestamp may hold.
pub fn rendered_instant(at: SystemTime) -> Option<String> {
    let at = ::aws_smithy_types::DateTime::from(at);
    ::aws_smithy_types::DateTime::from_secs(at.secs())
        .fmt(::aws_smithy_types::date_time::Format::DateTime)
        .ok()
}

/// The same, for a timestamp the record may hold nothing for.
fn optional_instant(at: Option<::aws_smithy_types::DateTime>) -> Option<String> {
    at.and_then(|at| SystemTime::try_from(at).ok())
        .and_then(rendered_instant)
}

/// The custom data of an OpsItem as ONE attribute value: the key-value data the OpsItem
/// carries, each value beside the type it was written with, as a JSON document — which is
/// how a map is carried by a content entry whose values are plain strings
/// (`OpsEntityItemEntry`).
fn rendered_operational_data(record: &OpsItemRecord) -> Option<String> {
    if record.operational_data.is_empty() {
        return None;
    }
    let document: ::serde_json::Map<String, ::serde_json::Value> = record
        .operational_data
        .iter()
        .map(|(key, data)| {
            let mut entry = ::serde_json::Map::new();
            if let Some(value) = data.value.as_deref() {
                entry.insert("Value".to_string(), ::serde_json::Value::from(value));
            }
            if let Some(kind) = data.r#type.as_deref() {
                entry.insert("Type".to_string(), ::serde_json::Value::from(kind));
            }
            (key.clone(), ::serde_json::Value::Object(entry))
        })
        .collect();
    Some(::serde_json::Value::Object(document).to_string())
}

/// The value `record` reports for one attribute of its `AWS:OpsItem` item, or `None` when it
/// reports none — either because the attribute is not one of that type's
/// ([`OPS_ITEM_ATTRIBUTES`]) or because the OpsItem holds no value for it.
///
/// Every value is projected from what the OpsItem's own writes persisted, so OpsData about
/// an OpsItem and the OpsItem a read reports can never disagree.
pub fn ops_item_attribute(record: &OpsItemRecord, attribute: &str) -> Option<String> {
    match attribute {
        "CreatedBy" => Some(record.created_by.clone()),
        "CreatedTime" => rendered_instant(record.created_time),
        "LastModifiedBy" => Some(record.last_modified_by.clone()),
        "LastModifiedTime" => rendered_instant(record.last_modified_time),
        "Priority" => record.priority.map(|priority| priority.to_string()),
        "Source" => Some(record.source.clone()),
        "Status" => Some(record.status.clone()),
        "OpsItemId" => Some(record.ops_item_id.clone()),
        "Title" => Some(record.title.clone()),
        "OperationalData" => rendered_operational_data(record),
        "Category" => record.category.clone(),
        "Severity" => record.severity.clone(),
        "OpsItemType" => Some(record.ops_item_type.clone()),
        "ActualStartTime" => optional_instant(record.actual_start_time),
        "ActualEndTime" => optional_instant(record.actual_end_time),
        "PlannedStartTime" => optional_instant(record.planned_start_time),
        "PlannedEndTime" => optional_instant(record.planned_end_time),
        _ => None,
    }
    .filter(|value| value.len() <= MAX_ATTRIBUTE_VALUE)
}

/// One item of OpsData: the resource it is about, and what it reports about it.
///
/// Held as the id, the capture instant and the content the summary serves, so the store is
/// read once and released before anything is filtered, counted or paged — the same way the
/// Inventory read snapshots the nodes it is about to report.
#[derive(Debug, Clone)]
pub struct OpsDataItem {
    /// The OpsData type this item is of, one of [`OPS_DATA_TYPES`].
    pub type_name: String,
    /// The resource this OpsData is about: the OpsItem's id, or the node's id.
    pub id: String,
    /// When this OpsData was captured, as [`rendered_instant`] spells it.
    pub capture_time: Option<String>,
    /// Every attribute of the type this resource holds a value for, and no entry for one it
    /// does not.
    pub content: BTreeMap<String, String>,
}

impl OpsDataItem {
    /// The value this item reports for `attribute`, or `None` when it reports none.
    pub fn attribute(&self, attribute: &str) -> Option<&str> {
        self.content.get(attribute).map(String::as_str)
    }

    /// Whether this item satisfies ONE filter: the filter's key names an attribute of THIS
    /// item's type, and the item's value for it stands in the filter's relation to one of
    /// the filter's values.
    ///
    /// A filter over another OpsData type is about other OpsData, so this item has nothing
    /// to satisfy it with — as has an item that holds no value for the attribute, whatever
    /// the operator. `Exists` asks only whether the attribute is reported at all; every
    /// other operator is the comparison this service filters by everywhere
    /// ([`super::operator_matches`]), and the modelled operator defaults to `Equal`, the
    /// relation a key and a value state on their own.
    pub fn satisfies(&self, filter: &OpsFilter) -> bool {
        let Some((type_name, attribute)) = split_qualified_attribute(&filter.key) else {
            return false;
        };
        if type_name != self.type_name {
            return false;
        }
        let Some(held) = self.attribute(attribute) else {
            return false;
        };
        let operator = filter.r#type.as_deref().unwrap_or(super::OPERATOR_EQUAL);
        if operator == OPERATOR_EXISTS {
            return true;
        }
        filter
            .values
            .iter()
            .any(|wanted| super::operator_matches(operator, held, wanted))
    }

    /// Whether this item satisfies EVERY one of `filters`, which is what a summary selects
    /// by: the filters intersect and each filter's values union.
    pub fn satisfies_all(&self, filters: &[OpsFilter]) -> bool {
        filters.iter().all(|filter| self.satisfies(filter))
    }
}

/// The `AWS:OpsItem` OpsData one OpsItem reports: every attribute of that type the record
/// holds a value for, captured when the OpsItem was last written — OpsData about a resource
/// was captured when that resource last changed, so two reads of an unchanged OpsItem report
/// the same instant.
pub fn ops_item_item(record: &OpsItemRecord) -> OpsDataItem {
    let content = OPS_ITEM_ATTRIBUTES
        .iter()
        .filter_map(|attribute| {
            ops_item_attribute(record, attribute).map(|value| ((*attribute).to_string(), value))
        })
        .collect();
    OpsDataItem {
        type_name: OPS_DATA_TYPE_OPS_ITEM.to_string(),
        id: record.ops_item_id.clone(),
        capture_time: rendered_instant(record.last_modified_time),
        content,
    }
}

/// The `AWS:EC2InstanceInformation` OpsData one managed node reports: the instance
/// information Inventory already reports for that node
/// ([`super::inventory::instance_information_content`]), under the node's own id, so the two
/// reads of one node never disagree.
pub fn instance_information_item(id: &str, node: &ManagedNodeRecord) -> OpsDataItem {
    let content = INSTANCE_INFORMATION_ATTRIBUTES
        .iter()
        .filter_map(|attribute| {
            instance_information_attribute(node, attribute)
                .map(|value| ((*attribute).to_string(), value.to_string()))
        })
        .collect();
    OpsDataItem {
        type_name: OPS_DATA_TYPE_EC2_INSTANCE_INFORMATION.to_string(),
        id: id.to_string(),
        capture_time: super::inventory::capture_time(node),
        content,
    }
}

impl ComAmazonawsSsmAmazonssmState {
    /// The OpsData of type `type_name` the Regions `regions` hold, in ascending id order.
    ///
    /// The order is what makes a summary's resume cursor total: it is the id of the resource
    /// the next page starts at, not an offset into a list that may have changed. A published
    /// type this service holds no OpsData of yields nothing, which is what has been reported
    /// for it.
    pub fn ops_data(&self, regions: &[String], type_name: &str) -> Vec<OpsDataItem> {
        let mut items: Vec<OpsDataItem> = match type_name {
            OPS_DATA_TYPE_OPS_ITEM => self
                .ops_items
                .values()
                .filter(|record| regions.iter().any(|region| region == &record.region))
                .map(ops_item_item)
                .collect(),
            OPS_DATA_TYPE_EC2_INSTANCE_INFORMATION => regions
                .iter()
                .flat_map(|region| self.managed_nodes_in_region(region))
                .map(|(id, node)| instance_information_item(id, node))
                .collect(),
            _ => Vec::new(),
        };
        items.sort_by(|left, right| left.id.cmp(&right.id));
        items
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A filter key is refused unless it names an attribute of a published OpsData type.
    #[test]
    fn a_filter_key_names_an_attribute_of_a_published_type() {
        for (key, served) in [
            ("AWS:OpsItem.Status", true),
            ("AWS:OpsItem.Title", true),
            ("AWS:EC2InstanceInformation.PlatformType", true),
            // A published type this service holds no OpsData of publishes attributes this
            // emulator does not know, so any attribute name is served against the nothing
            // it has reported.
            ("AWS:ComplianceSummary.Status", true),
            ("AWS:OpsItemTrendline.Anything", true),
            ("AWS:OpsItem.NotAnAttribute", false),
            ("AWS:NotAType.Status", false),
            ("Custom:Mine.Status", false),
            ("AWS:OpsItem", false),
            ("Status", false),
        ] {
            assert_eq!(
                filter_key_rejection(key).is_none(),
                served,
                "key {key:?}: expected served={served}, got {:?}",
                filter_key_rejection(key)
            );
        }
    }
}
