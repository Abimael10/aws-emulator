// SPDX-License-Identifier: CC0-1.0

//! The property graph this engine stores, and how one of its elements is spelled on the
//! wire.
//!
//! This is the DATA the openCypher endpoints read and write: nodes, each carrying labels
//! and properties, and the directed, typed relationships between them. Every element is
//! addressed by an id the engine minted when the element was created, which is what a
//! query reports it by and what a relationship names its endpoints by.
//!
//! The store holds the graph and nothing about queries: deciding WHICH elements a query
//! reaches is [`super::open_cypher`]'s, so every reading of the graph — a query, a
//! summary, a reset — answers from the one copy here.
//!
//! What a single store may hold is bounded ([`MAX_NODES`], [`MAX_RELATIONSHIPS`],
//! [`MAX_LABELS_PER_NODE`], [`MAX_NAME_BYTES`] and the store's byte ceiling,
//! [`super::open_cypher::MAX_STORE_BYTES`]): the graph grows from what callers write, so
//! the ceilings are what keep the memory one emulator process can be made to take
//! bounded. EVERY string an element is stored with is charged against the byte ceiling —
//! its id, its labels, a relationship's type and endpoints, and its properties — so a
//! sequence of writes cannot grow the store through a string nothing counted. A write
//! past a ceiling is refused with the ceiling it met rather than taken.

use std::collections::{BTreeMap, BTreeSet};

use ::serde_json::Value;

/// The member a graph element reports its id in. Neptune spells the members of an element
/// with a `~` prefix, which is what keeps them apart from the element's own property
/// names.
pub const ELEMENT_ID_MEMBER: &str = "~id";

/// The member naming which KIND of element is being reported.
pub const ELEMENT_ENTITY_TYPE_MEMBER: &str = "~entityType";

/// The member a node reports its labels in.
pub const NODE_LABELS_MEMBER: &str = "~labels";

/// The member an element reports its properties in.
pub const ELEMENT_PROPERTIES_MEMBER: &str = "~properties";

/// The member a relationship reports its type in.
pub const RELATIONSHIP_TYPE_MEMBER: &str = "~type";

/// The members a relationship reports its endpoints in: the node it leaves and the node
/// it enters.
pub const RELATIONSHIP_START_MEMBER: &str = "~start";
pub const RELATIONSHIP_END_MEMBER: &str = "~end";

/// The `~entityType` of each kind of element.
pub const NODE_ENTITY_TYPE: &str = "node";
pub const RELATIONSHIP_ENTITY_TYPE: &str = "relationship";

/// The predicate the statement about an element's LABEL is written with.
///
/// Every statement about an element is a quad whose PREDICATE position names what the
/// statement says — a property key, an edge label, or this one for the element's own label
/// — so the label predicate is one of the graph's predicates like any other
/// ([`PropertyGraph::distinct_predicate_count`]).
pub const LABEL_PREDICATE_KEY: &str = "~label";

/// The most nodes one store holds.
///
/// Emulator resource guard, NOT a quota AWS publishes: a caller chooses how much it
/// writes, so this is what bounds the memory a sequence of writes can take. A create past
/// it is refused with the ceiling it met.
pub const MAX_NODES: usize = 50_000;

/// The most relationships one store holds, for the same reason. Higher than
/// [`MAX_NODES`], because a graph has more edges than vertices.
pub const MAX_RELATIONSHIPS: usize = 100_000;

/// The most labels one node carries.
///
/// Emulator resource guard: a label is de-duplicated by value, so a node cannot hold the
/// same one twice, but a sequence of writes naming DIFFERENT labels would otherwise grow
/// one node without end.
pub const MAX_LABELS_PER_NODE: usize = 64;

/// The longest id, label or relationship type this store keeps, in bytes.
///
/// Emulator resource guard: each is a string a caller chose and each is stored for as
/// long as the element is.
pub const MAX_NAME_BYTES: usize = 4096;

/// One stored node: the labels it carries and the properties set on it.
///
/// `sequence` is the order the store took it in, which is the order a scan hands nodes to
/// a query: ids are unpredictable by construction, so creation order is what makes a scan
/// answer the same way twice.
#[derive(Debug, Clone, Default)]
pub struct GraphNode {
    /// Every label on the node, in the order they were added, each at most once.
    pub labels: Vec<String>,
    /// Every property set on the node, by key.
    pub properties: BTreeMap<String, Value>,
    /// When the store took this node, counted in writes.
    pub sequence: u64,
}

