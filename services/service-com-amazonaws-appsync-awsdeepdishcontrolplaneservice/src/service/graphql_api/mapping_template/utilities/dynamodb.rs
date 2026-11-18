// SPDX-License-Identifier: CC0-1.0

//! The `$util.dynamodb` helpers a mapping template MARSHALS a value with — the DynamoDB
//! helpers of the AppSync resolver mapping-template utility reference, "used to simplify
//! creating DynamoDB request objects and handling their results".
//!
//! One rule covers all of them: each helper renders a value as the ATTRIBUTE VALUE DynamoDB
//! holds it as — a one-member object whose name is the attribute type (`S`, `N`, `B`, `BOOL`,
//! `NULL`, `L`, `M`, `SS`, `NS`, `BS`) and whose value is the marshalled content — with one
//! typed helper per attribute type and a general converter ([`attribute_value`]) that picks
//! the type from the value it is handed and marshals a list and a map through itself. Every
//! one of them has a `…Json` sibling that answers the SAME representation already serialized,
//! which is what lets a template inline it into the request object it is writing
//! (`"key": {"id": $util.dynamodb.toDynamoDBJson($ctx.args.id)}`), because a string is
//! written into the rendered text as itself ([`super::super::values::rendered`]).
//!
//! Two of them are not conversions of one value: `toS3Object` composes the S3 object
//! reference DynamoDB holds as text under an `S` attribute, and `fromS3ObjectJson` reads one
//! back — the one helper whose name ends in `Json` without being another's serialized
//! sibling.
//!
//! The value a helper is handed is WEIGHED before it is walked, by the evaluation's own
//! reading of what a value may be ([`super::super::weigh`]): the marshalled copy nests as
//! deeply as its source and holds more than it, so a value past either memory bound of a run
//! is refused here rather than converted. A name none of these helpers carries is answered as
//! no such helper, as every other unimplemented helper of the language is.

use ::serde_json::{Map as JsonMap, Value as Json};

use super::values;

/// What one `$util.dynamodb.<helper>` call came to, or `None` when `helper` names no helper of
/// this family — which the caller reports as the fault naming it.
pub fn call(helper: &str, args: &[Json]) -> Result<Option<Json>, String> {
    // `fromS3ObjectJson` READS a representation rather than writing one, so its name ends in
    // `Json` without being the serialized sibling of a `fromS3Object` helper: it is answered
    // before the suffix is read as that sibling.
    if helper == FROM_S3_OBJECT {
        return from_s3_object(args.first()).map(Some);
    }
    let (named, serialized) = helper
        .strip_suffix(JSON_SUFFIX)
        .map_or((helper, false), |named| (named, true));
    let Some(marshalled) = marshalled(named, args)? else {
        return Ok(None);
    };
    // The `…Json` sibling is the same representation, serialized — "the same as … but
    // returns the DynamoDB attribute value as a JSON-encoded string".
    Ok(Some(if serialized {
        Json::String(marshalled.to_string())
    } else {
        marshalled
    }))
}

