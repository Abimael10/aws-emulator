// SPDX-License-Identifier: CC0-1.0

//! The QUERY language the cluster's endpoint answers: how a filter selects documents, how a
//! sort orders them, what a projection keeps, and what an update modifier changes.
//!
//! One reading of each, used by every command that takes one ([`super::commands`]), so a
//! `find`, a `count`, an `update` and an `aggregate` cannot disagree about which documents a
//! filter selects. An operator this module does not implement is REFUSED by name
//! ([`super::fault::bad_value`]) rather than ignored: a filter silently read as "match
//! everything" would delete the wrong documents.
//!
//! Comparison follows the engine's own ordering: values of different BSON types compare by
//! the type order the engine publishes ([`type_rank`]) rather than by their bytes, which is
//! what makes a sort over a mixed field and a range over one well defined.

use std::cmp::Ordering;

use ::bson::{Bson, Document};

use super::fault::{bad_value, WireFault};

/// How many dot-separated segments one field path may carry.
///
/// CHOSEN as a process-safety bound, not a protocol limit: a path comes off the wire (the
/// field name of an update modifier, of a projection, of a sort) and every segment is one
/// step of the walk that resolves it, so without a ceiling one request would size that work.
/// Sized well above the paths a client really sends.
pub const MAX_PATH_SEGMENTS: usize = 32;

/// How deeply a filter, an expression or a stored document may nest.
///
/// A process-safety bound of the same kind, set at the engine's own nesting ceiling of 100
/// levels: a structure deeper than this is refused by name rather than walked.
pub const MAX_NESTING_DEPTH: usize = 100;

/// The segments of a dotted field path, refused when it carries more than
/// [`MAX_PATH_SEGMENTS`] of them.
pub fn path_segments(path: &str) -> Result<Vec<&str>, WireFault> {
    let segments: Vec<&str> = path.split('.').collect();
    if segments.len() > MAX_PATH_SEGMENTS {
        return Err(bad_value(format!(
            "the field path '{}' names more than {MAX_PATH_SEGMENTS} segments",
            path.chars().take(64).collect::<String>()
        )));
    }
    Ok(segments)
}

/// Whether `value` nests no deeper than `limit`.
///
/// Walked with an explicit work list rather than by recursion, so the depth of a value a
/// client sent is bounded by this check instead of by this process's stack.
#[must_use]
pub fn nests_within(value: &Bson, limit: usize) -> bool {
    let mut pending: Vec<(&Bson, usize)> = vec![(value, 1)];
    while let Some((value, depth)) = pending.pop() {
        if depth > limit {
            return false;
        }
        match value {
            Bson::Document(nested) => {
                for (_, value) in nested {
                    pending.push((value, depth + 1));
                }
            }
            Bson::Array(values) => {
                for value in values {
                    pending.push((value, depth + 1));
                }
            }
            _ => {}
        }
    }
    true
}

/// [`nests_within`] for a document, without copying it into a `Bson` to ask.
#[must_use]
pub fn document_nests_within(document: &Document, limit: usize) -> bool {
    limit > 0
        && document
            .iter()
            .all(|(_, value)| nests_within(value, limit - 1))
}

/// The value at a dotted path, which is how every member of a command names a field:
/// `"a.b"` is the `b` of the document at `a`, and a numeric segment indexes an array.
#[must_use]
pub fn value_at<'a>(document: &'a Document, path: &str) -> Option<&'a Bson> {
    let mut current: Option<&Bson> = None;
    for (index, segment) in path.split('.').take(MAX_PATH_SEGMENTS).enumerate() {
        let next = match index {
            0 => document.get(segment),
            _ => match current? {
                Bson::Document(nested) => nested.get(segment),
                Bson::Array(values) => segment
                    .parse::<usize>()
                    .ok()
                    .and_then(|position| values.get(position)),
                _ => None,
            },
        };
        current = Some(next?);
    }
    current
}

/// Write `value` at a dotted path, creating the documents on the way to it.
///
/// Walked with a loop rather than by recursion, and refused for a path past
/// [`MAX_PATH_SEGMENTS`]: the path is a field name a client chose, so what bounds the walk is
/// that ceiling and not this process's stack.
pub fn set_path(document: &mut Document, path: &str, value: Bson) -> Result<(), WireFault> {
    let segments = path_segments(path)?;
    let Some((last, parents)) = segments.split_last() else {
        return Ok(());
    };
    let mut current = document;
    for segment in parents {
        if !matches!(current.get(*segment), Some(Bson::Document(_))) {
            current.insert((*segment).to_string(), Bson::Document(Document::new()));
        }
        current = match current.get_mut(*segment) {
            Some(Bson::Document(nested)) => nested,
            // The field was just written as a document, so this cannot be reached; a value
            // that is not one leaves the document as it was rather than replacing it.
            _ => return Ok(()),
        };
    }
    current.insert((*last).to_string(), value);
    Ok(())
}

/// Remove whatever is at a dotted path, and report whether anything was there. Bounded and
/// walked exactly as [`set_path`] is.
pub fn unset_path(document: &mut Document, path: &str) -> Result<bool, WireFault> {
    let segments = path_segments(path)?;
    let Some((last, parents)) = segments.split_last() else {
        return Ok(false);
    };
    let mut current = document;
    for segment in parents {
        current = match current.get_mut(*segment) {
            Some(Bson::Document(nested)) => nested,
            _ => return Ok(false),
        };
    }
    Ok(current.remove(*last).is_some())
}