/// One stored relationship: its type, the nodes it joins and the properties set on it.
#[derive(Debug, Clone, Default)]
pub struct GraphRelationship {
    /// The relationship type. A relationship carries exactly one.
    pub rel_type: String,
    /// The id of the node the relationship leaves.
    pub from_node: String,
    /// The id of the node the relationship enters.
    pub to_node: String,
    /// Every property set on the relationship, by key.
    pub properties: BTreeMap<String, Value>,
    /// When the store took this relationship, counted in writes.
    pub sequence: u64,
}

/// Why a write the store was handed was not taken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphWriteError {
    /// The store already holds [`MAX_NODES`] nodes.
    NodeLimit { ceiling: usize },
    /// The store already holds [`MAX_RELATIONSHIPS`] relationships.
    RelationshipLimit { ceiling: usize },
    /// The store already holds as many BYTES of element data as it keeps.
    ByteLimit { ceiling: usize },
    /// The node already carries as many labels as one node carries.
    LabelLimit { ceiling: usize },
    /// An id, a label or a relationship type is longer than this store keeps one.
    NameTooLong { size: usize, ceiling: usize },
    /// The id the write asked for is one the store already holds.
    DuplicateId { id: String },
    /// A relationship was written between a node the store does not hold.
    MissingEndpoint { id: String },
}

/// The nodes and relationships this engine holds.
///
/// `Clone` is what makes a query that writes ATOMIC: the query runs against a copy and
/// the copy replaces this one only once the whole query has succeeded (see
/// [`super::open_cypher::run_query`]).
#[derive(Debug, Default, Clone)]
pub struct PropertyGraph {
    /// Every node, by id.
    nodes: BTreeMap<String, GraphNode>,
    /// Every relationship, by id.
    relationships: BTreeMap<String, GraphRelationship>,
    /// How many writes the store has taken, which is where the next element's
    /// [`GraphNode::sequence`] comes from.
    writes: u64,
    /// How many bytes of element data the store holds, kept as the writes happen so the
    /// ceiling can be taken before a write rather than measured over the whole store on
    /// every one.
    bytes: usize,
}

impl PropertyGraph {
    /// How many nodes the store holds.
    #[must_use]
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// How many relationships the store holds.
    #[must_use]
    pub fn relationship_count(&self) -> usize {
        self.relationships.len()
    }

    /// How many distinct PREDICATES the store holds.
    ///
    /// Every statement the store holds about an element is a quad whose predicate position
    /// names what the statement says: the key of a property set on a node or on a
    /// relationship, the type of a relationship, and [`LABEL_PREDICATE_KEY`] for the
    /// statement carrying an element's own label. The count is of the DISTINCT ones, so a
    /// predicate many elements use is one predicate, and an empty graph holds none.
    ///
    /// ONE pass over the store answers it, bounded by what the store holds rather than by
    /// the request.
    #[must_use]
    pub fn distinct_predicate_count(&self) -> usize {
        let mut predicates: BTreeSet<&str> = BTreeSet::new();
        for node in self.nodes.values() {
            if !node.labels.is_empty() {
                predicates.insert(LABEL_PREDICATE_KEY);
            }
            predicates.extend(node.properties.keys().map(String::as_str));
        }
        for relationship in self.relationships.values() {
            predicates.insert(relationship.rel_type.as_str());
            predicates.extend(relationship.properties.keys().map(String::as_str));
        }
        predicates.len()
    }

    /// How many nodes the store holds carrying each label, by label.
    ///
    /// ONE pass over the store answers every label at once, so a caller asking about many
    /// labels costs no more than a caller asking about one: the work is bounded by what the
    /// store holds ([`MAX_NODES`] nodes of at most [`MAX_LABELS_PER_NODE`] labels each) and
    /// not by the request. A label no node carries is absent from the map, which is the
    /// count zero.
    #[must_use]
    pub fn label_counts(&self) -> BTreeMap<String, usize> {
        let mut counted: BTreeMap<String, usize> = BTreeMap::new();
        for node in self.nodes.values() {
            for label in &node.labels {
                *counted.entry(label.clone()).or_default() += 1;
            }
        }
        counted
    }

