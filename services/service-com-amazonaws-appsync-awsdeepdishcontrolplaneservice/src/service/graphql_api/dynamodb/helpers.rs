// SPDX-License-Identifier: CC0-1.0

//! The request objects the DynamoDB helper module of the APPSYNC_JS runtime BUILDS: one helper
//! per call an `AMAZON_DYNAMODB` data source serves, each turning the payload a resolver's code
//! handed it into the request object that data source is called with ([`super::answered`]).
//!
//! A resolver written for this runtime does not have to write that request object out: AppSync
//! publishes a module of helpers for it — `get`, `put`, `remove`, `update`, `query`, `scan` and
//! `sync`, beside the `operations` an update states its changes with — and a resolver imports
//! them and returns what they build
//! ([`crate::ops::evaluate_code::appsync_js`] declares the module they are reached through).
//! Two things make a helper's answer the request the data source then makes:
//!
//! * every value the payload carries is a PLAIN value — the key of an item, the item itself,
//!   the operands of a condition — and the request object carries DynamoDB's own attribute
//!   values, so each is marshalled through the service's one reading of that conversion
//!   (`crate::service::graphql_api::mapping_template::utilities::dynamodb`): a value this
//!   module marshals and the same value marshalled by a Velocity template carry one
//!   representation;
//! * a condition, a key condition, a filter, a projection and an update are stated as OBJECTS
//!   and travel as EXPRESSIONS, so each is compiled to the expression text and the two
//!   parameter maps the call carries ([`expressions`]).
//!
//! Nothing here reads or writes any state: a helper is a pure transformation of the payload it
//! was handed, and what the built request then DOES is the data source's own ([`super`]).
//! A payload no request can be built from is refused by the member a client has to correct,
//! which the runtime raises in the resolver's own code.

use ::serde_json::{Map as JsonMap, Value as Json};

use crate::service::graphql_api::mapping_template::utilities::dynamodb::attribute_value;
use crate::service::graphql_api::mapping_template::weigh;

use super::versioned::VERSION_ATTRIBUTE;
use super::{
    ATTRIBUTE_VALUES_MEMBER, A_COUNT_IS_A_NUMBER, A_FLAG_IS_A_BOOLEAN, A_MEMBER_IS_A_MAP,
    CONDITION_MEMBER, CONSISTENT_READ_MEMBER, FILTER_MEMBER, INDEX_MEMBER, KEY_MEMBER,
    LIMIT_MEMBER, NEXT_TOKEN_MEMBER, OPERATION_MEMBER, PROJECTION_MEMBER, QUERY_MEMBER,
    SCAN_INDEX_FORWARD_MEMBER, SEGMENT_MEMBER, SELECT_MEMBER, TOTAL_SEGMENTS_MEMBER,
    UPDATE_MEMBER,
};

mod expressions;

/// What one helper of the module built, or `None` when `helper` names no helper of it — which
/// the caller reports as the fault naming it.
pub fn call(helper: &str, args: &[Json]) -> Result<Option<Json>, String> {
    let payload = args.first().unwrap_or(&Json::Null);
    let built = match helper {
        GET_HELPER => get(payload)?,
        PUT_HELPER => put(payload)?,
        REMOVE_HELPER => remove(payload)?,
        UPDATE_HELPER => update(payload)?,
        QUERY_HELPER => query(payload)?,
        SCAN_HELPER => scan(payload)?,
        SYNC_HELPER => sync(payload)?,
        _ => return Ok(None),
    };
    Ok(Some(built))
}

/// `get` — the `GetItem` request for the key the payload addresses, read consistently where it
/// asks for that and projected onto the attributes it names where it names any.
fn get(payload: &Json) -> Result<Json, String> {
    let mut request = addressing(GET_ITEM, payload)?;
    carry_flag(&mut request, payload, CONSISTENT_READ_MEMBER)?;
    carry_expression(&mut request, payload, PROJECTION_MEMBER, expressions::projection)?;
    Ok(Json::Object(request))
}

