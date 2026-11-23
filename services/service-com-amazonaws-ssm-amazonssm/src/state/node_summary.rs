// SPDX-License-Identifier: CC0-1.0

//! The node SUMMARY of the Systems Manager integrated experience: the counts a summary
//! groups the managed nodes of [`super::node_listing`] into.
//!
//! `ListNodesSummary` "generates a summary of managed instance/node metadata based on the
//! filters and aggregators you specify. Results are grouped by the input aggregator you
//! specify", so a summary is the SAME node selection a node listing makes
//! ([`super::node_listing::matches_filter`]) counted by one attribute of the nodes instead of
//! reported node by node. Nothing here holds a second copy of anything: the groups are read
//! off the node store the listing and the node describes read, so a count can never disagree
//! with the nodes it counts.
//!
//! ## What a row says
//!
//! One row is one GROUP: the attribute value the nodes of it share, and how many of them
//! there are under [`COUNT_KEY`]. A nested aggregator ("information about aggregators used to
//! refine a node summary", `NodeAggregator$Aggregators`) groups each group again, so a row
//! then carries both attribute values beside the count — which is why the modelled
//! `NodeSummary` map holds up to three entries (`len 0..3`), and why an aggregation nested
//! deeper than [`MAX_AGGREGATOR_DEPTH`] levels has no row that could report it
//! ([`aggregator_rejection`]).
//!
//! A node that holds NO value for the attribute being aggregated is in no group of it: a
//! count is of nodes that carry the value it names, and nothing invents a value — a hybrid
//! node that reports no zone of its own is reported under no zone rather than under an empty
//! one. The groups are therefore a partition of the nodes that carry the attribute, and the
//! counts of one aggregator sum to at most the number of nodes the filters selected.
//!
//! ## The order a page resumes in
//!
//! The rows of one aggregator are in ascending order of the value they group by, and the
//! aggregators are walked in the order the request named them, so the sequence is total and a
//! page resumes AT A ROW ([`SummaryRow::position`]) rather than at an offset into a list that
//! may have changed between two calls.

use std::collections::BTreeMap;
use std::collections::HashMap;

use crate::ops::types::{
    NodeAggregator, NodeFilter, NODE_AGGREGATOR_TYPE_VALUES, NODE_ATTRIBUTE_NAME_VALUES,
    NODE_TYPE_NAME_VALUES,
};

use super::{ComAmazonawsSsmAmazonssmState, ManagedNodeRecord};

/// The key a summary row carries how many nodes fell in its group under.
///
/// `NodeSummary` is a map of "attribute name" to "attribute value" and a count aggregator
/// reports the count beside the attributes it grouped by, so the count travels as an entry of
/// the row like the attributes do. It is THE key this service reports an aggregated count
/// under ([`super::inventory::AGGREGATION_COUNT_KEY`], what the aggregated inventory reads
/// report theirs under), stated once for both.
pub const COUNT_KEY: &str = super::inventory::AGGREGATION_COUNT_KEY;

/// How deep an aggregation this summary can report: one aggregator and one nested under it.
///
/// Read off the modelled row rather than chosen: a `NodeSummary` holds at most three entries
/// (`len 0..3`) and one of them is [`COUNT_KEY`], so a row can carry at most two attribute
/// values — an aggregation nested deeper has no row that could report it.
pub const MAX_AGGREGATOR_DEPTH: usize = 2;

/// Which read a summary's continuation token was minted for
/// ([`super::parameter_pagination::scoped_page_token`]): the operation's own name, so a token
/// another paginated read of this service handed out resumes nothing here.
const CURSOR_SCOPE: &str = "ListNodesSummary";

/// One row of a summary: the attribute values the group it counts shares, outermost
/// aggregator first, and how many nodes are in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryRow {
    /// Which aggregator of the request the row belongs to, in the order the request named
    /// them — part of the row's position, because two aggregators group the same nodes by
    /// different attributes and so report rows of their own.
    pub aggregator: usize,
    /// The `(attribute name, attribute value)` pairs the group shares, outermost first.
    pub grouped_by: Vec<(String, String)>,
    /// How many of the selected nodes carry those values.
    pub count: usize,
}

