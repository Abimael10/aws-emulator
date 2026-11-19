// SPDX-License-Identifier: CC0-1.0

//! The refusal of an `AttributeValue` a request sends with NO datatype — or with several —
//! stated behind the member it stands in.
//!
//! What is wrong with such a value is one fact, worded once
//! ([`crate::ops::types::EMPTY_ATTRIBUTE_VALUE_MESSAGE`] and its ambiguous twin), and most
//! members state that fact alone. A member whose value is read as ONE THING rather than as a
//! value in its own right states it behind the sentence for that thing — a resume token that
//! carries an unreadable value is the invalid token it is
//! ([`crate::state::START_KEY_INVALID_MESSAGE`]), with the datatype fact as the reason — and a
//! member that is a TABLE of entries states it as that table carrying an invalid value under
//! the key the value sits at.
//!
//! It is read HERE, off the request body, because a value the codec cannot read never becomes
//! an input: the request is refused while it is being deserialized, before any handler could
//! say which member the value stood in — the codec's error carries the fact and no path
//! (`vendor/smithy-schema-server`'s `within_map_entry` re-roots an absent MEMBER and leaves
//! every other failure as it stands). So the body is read ahead of the dispatch that would
//! refuse it, in the one scope every request this service serves already passes through
//! ([`super::OperationScopedClauseForm`]), and only for the members that state an envelope of
//! their own: every other value keeps the codec's own refusal, unchanged.
//!
//! Nothing about the model distinguishes the two groups — the same union, the same defect, a
//! different sentence — so [`ENVELOPED_VALUE_MEMBERS`] is the only place the difference is
//! recorded, and a member later found to state one needs its own entry there.

use aws_smithy_runtime_api::http::Request as RtRequest;
use serde_json::{Map, Value};

use crate::ops::types::{AMBIGUOUS_ATTRIBUTE_VALUE_MESSAGE, EMPTY_ATTRIBUTE_VALUE_MESSAGE};

/// The datatypes one `AttributeValue` may carry, as the model spells them on the wire —
/// exactly the ten `smithy_query(query="shape", shape="com.amazonaws.dynamodb#AttributeValue")`
/// declares (`S N B SS NS BS M L NULL BOOL`), which is the set the reader of that union
/// recognises (`crate::ops::types::AttributeValue`). A member name outside it is not a
/// datatype at all and is skipped by that reader, so it is skipped here too.
const DATATYPES: [&str; 10] = ["S", "N", "B", "SS", "NS", "BS", "M", "L", "NULL", "BOOL"];

/// The two datatypes a value NESTS through, which is why the walk below recurses: the values
/// inside them are `AttributeValue`s read by the same reader, so a defect inside one is the
/// same defect.
const NESTED_MAP: &str = "M";
const NESTED_LIST: &str = "L";

/// The members whose values this service states a datatype defect BEHIND a sentence of their
/// own, each with the FORM it states it in.
///
/// A resume token is read as a whole — a caller gets it from the previous page's
/// `LastEvaluatedKey` rather than composing it — so a token this service cannot read a value
/// out of is the invalid token it is ([`Stated::Behind`]), with the datatype fact as the
/// reason and no entry of it named. A substitution table is a table of entries a caller wrote
/// one at a time, so a value it cannot substitute is reported as that table carrying an
/// invalid value under the KEY it sits at ([`Stated::UnderKey`]) — the same wording the
/// NAMES table's own invalid entry is reported under
/// (`crate::state`'s `check_tables_not_empty`).
const ENVELOPED_VALUE_MEMBERS: &[(&str, Stated)] = &[
    (
        "ExclusiveStartKey",
        Stated::Behind(crate::state::START_KEY_INVALID_MESSAGE),
    ),
    ("ExpressionAttributeValues", Stated::UnderKey),
];