    /// How many relationships the store holds of each type, by type. One pass, for the same
    /// reason [`Self::label_counts`] takes one.
    #[must_use]
    pub fn relationship_type_counts(&self) -> BTreeMap<String, usize> {
        let mut counted: BTreeMap<String, usize> = BTreeMap::new();
        for relationship in self.relationships.values() {
            *counted
                .entry(relationship.rel_type.clone())
                .or_default() += 1;
        }
        counted
    }

    /// How many bytes of element data the store holds: every id, label, relationship
    /// type, endpoint id and property it keeps.
    #[must_use]
    pub fn stored_bytes(&self) -> usize {
        self.bytes
    }

    /// The node `id` names, or `None` for an id the store does not hold.
    #[must_use]
    pub fn node(&self, id: &str) -> Option<&GraphNode> {
        self.nodes.get(id)
    }

    /// The relationship `id` names, or `None` for an id the store does not hold.
    #[must_use]
    pub fn relationship(&self, id: &str) -> Option<&GraphRelationship> {
        self.relationships.get(id)
    }

    /// Every node id, in the order the store took them.
    #[must_use]
    pub fn node_ids(&self) -> Vec<String> {
        Self::in_store_order(self.nodes.iter().map(|(id, node)| (node.sequence, id)))
    }

    /// Every relationship id, in the order the store took them.
    #[must_use]
    pub fn relationship_ids(&self) -> Vec<String> {
        Self::in_store_order(
            self.relationships
                .iter()
                .map(|(id, relationship)| (relationship.sequence, id)),
        )
    }

    /// The ids of `elements`, oldest write first; ties (which the counter cannot produce)
    /// broken by id, so the order is total.
    fn in_store_order<'ids>(
        elements: impl Iterator<Item = (u64, &'ids String)>,
    ) -> Vec<String> {
        let mut ordered: Vec<(u64, &String)> = elements.collect();
        ordered.sort_unstable();
        ordered
            .into_iter()
            .map(|(_, id)| id.clone())
            .collect()
    }

    /// Store a node carrying `labels` and `properties`, and report the id it is addressed
    /// by.
    ///
    /// `id` is the id the write asked for, for a caller that names the element it creates;
    /// a write that names none is given a fresh one ([`new_element_id`]).
    pub fn insert_node(
        &mut self,
        id: Option<String>,
        labels: Vec<String>,
        properties: BTreeMap<String, Value>,
    ) -> Result<String, GraphWriteError> {
        if self.nodes.len() >= MAX_NODES {
            return Err(GraphWriteError::NodeLimit { ceiling: MAX_NODES });
        }
        if labels.len() > MAX_LABELS_PER_NODE {
            return Err(GraphWriteError::LabelLimit {
                ceiling: MAX_LABELS_PER_NODE,
            });
        }
        for label in &labels {
            named_within(label)?;
        }
        let id = self.claim_id(id)?;
        // Every string the element is stored with is charged, not only its properties:
        // its id and its labels are data a caller chose too.
        let written = id
            .len()
            .saturating_add(names_bytes(&labels))
            .saturating_add(properties_bytes(&properties));
        self.room_for(written, 0)?;
        self.writes = self.writes.saturating_add(1);
        self.bytes = self.bytes.saturating_add(written);
        self.nodes.insert(
            id.clone(),
            GraphNode {
                labels,
                properties,
                sequence: self.writes,
            },
        );
        Ok(id)
    }

    /// Store a relationship of `rel_type` from `from_node` to `to_node`, and report the id
    /// it is addressed by. Both endpoints must be nodes the store holds.
    pub fn insert_relationship(
        &mut self,
        id: Option<String>,
        rel_type: String,
        from_node: &str,
        to_node: &str,
        properties: BTreeMap<String, Value>,
    ) -> Result<String, GraphWriteError> {
        if self.relationships.len() >= MAX_RELATIONSHIPS {
            return Err(GraphWriteError::RelationshipLimit {
                ceiling: MAX_RELATIONSHIPS,
            });
        }
        for endpoint in [from_node, to_node] {
            if !self.nodes.contains_key(endpoint) {
                return Err(GraphWriteError::MissingEndpoint {
                    id: endpoint.to_string(),
                });
            }
        }
        named_within(&rel_type)?;
        let id = self.claim_id(id)?;
        // The id, the type and both endpoint ids are stored with the relationship, so
        // they are charged with its properties.
        let written = id
            .len()
            .saturating_add(rel_type.len())
            .saturating_add(from_node.len())
            .saturating_add(to_node.len())
            .saturating_add(properties_bytes(&properties));
        self.room_for(written, 0)?;
        self.writes = self.writes.saturating_add(1);
        self.bytes = self.bytes.saturating_add(written);
        self.relationships.insert(
            id.clone(),
            GraphRelationship {
                rel_type,
                from_node: from_node.to_string(),
                to_node: to_node.to_string(),
                properties,
                sequence: self.writes,
            },
        );
        Ok(id)
    }

