// SPDX-License-Identifier: CC0-1.0

//! Which OpsItems a `DescribeOpsItems` query selects, and in what order it reports them.
//!
//! "Query a set of OpsItems" (`DescribeOpsItems`) over "one or more filters to limit the
//! response" (`$OpsItemFilters`), each filter a `Key`, an `Operator` and the `Values` that
//! operator accepts (`OpsItemFilter`). So a query is a CONJUNCTION of criteria over the
//! OpsItems the Region holds: an OpsItem is reported when it satisfies EVERY filter, and one
//! filter is satisfied when any of its values holds against the property its key names
//! ([`satisfies`]).
//!
//! What a key names is the record's own member ([`held_for`]): nothing is derived at read time
//! and nothing is written, so a query reports exactly what the writes of those OpsItems
//! persisted. A key naming a property of a change request, an access request or an insight
//! sub-record selects NOTHING here, because this store holds no such sub-record for an
//! OpsItem — rather than being answered from a member that is not what the key asks about.
//!
//! The comparison reads both sides under the TYPE the key names: a time key compares instants,
//! `Priority` compares numbers, and every other key compares text, so a value that is no
//! instant (or no number) for such a key selects nothing instead of being compared as a
//! string. The four operators are the modelled `OpsItemFilterOperator` values, and which of
//! them a given key accepts is published per key ([`PUBLISHED_KEY_OPERATORS`]): a filter
//! pairing a key with an operator that key does not accept is refused before anything is
//! selected ([`filter_rejection`]).
//!
//! The work one query does is bounded by the store's own bound on how many OpsItems it holds
//! ([`super::ops_items::MAX_OPS_ITEMS`]) times the filters the request carries, which the
//! request's own size bounds.

use std::cmp::Ordering;
use std::time::SystemTime;

use super::ops_items::{
    OpsItemRecord, RELATED_AUTOMATIONS_OPERATIONAL_DATA_KEY, RELATED_RESOURCES_OPERATIONAL_DATA_KEY,
};
use super::ComAmazonawsSsmAmazonssmState;
use crate::ops::types::OpsItemFilter;

/// The bounds the modelled `OpsItemMaxResults` carries (range 1..50).
pub const MIN_OPS_ITEM_PAGE_SIZE: i32 = 1;

/// See [`MIN_OPS_ITEM_PAGE_SIZE`]. It is also the page size a request that names no
/// `MaxResults` gets, the way every other read of this service serves one.
pub const MAX_OPS_ITEM_PAGE_SIZE: i32 = 50;

/// The page size an OpsItem query serves, or the refusal message for a `MaxResults` outside
/// the modelled range — which the protocol's boundary validator already refuses, so this is a
/// backstop rather than the first line of defence.
pub fn ops_item_page_size(max_results: Option<i32>) -> Result<usize, String> {
    super::page_size(max_results, MIN_OPS_ITEM_PAGE_SIZE, MAX_OPS_ITEM_PAGE_SIZE)
}

/// How many characters of a `Title` the `Equal` operator compares: "The Equals operator for
/// Title matches the first 100 characters. If you specify more than 100 characters, they are
/// ignored" (`DescribeOpsItems`'s `OpsItemFilters` doc).
const MAX_TITLE_EQUAL_CHARS: usize = 100;

/// The modelled `OpsItemFilterOperator` values, each named where it is read.
const OPERATOR_EQUAL: &str = "Equal";
/// See [`OPERATOR_EQUAL`].
const OPERATOR_CONTAINS: &str = "Contains";
/// See [`OPERATOR_EQUAL`].
const OPERATOR_GREATER_THAN: &str = "GreaterThan";
/// See [`OPERATOR_EQUAL`].
const OPERATOR_LESS_THAN: &str = "LessThan";