/// `put` — the `PutItem` request writing the item the payload carries at the key it addresses,
/// held to the condition it states.
fn put(payload: &Json) -> Result<Json, String> {
    let mut request = addressing(PUT_ITEM, payload)?;
    let item = match stated(payload, ITEM_MEMBER) {
        None => JsonMap::new(),
        Some(Json::Object(stated)) => stated.clone(),
        Some(_) => return Err(format!("{A_MEMBER_IS_A_MAP} {ITEM_MEMBER}.")),
    };
    request.insert(ATTRIBUTE_VALUES_MEMBER.to_string(), marshalled(&item)?);
    carry_expression(&mut request, payload, CONDITION_MEMBER, expressions::predicate)?;
    carry_write_members(&mut request, payload)?;
    Ok(Json::Object(request))
}

/// `remove` — the `DeleteItem` request removing the item at the key the payload addresses, held
/// to the condition it states.
fn remove(payload: &Json) -> Result<Json, String> {
    let mut request = addressing(DELETE_ITEM, payload)?;
    carry_expression(&mut request, payload, CONDITION_MEMBER, expressions::predicate)?;
    carry_write_members(&mut request, payload)?;
    Ok(Json::Object(request))
}

/// `update` — the `UpdateItem` request making the changes the payload's `update` states to the
/// item at the key it addresses, held to the condition it states.
fn update(payload: &Json) -> Result<Json, String> {
    let mut request = addressing(UPDATE_ITEM, payload)?;
    let Some(stated_update) = stated(payload, UPDATE_MEMBER) else {
        return Err(format!("{AN_UPDATE_STATES} {UPDATE_MEMBER}."));
    };
    request.insert(
        UPDATE_MEMBER.to_string(),
        expressions::update(stated_update, UPDATE_MEMBER)?,
    );
    carry_expression(&mut request, payload, CONDITION_MEMBER, expressions::predicate)?;
    carry_write_members(&mut request, payload)?;
    Ok(Json::Object(request))
}

/// `query` — the `Query` request selecting by the key condition the payload states, over the
/// page it asks for.
fn query(payload: &Json) -> Result<Json, String> {
    let mut request = request_of(QUERY);
    let Some(stated_query) = stated(payload, QUERY_MEMBER) else {
        return Err(format!("{A_QUERY_STATES} {QUERY_MEMBER}."));
    };
    request.insert(
        QUERY_MEMBER.to_string(),
        expressions::predicate(stated_query, QUERY_MEMBER)?,
    );
    carry_flag(&mut request, payload, SCAN_INDEX_FORWARD_MEMBER)?;
    carry_selection(&mut request, payload)?;
    Ok(Json::Object(request))
}

/// `scan` — the `Scan` request reading the page the payload asks for, of the segment it names
/// where it names one.
fn scan(payload: &Json) -> Result<Json, String> {
    let mut request = request_of(SCAN);
    carry_count(&mut request, payload, SEGMENT_MEMBER)?;
    carry_count(&mut request, payload, TOTAL_SEGMENTS_MEMBER)?;
    carry_selection(&mut request, payload)?;
    Ok(Json::Object(request))
}

/// `sync` — the `Sync` request reading what changed since the point in time the payload states,
/// which is the call a resolver over a data source configured for delta sync makes.
fn sync(payload: &Json) -> Result<Json, String> {
    let mut request = request_of(SYNC);
    carry_text(&mut request, payload, BASE_PARTITION_KEY_MEMBER)?;
    carry_text(&mut request, payload, DELTA_INDEX_NAME_MEMBER)?;
    carry_count(&mut request, payload, LIMIT_MEMBER)?;
    carry_count(&mut request, payload, LAST_SYNC_MEMBER)?;
    carry_text(&mut request, payload, NEXT_TOKEN_MEMBER)?;
    carry_expression(&mut request, payload, FILTER_MEMBER, expressions::predicate)?;
    Ok(Json::Object(request))
}

/// The filter expression a filter stated as an OBJECT comes to — `{expression,
/// expressionNames, expressionValues}`, the three members the `filter` of a selection carries.
///
/// The ONE translation of that filter language, which a VTL mapping template reaches through
/// the transform helper AppSync publishes for exactly this
/// (`crate::service::graphql_api::mapping_template::utilities`) and a resolver written for the
/// APPSYNC_JS runtime through the module above: a filter stated in a template and the same
/// filter stated in code therefore select on the same expression, parameterised the same way.
pub fn filter_expression(stated: &Json) -> Result<Json, String> {
    expressions::predicate(stated, FILTER_MEMBER)
}

