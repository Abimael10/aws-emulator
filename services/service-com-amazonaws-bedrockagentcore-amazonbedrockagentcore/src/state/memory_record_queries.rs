// SPDX-License-Identifier: CC0-1.0

//! The QUERY side of the long-term memory record plane: which of a memory container's
//! records a listing reports, and in which order.
//!
//! A record is stored by the container ([`super::StoredMemoryRecord`]), so a listing is a
//! VIEW over what was stored: it narrows the container's records by the namespace, the
//! strategy association and the metadata the request asks for, and reports what is left.
//! Nothing here writes.
//!
//! The narrowing is stated once, in [`MemoryRecordSelection`], because the records surface
//! offers it on more than one call: the same `namespace` / `namespacePath` /
//! `metadataFilters` members scope a plain listing and a relevance search alike, so the two
//! cannot read one filter two different ways.
//!
//! A `namespace` is a hierarchy written with `/` between its levels, which is what the two
//! namespace members mean by their wording: `namespace` is "the exact namespace to filter
//! memory records by", so it selects the records stored under that namespace itself, and
//! `namespacePath` is used "for hierarchical retrievals [to] return all memory records
//! where namespace falls under the same parent hierarchy", so it selects the records of
//! that level and of every level beneath it (source: smithy_query query="operation"
//! operation="ListMemoryRecords").
//!
//! The work one listing does is bounded by the store's own ceiling
//! ([`super::MAX_MEMORY_RECORDS_PER_MEMORY`] records in a container) and by the modelled
//! `@length(min: 1, max: 5)` on the filter list, so neither the scan nor the sort grows
//! with request volume.

use std::cmp::Ordering;

use super::{
    normalize_memory_id, ComAmazonawsBedrockagentcoreAmazonbedrockagentcoreState,
    StoredMemoryRecord,
};
use crate::ops::types::{
    MemoryMetadataFilterExpression, MemoryRecordLeftExpression, MemoryRecordMetadataValue,
    MemoryRecordRightExpression, MemoryRecordSummary,
};

/// The character that separates one level of a namespace from the next.
///
/// `Namespace`'s pattern (`^[a-zA-Z0-9/*][a-zA-Z0-9-_/*]*(?::[a-zA-Z0-9-_/*]+)*...$`)
/// admits `/` inside a level-separated path and `:` between whole paths, and the member
/// documented "for hierarchical retrievals" is read against that path, so a level boundary
/// is a `/`.
const NAMESPACE_SEPARATOR: char = '/';

/// The most metadata filter expressions one request carries.
///
/// `MemoryMetadataFilterList` is modelled `@length(min: 1, max: 5)`, so the request
/// boundary already refuses more; restating it here keeps the bound on the work a
/// selection does (expressions x the container's records) LOCAL to this crate rather than
/// inherited from the validator.
pub const MAX_MEMORY_METADATA_FILTERS: usize = 5;

/// `MemoryRecordOperatorType`'s values, the relationships a metadata filter expression
/// states between a record's metadata entry and the value it is compared against (source:
/// smithy_query query="shape" shape="MemoryRecordOperatorType": EQUALS_TO | EXISTS |
/// NOT_EXISTS | BEFORE | AFTER | CONTAINS | GREATER_THAN | GREATER_THAN_OR_EQUALS |
/// LESS_THAN | LESS_THAN_OR_EQUALS). A value outside the set is refused at the boundary.
const OPERATOR_EQUALS_TO: &str = "EQUALS_TO";
/// See [`OPERATOR_EQUALS_TO`].
const OPERATOR_EXISTS: &str = "EXISTS";
/// See [`OPERATOR_EQUALS_TO`].
const OPERATOR_NOT_EXISTS: &str = "NOT_EXISTS";
/// See [`OPERATOR_EQUALS_TO`].
const OPERATOR_BEFORE: &str = "BEFORE";
/// See [`OPERATOR_EQUALS_TO`].
const OPERATOR_AFTER: &str = "AFTER";
/// See [`OPERATOR_EQUALS_TO`].
const OPERATOR_CONTAINS: &str = "CONTAINS";
/// See [`OPERATOR_EQUALS_TO`].
const OPERATOR_GREATER_THAN: &str = "GREATER_THAN";
/// See [`OPERATOR_EQUALS_TO`].
const OPERATOR_GREATER_THAN_OR_EQUALS: &str = "GREATER_THAN_OR_EQUALS";
/// See [`OPERATOR_EQUALS_TO`].
const OPERATOR_LESS_THAN: &str = "LESS_THAN";
/// See [`OPERATOR_EQUALS_TO`].
const OPERATOR_LESS_THAN_OR_EQUALS: &str = "LESS_THAN_OR_EQUALS";