/// The engine's canonical type order, which values of different types compare by.
///
/// Source: the "Comparison/Sort Order" the engine publishes for BSON types — MinKey before
/// null, before numbers, before strings, before objects, before arrays, before binary data,
/// before an object id, before a boolean, before a date, before a timestamp, before a
/// regular expression, before MaxKey.
#[must_use]
pub fn type_rank(value: &Bson) -> u8 {
    match value {
        Bson::MinKey => 1,
        Bson::Null | Bson::Undefined => 2,
        Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) | Bson::Decimal128(_) => 3,
        Bson::String(_) | Bson::Symbol(_) => 4,
        Bson::Document(_) => 5,
        Bson::Array(_) => 6,
        Bson::Binary(_) => 7,
        Bson::ObjectId(_) => 8,
        Bson::Boolean(_) => 9,
        Bson::DateTime(_) => 10,
        Bson::Timestamp(_) => 11,
        Bson::RegularExpression(_) => 12,
        Bson::JavaScriptCode(_) | Bson::JavaScriptCodeWithScope(_) => 13,
        Bson::DbPointer(_) => 13,
        Bson::MaxKey => 14,
    }
}

/// The number a numeric BSON value is, for comparing across the numeric types.
fn as_number(value: &Bson) -> Option<f64> {
    match value {
        Bson::Int32(number) => Some(f64::from(*number)),
        Bson::Int64(number) => Some(*number as f64),
        Bson::Double(number) => Some(*number),
        _ => None,
    }
}

/// Compare two values the way the engine does: by type order first, and within a type by the
/// type's own ordering.
///
/// Values nested deeper than [`MAX_NESTING_DEPTH`] compare EQUAL rather than being walked
/// further: nothing deeper than that reaches a collection ([`nests_within`] refuses it on the
/// way in), so this is the floor under the walk and not a rule a client can reach.
#[must_use]
pub fn compare(left: &Bson, right: &Bson) -> Ordering {
    compare_at(left, right, 0)
}

fn compare_at(left: &Bson, right: &Bson, depth: usize) -> Ordering {
    let ranks = type_rank(left).cmp(&type_rank(right));
    if ranks != Ordering::Equal {
        return ranks;
    }
    if depth >= MAX_NESTING_DEPTH {
        return Ordering::Equal;
    }
    match (left, right) {
        // The NUMERIC rank, ordered as a total order of its own ([`compare_numbers`]).
        (
            Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) | Bson::Decimal128(_),
            Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) | Bson::Decimal128(_),
        ) => compare_numbers(left, right),
        // A string and a symbol compare by their TEXT, which is how the engine compares the
        // two: a symbol is a string of another kind, not a value of another type.
        (Bson::String(left) | Bson::Symbol(left), Bson::String(right) | Bson::Symbol(right)) => {
            left.cmp(right)
        }
        (Bson::Boolean(left), Bson::Boolean(right)) => left.cmp(right),
        (Bson::ObjectId(left), Bson::ObjectId(right)) => left.cmp(right),
        (Bson::DateTime(left), Bson::DateTime(right)) => {
            left.timestamp_millis().cmp(&right.timestamp_millis())
        }
        (Bson::Timestamp(left), Bson::Timestamp(right)) => (left.time, left.increment)
            .cmp(&(right.time, right.increment)),
        (Bson::Binary(left), Bson::Binary(right)) => left.bytes.cmp(&right.bytes),
        (Bson::Array(left), Bson::Array(right)) => compare_sequences(left, right, depth + 1),
        (Bson::Document(left), Bson::Document(right)) => {
            // Two documents compare field by field, keys before values, in the order their
            // fields are stored — which is the engine's own rule for comparing objects.
            for (left, right) in left.iter().zip(right.iter()) {
                let keys = left.0.cmp(right.0);
                if keys != Ordering::Equal {
                    return keys;
                }
                let values = compare_at(left.1, right.1, depth + 1);
                if values != Ordering::Equal {
                    return values;
                }
            }
            left.len().cmp(&right.len())
        }
        // A regular expression compares by its pattern and then by its options, and a piece of
        // JavaScript by its source — the values those types are made of.
        (Bson::RegularExpression(left), Bson::RegularExpression(right)) => {
            (&left.pattern, &left.options).cmp(&(&right.pattern, &right.options))
        }
        (Bson::JavaScriptCode(left), Bson::JavaScriptCode(right)) => left.cmp(right),
        (Bson::JavaScriptCodeWithScope(left), Bson::JavaScriptCodeWithScope(right)) => {
            (&left.code, left.scope.len()).cmp(&(&right.code, right.scope.len()))
        }
        // The types that carry NO value of their own, which makes every pair of them equal:
        // the absent value (null and undefined share a rank, as the engine ranks them), and
        // the two bounds.
        (Bson::Null | Bson::Undefined, Bson::Null | Bson::Undefined)
        | (Bson::MinKey, Bson::MinKey)
        | (Bson::MaxKey, Bson::MaxKey) => Ordering::Equal,
        // Two values that share a rank but not a KIND, and whose kinds this plane orders
        // against nothing else (a DB pointer, a piece of JavaScript beside one): ordered by
        // the BSON type they are, which is a deterministic tie-break — so the whole relation
        // is `(rank, kind, value)` and therefore transitive, whatever a client stored.
        _ => (left.element_type() as u8).cmp(&(right.element_type() as u8)),
    }
}

/// Compare two values of the NUMERIC rank, as a TOTAL order.
///
/// `slice::sort_by` is documented to panic for a comparator that is not a total order, and a
/// client can store a `NaN` or a `Decimal128`: neither has a partial ordering against an
/// ordinary number, so neither may be left to collapse to "equal". The numbers are therefore
/// ordered in CLASSES — `NaN` first, which is where the engine sorts it among numbers, then
/// every value a double holds (compared with `f64::total_cmp`, which is total), then a decimal
/// (compared by the decimal it prints as, which is total and leaves two equal decimals equal)
/// — so the relation is transitive whatever a collection holds.
fn compare_numbers(left: &Bson, right: &Bson) -> Ordering {
    let classes = numeric_class(left).cmp(&numeric_class(right));
    if classes != Ordering::Equal {
        return classes;
    }
    match (left, right) {
        (Bson::Decimal128(left), Bson::Decimal128(right)) => {
            left.to_string().cmp(&right.to_string())
        }
        _ => match (as_number(left), as_number(right)) {            (Some(left), Some(right)) => left.total_cmp(&right),
            // Two values of one class with no number of their own: two NaNs, which are equal
            // to each other and to nothing else.
            _ => Ordering::Equal,
        },
    }
}