    /// The id a write will be stored under: the one it asked for, once no element already
    /// holds it and it is no longer than one this store keeps, or a fresh one.
    fn claim_id(&self, asked: Option<String>) -> Result<String, GraphWriteError> {
        match asked {
            Some(id) => {
                named_within(&id)?;
                if self.nodes.contains_key(&id) || self.relationships.contains_key(&id) {
                    return Err(GraphWriteError::DuplicateId { id });
                }
                Ok(id)
            }
            None => Ok(new_element_id()),
        }
    }

    /// Add `label` to the node `id` names, and report whether a node took it. A label the
    /// node already carries is not added twice.
    ///
    /// A label is data a caller chose, so it is charged against the store's ceiling and
    /// counted against how many labels one node carries: labels are de-duplicated by
    /// value but a sequence of distinct ones would otherwise grow one node without end.
    pub fn add_node_label(&mut self, id: &str, label: &str) -> Result<bool, GraphWriteError> {
        named_within(label)?;
        let Some(node) = self.nodes.get(id) else {
            return Ok(false);
        };
        if node.labels.iter().any(|held| held == label) {
            return Ok(true);
        }
        if node.labels.len() >= MAX_LABELS_PER_NODE {
            return Err(GraphWriteError::LabelLimit {
                ceiling: MAX_LABELS_PER_NODE,
            });
        }
        self.room_for(label.len(), 0)?;
        if let Some(node) = self.nodes.get_mut(id) {
            node.labels.push(label.to_string());
        }
        self.bytes = self.bytes.saturating_add(label.len());
        Ok(true)
    }

    /// Remove `label` from the node `id` names, and report whether a node was reached.
    pub fn remove_node_label(&mut self, id: &str, label: &str) -> bool {
        let Some(node) = self.nodes.get_mut(id) else {
            return false;
        };
        let held = node.labels.len();
        node.labels.retain(|kept| kept != label);
        if node.labels.len() < held {
            self.bytes = self.bytes.saturating_sub(label.len());
        }
        true
    }

    /// Set, or with `None` remove, a property of the element `id` names, and report
    /// whether an element was reached.
    ///
    /// openCypher has no "property set to null": assigning null REMOVES the property, so
    /// one writer serves both.
    pub fn set_element_property(
        &mut self,
        id: &str,
        key: &str,
        value: Option<Value>,
    ) -> Result<bool, GraphWriteError> {
        let held = self
            .node(id)
            .map(|node| &node.properties)
            .or_else(|| self.relationship(id).map(|held| &held.properties));
        let Some(held) = held else {
            return Ok(false);
        };
        let removed = held
            .get_key_value(key)
            .map_or(0, |(key, value)| property_bytes(key, value));
        let written = value
            .as_ref()
            .map_or(0, |value| property_bytes(key, value));
        self.room_for(written, removed)?;
        let properties = if let Some(node) = self.nodes.get_mut(id) {
            &mut node.properties
        } else if let Some(relationship) = self.relationships.get_mut(id) {
            &mut relationship.properties
        } else {
            return Ok(false);
        };
        match value {
            Some(value) => {
                properties.insert(key.to_string(), value);
            }
            None => {
                properties.remove(key);
            }
        }
        self.bytes = self.bytes.saturating_sub(removed).saturating_add(written);
        Ok(true)
    }