impl StoredMemoryRecord {
    /// This record as the modelled `MemoryRecordSummary`, the shape a listing of a
    /// container's records reports each one as.
    ///
    /// `score` is left out: it is "the relevance score of the memory record when returned
    /// as part of a search result" (source: smithy_query query="shape"
    /// shape="MemoryRecordSummary"), and a listing ranks nothing — it reports the records
    /// the request's filters select, so no record of one has a relevance to report.
    #[must_use]
    pub fn to_summary(&self) -> MemoryRecordSummary {
        MemoryRecordSummary {
            memory_record_id: self.memory_record_id.clone(),
            content: self.content.clone(),
            memory_strategy_id: self.memory_strategy_id.clone(),
            namespaces: self.namespaces.clone(),
            created_at: self.created_at,
            score: None,
            timestamp: Some(self.timestamp),
            metadata: self.metadata.clone(),
        }
    }

    /// The key a listing orders this record by: the instant it was created, with the
    /// write sequence breaking a tie between two records created at the same instant.
    ///
    /// A listing reports the records in the REVERSE of this order (the most recently
    /// created record first), and the key is a TOTAL order over a container's records, so
    /// two pages of one listing cannot disagree about where a record sits.
    #[must_use]
    pub fn order_key(&self) -> (i64, u64) {
        (self.created_at.secs(), self.seq)
    }

    /// Whether this record is stored under the namespace `namespace` names exactly.
    #[must_use]
    pub fn in_namespace(&self, namespace: &str) -> bool {
        self.namespaces
            .iter()
            .any(|stored| stored.as_str() == namespace)
    }

    /// Whether this record's namespace falls under the hierarchy `path` names: that level
    /// itself, or any level beneath it.
    #[must_use]
    pub fn under_namespace_path(&self, path: &str) -> bool {
        let parent = path.trim_end_matches(NAMESPACE_SEPARATOR);
        self.namespaces.iter().any(|stored| {
            stored.as_str() == parent
                || stored
                    .strip_prefix(parent)
                    .is_some_and(|beneath| beneath.starts_with(NAMESPACE_SEPARATOR))
        })
    }
}

/// The narrowing one memory-record listing asks for: the namespace scope, the strategy
/// association and the metadata expressions a reported record has to satisfy.
///
/// Every member is optional on the wire, and an absent one narrows nothing; several
/// members narrow TOGETHER, so a record is reported only when it satisfies all of them.
/// The two namespace members are documented as alternatives of one another while the model
/// marks neither required, so a request that states neither is answered with the
/// container's records rather than refused: a refusal this service has no recorded wording
/// for would be a worse answer than the listing the request asked for.
#[derive(Debug, Clone, Copy)]
pub struct MemoryRecordSelection<'a> {
    /// The exact namespace a reported record is stored under.
    pub namespace: Option<&'a str>,
    /// The namespace hierarchy a reported record's namespace falls under.
    pub namespace_path: Option<&'a str>,
    /// The strategy a reported record is grouped under.
    pub memory_strategy_id: Option<&'a str>,
    /// The metadata expressions a reported record's metadata satisfies, each evaluated
    /// against the entry its `left.metadataKey` names.
    pub metadata_filters: &'a [MemoryMetadataFilterExpression],
}