/// The `OpsItemFilterKey` values this store answers, each beside the member it names.
const KEY_STATUS: &str = "Status";
/// See [`KEY_STATUS`].
const KEY_CREATED_BY: &str = "CreatedBy";
/// See [`KEY_STATUS`].
const KEY_SOURCE: &str = "Source";
/// See [`KEY_STATUS`].
const KEY_PRIORITY: &str = "Priority";
/// See [`KEY_STATUS`].
const KEY_TITLE: &str = "Title";
/// See [`KEY_STATUS`].
const KEY_OPS_ITEM_ID: &str = "OpsItemId";
/// See [`KEY_STATUS`].
const KEY_CREATED_TIME: &str = "CreatedTime";
/// See [`KEY_STATUS`].
const KEY_LAST_MODIFIED_TIME: &str = "LastModifiedTime";
/// See [`KEY_STATUS`].
const KEY_ACTUAL_START_TIME: &str = "ActualStartTime";
/// See [`KEY_STATUS`].
const KEY_ACTUAL_END_TIME: &str = "ActualEndTime";
/// See [`KEY_STATUS`].
const KEY_PLANNED_START_TIME: &str = "PlannedStartTime";
/// See [`KEY_STATUS`].
const KEY_PLANNED_END_TIME: &str = "PlannedEndTime";
/// See [`KEY_STATUS`].
const KEY_OPERATIONAL_DATA: &str = "OperationalData";
/// See [`KEY_STATUS`].
const KEY_OPERATIONAL_DATA_KEY: &str = "OperationalDataKey";
/// See [`KEY_STATUS`].
const KEY_OPERATIONAL_DATA_VALUE: &str = "OperationalDataValue";
/// See [`KEY_STATUS`].
const KEY_RESOURCE_ID: &str = "ResourceId";
/// See [`KEY_STATUS`].
const KEY_AUTOMATION_ID: &str = "AutomationId";
/// See [`KEY_STATUS`].
const KEY_CATEGORY: &str = "Category";
/// See [`KEY_STATUS`].
const KEY_SEVERITY: &str = "Severity";
/// See [`KEY_STATUS`].
const KEY_OPS_ITEM_TYPE: &str = "OpsItemType";
/// See [`KEY_STATUS`].
const KEY_ACCOUNT_ID: &str = "AccountId";

/// The member of an `OperationalData` filter value naming the operational-data key, and the
/// one naming its value: "specify a key-value pair […] as a JSON string:
/// `{"key":"key_name","value":"a_value"}`" (`DescribeOpsItems`'s `OpsItemFilters` doc).
const DATA_PAIR_KEY_MEMBER: &str = "key";
/// See [`DATA_PAIR_KEY_MEMBER`].
const DATA_PAIR_VALUE_MEMBER: &str = "value";

/// Which operators each filter key accepts, for the keys whose operators are PUBLISHED.
///
/// `DescribeOpsItems` documents the query surface as one list of "Key: … Operations: …"
/// pairs (`smithy_query(query="doc",shape="DescribeOpsItemsRequest",member="OpsItemFilters")`),
/// which is a per-KEY restriction: `OpsItemFilterOperator` declares four operators, and a
/// key admits only the ones its own row lists — `Priority` compares for equality alone,
/// `CreatedTime` orders and never equates, `ResourceId` only contains. The rows below are
/// that list, with the documented "Equals" read as the enumerated type's own spelling
/// ([`OPERATOR_EQUAL`]).
///
/// A key the list does not name has no row and is held to nothing here: the published
/// surface says what each of these keys takes and says nothing about the others (the
/// change-request, access-request and insight keys among them), and a filter is not refused
/// on a restriction no documentation states. The documented `LastModifiedBy` names no value
/// of the modelled `OpsItemFilterKey`, so no row can be reached for it.
const PUBLISHED_KEY_OPERATORS: &[(&str, &[&str])] = &[
    (KEY_CREATED_TIME, &[OPERATOR_GREATER_THAN, OPERATOR_LESS_THAN]),
    (
        KEY_LAST_MODIFIED_TIME,
        &[OPERATOR_GREATER_THAN, OPERATOR_LESS_THAN],
    ),
    (KEY_PRIORITY, &[OPERATOR_EQUAL]),
    (KEY_SOURCE, &[OPERATOR_CONTAINS, OPERATOR_EQUAL]),
    (KEY_STATUS, &[OPERATOR_EQUAL]),
    (KEY_TITLE, &[OPERATOR_EQUAL, OPERATOR_CONTAINS]),
    (KEY_OPERATIONAL_DATA, &[OPERATOR_EQUAL]),
    (KEY_OPERATIONAL_DATA_KEY, &[OPERATOR_EQUAL]),
    (
        KEY_OPERATIONAL_DATA_VALUE,
        &[OPERATOR_EQUAL, OPERATOR_CONTAINS],
    ),
    (KEY_OPS_ITEM_ID, &[OPERATOR_EQUAL]),
    (KEY_RESOURCE_ID, &[OPERATOR_CONTAINS]),
    (KEY_AUTOMATION_ID, &[OPERATOR_EQUAL]),
    (KEY_ACCOUNT_ID, &[OPERATOR_EQUAL]),
];

/// How OpsCenter states a filter it will not query on: one fixed word, naming neither the
/// key nor the operator that was refused.
///
/// The class it answers under is `OpsCenterInvalidArgumentException`
/// ([`crate::scaffolded::Scaffolded::ops_center_invalid_argument`]): the operator is a value
/// its enumerated type declares, so the fault is the PAIR the filter names rather than
/// either value on its own.
pub const INVALID_FILTER: &str = "InvalidFilter";