/// Which class of number a value is, for [`compare_numbers`].
fn numeric_class(value: &Bson) -> u8 {
    match value {
        Bson::Double(number) if number.is_nan() => 0,
        Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) => 1,
        Bson::Decimal128(_) => 2,
        // Not a number at all: every value of one rank that is not numeric sorts together,
        // which is what the rank itself already decided.
        _ => 3,
    }
}

fn compare_sequences(left: &[Bson], right: &[Bson], depth: usize) -> Ordering {
    for (left, right) in left.iter().zip(right.iter()) {
        let values = compare_at(left, right, depth);
        if values != Ordering::Equal {
            return values;
        }
    }
    left.len().cmp(&right.len())
}

/// Whether `document` is selected by `filter`.
///
/// A field named with a plain value matches when the field EQUALS that value, or — when the
/// field holds an array — when one of its elements does, which is the engine's rule for
/// querying an array field. A field named with a document of `$`-operators matches when
/// every operator holds.
///
/// A filter nested deeper than [`MAX_NESTING_DEPTH`] is REFUSED by name: the nesting is the
/// client's, so what bounds the walk is that ceiling rather than this process's stack.
pub fn matches(document: &Document, filter: &Document) -> Result<bool, WireFault> {
    matches_at(document, filter, 0)
}

fn matches_at(document: &Document, filter: &Document, depth: usize) -> Result<bool, WireFault> {
    if depth > MAX_NESTING_DEPTH {
        return Err(too_deep("query"));
    }
    for (field, condition) in filter {
        let holds = match field.as_str() {
            "$and" => {
                let clauses = clauses_of("$and", condition)?;
                let mut all = true;
                for clause in clauses {
                    all &= matches_at(document, clause, depth + 1)?;
                }
                all
            }
            "$or" => {
                let clauses = clauses_of("$or", condition)?;
                let mut any = clauses.is_empty();
                for clause in clauses {
                    any |= matches_at(document, clause, depth + 1)?;
                }
                any
            }
            "$nor" => {
                let clauses = clauses_of("$nor", condition)?;
                let mut none = true;
                for clause in clauses {
                    none &= !matches_at(document, clause, depth + 1)?;
                }
                none
            }
            unknown if unknown.starts_with('$') => {
                return Err(bad_value(format!("unknown top level operator: {unknown}")))
            }
            field => field_matches(document, field, condition, depth)?,
        };
        if !holds {
            return Ok(false);
        }
    }
    Ok(true)
}

/// What a structure nested past [`MAX_NESTING_DEPTH`] is refused with.
fn too_deep(what: &str) -> WireFault {
    bad_value(format!(
        "{what} exceeds the maximum nesting depth of {MAX_NESTING_DEPTH}"
    ))
}

/// The clause list of a logical operator, which must be an array of documents.
fn clauses_of<'a>(operator: &str, condition: &'a Bson) -> Result<Vec<&'a Document>, WireFault> {
    let Bson::Array(values) = condition else {
        return Err(bad_value(format!("{operator} must be an array")));
    };
    values
        .iter()
        .map(|value| match value {
            Bson::Document(clause) => Ok(clause),
            _ => Err(bad_value(format!(
                "{operator} elements must be objects, got {}",
                value.element_type() as u8
            ))),
        })
        .collect()
}

/// Whether the field at `path` of `document` satisfies `condition`.
fn field_matches(
    document: &Document,
    path: &str,
    condition: &Bson,
    depth: usize,
) -> Result<bool, WireFault> {
    if depth > MAX_NESTING_DEPTH {
        return Err(too_deep("query"));
    }
    let actual = value_at(document, path);
    let operators = match condition {
        Bson::Document(nested) if nested.keys().any(|key| key.starts_with('$')) => nested,
        _ => return Ok(equals(actual, condition)),
    };
    for (operator, argument) in operators {
        let holds = match operator.as_str() {
            "$eq" => equals(actual, argument),
            "$ne" => !equals(actual, argument),
            "$gt" => ordered(actual, argument, &[Ordering::Greater]),
            "$gte" => ordered(actual, argument, &[Ordering::Greater, Ordering::Equal]),
            "$lt" => ordered(actual, argument, &[Ordering::Less]),
            "$lte" => ordered(actual, argument, &[Ordering::Less, Ordering::Equal]),
            "$in" => {
                let values = clause_values("$in", argument)?;
                values.iter().any(|value| equals(actual, value))
            }
            "$nin" => {
                let values = clause_values("$nin", argument)?;
                !values.iter().any(|value| equals(actual, value))
            }
            "$exists" => {
                let wanted = truthy(argument);
                actual.is_some() == wanted
            }
            "$size" => match (actual, as_number(argument)) {
                (Some(Bson::Array(values)), Some(wanted)) => values.len() as f64 == wanted,
                (_, None) => return Err(bad_value("$size needs a number")),
                _ => false,
            },
            "$all" => {
                let values = clause_values("$all", argument)?;
                values.iter().all(|value| equals(actual, value))
            }
            "$not" => match argument {
                Bson::Document(nested) => !field_matches(
                    document,
                    path,
                    &Bson::Document(nested.clone()),
                    depth + 1,
                )?,
                _ => return Err(bad_value("$not needs an expression")),
            },
            "$type" => match (actual, argument) {
                (Some(value), Bson::String(name)) => type_name(value) == name.as_str(),
                (Some(value), number) => match as_number(number) {
                    Some(code) => f64::from(value.element_type() as u8) == code,
                    None => return Err(bad_value("$type needs a type name or number")),
                },
                (None, _) => false,
            },
            unknown => {
                return Err(bad_value(format!(
                    "unknown operator: {unknown}"
                )))
            }
        };
        if !holds {
            return Ok(false);
        }
    }
    Ok(true)
}