impl MemoryRecordSelection<'_> {
    /// Refuse a selection that cannot be evaluated as asked, with the sentence a handler
    /// answers `ValidationException` with; `Ok(())` for one that can.
    ///
    /// What reaches here past the modelled constraints is an expression whose operator
    /// compares against a value with no value to compare against: `right` is optional
    /// because `EXISTS` and `NOT_EXISTS` take none, and is "required for all operators
    /// except EXISTS and NOT_EXISTS" (source: smithy_query query="shape"
    /// shape="MemoryMetadataFilterExpression").
    pub fn checked(&self) -> Result<(), String> {
        if self.metadata_filters.len() > MAX_MEMORY_METADATA_FILTERS {
            return Err(format!(
                "metadataFilters carries {} expressions; at most {MAX_MEMORY_METADATA_FILTERS} \
                 are accepted",
                self.metadata_filters.len()
            ));
        }
        for expression in self.metadata_filters {
            let MemoryMetadataFilterExpression {
                left: MemoryRecordLeftExpression::metadataKey(key),
                operator,
                right,
            } = expression;
            if right.is_none()
                && !matches!(
                    operator.as_str(),
                    OPERATOR_EXISTS | OPERATOR_NOT_EXISTS
                )
            {
                return Err(format!(
                    "metadataFilters expression on key {key} uses operator {operator}, which \
                     requires right.metadataValue"
                ));
            }
        }
        Ok(())
    }

    /// Whether `record` is one this selection reports.
    #[must_use]
    pub fn holds(&self, record: &StoredMemoryRecord) -> bool {
        self.namespace
            .is_none_or(|namespace| record.in_namespace(namespace))
            && self
                .namespace_path
                .is_none_or(|path| record.under_namespace_path(path))
            && self
                .memory_strategy_id
                .is_none_or(|strategy| record.memory_strategy_id == strategy)
            && self
                .metadata_filters
                .iter()
                .all(|expression| metadata_satisfies(record, expression))
    }
}

/// Whether `record`'s metadata satisfies one expression.
///
/// The entry the expression reads is the one its `left.metadataKey` names; a record
/// carrying no such entry satisfies `NOT_EXISTS` and nothing else. An expression whose two
/// sides are values of different kinds — a number compared against a timestamp, say —
/// selects nothing, because there is no relationship of the stated kind between them.
fn metadata_satisfies(
    record: &StoredMemoryRecord,
    expression: &MemoryMetadataFilterExpression,
) -> bool {
    let MemoryMetadataFilterExpression {
        left: MemoryRecordLeftExpression::metadataKey(key),
        operator,
        right,
    } = expression;
    let stored = record
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get(key));
    match operator.as_str() {
        OPERATOR_EXISTS => return stored.is_some(),
        OPERATOR_NOT_EXISTS => return stored.is_none(),
        _ => {}
    }
    let (Some(stored), Some(MemoryRecordRightExpression::metadataValue(wanted))) = (stored, right)
    else {
        return false;
    };
    match operator.as_str() {
        OPERATOR_EQUALS_TO => same_value(stored, wanted),
        OPERATOR_CONTAINS => contains(stored, wanted),
        // BEFORE and AFTER are relationships between INSTANTS, so they hold between two
        // timestamps and between nothing else; the four inequality operators below are the
        // same comparison over any pair of values of one ordered kind.
        OPERATOR_BEFORE => matches!(instant_order(stored, wanted), Some(Ordering::Less)),
        OPERATOR_AFTER => matches!(instant_order(stored, wanted), Some(Ordering::Greater)),
        OPERATOR_LESS_THAN => matches!(compare(stored, wanted), Some(Ordering::Less)),
        OPERATOR_LESS_THAN_OR_EQUALS => {
            matches!(compare(stored, wanted), Some(Ordering::Less | Ordering::Equal))
        }
        OPERATOR_GREATER_THAN => matches!(compare(stored, wanted), Some(Ordering::Greater)),
        OPERATOR_GREATER_THAN_OR_EQUALS => matches!(
            compare(stored, wanted),
            Some(Ordering::Greater | Ordering::Equal)
        ),
        // Every value `MemoryRecordOperatorType` declares is evaluated above, and the
        // boundary refuses one outside that set, so nothing else states a relationship a
        // record can be in.
        _ => false,
    }
}