/// Whether `filter` pairs a key with an operator that key does not accept
/// ([`PUBLISHED_KEY_OPERATORS`]).
fn pairs_an_unaccepted_operator(filter: &OpsItemFilter) -> bool {
    PUBLISHED_KEY_OPERATORS
        .iter()
        .find(|(key, _)| *key == filter.key)
        .is_some_and(|(_, accepted)| !accepted.contains(&filter.operator.as_str()))
}

/// Why `filters` are not filters a query can be run on, or `None` when every one of them
/// is.
///
/// Read BEFORE any OpsItem is selected, so a query carrying such a filter answers the
/// refusal rather than results the live service would never have returned: a client reading
/// a 200 for it would be reading results for a query AWS rejects.
pub fn filter_rejection(filters: &[OpsItemFilter]) -> Option<&'static str> {
    filters
        .iter()
        .any(pairs_an_unaccepted_operator)
        .then_some(INVALID_FILTER)
}

/// What a filter compares one OpsItem against: the value(s) the record holds for the filter's
/// key, under the type the comparison reads them as.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Held {
    /// The record holds nothing this key names, so no comparison can hold.
    Nothing,
    /// Text — one value for a key naming a text member, several for one naming a map's keys
    /// or values.
    Text(Vec<String>),
    /// A number, which a filter value is read as one of to be compared.
    Number(i64),
    /// An instant, which a filter value is read as one of to be compared.
    Instant(SystemTime),
}

/// One text value held for a key.
fn text(value: &str) -> Held {
    Held::Text(vec![value.to_string()])
}

/// The text value held for a key the record may carry nothing for.
fn optional_text(value: Option<&str>) -> Held {
    value.map_or(Held::Nothing, text)
}

/// The instant held for a timestamp member the record may carry nothing for.
fn optional_instant(value: Option<::aws_smithy_types::DateTime>) -> Held {
    value
        .and_then(|at| SystemTime::try_from(at).ok())
        .map_or(Held::Nothing, Held::Instant)
}

/// The text of ONE operational-data value, which is what the keys naming a related resource
/// or a related Automation execution are compared against.
fn data_value(record: &OpsItemRecord, key: &str) -> Held {
    optional_text(
        record
            .operational_data
            .get(key)
            .and_then(|data| data.value.as_deref()),
    )
}

/// What `record` holds for the filter key `key`.
///
/// A key this store can answer names a member of the record; `Held::Nothing` is the answer for
/// every other key the modelled enum declares — the access-request, change-request and insight
/// keys, each of which names a property of a sub-record OpsCenter keeps for those OpsItem kinds
/// and this store holds none of.
fn held_for(record: &OpsItemRecord, key: &str) -> Held {
    match key {
        KEY_STATUS => text(&record.status),
        KEY_CREATED_BY => text(&record.created_by),
        KEY_SOURCE => text(&record.source),
        KEY_TITLE => text(&record.title),
        KEY_OPS_ITEM_ID => text(&record.ops_item_id),
        KEY_OPS_ITEM_TYPE => text(&record.ops_item_type),
        KEY_CATEGORY => optional_text(record.category.as_deref()),
        KEY_SEVERITY => optional_text(record.severity.as_deref()),
        // An OpsItem belongs to the account that holds it, and this emulator serves one.
        KEY_ACCOUNT_ID => text(::engine::emulator_account_id()),
        KEY_PRIORITY => record
            .priority
            .map_or(Held::Nothing, |priority| Held::Number(i64::from(priority))),
        KEY_CREATED_TIME => Held::Instant(record.created_time),
        KEY_LAST_MODIFIED_TIME => Held::Instant(record.last_modified_time),
        KEY_ACTUAL_START_TIME => optional_instant(record.actual_start_time),
        KEY_ACTUAL_END_TIME => optional_instant(record.actual_end_time),
        KEY_PLANNED_START_TIME => optional_instant(record.planned_start_time),
        KEY_PLANNED_END_TIME => optional_instant(record.planned_end_time),
        KEY_OPERATIONAL_DATA_KEY => Held::Text(record.operational_data.keys().cloned().collect()),
        KEY_OPERATIONAL_DATA_VALUE => Held::Text(
            record
                .operational_data
                .values()
                .filter_map(|data| data.value.clone())
                .collect(),
        ),
        KEY_RESOURCE_ID => data_value(record, RELATED_RESOURCES_OPERATIONAL_DATA_KEY),
        KEY_AUTOMATION_ID => data_value(record, RELATED_AUTOMATIONS_OPERATIONAL_DATA_KEY),
        _ => Held::Nothing,
    }
}