/// The same filter expression, written in the form the `$util.transform` helper AppSync
/// publishes answers it in: the attribute and the operator name each placeholder and every
/// comparison is parenthesised, which is what a mapping template reading that answer sees.
///
/// The same ONE translation as [`filter_expression`] — one reading of the language, two forms
/// of the document it is written into ([`expressions::published_predicate`]) — reached by the
/// transformation helper of the template language
/// (`crate::service::graphql_api::mapping_template::transform`).
pub fn published_filter_expression(stated: &Json) -> Result<Json, String> {
    expressions::published_predicate(stated, FILTER_MEMBER)
}

/// The members every SELECTION of a page states: what it filters by, which index it reads, how
/// many values, whether the read is consistent, what it selects, which attributes it projects
/// and where the previous page left off.
fn carry_selection(request: &mut JsonMap<String, Json>, payload: &Json) -> Result<(), String> {
    carry_expression(request, payload, FILTER_MEMBER, expressions::predicate)?;
    carry_text(request, payload, INDEX_MEMBER)?;
    carry_count(request, payload, LIMIT_MEMBER)?;
    carry_flag(request, payload, CONSISTENT_READ_MEMBER)?;
    carry_text(request, payload, SELECT_MEMBER)?;
    carry_text(request, payload, NEXT_TOKEN_MEMBER)?;
    carry_expression(request, payload, PROJECTION_MEMBER, expressions::projection)
}

/// The members a WRITE states beside the change it makes: the version it expects of the item it
/// is changing, and the two members a data source configured for delta sync reads — each
/// carried as the payload stated it, because what they then mean is the data source's own
/// ([`super::versioned`]).
fn carry_write_members(
    request: &mut JsonMap<String, Json>,
    payload: &Json,
) -> Result<(), String> {
    carry_count(request, payload, VERSION_ATTRIBUTE)?;
    carry_text(request, payload, CUSTOM_PARTITION_KEY_MEMBER)?;
    carry_flag(request, payload, POPULATE_INDEX_FIELDS_MEMBER)?;
    Ok(())
}

/// The request object of an operation addressing ONE item: its `operation` and the key it
/// addresses, which every such payload states.
fn addressing(operation: &str, payload: &Json) -> Result<JsonMap<String, Json>, String> {
    let mut request = request_of(operation);
    let key = match stated(payload, KEY_MEMBER) {
        Some(Json::Object(stated)) if !stated.is_empty() => stated,
        _ => return Err(format!("{AN_OPERATION_STATES} {KEY_MEMBER}.")),
    };
    request.insert(KEY_MEMBER.to_string(), marshalled(key)?);
    Ok(request)
}

/// The request object of `operation`, with nothing but the operation it names.
fn request_of(operation: &str) -> JsonMap<String, Json> {
    let mut request = JsonMap::new();
    request.insert(
        OPERATION_MEMBER.to_string(),
        Json::String(operation.to_string()),
    );
    request
}

/// What `member` of the payload states, or `None` for a member it does not state at all — a
/// member stated as `null` among them, because a payload written from a context that carried no
/// such value states it that way.
fn stated<'a>(payload: &'a Json, member: &str) -> Option<&'a Json> {
    payload.get(member).filter(|held| !held.is_null())
}

/// Every value of `values` as the attribute value DynamoDB holds it as, which is one map of
/// attribute values — a key, or the item a write carries.
fn marshalled(values: &JsonMap<String, Json>) -> Result<Json, String> {
    let mut marshalled = JsonMap::new();
    for (name, value) in values {
        // The marshalled copy nests as deeply as its source and holds more than it, so a value
        // past either memory bound of a run is refused here rather than converted.
        weigh(value, 0)?;
        marshalled.insert(name.clone(), attribute_value(value));
    }
    Ok(Json::Object(marshalled))
}

/// Carry the expression `member` of the payload states, built by `build`, where it states one.
fn carry_expression(
    request: &mut JsonMap<String, Json>,
    payload: &Json,
    member: &str,
    build: fn(&Json, &str) -> Result<Json, String>,
) -> Result<(), String> {
    let Some(stated) = stated(payload, member) else {
        return Ok(());
    };
    request.insert(member.to_string(), build(stated, member)?);
    Ok(())
}