/// The attribute value `named` renders `args` as, or `None` for a name this family does not
/// carry.
fn marshalled(named: &str, args: &[Json]) -> Result<Option<Json>, String> {
    let argument = |index: usize| args.get(index).cloned().unwrap_or(Json::Null);
    // Every value a helper walks is weighed first, so what it allocates is bounded by the
    // same two ceilings the rest of an evaluation is.
    super::super::weigh(&argument(0), 0)?;
    let marshalled = match named {
        // "A general object conversion tool": the type comes from the value.
        "toDynamoDB" => attribute_value(&argument(0)),
        // The scalars.
        "toString" => typed(STRING, Json::String(text(&argument(0)))),
        "toNumber" => typed(NUMBER, number(&argument(0))?),
        "toBinary" => typed(BINARY, Json::String(text(&argument(0)))),
        "toBoolean" => typed(BOOLEAN, Json::Bool(values::truthy(Some(&argument(0))))),
        "toNull" => typed(NULL, Json::Null),
        // The sets, each over the elements of a list.
        "toStringSet" => set(STRING_SET, &argument(0), |element| {
            Ok(Json::String(text(element)))
        })?,
        "toNumberSet" => set(NUMBER_SET, &argument(0), number)?,
        "toBinarySet" => set(BINARY_SET, &argument(0), |element| {
            Ok(Json::String(text(element)))
        })?,
        // The collections, whose elements and members are marshalled through the general
        // converter, so a nested list or map carries its own representation.
        "toList" => match argument(0) {
            Json::Array(elements) => typed(
                LIST,
                Json::Array(elements.iter().map(attribute_value).collect()),
            ),
            other => return Err(format!("{NOT_A_LIST} {}", values::rendered(&other))),
        },
        "toMap" => typed(MAP, Json::Object(marshalled_members(&argument(0))?)),
        // "Creates a copy of the map where each value has been converted to its DynamoDB
        // representation" — the map ITSELF, which is what an `attributeValues` member is.
        "toMapValues" => Json::Object(marshalled_members(&argument(0))?),
        // The S3 object reference, which DynamoDB holds as text under a string attribute.
        "toS3Object" => typed(STRING, Json::String(s3_object(args).to_string())),
        _ => return Ok(None),
    };
    Ok(Some(marshalled))
}

/// `value` as the attribute value DynamoDB holds it as, by its own type: the general
/// conversion `$util.dynamodb.toDynamoDB` performs, and what every element of a list and
/// member of a map is marshalled through.
#[must_use]
pub fn attribute_value(value: &Json) -> Json {
    match value {
        Json::Null => typed(NULL, Json::Null),
        Json::Bool(decided) => typed(BOOLEAN, Json::Bool(*decided)),
        Json::Number(count) => typed(NUMBER, Json::Number(count.clone())),
        Json::String(text) => typed(STRING, Json::String(text.clone())),
        Json::Array(elements) => typed(
            LIST,
            Json::Array(elements.iter().map(attribute_value).collect()),
        ),
        Json::Object(members) => typed(
            MAP,
            Json::Object(
                members
                    .iter()
                    .map(|(name, member)| (name.clone(), attribute_value(member)))
                    .collect(),
            ),
        ),
    }
}

/// One attribute value: the one-member object naming the attribute `kind` and the content it
/// holds.
fn typed(kind: &str, content: Json) -> Json {
    let mut attribute = JsonMap::new();
    attribute.insert(kind.to_string(), content);
    Json::Object(attribute)
}

/// Every member of a map, marshalled through [`attribute_value`]. A value that is no map is
/// nothing these two helpers can convert.
fn marshalled_members(value: &Json) -> Result<JsonMap<String, Json>, String> {
    let Json::Object(members) = value else {
        return Err(format!("{NOT_A_MAP} {}", values::rendered(value)));
    };
    Ok(members
        .iter()
        .map(|(name, member)| (name.clone(), attribute_value(member)))
        .collect())
}

/// One set: the elements of a list, each converted by `element_of`, under the attribute name
/// `kind`. A value that is no list is nothing a set helper can convert.
fn set(
    kind: &str,
    value: &Json,
    element_of: impl Fn(&Json) -> Result<Json, String>,
) -> Result<Json, String> {
    let Json::Array(elements) = value else {
        return Err(format!("{NOT_A_LIST} {}", values::rendered(value)));
    };
    let mut converted = Vec::with_capacity(elements.len());
    for element in elements {
        converted.push(element_of(element)?);
    }
    Ok(typed(kind, Json::Array(converted)))
}

/// `value` as the TEXT a string, a binary or an element of either set carries: a string is
/// its own content and every other value the text the language renders it as.
fn text(value: &Json) -> String {
    match value {
        Json::String(text) => text.clone(),
        Json::Null => String::new(),
        other => values::rendered(other),
    }
}