impl SummaryRow {
    /// The row's position in the summary's total order — the cursor a page that stops here
    /// resumes after.
    ///
    /// The aggregator's ordinal leads, then the values the row groups by in the order they
    /// were grouped, so comparing two positions as TEXT orders them exactly as the walk
    /// reports them ([`super::parameter_pagination::already_served`]).
    #[must_use]
    pub fn position(&self) -> String {
        let separator = super::parameter_pagination::CURSOR_SEPARATOR;
        let mut position = self.aggregator.to_string();
        for (_, value) in &self.grouped_by {
            position.push(separator);
            position.push_str(value);
        }
        position
    }

    /// The modelled `NodeSummary` for this row: the attributes it grouped by, and the count
    /// of nodes under [`COUNT_KEY`].
    #[must_use]
    pub fn to_summary(&self) -> HashMap<String, String> {
        let mut summary: HashMap<String, String> = self
            .grouped_by
            .iter()
            .map(|(attribute, value)| (attribute.clone(), value.clone()))
            .collect();
        summary.insert(COUNT_KEY.to_string(), self.count.to_string());
        summary
    }
}

/// The `NextToken` that resumes a summary AFTER the row at `position`, minted through this
/// crate's one mint of an authenticated cursor under this store's own key.
#[must_use]
pub fn page_token(key: &super::parameter_pagination::PageTokenKey, position: &str) -> String {
    super::parameter_pagination::scoped_page_token(key, CURSOR_SCOPE, position)
}

/// The row position a `NextToken` resumes a summary at, or `None` for any value THIS store
/// did not mint for THIS read — which is what `InvalidNextToken` reports.
#[must_use]
pub fn page_cursor(key: &super::parameter_pagination::PageTokenKey, token: &str) -> Option<String> {
    super::parameter_pagination::scoped_page_cursor(key, CURSOR_SCOPE, token)
}

/// Why `aggregators` names no aggregation this summary can make, or `None` when it does.
///
/// Every rule is one the model states: `AggregatorType` is "the aggregator type for limiting a
/// node summary. Currently, only Count is supported", `TypeName` is "the data type name to use
/// for viewing counts of nodes. Currently, only Instance is supported" and `AttributeName` is
/// "the name of a node attribute on which to limit the count of nodes" — each a closed enum
/// (`crate::ops::types::NODE_AGGREGATOR_TYPE_VALUES`, `NODE_TYPE_NAME_VALUES`,
/// `NODE_ATTRIBUTE_NAME_VALUES`) the centralized request walk refuses anything outside of
/// before a handler runs, so those three arms are BACKSTOPS — and a row reports at most
/// [`MAX_AGGREGATOR_DEPTH`] attributes beside its count, which is the rule left for this
/// function to apply: a nesting deeper than that is an aggregation no row of the modelled
/// `NodeSummary` could carry.
#[must_use]
pub fn aggregator_rejection(aggregators: &[NodeAggregator]) -> Option<String> {
    rejection_at_depth(aggregators, 1)
}

/// [`aggregator_rejection`] at nesting level `depth`, counting the outermost aggregator as
/// level one.
///
/// Every arm carries the one sentence this service states an aggregator it cannot aggregate by
/// with ([`super::inventory::INVALID_AGGREGATOR`], what the aggregated inventory reads answer
/// the same class with), which names nothing of the request.
fn rejection_at_depth(aggregators: &[NodeAggregator], depth: usize) -> Option<String> {
    if depth > MAX_AGGREGATOR_DEPTH {
        return Some(super::inventory::INVALID_AGGREGATOR.to_string());
    }
    for aggregator in aggregators {
        for (value, supported) in [
            (
                aggregator.aggregator_type.as_str(),
                NODE_AGGREGATOR_TYPE_VALUES,
            ),
            (aggregator.type_name.as_str(), NODE_TYPE_NAME_VALUES),
            (
                aggregator.attribute_name.as_str(),
                NODE_ATTRIBUTE_NAME_VALUES,
            ),
        ] {
            if !supported.contains(&value) {
                return Some(super::inventory::INVALID_AGGREGATOR.to_string());
            }
        }
        if let Some(nested) = aggregator.aggregators.as_deref() {
            if let Some(why) = rejection_at_depth(nested, depth + 1) {
                return Some(why);
            }
        }
    }
    None
}