    /// Replace every property of the element `id` names with `properties`, and report
    /// whether an element was reached.
    pub fn replace_element_properties(
        &mut self,
        id: &str,
        properties: BTreeMap<String, Value>,
    ) -> Result<bool, GraphWriteError> {
        let held = self
            .node(id)
            .map(|node| &node.properties)
            .or_else(|| self.relationship(id).map(|held| &held.properties));
        let Some(held) = held else {
            return Ok(false);
        };
        let removed = properties_bytes(held);
        let written = properties_bytes(&properties);
        self.room_for(written, removed)?;
        if let Some(node) = self.nodes.get_mut(id) {
            node.properties = properties;
        } else if let Some(relationship) = self.relationships.get_mut(id) {
            relationship.properties = properties;
        } else {
            return Ok(false);
        }
        self.bytes = self.bytes.saturating_sub(removed).saturating_add(written);
        Ok(true)
    }

    /// Whether the store has room for `written` bytes once `removed` have gone, or the
    /// ceiling it would be past.
    ///
    /// Taken BEFORE the write: the count of elements is capped and so is the size of one
    /// value, and this is what caps their product, so a sequence of requests each writing
    /// a larger value to the same element cannot grow the store without end.
    fn room_for(&self, written: usize, removed: usize) -> Result<(), GraphWriteError> {
        let after = self
            .bytes
            .saturating_sub(removed)
            .saturating_add(written);
        if after > super::open_cypher::MAX_STORE_BYTES {
            return Err(GraphWriteError::ByteLimit {
                ceiling: super::open_cypher::MAX_STORE_BYTES,
            });
        }
        Ok(())
    }

    /// The ids of every relationship with the node `id` names at either end.
    #[must_use]
    pub fn incident_relationship_ids(&self, id: &str) -> Vec<String> {
        Self::in_store_order(
            self.relationships
                .iter()
                .filter(|(_, relationship)| {
                    relationship.from_node == id || relationship.to_node == id
                })
                .map(|(rel_id, relationship)| (relationship.sequence, rel_id)),
        )
    }

    /// Remove the node `id` names, and report whether one was removed. Its relationships
    /// are NOT removed with it: a caller that means to delete those says so, and
    /// [`Self::incident_relationship_ids`] is what tells a delete they are there.
    pub fn remove_node(&mut self, id: &str) -> bool {
        match self.nodes.remove(id) {
            Some(node) => {
                let held = id
                    .len()
                    .saturating_add(names_bytes(&node.labels))
                    .saturating_add(properties_bytes(&node.properties));
                self.bytes = self.bytes.saturating_sub(held);
                true
            }
            None => false,
        }
    }

    /// Remove the relationship `id` names, and report whether one was removed.
    pub fn remove_relationship(&mut self, id: &str) -> bool {
        match self.relationships.remove(id) {
            Some(relationship) => {
                let held = id
                    .len()
                    .saturating_add(relationship.rel_type.len())
                    .saturating_add(relationship.from_node.len())
                    .saturating_add(relationship.to_node.len())
                    .saturating_add(properties_bytes(&relationship.properties));
                self.bytes = self.bytes.saturating_sub(held);
                true
            }
            None => false,
        }
    }

    /// Empty the store: every node, every relationship and the data on them, which is
    /// what a database reset leaves behind.
    pub fn clear(&mut self) {
        self.nodes.clear();
        self.relationships.clear();
        self.bytes = 0;
    }

    /// How many bytes of the store the element `id` names takes: its id, its labels or
    /// its type and endpoints, and its properties. `0` for an id the store does not hold.
    ///
    /// This is what a reader charges itself for rendering the element, so an answer
    /// carrying the same element many times pays for each one.
    #[must_use]
    pub fn element_bytes(&self, id: &str) -> usize {
        if let Some(node) = self.nodes.get(id) {
            return id
                .len()
                .saturating_add(names_bytes(&node.labels))
                .saturating_add(properties_bytes(&node.properties));
        }
        if let Some(relationship) = self.relationships.get(id) {
            return id
                .len()
                .saturating_add(relationship.rel_type.len())
                .saturating_add(relationship.from_node.len())
                .saturating_add(relationship.to_node.len())
                .saturating_add(properties_bytes(&relationship.properties));
        }
        0
    }