/// `value` as the NUMBER a number attribute carries, which an attribute value holds as a JSON
/// number. Text that reads as a number is taken as that number — the form an argument built
/// out of template text arrives in — and anything else is nothing a number helper can
/// convert.
fn number(value: &Json) -> Result<Json, String> {
    match value {
        Json::Number(count) => Ok(Json::Number(count.clone())),
        Json::String(text) => text
            .trim()
            .parse::<i64>()
            .map(Json::from)
            .or_else(|_| text.trim().parse::<f64>().map(values::number))
            .map_err(|_| format!("{NOT_A_NUMBER} {text}")),
        other => Err(format!("{NOT_A_NUMBER} {}", values::rendered(other))),
    }
}

/// The S3 object reference `args` describes — the key, the bucket, the Region and, where the
/// call states one, the version — as the document DynamoDB holds it as.
fn s3_object(args: &[Json]) -> Json {
    let stated = |index: usize| args.get(index).map_or_else(String::new, text);
    let mut object = JsonMap::new();
    object.insert(S3_KEY.to_string(), Json::String(stated(0)));
    object.insert(S3_BUCKET.to_string(), Json::String(stated(1)));
    object.insert(S3_REGION.to_string(), Json::String(stated(2)));
    if args.get(3).is_some_and(|version| !version.is_null()) {
        object.insert(S3_VERSION.to_string(), Json::String(stated(3)));
    }
    let mut reference = JsonMap::new();
    reference.insert(S3_MEMBER.to_string(), Json::Object(object));
    Json::Object(reference)
}

/// The key, bucket, Region and version an S3 object reference carries, read back out of the
/// representation [`s3_object`] writes: the text itself, or the string attribute value
/// holding it.
fn from_s3_object(value: Option<&Json>) -> Result<Json, String> {
    let held = match value {
        Some(Json::String(held)) => held.clone(),
        Some(Json::Object(attribute)) => attribute
            .get(STRING)
            .and_then(Json::as_str)
            .ok_or_else(|| NOT_AN_S3_OBJECT.to_string())?
            .to_string(),
        other => {
            return Err(format!(
                "{NOT_AN_S3_OBJECT} {}",
                other.map_or_else(String::new, values::rendered)
            ))
        }
    };
    let parsed: Json =
        ::serde_json::from_str(&held).map_err(|_| format!("{NOT_AN_S3_OBJECT} {held}"))?;
    parsed
        .get(S3_MEMBER)
        .cloned()
        .ok_or_else(|| format!("{NOT_AN_S3_OBJECT} {held}"))
}

/// The attribute names DynamoDB holds each type of value under, which is what every helper of
/// this family writes.
const STRING: &str = "S";
const NUMBER: &str = "N";
const BINARY: &str = "B";
const BOOLEAN: &str = "BOOL";
const NULL: &str = "NULL";
const LIST: &str = "L";
const MAP: &str = "M";
const STRING_SET: &str = "SS";
const NUMBER_SET: &str = "NS";
const BINARY_SET: &str = "BS";

/// The members of the S3 object reference DynamoDB holds as text, and the one it is held
/// under.
const S3_MEMBER: &str = "s3";
const S3_KEY: &str = "key";
const S3_BUCKET: &str = "bucket";
const S3_REGION: &str = "region";
const S3_VERSION: &str = "version";

/// What the SERIALIZED sibling of a helper is named, and the one helper whose name ends that
/// way without being one.
const JSON_SUFFIX: &str = "Json";
const FROM_S3_OBJECT: &str = "fromS3ObjectJson";

/// The sentences a marshalling fault states.
const NOT_A_LIST: &str = "This value is not a list:";
const NOT_A_MAP: &str = "This value is not a map:";
const NOT_A_NUMBER: &str = "This value is not a number:";
const NOT_AN_S3_OBJECT: &str = "This value is not an S3 object reference:";
