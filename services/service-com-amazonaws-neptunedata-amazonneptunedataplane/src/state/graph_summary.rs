// SPDX-License-Identifier: CC0-1.0

//! The graph summary a statistics generation run computes from the stored property graph.
//!
//! The summary is "a read-only list of node and edge labels and property keys, along with
//! counts of nodes, edges, and properties" (`PropertygraphSummary`'s own documentation), and
//! every number in it is counted HERE, from [`PropertyGraph`] — the one copy of the graph
//! the query endpoints read and write — so what the graph-summary endpoint reports follows
//! from what the graph held when the run counted it. Like the characteristic sets beside it
//! (see [`super::statistics`]), a run is a SNAPSHOT: a write after it does not change the
//! summary that run produced, only what the next run computes, which is what makes
//! `lastStatisticsComputationTime` the instant the reported numbers are true as of.
//!
//! What each member counts, from the member's TYPE and the Neptune graph-summary response
//! it is modelled on — the on-disk model's prose for `numNodeProperties` and
//! `nodeProperties` is transposed (the `Long` member carries the sentence about a list and
//! the list member the sentence about a number), so the shape of the member decides:
//!
//! * `numNodes` / `numEdges` — how many nodes and relationships the graph holds.
//! * `nodeLabels` / `edgeLabels` — the DISTINCT labels, a node's labels and a
//!   relationship's type; `numNodeLabels` / `numEdgeLabels` are how many there are.
//! * `nodeProperties` / `edgeProperties` — one entry per distinct property key, carrying
//!   how many elements that key is set on; `numNodeProperties` / `numEdgeProperties` are
//!   how many distinct keys there are.
//! * `totalNodePropertyValues` / `totalEdgePropertyValues` — "the total number of usages of
//!   all node properties": one usage per key set on one element.
//! * `nodeStructures` / `edgeStructures` — one entry per distinct STRUCTURE, with the
//!   number of elements that have it ("Number of nodes that have this specific
//!   structure"). A node's structure is the set of property keys on it together with the
//!   distinct labels of the relationships leaving it ("A list of distinct outgoing edge
//!   labels present in this specific structure"); a relationship's structure is the set of
//!   property keys on it.
//!
//! Every list is in sorted order and every structure is keyed by sorted sets, so two reads
//! of one run report the same summary in the same order.

use std::collections::{BTreeMap, BTreeSet};

use crate::ops::types::{IllegalArgumentException, GRAPH_SUMMARY_TYPE_VALUES};

use super::PropertyGraph;

/// `PropertygraphSummaryValueMap$version` — "the version of this graph summary response".
///
/// Source: the graph summary response Neptune publishes for a property graph, whose
/// `version` is `v1`. It names the shape of the response, so it is this engine's constant
/// and is not read from the graph.
pub const GRAPH_SUMMARY_VERSION: &str = "v1";

/// The `basic` summary mode, by its wire spelling: the counts and lists, without the
/// structures.
pub const SUMMARY_MODE_BASIC: &str = "basic";

/// The `detailed` summary mode, which adds the node and edge structures.
pub const SUMMARY_MODE_DETAILED: &str = "detailed";

/// Which summary a request asks for, as the model's `GraphSummaryType` declares them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SummaryMode {
    /// The default: every count and list, and no structures.
    Basic,
    /// The counts and lists, plus `nodeStructures` and `edgeStructures`.
    Detailed,
}

impl SummaryMode {
    /// The mode `value` spells, or `None` for a value the member does not accept.
    ///
    /// `value` is a MODELLED wire value by the time a handler reads it — the request's own
    /// spelling is resolved to one at the service boundary (see
    /// [`graph_summary_mode_wire_value`]) — so `None` is the value that names no mode.
    #[must_use]
    pub fn from_wire(value: &str) -> Option<Self> {
        match value {
            SUMMARY_MODE_BASIC => Some(Self::Basic),
            SUMMARY_MODE_DETAILED => Some(Self::Detailed),
            _ => None,
        }
    }

    /// Whether the structures are part of the summary this mode asks for: they are "only
    /// present when the requested mode is DETAILED".
    #[must_use]
    pub fn reports_structures(self) -> bool {
        matches!(self, Self::Detailed)
    }
}

/// The modelled `GraphSummaryType` value `value` spells, read WITHOUT regard to ASCII case,
/// or `None` for a value that spells none of them.
///
/// The member's own documentation states its values in upper case — "Mode can take one of
/// two values: BASIC (the default), and DETAILED" — while the model's value set is lower
/// case, so the spelling a client reads in the documentation and the spelling the model
/// lists are the same value in two cases, and both name it. The value set is the model's
/// ([`GRAPH_SUMMARY_TYPE_VALUES`]): a value outside it still names no mode, in any case.
#[must_use]
pub fn graph_summary_mode_wire_value(value: &str) -> Option<&'static str> {
    GRAPH_SUMMARY_TYPE_VALUES
        .iter()
        .copied()
        .find(|modelled| modelled.eq_ignore_ascii_case(value))
}

/// The `IllegalArgumentException` payload for a summary mode this service does not support,
/// naming the value that was wrong and the modes the member accepts.
pub fn unsupported_graph_summary_mode(mode: &str) -> IllegalArgumentException {
    super::unsupported_member_value("mode", mode, GRAPH_SUMMARY_TYPE_VALUES)
}

/// One node structure: how many nodes have it, and what it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeStructureCounts {
    /// "Number of nodes that have this specific structure."
    pub count: i64,
    /// The property keys set on those nodes, sorted.
    pub node_properties: Vec<String>,
    /// "A list of distinct outgoing edge labels present in this specific structure",
    /// sorted.
    pub distinct_outgoing_edge_labels: Vec<String>,
}