    /// The node `id` names as this service spells a node on the wire, or `Null` for an id
    /// the store does not hold.
    #[must_use]
    pub fn node_document(&self, id: &str) -> Value {
        let Some(node) = self.nodes.get(id) else {
            return Value::Null;
        };
        ::serde_json::json!({
            ELEMENT_ID_MEMBER: id,
            ELEMENT_ENTITY_TYPE_MEMBER: NODE_ENTITY_TYPE,
            NODE_LABELS_MEMBER: node.labels.clone(),
            ELEMENT_PROPERTIES_MEMBER: properties_document(&node.properties),
        })
    }

    /// The relationship `id` names as this service spells a relationship on the wire, or
    /// `Null` for an id the store does not hold.
    #[must_use]
    pub fn relationship_document(&self, id: &str) -> Value {
        let Some(relationship) = self.relationships.get(id) else {
            return Value::Null;
        };
        ::serde_json::json!({
            ELEMENT_ID_MEMBER: id,
            ELEMENT_ENTITY_TYPE_MEMBER: RELATIONSHIP_ENTITY_TYPE,
            RELATIONSHIP_TYPE_MEMBER: relationship.rel_type.clone(),
            RELATIONSHIP_START_MEMBER: relationship.from_node.clone(),
            RELATIONSHIP_END_MEMBER: relationship.to_node.clone(),
            ELEMENT_PROPERTIES_MEMBER: properties_document(&relationship.properties),
        })
    }
}

/// An element's properties as a JSON object, keys in their stored (sorted) order.
fn properties_document(properties: &BTreeMap<String, Value>) -> Value {
    Value::Object(
        properties
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
    )
}

/// How many bytes of the store one set of properties takes.
fn properties_bytes(properties: &BTreeMap<String, Value>) -> usize {
    properties
        .iter()
        .fold(0, |total, (key, value)| {
            total.saturating_add(property_bytes(key, value))
        })
}

/// How many bytes of the store a set of names (an element's labels) takes.
fn names_bytes(names: &[String]) -> usize {
    names
        .iter()
        .fold(0, |total, name| total.saturating_add(name.len()))
}

/// Refuse a name longer than this store keeps one.
///
/// Emulator resource guard: an id, a label and a relationship type are each a string a
/// caller chose, and each is stored for as long as the element is, so the length of one
/// is bounded the way the size of a value is.
fn named_within(name: &str) -> Result<(), GraphWriteError> {
    if name.len() > MAX_NAME_BYTES {
        return Err(GraphWriteError::NameTooLong {
            size: name.len(),
            ceiling: MAX_NAME_BYTES,
        });
    }
    Ok(())
}

/// How many bytes of the store one property takes: its name and its value, measured as
/// the JSON the store holds it as.
fn property_bytes(key: &str, value: &Value) -> usize {
    key.len().saturating_add(value_bytes(value, 0))
}

/// How many bytes one stored value takes.
///
/// A stored value is a primitive or a list of them ([`super::property_json`] refuses
/// anything else), so this descends only as deep as the value the evaluation wrote; the
/// depth is capped all the same, so nothing can drive it deeper than the writer admits.
fn value_bytes(value: &Value, depth: usize) -> usize {
    /// The bytes a value of a fixed size is counted as: enough to cover the number or the
    /// flag and the space the map entry holding it takes.
    const SCALAR_BYTES: usize = 16;
    if depth >= MAX_VALUE_NESTING {
        return SCALAR_BYTES;
    }
    let deeper = depth.saturating_add(1);
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => SCALAR_BYTES,
        Value::String(text) => SCALAR_BYTES.saturating_add(text.len()),
        Value::Array(items) => items.iter().fold(SCALAR_BYTES, |total, item| {
            total.saturating_add(value_bytes(item, deeper))
        }),
        Value::Object(entries) => entries.iter().fold(SCALAR_BYTES, |total, (key, entry)| {
            total
                .saturating_add(key.len())
                .saturating_add(value_bytes(entry, deeper))
        }),
    }
}

/// The deepest a stored value is measured to. A value is written through
/// [`super::property_json`], which refuses one nested deeper than the evaluation reads.
const MAX_VALUE_NESTING: usize = 128;

/// A fresh id for an element the engine is creating.
///
/// Version 4, so the id is derived from nothing the caller sent: an element a caller did
/// not name is addressed by an id no other caller can guess or collide with.
#[must_use]
pub fn new_element_id() -> String {
    ::uuid::Uuid::new_v4().to_string()
}