/// The summary of the managed nodes of `regions` that `filters` select, grouped by
/// `aggregators`, in the total order a page walks.
///
/// The selection is the node listing's own ([`super::node_listing::matches_filter`]): every
/// filter has to be satisfied and each filter's values union, so one request's summary counts
/// exactly the nodes the same request's listing would report.
#[must_use]
pub fn summarise(
    store: &ComAmazonawsSsmAmazonssmState,
    regions: &[String],
    filters: &[NodeFilter],
    aggregators: &[NodeAggregator],
) -> Vec<SummaryRow> {
    let selected: Vec<(&str, &ManagedNodeRecord)> = regions
        .iter()
        .flat_map(|region| {
            store
                .managed_nodes_in_region(region)
                .map(move |(_, node)| (region.as_str(), node))
        })
        .filter(|(region, node)| {
            filters
                .iter()
                .all(|filter| super::node_listing::matches_filter(node, region, filter))
        })
        .collect();

    let mut rows = Vec::new();
    for (ordinal, aggregator) in aggregators.iter().enumerate() {
        group(&selected, ordinal, aggregator, &[], &mut rows);
    }
    rows
}

/// The rows the nodes `selected` contribute under `aggregator`, appended to `rows`: one per
/// distinct value they hold for the aggregator's attribute, in ascending order of that value,
/// each carrying the values `grouped_by` already fixed for it.
///
/// A nested aggregator refines each group, so a group with one under it contributes its
/// nested rows instead of a row of its own — the refined counts are what such a summary
/// reports.
fn group(
    selected: &[(&str, &ManagedNodeRecord)],
    ordinal: usize,
    aggregator: &NodeAggregator,
    grouped_by: &[(String, String)],
    rows: &mut Vec<SummaryRow>,
) {
    let mut groups: BTreeMap<&str, Vec<(&str, &ManagedNodeRecord)>> = BTreeMap::new();
    for (region, node) in selected {
        // A node that holds no value for the attribute is in no group of it (see the module
        // docs): a count is of the nodes that carry the value it names.
        if let Some(value) = attribute_value(node, region, &aggregator.attribute_name) {
            groups.entry(value).or_default().push((region, node));
        }
    }
    for (value, nodes) in groups {
        let mut narrowed = grouped_by.to_vec();
        narrowed.push((aggregator.attribute_name.clone(), value.to_string()));
        match aggregator.aggregators.as_deref() {
            Some(nested) if !nested.is_empty() => {
                for refinement in nested {
                    group(&nodes, ordinal, refinement, &narrowed, rows);
                }
            }
            _ => rows.push(SummaryRow {
                aggregator: ordinal,
                grouped_by: narrowed,
                count: nodes.len(),
            }),
        }
    }
}

/// The value `node`, held in `region`, is grouped under for the node attribute `attribute`, or
/// `None` when the record holds none.
///
/// Every value of the modelled `NodeAttributeName`
/// (`crate::ops::types::NODE_ATTRIBUTE_NAME_VALUES`) names an attribute a node is also
/// FILTERED by, so the value a summary groups by is read through the node listing's one
/// reading of a node's attributes ([`super::node_listing::filter_values`]): a summary and a
/// listing of the same request can never disagree about what a node holds.
#[must_use]
pub fn attribute_value<'a>(
    node: &'a ManagedNodeRecord,
    region: &'a str,
    attribute: &str,
) -> Option<&'a str> {
    super::node_listing::filter_values(node, region, attribute)
        .first()
        .copied()
}