/// One edge structure: how many relationships have it, and the property keys on them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdgeStructureCounts {
    /// "The number of edges that have this specific structure."
    pub count: i64,
    /// The property keys set on those relationships, sorted.
    pub edge_properties: Vec<String>,
}

/// The whole graph summary one generation run counted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GraphSummaryCounts {
    /// How many nodes the graph held.
    pub num_nodes: i64,
    /// How many relationships the graph held.
    pub num_edges: i64,
    /// The distinct node labels, sorted.
    pub node_labels: Vec<String>,
    /// The distinct relationship types, sorted.
    pub edge_labels: Vec<String>,
    /// Each distinct node property key with how many nodes carry it, by key.
    pub node_properties: Vec<(String, i64)>,
    /// Each distinct relationship property key with how many relationships carry it, by
    /// key.
    pub edge_properties: Vec<(String, i64)>,
    /// How many node property usages there are in all.
    pub total_node_property_values: i64,
    /// How many relationship property usages there are in all.
    pub total_edge_property_values: i64,
    /// One entry per distinct node structure.
    pub node_structures: Vec<NodeStructureCounts>,
    /// One entry per distinct edge structure.
    pub edge_structures: Vec<EdgeStructureCounts>,
}

impl GraphSummaryCounts {
    /// How many distinct node labels the graph held.
    #[must_use]
    pub fn num_node_labels(&self) -> i64 {
        counted(self.node_labels.len())
    }

    /// How many distinct relationship types the graph held.
    #[must_use]
    pub fn num_edge_labels(&self) -> i64 {
        counted(self.edge_labels.len())
    }

    /// How many distinct node property keys the graph held.
    #[must_use]
    pub fn num_node_properties(&self) -> i64 {
        counted(self.node_properties.len())
    }

    /// How many distinct relationship property keys the graph held.
    #[must_use]
    pub fn num_edge_properties(&self) -> i64 {
        counted(self.edge_properties.len())
    }
}

/// Count the whole summary of `graph`, the way a statistics generation run does.
///
/// ONE pass over the relationships and one over the nodes, each bounded by what the store
/// holds rather than by any request: the relationships are read first because a node's
/// structure names the labels of the relationships LEAVING it, so they have to be collected
/// by start node before the node structures are formed.
#[must_use]
pub fn graph_summary(graph: &PropertyGraph) -> GraphSummaryCounts {
    let mut edge_labels: BTreeSet<String> = BTreeSet::new();
    let mut edge_properties: BTreeMap<String, i64> = BTreeMap::new();
    let mut edge_structures: BTreeMap<Vec<String>, i64> = BTreeMap::new();
    let mut outgoing: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut total_edge_property_values: i64 = 0;

    for id in graph.relationship_ids() {
        let Some(relationship) = graph.relationship(&id) else {
            continue;
        };
        edge_labels.insert(relationship.rel_type.clone());
        outgoing
            .entry(relationship.from_node.clone())
            .or_default()
            .insert(relationship.rel_type.clone());
        let keys: Vec<String> = relationship.properties.keys().cloned().collect();
        for key in &keys {
            bump(&mut edge_properties, key);
        }
        total_edge_property_values = total_edge_property_values.saturating_add(counted(keys.len()));
        *edge_structures.entry(keys).or_default() += 1;
    }

    let mut node_labels: BTreeSet<String> = BTreeSet::new();
    let mut node_properties: BTreeMap<String, i64> = BTreeMap::new();
    let mut node_structures: BTreeMap<(Vec<String>, Vec<String>), i64> = BTreeMap::new();
    let mut total_node_property_values: i64 = 0;

    for id in graph.node_ids() {
        let Some(node) = graph.node(&id) else {
            continue;
        };
        node_labels.extend(node.labels.iter().cloned());
        let keys: Vec<String> = node.properties.keys().cloned().collect();
        for key in &keys {
            bump(&mut node_properties, key);
        }
        total_node_property_values = total_node_property_values.saturating_add(counted(keys.len()));
        let leaving: Vec<String> = outgoing
            .get(&id)
            .map(|types| types.iter().cloned().collect())
            .unwrap_or_default();
        *node_structures.entry((keys, leaving)).or_default() += 1;
    }

    GraphSummaryCounts {
        num_nodes: counted(graph.node_count()),
        num_edges: counted(graph.relationship_count()),
        node_labels: node_labels.into_iter().collect(),
        edge_labels: edge_labels.into_iter().collect(),
        node_properties: node_properties.into_iter().collect(),
        edge_properties: edge_properties.into_iter().collect(),
        total_node_property_values,
        total_edge_property_values,
        node_structures: node_structures
            .into_iter()
            .map(
                |((node_properties, distinct_outgoing_edge_labels), count)| NodeStructureCounts {
                    count,
                    node_properties,
                    distinct_outgoing_edge_labels,
                },
            )
            .collect(),
        edge_structures: edge_structures
            .into_iter()
            .map(|(edge_properties, count)| EdgeStructureCounts {
                count,
                edge_properties,
            })
            .collect(),
    }
}

/// Count one more use of `key`.
fn bump(counts: &mut BTreeMap<String, i64>, key: &str) {
    let seen = counts.entry(key.to_string()).or_default();
    *seen = seen.saturating_add(1);
}

/// A count as the modelled `Long` members report it. The graph's own ceilings keep every
/// count this engine can produce far inside the range, so the saturation below is
/// unreachable and is here so a count can never wrap.
fn counted(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}