/// Carry the boolean `member` of the payload states where it states one; a member of another
/// shape is no flag, so the payload is refused by that member's name rather than built into a
/// request that asks something else.
fn carry_flag(
    request: &mut JsonMap<String, Json>,
    payload: &Json,
    member: &str,
) -> Result<(), String> {
    match stated(payload, member) {
        None => Ok(()),
        Some(Json::Bool(decided)) => {
            request.insert(member.to_string(), Json::Bool(*decided));
            Ok(())
        }
        Some(_) => Err(format!("{A_FLAG_IS_A_BOOLEAN} {member}.")),
    }
}

/// Carry the numeric `member` of the payload states where it states one, under the same
/// reading.
fn carry_count(
    request: &mut JsonMap<String, Json>,
    payload: &Json,
    member: &str,
) -> Result<(), String> {
    match stated(payload, member) {
        None => Ok(()),
        Some(held) => {
            let count = held
                .as_i64()
                .ok_or_else(|| format!("{A_COUNT_IS_A_NUMBER} {member}."))?;
            request.insert(member.to_string(), ::serde_json::json!(count));
            Ok(())
        }
    }
}

/// Carry the textual `member` of the payload states where it states one, under the same
/// reading.
fn carry_text(
    request: &mut JsonMap<String, Json>,
    payload: &Json,
    member: &str,
) -> Result<(), String> {
    match stated(payload, member) {
        None => Ok(()),
        Some(Json::String(text)) => {
            request.insert(member.to_string(), Json::String(text.clone()));
            Ok(())
        }
        Some(_) => Err(format!("{A_MEMBER_IS_TEXT} {member}.")),
    }
}

/// Which update operation `assigned` STATES and the arguments it was called with, or `None` for
/// a value that is no operation of this module at all — which an update assigns as the new
/// value of its attribute.
///
/// An operation is a value the module's own `operations.*` built ([`OPERATION_MARKER`]), so an
/// object of the resolver's own that happens to carry attributes of its own is assigned rather
/// than read as a change.
fn operation_stated(assigned: &Json) -> Option<(&str, Vec<Json>)> {
    let named = assigned.get(OPERATION_MARKER)?.as_str()?;
    let arguments = assigned
        .get(OPERATION_ARGUMENTS)
        .and_then(Json::as_array)
        .cloned()
        .unwrap_or_default();
    Some((named, arguments))
}

/// The member one of the module's `operations.*` values STATES the operation it performs in,
/// and the member carrying the arguments that operation was called with.
///
/// The values are built by the module's own source
/// (`crate::ops::evaluate_code::appsync_js`), which is the only thing that writes them: they
/// are read back here, and an update is what reads them ([`expressions::update`]).
pub const OPERATION_MARKER: &str = "__appsyncDdbOperation";
/// See [`OPERATION_MARKER`].
pub const OPERATION_ARGUMENTS: &str = "arguments";

/// The helpers this module publishes, by the names it exports them under.
pub const HELPERS: &[&str] = &[
    GET_HELPER,
    PUT_HELPER,
    REMOVE_HELPER,
    UPDATE_HELPER,
    QUERY_HELPER,
    SCAN_HELPER,
    SYNC_HELPER,
];

const GET_HELPER: &str = "get";
const PUT_HELPER: &str = "put";
const REMOVE_HELPER: &str = "remove";
const UPDATE_HELPER: &str = "update";
const QUERY_HELPER: &str = "query";
const SCAN_HELPER: &str = "scan";
const SYNC_HELPER: &str = "sync";

/// The `operation` each helper's request object names, which is what the data source serves the
/// call as ([`super::Call`]).
const GET_ITEM: &str = "GetItem";
const PUT_ITEM: &str = "PutItem";
const UPDATE_ITEM: &str = "UpdateItem";
const DELETE_ITEM: &str = "DeleteItem";
const QUERY: &str = "Query";
const SCAN: &str = "Scan";
const SYNC: &str = "Sync";