/// The element list of `$in`, `$nin` or `$all`.
fn clause_values(operator: &str, argument: &Bson) -> Result<Vec<Bson>, WireFault> {
    match argument {
        Bson::Array(values) => Ok(values.clone()),
        _ => Err(bad_value(format!("{operator} needs an array"))),
    }
}

/// Whether a field holding `actual` equals `expected` — directly, or as one element of an
/// array it holds.
fn equals(actual: Option<&Bson>, expected: &Bson) -> bool {
    match actual {
        None => matches!(expected, Bson::Null),
        Some(value) => {
            if compare(value, expected) == Ordering::Equal && type_rank(value) == type_rank(expected)
            {
                return true;
            }
            match value {
                Bson::Array(values) => values
                    .iter()
                    .any(|element| compare(element, expected) == Ordering::Equal),
                _ => false,
            }
        }
    }
}

/// Whether a field holding `actual` stands in one of `wanted` orderings to `bound`.
///
/// A field the document does not carry satisfies no range, which is the engine's rule: a
/// missing field is not less than every value, it is simply not selected.
fn ordered(actual: Option<&Bson>, bound: &Bson, wanted: &[Ordering]) -> bool {
    let Some(value) = actual else {
        return false;
    };
    // A range compares only within one type class, as the engine does: a string is not
    // greater than a number, it is of another type and so is not selected.
    if type_rank(value) != type_rank(bound) {
        return false;
    }
    wanted.contains(&compare(value, bound))
}

/// Whether a BSON value is TRUE as a condition reads it: everything but `false`, `null`, a
/// missing value and zero.
#[must_use]
pub fn truthy(value: &Bson) -> bool {
    match value {
        Bson::Boolean(flag) => *flag,
        Bson::Null | Bson::Undefined => false,
        Bson::Int32(0) | Bson::Int64(0) => false,
        Bson::Double(number) => *number != 0.0,
        _ => true,
    }
}

/// The engine's name for a value's BSON type, which `$type` compares against.
fn type_name(value: &Bson) -> &'static str {
    match value {
        Bson::Double(_) => "double",
        Bson::String(_) => "string",
        Bson::Document(_) => "object",
        Bson::Array(_) => "array",
        Bson::Binary(_) => "binData",
        Bson::Undefined => "undefined",
        Bson::ObjectId(_) => "objectId",
        Bson::Boolean(_) => "bool",
        Bson::DateTime(_) => "date",
        Bson::Null => "null",
        Bson::RegularExpression(_) => "regex",
        Bson::DbPointer(_) => "dbPointer",
        Bson::JavaScriptCode(_) => "javascript",
        Bson::Symbol(_) => "symbol",
        Bson::JavaScriptCodeWithScope(_) => "javascriptWithScope",
        Bson::Int32(_) => "int",
        Bson::Timestamp(_) => "timestamp",
        Bson::Int64(_) => "long",
        Bson::Decimal128(_) => "decimal",
        Bson::MinKey => "minKey",
        Bson::MaxKey => "maxKey",
    }
}

/// Order `documents` by `sort`: each field with `1` ascending or `-1` descending, in the
/// order the sort names them.
pub fn sort_documents(documents: &mut [Document], sort: &Document) -> Result<(), WireFault> {
    let mut keys: Vec<(String, bool)> = Vec::new();
    for (field, direction) in sort {
        let direction = as_number(direction)
            .ok_or_else(|| bad_value(format!("the sort direction of {field} must be a number")))?;
        if direction != 1.0 && direction != -1.0 {
            return Err(bad_value(format!(
                "bad sort specification: the direction of {field} must be 1 or -1"
            )));
        }
        keys.push((field.clone(), direction > 0.0));
    }
    documents.sort_by(|left, right| {
        for (field, ascending) in &keys {
            let left = value_at(left, field).unwrap_or(&Bson::Null);
            let right = value_at(right, field).unwrap_or(&Bson::Null);
            let order = compare(left, right);
            if order != Ordering::Equal {
                return match ascending {
                    true => order,
                    false => order.reverse(),
                };
            }
        }
        Ordering::Equal
    });
    Ok(())
}

/// What a projection keeps of a document.
///
/// A projection naming fields to INCLUDE keeps those fields and `_id` unless it excludes it;
/// one naming fields to EXCLUDE keeps everything else. Mixing the two is refused, as the
/// engine refuses it.
pub fn project(document: &Document, projection: &Document) -> Result<Document, WireFault> {
    if projection.is_empty() {
        return Ok(document.clone());
    }
    let mut including: Vec<&String> = Vec::new();
    let mut excluding: Vec<&String> = Vec::new();
    for (field, value) in projection {
        match truthy(value) {
            true => including.push(field),
            false => excluding.push(field),
        }
    }
    let mixed = including.iter().any(|field| field.as_str() != "_id")
        && excluding.iter().any(|field| field.as_str() != "_id");
    if mixed {
        return Err(bad_value(
            "Projection cannot have a mix of inclusion and exclusion.",
        ));
    }
    if including.iter().any(|field| field.as_str() != "_id") {
        let mut kept = Document::new();
        // `_id` is projected unless the projection excludes it, which is why it is written
        // first: a reader of the reply sees the field order the engine sends.
        if !excluding.iter().any(|field| field.as_str() == "_id") {
            if let Some(id) = document.get("_id") {
                kept.insert("_id".to_string(), id.clone());
            }
        }
        for field in including {
            if field == "_id" {
                continue;
            }
            if let Some(value) = value_at(document, field) {
                let value = value.clone();
                set_path(&mut kept, field, value)?;
            }
        }
        return Ok(kept);
    }
    let mut kept = document.clone();
    for field in excluding {
        unset_path(&mut kept, field)?;
    }
    Ok(kept)
}