/// How one member states a datatype defect of a value it carries.
#[derive(Debug, Clone, Copy)]
enum Stated {
    /// Behind a sentence about that member's value as a WHOLE, which names no entry of it.
    Behind(&'static str),
    /// As the member carrying an invalid value, named by the KEY the value sits under.
    UnderKey,
}

impl Stated {
    /// The whole sentence for `defect`, found under `key` of `member`.
    ///
    /// The key is the CALLER's text, so it is echoed held to the ceiling every clause of this
    /// service quotes a rejected value at ([`crate::state::echoed_clause_value`]): this layer
    /// answers before the handler that holds a substitution key to its own length, so the
    /// sentence cannot be sized by the request.
    fn sentence(self, member: &str, key: &str, defect: &str) -> String {
        match self {
            Self::Behind(stated) => format!("{stated}: {defect}"),
            Self::UnderKey => {
                let echoed = crate::state::echoed_clause_value(key);
                format!("{member} contains invalid value: {defect} for key {echoed}")
            }
        }
    }
}

/// The operations whose input declares one of the members above AT THE REQUEST LEVEL, sorted.
///
/// The gate on reading the body at all: a request of any other operation carries no such
/// member, so no walk over its body could answer anything, and the body of an item write or a
/// batch — the largest this service serves — is never parsed a second time for this layer.
/// Read off the model: `ExclusiveStartKey` is declared by the two table walks
/// (`smithy_query(query="operation", operation="Query,Scan")`) and `ExpressionAttributeValues`
/// by those two plus the three single-item writes
/// (`smithy_query(query="operation", operation="PutItem,UpdateItem,DeleteItem")`). The same
/// members nested inside a `TransactItems` or `RequestItems` ELEMENT belong to that element
/// rather than to the request and keep the codec's own refusal.
const MEMBER_BEARING_OPERATIONS: [&str; 5] =
    ["DeleteItem", "PutItem", "Query", "Scan", "UpdateItem"];

/// The sentence refusing `req`'s body for an `AttributeValue` with no datatype, or with
/// several, standing under one of the members above — or `None` when no such member carries
/// one, which is every request of every operation that carries none of them.
///
/// The answer is the first defect found, over the members in the order above and the
/// attributes of one member in the order the body's reader reports them, so one body earns one
/// sentence rather than a sentence that depends on the order a map happened to be walked in.
pub fn enveloped_refusal(operation: &str, req: &RtRequest) -> Option<String> {
    if MEMBER_BEARING_OPERATIONS.binary_search(&operation).is_err() {
        return None;
    }
    let body = req.body().bytes()?;
    let document = ::serde_json::from_slice::<Value>(body).ok()?;
    let members = document.as_object()?;
    ENVELOPED_VALUE_MEMBERS.iter().find_map(|(member, stated)| {
        let carried = members.get(*member)?.as_object()?;
        let (key, defect) = first_defect(carried)?;
        Some(stated.sentence(member, key, defect))
    })
}

/// The first datatype defect among the attribute values of one map member, as the KEY it was
/// found under and the fact about it — or `None` when every value under it carries exactly one
/// datatype.
///
/// The key is the member's OWN entry, whichever level of that entry's value the defect sits
/// at: what a caller fixes is the entry it wrote.
fn first_defect(carried: &Map<String, Value>) -> Option<(&str, &'static str)> {
    carried
        .iter()
        .find_map(|(key, value)| Some((key.as_str(), defect_in(value)?)))
}

/// The datatype defect of ONE value, looked for in the value itself and then inside the
/// containers it nests through.
///
/// Recursive, and bounded by the JSON reader above rather than by the request: a document
/// nesting deeper than that reader descends does not parse at all, so what this walk is handed
/// is already no deeper than that reader's own limit.
///
/// A value that is not a JSON object carries no datatype the reader could count: its class is
/// what is wrong with it, which is the marshalling failure another layer answers
/// ([`super::request_layer`]), so nothing is reported for it here.
fn defect_in(value: &Value) -> Option<&'static str> {
    let stated = value.as_object()?;
    match DATATYPES
        .iter()
        .filter(|datatype| stated.contains_key(**datatype))
        .count()
    {
        0 => return Some(EMPTY_ATTRIBUTE_VALUE_MESSAGE),
        1 => {}
        _ => return Some(AMBIGUOUS_ATTRIBUTE_VALUE_MESSAGE),
    }
    if let Some(entries) = stated.get(NESTED_MAP).and_then(Value::as_object) {
        return entries.values().find_map(defect_in);
    }
    stated
        .get(NESTED_LIST)
        .and_then(Value::as_array)?
        .iter()
        .find_map(defect_in)
}