/// The members of a payload that are not members of the request object the data source already
/// reads under the same name: the item a write carries, and the four a delta-sync request and a
/// write through such a data source state.
const ITEM_MEMBER: &str = "item";
const BASE_PARTITION_KEY_MEMBER: &str = "basePartitionKey";
const DELTA_INDEX_NAME_MEMBER: &str = "deltaIndexName";
const LAST_SYNC_MEMBER: &str = "lastSync";
const CUSTOM_PARTITION_KEY_MEMBER: &str = "customPartitionKey";
const POPULATE_INDEX_FIELDS_MEMBER: &str = "populateIndexFields";

/// The sentences a payload no request can be built from is refused with, written as every other
/// refusal of this data source is: each names the rule the payload broke and ends in the member
/// a client has to correct.
const AN_OPERATION_STATES: &str =
    "This DynamoDB helper addresses one item, and the key of that item is not stated in";
const AN_UPDATE_STATES: &str = "The update helper states the changes it makes in";
const A_QUERY_STATES: &str = "The query helper states the key condition it selects by in";
const A_MEMBER_IS_TEXT: &str =
    "This member of a DynamoDB helper's payload is text, which is not what is carried at";

#[cfg(test)]
mod tests {
    use ::serde_json::json;

    use super::*;

    /// RULE: each helper builds the request object of ONE operation, with every plain value the
    /// payload carried marshalled as the attribute value DynamoDB holds it as.
    #[test]
    fn each_helper_builds_the_request_object_of_its_operation() {
        // The rows differ along the helper, along the TYPES the payload's values carry, and
        // along which optional members it states.
        let rows: &[(&str, &str, Json, Json)] = &[
            (
                "a get of one key",
                "get",
                json!({"key": {"id": "N-1"}}),
                json!({"operation": "GetItem", "key": {"id": {"S": "N-1"}}}),
            ),
            (
                "a consistent get projecting two attributes",
                "get",
                json!({
                    "key": {"id": "N-1"},
                    "consistentRead": true,
                    "projection": ["id", "board"],
                }),
                json!({
                    "operation": "GetItem",
                    "key": {"id": {"S": "N-1"}},
                    "consistentRead": true,
                    "projection": {
                        "expression": "#n0, #n1",
                        "expressionNames": {"#n0": "id", "#n1": "board"},
                    },
                }),
            ),
            (
                "a put of an item carrying every scalar type",
                "put",
                json!({
                    "key": {"id": "N-1"},
                    "item": {
                        "board": "ideas",
                        "votes": 2,
                        "pinned": true,
                        "tags": ["red", "blue"],
                        "meta": {"owner": "ann"},
                        "author": null,
                    },
                }),
                json!({
                    "operation": "PutItem",
                    "key": {"id": {"S": "N-1"}},
                    "attributeValues": {
                        "board": {"S": "ideas"},
                        "votes": {"N": 2},
                        "pinned": {"BOOL": true},
                        "tags": {"L": [{"S": "red"}, {"S": "blue"}]},
                        "meta": {"M": {"owner": {"S": "ann"}}},
                        "author": {"NULL": null},
                    },
                }),
            ),
            (
                "a conditional put stating the version it expects",
                "put",
                json!({
                    "key": {"id": "N-1"},
                    "item": {"board": "ideas"},
                    "condition": {"id": {"attributeExists": false}},
                    "_version": 3,
                }),
                json!({
                    "operation": "PutItem",
                    "key": {"id": {"S": "N-1"}},
                    "attributeValues": {"board": {"S": "ideas"}},
                    "condition": {
                        "expression": "attribute_not_exists(#n0)",
                        "expressionNames": {"#n0": "id"},
                    },
                    "_version": 3,
                }),
            ),
            (
                "a remove of one key",
                "remove",
                json!({"key": {"id": "N-1"}}),
                json!({"operation": "DeleteItem", "key": {"id": {"S": "N-1"}}}),
            ),
            (
                "an update setting one attribute",
                "update",
                json!({"key": {"id": "N-1"}, "update": {"board": "todo"}}),
                json!({
                    "operation": "UpdateItem",
                    "key": {"id": {"S": "N-1"}},
                    "update": {
                        "expression": "SET #n0 = :v0",
                        "expressionNames": {"#n0": "board"},
                        "expressionValues": {":v0": {"S": "todo"}},
                    },
                }),
            ),
            (
                "a query of one index, newest first",
                "query",
                json!({
                    "query": {"board": {"eq": "ideas"}},
                    "index": "by-board",
                    "limit": 10,
                    "scanIndexForward": false,
                    "nextToken": "a-token",
                }),
                json!({
                    "operation": "Query",
                    "query": {
                        "expression": "#n0 = :v0",
                        "expressionNames": {"#n0": "board"},
                        "expressionValues": {":v0": {"S": "ideas"}},
                    },
                    "index": "by-board",
                    "limit": 10,
                    "scanIndexForward": false,
                    "nextToken": "a-token",
                }),
            ),
            (
                "a scan of one segment, filtered",
                "scan",
                json!({
                    "filter": {"board": {"eq": "ideas"}},
                    "segment": 0,
                    "totalSegments": 2,
                    "select": "ALL_ATTRIBUTES",
                }),
                json!({
                    "operation": "Scan",
                    "filter": {
                        "expression": "#n0 = :v0",
                        "expressionNames": {"#n0": "board"},
                        "expressionValues": {":v0": {"S": "ideas"}},
                    },
                    "segment": 0,
                    "totalSegments": 2,
                    "select": "ALL_ATTRIBUTES",
                }),
            ),
            (
                "a sync since a point in time",
                "sync",
                json!({"basePartitionKey": "Note", "deltaIndexName": "deltas", "lastSync": 1}),
                json!({
                    "operation": "Sync",
                    "basePartitionKey": "Note",
                    "deltaIndexName": "deltas",
                    "lastSync": 1,
                }),
            ),
        ];
        for (case, helper, payload, expected) in rows {
            let actual = call(helper, ::std::slice::from_ref(payload));
            assert_eq!(
                actual.as_ref(),
                Ok(&Some(expected.clone())),
                "case {case:?}: expected {expected}, got {actual:?}"
            );
        }
    }