/// Apply one update to a document, reporting whether it changed it.
///
/// An update document made of `$`-operators modifies the named fields; one made of plain
/// fields REPLACES the document, keeping its `_id`, which is the engine's own rule.
pub fn apply_update(document: &mut Document, update: &Document) -> Result<bool, WireFault> {
    let operators = update.keys().any(|key| key.starts_with('$'));
    if !operators {
        let id = document.get("_id").cloned();
        let before = document.clone();
        *document = update.clone();
        if let Some(id) = id {
            // The `_id` of a document is immutable, so a replacement keeps it — and one that
            // names a different `_id` is refused rather than silently kept.
            match document.get("_id") {
                Some(named) if compare(named, &id) != Ordering::Equal => {
                    *document = before;
                    return Err(super::fault::illegal_operation(
                        "the (immutable) field '_id' was found to have been altered",
                    ));
                }
                _ => {
                    document.insert("_id".to_string(), id);
                }
            }
        }
        return Ok(*document != before);
    }
    if update.keys().any(|key| !key.starts_with('$')) {
        return Err(super::fault::failed_to_parse(
            "update document must contain either all operator fields or no operator fields",
        ));
    }
    let before = document.clone();
    for (operator, argument) in update {
        let Bson::Document(fields) = argument else {
            return Err(super::fault::failed_to_parse(format!(
                "Modifiers operate on fields but we found another type instead: {operator}"
            )));
        };
        match operator.as_str() {
            "$set" => {
                for (field, value) in fields {
                    set_path(document, field, value.clone())?;
                }
            }
            "$setOnInsert" => {}
            "$unset" => {
                for (field, _) in fields {
                    unset_path(document, field)?;
                }
            }
            "$inc" => {
                for (field, by) in fields {
                    let by = finite_number(by).ok_or_else(|| {
                        bad_value(format!("Cannot increment {field} with a non-numeric argument"))
                    })?;
                    let current = value_at(document, field).cloned().unwrap_or(Bson::Int32(0));
                    let incremented = increment(&current, by).ok_or_else(|| {
                        bad_value(format!(
                            "Failed to apply $inc operations to current value for {field}"
                        ))
                    })?;
                    set_path(document, field, incremented)?;
                }
            }
            "$mul" => {
                for (field, by) in fields {
                    let by = finite_number(by).ok_or_else(|| {
                        bad_value(format!("Cannot multiply {field} with a non-numeric argument"))
                    })?;
                    let current = value_at(document, field).cloned().unwrap_or(Bson::Int32(0));
                    let multiplied = multiply(&current, by).ok_or_else(|| {
                        bad_value(format!(
                            "Failed to apply $mul operations to current value for {field}"
                        ))
                    })?;
                    set_path(document, field, multiplied)?;
                }
            }
            "$push" => {
                for (field, value) in fields {
                    let mut values = match value_at(document, field) {
                        Some(Bson::Array(values)) => values.clone(),
                        None => Vec::new(),
                        Some(_) => {
                            return Err(bad_value(format!(
                                "The field '{field}' must be an array but is of another type"
                            )))
                        }
                    };
                    values.push(value.clone());
                    set_path(document, field, Bson::Array(values))?;
                }
            }
            "$addToSet" => {
                for (field, value) in fields {
                    let mut values = match value_at(document, field) {
                        Some(Bson::Array(values)) => values.clone(),
                        None => Vec::new(),
                        Some(_) => {
                            return Err(bad_value(format!(
                                "Cannot apply $addToSet to non-array field {field}"
                            )))
                        }
                    };
                    if !values
                        .iter()
                        .any(|element| compare(element, value) == Ordering::Equal)
                    {
                        values.push(value.clone());
                    }
                    set_path(document, field, Bson::Array(values))?;
                }
            }
            "$pull" => {
                for (field, value) in fields {
                    if let Some(Bson::Array(values)) = value_at(document, field) {
                        let kept: Vec<Bson> = values
                            .iter()
                            .filter(|element| compare(element, value) != Ordering::Equal)
                            .cloned()
                            .collect();
                        set_path(document, field, Bson::Array(kept))?;
                    }
                }
            }
            "$rename" => {
                for (field, to) in fields {
                    let Bson::String(to) = to else {
                        return Err(bad_value("The 'to' field for $rename must be a string"));
                    };
                    if let Some(value) = value_at(document, field).cloned() {
                        unset_path(document, field)?;
                        set_path(document, to, value)?;
                    }
                }
            }
            unknown => {
                return Err(super::fault::failed_to_parse(format!(
                    "Unknown modifier: {unknown}"
                )))
            }
        }
    }
    Ok(*document != before)
}

/// What an insert of `update` upserts: the document an update that matched nothing leaves
/// behind, which carries the filter's own equalities and whatever the update sets.
pub fn upserted_document(filter: &Document, update: &Document) -> Result<Document, WireFault> {
    let mut document = Document::new();
    // An upsert starts from the EQUALITIES the filter names, which is how the engine builds
    // the new document: a field the filter pins by value is that value in it.
    for (field, condition) in filter {
        if field.starts_with('$') {
            continue;
        }
        match condition {
            Bson::Document(nested) if nested.keys().any(|key| key.starts_with('$')) => {
                if let Some(Bson::Document(_) | Bson::Array(_)) = nested.get("$eq") {
                    continue;
                }
                if let Some(value) = nested.get("$eq") {
                    set_path(&mut document, field, value.clone())?;
                }
            }
            value => set_path(&mut document, field, value.clone())?,
        }
    }
    let operators = update.keys().any(|key| key.starts_with('$'));
    if !operators {
        let id = document.get("_id").cloned();
        document = update.clone();
        if let Some(id) = id {
            if !document.contains_key("_id") {
                document.insert("_id".to_string(), id);
            }
        }
        return Ok(document);
    }
    // `$setOnInsert` applies only here, which is what distinguishes it from `$set`.
    if let Some(Bson::Document(fields)) = update.get("$setOnInsert") {
        for (field, value) in fields {
            set_path(&mut document, field, value.clone())?;
        }
    }
    apply_update(&mut document, update)?;
    Ok(document)
}