/// `value` read as the instant a time key is compared against: this service's ONE reading
/// of a caller-supplied timestamp ([`super::request_instant`]), so a time key here and an
/// instant filter of another report accept the same spellings.
fn instant(value: &str) -> Option<SystemTime> {
    super::request_instant(value)
}

/// The first `characters` characters of `value`, which is what the `Equal` operator compares a
/// `Title` by.
fn first_characters(value: &str, characters: usize) -> String {
    value.chars().take(characters).collect()
}

/// Whether `ordering` is what `operator` asks for, for the two operators that order values.
fn ordered(operator: &str, ordering: Ordering) -> bool {
    match operator {
        OPERATOR_GREATER_THAN => ordering == Ordering::Greater,
        OPERATOR_LESS_THAN => ordering == Ordering::Less,
        _ => ordering == Ordering::Equal,
    }
}

/// Whether the text `held` satisfies `operator` against the filter's `wanted` value.
///
/// `equal_characters` is how many leading characters `Equal` compares, for the one key whose
/// equality is documented as a prefix comparison ([`MAX_TITLE_EQUAL_CHARS`]).
fn text_holds(held: &str, operator: &str, wanted: &str, equal_characters: Option<usize>) -> bool {
    match operator {
        OPERATOR_CONTAINS => held.contains(wanted),
        OPERATOR_EQUAL => match equal_characters {
            Some(characters) => {
                first_characters(held, characters) == first_characters(wanted, characters)
            }
            None => held == wanted,
        },
        _ => ordered(operator, held.cmp(wanted)),
    }
}

/// Whether what the record holds satisfies `operator` against one filter value.
fn holds(held: &Held, operator: &str, wanted: &str, equal_characters: Option<usize>) -> bool {
    match held {
        Held::Nothing => false,
        Held::Text(values) => values
            .iter()
            .any(|value| text_holds(value, operator, wanted, equal_characters)),
        // A filter value that is no number names no priority to compare, and one that is no
        // instant names no time: such a value selects nothing rather than being compared as
        // text, which would order `10` before `9`.
        Held::Number(number) => wanted
            .trim()
            .parse::<i64>()
            .is_ok_and(|wanted| ordered(operator, number.cmp(&wanted))),
        Held::Instant(at) => instant(wanted).is_some_and(|wanted| ordered(operator, at.cmp(&wanted))),
    }
}

/// Whether `record` holds the operational-data PAIR the filter value `wanted` names: the
/// `{"key":…,"value":…}` document an `OperationalData` filter carries. A value that is not one
/// names no pair, so it selects nothing.
fn holds_data_pair(record: &OpsItemRecord, wanted: &str) -> bool {
    let Ok(::serde_json::Value::Object(pair)) = ::serde_json::from_str(wanted) else {
        return false;
    };
    let Some(key) = pair.get(DATA_PAIR_KEY_MEMBER).and_then(|key| key.as_str()) else {
        return false;
    };
    let value = pair
        .get(DATA_PAIR_VALUE_MEMBER)
        .and_then(|value| value.as_str());
    record
        .operational_data
        .get(key)
        .is_some_and(|data| data.value.as_deref() == value)
}

/// Whether `record` satisfies ONE filter: any of the filter's values holds against what the
/// record holds for its key.
///
/// A filter carrying NO value holds against nothing, because every value it offers has to hold
/// and it offers none.
pub fn satisfies(record: &OpsItemRecord, filter: &OpsItemFilter) -> bool {
    if filter.key == KEY_OPERATIONAL_DATA {
        return filter
            .values
            .iter()
            .any(|wanted| holds_data_pair(record, wanted));
    }
    let held = held_for(record, &filter.key);
    let equal_characters = (filter.key == KEY_TITLE).then_some(MAX_TITLE_EQUAL_CHARS);
    filter
        .values
        .iter()
        .any(|wanted| holds(&held, &filter.operator, wanted, equal_characters))
}

impl ComAmazonawsSsmAmazonssmState {
    /// The OpsItems of `region` that satisfy every one of `filters`, most recently created
    /// first.
    ///
    /// Newest first is how a queue of operational work is read, and two OpsItems created in the
    /// same instant are ordered by their ids — which are unique within the store — so the order
    /// is TOTAL and the id a page resumes at names one position in it.
    pub fn queried_ops_items(&self, region: &str, filters: &[OpsItemFilter]) -> Vec<&OpsItemRecord> {
        let mut selected: Vec<&OpsItemRecord> = self
            .ops_items
            .values()
            .filter(|held| held.region == region)
            .filter(|held| filters.iter().all(|filter| satisfies(held, filter)))
            .collect();
        selected.sort_by(|left, right| {
            right
                .created_time
                .cmp(&left.created_time)
                .then_with(|| left.ops_item_id.cmp(&right.ops_item_id))
        });
        selected
    }
}