/// Whether two metadata values are the same value: the same union member, holding the same
/// content.
fn same_value(stored: &MemoryRecordMetadataValue, wanted: &MemoryRecordMetadataValue) -> bool {
    use MemoryRecordMetadataValue as Value;
    match (stored, wanted) {
        (Value::stringValue(left), Value::stringValue(right)) => left == right,
        (Value::stringListValue(left), Value::stringListValue(right)) => left == right,
        (Value::numberValue(left), Value::numberValue(right)) => left == right,
        (Value::dateTimeValue(_), Value::dateTimeValue(_)) => {
            matches!(compare(stored, wanted), Some(Ordering::Equal))
        }
        _ => false,
    }
}

/// Whether `stored` contains `wanted`: a string contains it as a substring, and a list of
/// strings contains it as one of its elements (or, against a list, contains every element
/// of it).
fn contains(stored: &MemoryRecordMetadataValue, wanted: &MemoryRecordMetadataValue) -> bool {
    use MemoryRecordMetadataValue as Value;
    match (stored, wanted) {
        (Value::stringValue(text), Value::stringValue(part)) => text.contains(part.as_str()),
        (Value::stringListValue(list), Value::stringValue(element)) => list.contains(element),
        (Value::stringListValue(list), Value::stringListValue(elements)) => {
            elements.iter().all(|element| list.contains(element))
        }
        _ => false,
    }
}

/// How `stored` orders against `wanted` as INSTANTS, or `None` when either side is a value
/// of another kind: "before" and "after" are relationships in time, so there is no such
/// relationship between two numbers or two strings.
fn instant_order(
    stored: &MemoryRecordMetadataValue,
    wanted: &MemoryRecordMetadataValue,
) -> Option<Ordering> {
    use MemoryRecordMetadataValue as Value;
    match (stored, wanted) {
        (Value::dateTimeValue(_), Value::dateTimeValue(_)) => compare(stored, wanted),
        _ => None,
    }
}

/// How `stored` orders against `wanted`, or `None` when the two are of kinds no ordering
/// is defined over: numbers compare numerically, timestamps as instants and strings in
/// lexicographic order, while a list of strings has no order at all.
fn compare(
    stored: &MemoryRecordMetadataValue,
    wanted: &MemoryRecordMetadataValue,
) -> Option<Ordering> {
    use MemoryRecordMetadataValue as Value;
    match (stored, wanted) {
        (Value::numberValue(left), Value::numberValue(right)) => left.partial_cmp(right),
        (Value::dateTimeValue(left), Value::dateTimeValue(right)) => Some(
            left.secs()
                .cmp(&right.secs())
                .then(left.subsec_nanos().cmp(&right.subsec_nanos())),
        ),
        (Value::stringValue(left), Value::stringValue(right)) => Some(left.as_str().cmp(right)),
        _ => None,
    }
}

impl ComAmazonawsBedrockagentcoreAmazonbedrockagentcoreState {
    /// The long-term memory records of the container `memory_id` names that `selection`
    /// reports, most recently created first.
    ///
    /// A container this plane holds no record for reports none, which is not a statement
    /// about whether the CONTAINER exists — that is resolved against the component that
    /// owns it ([`super::memory_item_container_resolved`]) before this is read. The
    /// records are borrowed rather than cloned, so only the page a response carries is
    /// ever materialised.
    #[must_use]
    pub fn selected_memory_records(
        &self,
        memory_id: &str,
        selection: &MemoryRecordSelection<'_>,
    ) -> Vec<&StoredMemoryRecord> {
        let Some(memory) = self.memories.get(&normalize_memory_id(memory_id)) else {
            return Vec::new();
        };
        let mut selected: Vec<&StoredMemoryRecord> = memory
            .records
            .values()
            .filter(|record| selection.holds(record))
            .collect();
        selected.sort_by_key(|record| std::cmp::Reverse(record.order_key()));
        selected
    }
}