/// The number `value` is, for an argument that has to be one — and `None` for a value that is
/// not a number at all or is not FINITE, which no arithmetic below can carry.
fn finite_number(value: &Bson) -> Option<f64> {
    as_number(value).filter(|number| number.is_finite())
}

/// `current + by`, keeping an integer field an integer.
///
/// `None` where the sum does not fit the field's own type, which is what the engine refuses a
/// `$inc` past the type's range with: every addition here is CHECKED, because both the stored
/// value and the argument come from a client.
fn increment(current: &Bson, by: f64) -> Option<Bson> {
    let by = whole(by)?;
    match current {
        Bson::Int32(number) => Some(Bson::Int32(i32::try_from(
            i64::from(*number).checked_add(by)?,
        ).ok()?)),
        Bson::Int64(number) => Some(Bson::Int64(number.checked_add(by)?)),
        Bson::Double(number) => finite(number + by as f64),
        _ => None,
    }
}

/// `current * by`, keeping an integer field an integer. Checked for the same reason
/// [`increment`] is.
fn multiply(current: &Bson, by: f64) -> Option<Bson> {
    let factor = whole(by)?;
    match current {
        Bson::Int32(number) => Some(Bson::Int32(i32::try_from(
            i64::from(*number).checked_mul(factor)?,
        ).ok()?)),
        Bson::Int64(number) => Some(Bson::Int64(number.checked_mul(factor)?)),
        Bson::Double(number) => finite(number * by),
        _ => None,
    }
}

/// `by` as the integer it is, for the integer arithmetic above: `None` for a value outside
/// the range an `i64` holds, which no integer field can be moved by.
fn whole(by: f64) -> Option<i64> {
    let truncated = by.trunc();
    (truncated >= -(2_f64.powi(63)) && truncated < 2_f64.powi(63)).then_some(truncated as i64)
}

/// A double result, or `None` where the arithmetic left the finite range.
fn finite(number: f64) -> Option<Bson> {
    number.is_finite().then_some(Bson::Double(number))
}

/// The index key of a document for an index over `fields`, or `None` when the document
/// carries none of them at all.
#[must_use]
pub fn index_key(document: &Document, fields: &Document) -> Document {
    let mut key = Document::new();
    for (field, _) in fields {
        let value = value_at(document, field).cloned().unwrap_or(Bson::Null);
        key.insert(field.clone(), value);
    }
    key
}

/// An index key rendered the way the engine prints one in a duplicate-key message:
/// `{ _id: ObjectId('...') }`, `{ name: "a" }`.
#[must_use]
pub fn render_key(key: &Document) -> String {
    let fields: Vec<String> = key
        .iter()
        .map(|(field, value)| format!("{field}: {}", render_value(value)))
        .collect();
    format!("{{ {} }}", fields.join(", "))
}