    /// RULE: a payload no request can be built from is refused by the member to correct, and a
    /// name the module publishes no helper under is answered as no such helper.
    #[test]
    fn a_payload_no_request_can_be_built_from_is_refused() {
        let rows: &[(&str, &str, Json)] = &[
            ("a get addressing no key", "get", json!({})),
            ("a get whose key is empty", "get", json!({"key": {}})),
            ("a get whose key is not a map", "get", json!({"key": "N-1"})),
            (
                "a put whose item is not a map",
                "put",
                json!({"key": {"id": "N-1"}, "item": "ideas"}),
            ),
            (
                "an update stating no change",
                "update",
                json!({"key": {"id": "N-1"}}),
            ),
            (
                "an update naming an operation of no such kind",
                "update",
                json!({
                    "key": {"id": "N-1"},
                    "update": {"votes": {"__appsyncDdbOperation": "multiply", "arguments": [2]}},
                }),
            ),
            ("a query stating no key condition", "query", json!({})),
            (
                "a read whose consistency is not a flag",
                "get",
                json!({"key": {"id": "N-1"}, "consistentRead": "yes"}),
            ),
            (
                "a selection whose limit is not a number",
                "scan",
                json!({"limit": "ten"}),
            ),
            (
                "a selection whose index is not a name",
                "scan",
                json!({"index": 3}),
            ),
        ];
        for (case, helper, payload) in rows {
            let actual = call(helper, ::std::slice::from_ref(payload));
            assert!(
                actual.is_err(),
                "case {case:?}: expected the payload to be refused, got {actual:?}"
            );
        }
        let unknown = call("transact", &[json!({})]);
        assert_eq!(
            unknown,
            Ok(None),
            "case a name the module publishes no helper under: expected no such helper, got \
             {unknown:?}"
        );
    }

    /// RULE: a member the payload does not state, and one it states as `null`, is a member the
    /// built request does not carry — what the data source then does about it is its own
    /// default rather than a value this module invented.
    #[test]
    fn a_member_the_payload_does_not_state_is_not_carried() {
        let built = call(
            "get",
            &[json!({
                "key": {"id": "N-1"},
                "consistentRead": null,
                "projection": null,
            })],
        )
        .expect("the payload is one a request is built from")
        .expect("get is a helper of the module");
        assert_eq!(
            built,
            json!({"operation": "GetItem", "key": {"id": {"S": "N-1"}}}),
            "case a get stating its optional members as null: got {built}"
        );
    }
}