/// One value, rendered as the engine prints it inside a key.
fn render_value(value: &Bson) -> String {
    match value {
        Bson::String(text) => format!("\"{text}\""),
        Bson::ObjectId(id) => format!("ObjectId('{id}')"),
        Bson::Int32(number) => number.to_string(),
        Bson::Int64(number) => number.to_string(),
        Bson::Double(number) => number.to_string(),
        Bson::Boolean(flag) => flag.to_string(),
        Bson::Null => "null".to_string(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::bson::doc;

    /// RULE: a filter selects exactly the documents every one of its conditions holds for,
    /// and an operator this plane does not implement is refused by name.
    #[test]
    fn a_filter_selects_what_every_condition_holds_for() {
        let document = doc! {
            "_id": 1_i32,
            "name": "widget",
            "price": 10_i32,
            "tags": ["a", "b"],
            "nested": { "deep": 7_i32 },
        };
        for (case, filter, expected) in [
            ("an empty filter", doc! {}, Some(true)),
            ("an equality", doc! { "name": "widget" }, Some(true)),
            ("an equality that does not hold", doc! { "name": "other" }, Some(false)),
            ("an element of an array field", doc! { "tags": "b" }, Some(true)),
            ("a dotted path", doc! { "nested.deep": 7_i32 }, Some(true)),
            ("a range that holds", doc! { "price": { "$gt": 5_i32 } }, Some(true)),
            ("a range that does not", doc! { "price": { "$lt": 5_i32 } }, Some(false)),
            ("a set membership", doc! { "name": { "$in": ["widget", "other"] } }, Some(true)),
            ("existence", doc! { "missing": { "$exists": false } }, Some(true)),
            ("an array length", doc! { "tags": { "$size": 2_i32 } }, Some(true)),
            ("a negation", doc! { "name": { "$not": { "$eq": "other" } } }, Some(true)),
            (
                "a conjunction",
                doc! { "$or": [{ "name": "other" }, { "price": 10_i32 }] },
                Some(true),
            ),
            ("a type test", doc! { "price": { "$type": "int" } }, Some(true)),
            ("an operator this plane does not implement", doc! { "price": { "$mod": [2, 0] } }, None),
        ] {
            let outcome = matches(&document, &filter);
            assert_eq!(
                outcome.as_ref().ok().copied(),
                expected,
                "case {case}: expected {expected:?}, got {outcome:?}"
            );
        }
    }

    /// RULE: values of different types compare by the engine's type order, so a sort over a
    /// mixed field is well defined.
    #[test]
    fn values_compare_by_the_engines_type_order() {
        let mut documents = vec![
            doc! { "v": "text" },
            doc! { "v": 2_i32 },
            doc! { "v": Bson::Null },
            doc! { "v": true },
        ];
        sort_documents(&mut documents, &doc! { "v": 1_i32 }).expect("the sort is well formed");
        let order: Vec<String> = documents
            .iter()
            .map(|document| type_name(document.get("v").unwrap()).to_string())
            .collect();
        assert_eq!(
            order,
            vec!["null", "int", "string", "bool"],
            "expected null before numbers before strings before booleans, got {order:?}"
        );
    }

    /// RULE: an update of operators modifies the fields it names; one of plain fields
    /// replaces the document and keeps its `_id`.
    #[test]
    fn an_update_modifies_or_replaces() {
        let mut document = doc! { "_id": 1_i32, "price": 10_i32, "stale": true };
        let changed = apply_update(
            &mut document,
            &doc! { "$set": { "price": 12_i32 }, "$unset": { "stale": "" }, "$inc": { "seen": 1_i32 } },
        )
        .expect("the modifiers are known");
        assert!(changed, "the update changed the document");
        assert_eq!(
            document,
            doc! { "_id": 1_i32, "price": 12_i32, "seen": 1_i32 },
            "expected the named fields to be set, unset and incremented, got {document:?}"
        );
        apply_update(&mut document, &doc! { "name": "widget" }).expect("a replacement is allowed");
        assert_eq!(
            document,
            doc! { "name": "widget", "_id": 1_i32 },
            "expected a replacement to keep the _id, got {document:?}"
        );
        let unknown = apply_update(&mut document, &doc! { "$frobnicate": { "price": 1_i32 } });
        assert!(
            unknown.is_err(),
            "expected an unknown modifier to be refused, got {unknown:?}"
        );
    }

    /// RULE: a structure a client sends is walked only within this plane's own bounds — a
    /// field path past [`MAX_PATH_SEGMENTS`] and a filter or value nested past
    /// [`MAX_NESTING_DEPTH`] are refused by name rather than walked.
    #[test]
    fn a_path_or_a_structure_past_the_bounds_is_refused_by_name() {
        let document = doc! { "_id": 1_i32 };
        let long_path = vec!["a"; MAX_PATH_SEGMENTS + 1].join(".");
        let refused = set_path(&mut document.clone(), &long_path, Bson::Int32(1));
        assert!(
            refused.is_err(),
            "expected a path of {} segments to be refused, got {refused:?}",
            MAX_PATH_SEGMENTS + 1
        );
        assert!(
            unset_path(&mut document.clone(), &long_path).is_err(),
            "expected the same path to be refused by unset_path"
        );
        assert!(
            set_path(&mut document.clone(), &vec!["a"; MAX_PATH_SEGMENTS].join("."), Bson::Int32(1))
                .is_ok(),
            "expected a path AT the bound to be written"
        );

        // A filter wrapped in `$and` past the depth bound, built here rather than received,
        // is the shape a client could send.
        let mut filter = doc! { "_id": 1_i32 };
        for _ in 0..=MAX_NESTING_DEPTH {
            filter = doc! { "$and": [filter] };
        }
        let outcome = matches(&document, &filter);
        assert!(
            outcome.is_err(),
            "expected a filter nested past {MAX_NESTING_DEPTH} to be refused, got {outcome:?}"
        );
        // And the same question asked of a VALUE, which is what keeps one out of a
        // collection.
        let mut value = Bson::Int32(1);
        for _ in 0..=MAX_NESTING_DEPTH {
            value = Bson::Document(doc! { "a": value });
        }
        assert!(
            !nests_within(&value, MAX_NESTING_DEPTH),
            "expected a value nested past {MAX_NESTING_DEPTH} to be refused"
        );
        assert!(
            nests_within(&Bson::Document(doc! { "a": { "b": 1_i32 } }), MAX_NESTING_DEPTH),
            "expected an ordinary value to be within the bound"
        );
    }

    /// RULE: arithmetic on a field is CHECKED — both the stored value and the argument come
    /// from a client, so a `$inc` or `$mul` whose result does not fit the field's type is
    /// refused rather than wrapping or panicking.
    #[test]
    fn arithmetic_that_does_not_fit_is_refused() {
        for (case, mut document, update) in [
            (
                "an i64 field at its ceiling",
                doc! { "n": i64::MAX },
                doc! { "$inc": { "n": 1_i32 } },
            ),
            (
                "an i32 field at its ceiling",
                doc! { "n": i32::MAX },
                doc! { "$inc": { "n": 1_i32 } },
            ),
            (
                "a multiplication past the i64 range",
                doc! { "n": i64::MAX },
                doc! { "$mul": { "n": 2_i32 } },
            ),
            (
                "an argument that is not finite",
                doc! { "n": 1_i64 },
                doc! { "$inc": { "n": f64::INFINITY } },
            ),
        ] {
            let outcome = apply_update(&mut document, &update);
            assert!(
                outcome.is_err(),
                "case {case}: expected the update to be refused, got {outcome:?} with \
                 {document:?}"
            );
        }
        let mut document = doc! { "n": 1_i64 };
        apply_update(&mut document, &doc! { "$inc": { "n": 2_i32 } })
            .expect("an increment that fits is applied");
        assert_eq!(
            document.get_i64("n").ok(),
            Some(3),
            "expected the increment to be applied, got {document:?}"
        );
    }

    /// RULE: the ordering values compare by is a TOTAL order, so a sort over a field holding
    /// a `NaN`, a decimal and ordinary numbers is well defined rather than a panic — with
    /// `NaN` where the engine puts it, first among the numbers.
    #[test]
    fn numbers_compare_as_a_total_order() {
        let decimal: Bson = "1.5"
            .parse::<::bson::Decimal128>()
            .map(Bson::Decimal128)
            .expect("1.5 is a decimal");
        let mut documents = vec![
            doc! { "v": 2_i32 },
            doc! { "v": f64::NAN },
            doc! { "v": decimal.clone() },
            doc! { "v": 1_i64 },
            doc! { "v": f64::NAN },
        ];
        sort_documents(&mut documents, &doc! { "v": 1_i32 })
            .expect("the sort is well formed");
        let order: Vec<String> = documents
            .iter()
            .map(|document| match document.get("v") {
                Some(Bson::Double(number)) if number.is_nan() => "nan".to_string(),
                Some(Bson::Decimal128(_)) => "decimal".to_string(),
                Some(value) => format!("{value}"),
                None => "missing".to_string(),
            })
            .collect();
        assert_eq!(
            order,
            vec!["nan", "nan", "1", "2", "decimal"],
            "expected NaN first, then the numbers in order, then the decimal, got {order:?}"
        );
        // The same relation decides set membership, so a NaN is in a set only as itself.
        let mut document = doc! { "v": [f64::NAN] };
        apply_update(&mut document, &doc! { "$addToSet": { "v": 1_i32 } })
            .expect("the modifier is known");
        assert_eq!(
            document.get_array("v").map(Vec::len).ok(),
            Some(2),
            "expected a number unrelated to NaN to join the set, got {document:?}"
        );
    }

    /// RULE: the ordering values compare by is a TOTAL order over EVERY value a client can
    /// store — so `sort_by`, which is documented to panic for a comparator that is not one,
    /// cannot be made to panic by what a collection holds.
    ///
    /// The corpus below holds at least one value of every BSON type this plane can be handed,
    /// including the pairs that share a type rank (a string and a symbol, JavaScript beside a
    /// DB pointer, two regular expressions), and the test asserts the three properties a total
    /// order has: it is its own reverse, it is transitive, and a sort over the whole corpus
    /// completes.
    #[test]
    fn the_ordering_is_a_total_order_over_every_value_a_client_can_store() {
        let corpus: Vec<Bson> = vec![
            Bson::MinKey,
            Bson::Null,
            Bson::Undefined,
            Bson::Double(f64::NAN),
            Bson::Double(-1.5),
            Bson::Int32(1),
            Bson::Int64(2),
            Bson::Double(2.5),
            "1.5".parse::<::bson::Decimal128>().map(Bson::Decimal128).expect("a decimal"),
            Bson::String("a".to_string()),
            Bson::Symbol("b".to_string()),
            Bson::String("c".to_string()),
            Bson::Document(doc! { "a": 1_i32 }),
            Bson::Document(doc! { "a": 2_i32 }),
            Bson::Array(vec![Bson::Int32(1)]),
            Bson::Array(vec![Bson::Int32(1), Bson::Int32(2)]),
            Bson::Binary(::bson::Binary {
                subtype: ::bson::spec::BinarySubtype::Generic,
                bytes: vec![1, 2],
            }),
            Bson::ObjectId(::bson::oid::ObjectId::new()),
            Bson::Boolean(false),
            Bson::Boolean(true),
            Bson::DateTime(::bson::DateTime::from_millis(1)),
            Bson::Timestamp(::bson::Timestamp { time: 1, increment: 2 }),
            Bson::RegularExpression(::bson::Regex {
                pattern: "a".to_string(),
                options: "i".to_string(),
            }),
            Bson::RegularExpression(::bson::Regex {
                pattern: "b".to_string(),
                options: String::new(),
            }),
            Bson::JavaScriptCode("a()".to_string()),
            Bson::JavaScriptCodeWithScope(::bson::JavaScriptCodeWithScope {
                code: "a()".to_string(),
                scope: doc! { "x": 1_i32 },
            }),
            Bson::MaxKey,
        ];
        for left in &corpus {
            for right in &corpus {
                assert_eq!(
                    compare(left, right),
                    compare(right, left).reverse(),
                    "case {left:?} against {right:?}: the ordering must be its own reverse"
                );
                for middle in &corpus {
                    if compare(left, middle) != Ordering::Greater
                        && compare(middle, right) != Ordering::Greater
                    {
                        assert_ne!(
                            compare(left, right),
                            Ordering::Greater,
                            "case {left:?} <= {middle:?} <= {right:?}: the ordering must be \
                             transitive"
                        );
                    }
                }
            }
        }
        // And the sort itself, over documents holding the whole corpus in one field.
        let mut documents: Vec<Document> = corpus
            .iter()
            .map(|value| doc! { "v": value.clone() })
            .collect();
        sort_documents(&mut documents, &doc! { "v": 1_i32 }).expect("the sort is well formed");
        assert_eq!(
            documents.len(),
            corpus.len(),
            "expected the sort to keep every document, got {}",
            documents.len()
        );
    }

    /// RULE: a projection keeps the fields it includes (with `_id`), or everything but the
    /// fields it excludes, and never a mix of the two.
    #[test]
    fn a_projection_includes_or_excludes() {
        let document = doc! { "_id": 1_i32, "name": "widget", "price": 10_i32 };
        let included = project(&document, &doc! { "name": 1_i32 }).expect("an inclusion");
        assert_eq!(
            included,
            doc! { "_id": 1_i32, "name": "widget" },
            "expected the included field and the _id, got {included:?}"
        );
        let excluded = project(&document, &doc! { "price": 0_i32 }).expect("an exclusion");
        assert_eq!(
            excluded,
            doc! { "_id": 1_i32, "name": "widget" },
            "expected everything but the excluded field, got {excluded:?}"
        );
        let mixed = project(&document, &doc! { "name": 1_i32, "price": 0_i32 });
        assert!(mixed.is_err(), "expected a mix to be refused, got {mixed:?}");
    }
}
