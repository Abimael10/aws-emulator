// SPDX-License-Identifier: CC0-1.0

//! The VALUES a mapping template operates on, and what the language does with them:
//! how one is written into the rendered text, whether it is true, how two of them compare,
//! and the methods a reference may call on one.
//!
//! A template's values are the ones its context carries, which arrive as JSON, so JSON is
//! the value model here. The Java types Velocity itself works over map onto it directly: an
//! object is a `Map`, an array a `List`, and a string, a number and a boolean their own
//! scalars — which is why the methods below are the `Map`, `List` and `String` members a
//! template calls on them.
//!
//! A method this does not implement is a FAULT rather than a silently empty value: the
//! field that asked for it is reported as one that did not resolve, instead of being
//! answered with something nothing computed.

use ::serde_json::{Map as JsonMap, Value as Json};

/// How a value is written into the rendered text.
///
/// A string is written as itself and every other value as the JSON text of it, which is the
/// form a template's own output is read back as (an object or a list reaches the text only
/// through `$util.toJson`, and a number and a boolean render the same either way). The rule
/// is the LANGUAGE's, so it is the tree's one reading of it
/// ([`::service_common::velocity_template`]).
#[must_use]
pub fn rendered(value: &Json) -> String {
    ::service_common::velocity_template::rendered(value)
}

/// Whether a value is TRUE to a condition, by Velocity's own rule: a `boolean` decides by
/// its own value, and every other value decides by being there at all — which is why
/// `$util.isNullOrEmpty` exists for the question an empty string answers `true` to here.
#[must_use]
pub fn truthy(value: Option<&Json>) -> bool {
    ::service_common::velocity_template::truthy(value)
}

/// Whether two values are EQUAL: numbers by their numeric value, strings, booleans, lists
/// and objects by structure, and an absent value only to another absent one.
#[must_use]
pub fn equal(left: Option<&Json>, right: Option<&Json>) -> bool {
    ::service_common::velocity_template::equal(left, right)
}

/// How two values ORDER, for `<`, `<=`, `>` and `>=`: numbers numerically and strings
/// lexicographically. Anything else cannot be ordered, which is a fault.
pub fn ordering(left: Option<&Json>, right: Option<&Json>) -> Result<::std::cmp::Ordering, String> {
    ::service_common::velocity_template::ordering(left, right)
}

/// What `+`, `-`, `*`, `/` and `%` come to.
///
/// Two numbers are arithmetic, and `+` with a string on either side concatenates — the
/// Java operator a template reaches for when it builds a key out of parts. Anything else is
/// a fault.
pub fn arithmetic(operator: char, left: &Json, right: &Json) -> Result<Json, String> {
    ::service_common::velocity_template::arithmetic(operator, left, right)
}

/// A computed number as the value a template carries: an integral result stays an integer,
/// so a count or an index renders as `3` rather than as `3.0`.
#[must_use]
pub fn number(produced: f64) -> Json {
    ::service_common::velocity_template::number(produced)
}

/// Call `name` on `target` with `args`, and answer what it returned — `None` for a method
/// that returns nothing.
///
/// `target` is borrowed MUTABLY because the collection methods of the language mutate the
/// value they are called on: `put` and `add` are how a template builds up a map or a list it
/// has in hand.
pub fn method(target: &mut Json, name: &str, args: &[Json]) -> Result<Option<Json>, String> {
    let arg = |index: usize| args.get(index).cloned().unwrap_or(Json::Null);
    let text = |index: usize| match args.get(index) {
        Some(Json::String(text)) => text.clone(),
        Some(other) => rendered(other),
        None => String::new(),
    };
    match (&mut *target, name) {
        // The members of a `Map`.
        (Json::Object(map), "get") => Ok(Some(map.get(&text(0)).cloned().unwrap_or(Json::Null))),
        (Json::Object(map), "put") => Ok(map.insert(text(0), arg(1))),
        (Json::Object(map), "putAll") => {
            if let Json::Object(entries) = arg(0) {
                for (key, value) in entries {
                    map.insert(key, value);
                }
            }
            Ok(None)
        }
        (Json::Object(map), "remove") => Ok(map.remove(&text(0))),
        (Json::Object(map), "containsKey") => Ok(Some(Json::Bool(map.contains_key(&text(0))))),
        (Json::Object(map), "keySet") => Ok(Some(Json::Array(
            map.keys().map(|key| Json::String(key.clone())).collect(),
        ))),
        (Json::Object(map), "values") => {
            Ok(Some(Json::Array(map.values().cloned().collect())))
        }
        // An entry of an `entrySet` is read as `$entry.key` / `$entry.value`, which is what
        // the two members of each object below are.
        (Json::Object(map), "entrySet") => Ok(Some(Json::Array(
            map.iter()
                .map(|(key, value)| {
                    let mut entry = JsonMap::new();
                    entry.insert("key".to_string(), Json::String(key.clone()));
                    entry.insert("value".to_string(), value.clone());
                    Json::Object(entry)
                })
                .collect(),
        ))),
        (Json::Object(map), "size") => Ok(Some(count(map.len()))),
        (Json::Object(map), "isEmpty") => Ok(Some(Json::Bool(map.is_empty()))),
        (Json::Object(map), "clear") => {
            map.clear();
            Ok(None)
        }
        // The members of a `List`.
        (Json::Array(list), "add") => {
            match args.len() {
                // `add(index, element)` inserts, `add(element)` appends.
                2 => {
                    let at = index_of(&arg(0), list.len())?;
                    list.insert(at, arg(1));
                    Ok(None)
                }
                _ => {
                    list.push(arg(0));
                    Ok(Some(Json::Bool(true)))
                }
            }
        }
        (Json::Array(list), "addAll") => {
            if let Json::Array(appended) = arg(0) {
                list.extend(appended);
            }
            Ok(Some(Json::Bool(true)))
        }
        (Json::Array(list), "get") => {
            let at = index_of(&arg(0), list.len().saturating_sub(1))?;
            Ok(Some(list.get(at).cloned().unwrap_or(Json::Null)))
        }
        (Json::Array(list), "set") => {
            let at = index_of(&arg(0), list.len().saturating_sub(1))?;
            let replaced = list.get(at).cloned();
            if let Some(slot) = list.get_mut(at) {
                *slot = arg(1);
            }
            Ok(replaced)
        }
        (Json::Array(list), "remove") => {
            let at = index_of(&arg(0), list.len().saturating_sub(1))?;
            Ok((at < list.len()).then(|| list.remove(at)))
        }
        (Json::Array(list), "contains") => Ok(Some(Json::Bool(
            list.iter().any(|element| equal(Some(element), Some(&arg(0)))),
        ))),
        (Json::Array(list), "indexOf") => Ok(Some(count_or_absent(
            list.iter()
                .position(|element| equal(Some(element), Some(&arg(0)))),
        ))),
        (Json::Array(list), "size") => Ok(Some(count(list.len()))),
        (Json::Array(list), "isEmpty") => Ok(Some(Json::Bool(list.is_empty()))),
        (Json::Array(list), "clear") => {
            list.clear();
            Ok(None)
        }
        // The members of a `String`.
        (Json::String(subject), "length" | "size") => Ok(Some(count(subject.chars().count()))),
        (Json::String(subject), "isEmpty") => Ok(Some(Json::Bool(subject.is_empty()))),
        (Json::String(subject), "toUpperCase") => Ok(Some(Json::String(subject.to_uppercase()))),
        (Json::String(subject), "toLowerCase") => Ok(Some(Json::String(subject.to_lowercase()))),
        (Json::String(subject), "trim") => Ok(Some(Json::String(subject.trim().to_string()))),
        (Json::String(subject), "substring") => {
            let characters: Vec<char> = subject.chars().collect();
            let from = index_of(&arg(0), characters.len())?;
            let to = match args.len() {
                0 | 1 => characters.len(),
                _ => index_of(&arg(1), characters.len())?,
            };
            if from > to || to > characters.len() {
                return Err(format!("{OUTSIDE_THE_STRING} {from}..{to}"));
            }
            Ok(Some(Json::String(characters[from..to].iter().collect())))
        }
        (Json::String(subject), "replace" | "replaceAll") => {
            Ok(Some(Json::String(subject.replace(&text(0), &text(1)))))
        }
        (Json::String(subject), "split") => Ok(Some(Json::Array(
            subject
                .split(text(0).as_str())
                .map(|part| Json::String(part.to_string()))
                .collect(),
        ))),
        (Json::String(subject), "contains") => Ok(Some(Json::Bool(subject.contains(&text(0))))),
        (Json::String(subject), "startsWith") => {
            Ok(Some(Json::Bool(subject.starts_with(&text(0)))))
        }
        (Json::String(subject), "endsWith") => Ok(Some(Json::Bool(subject.ends_with(&text(0))))),
        (Json::String(subject), "indexOf") => {
            Ok(Some(count_or_absent(subject.find(&text(0)).map(|at| {
                subject.get(..at).map_or(at, |head| head.chars().count())
            }))))
        }
        (Json::String(subject), "concat") => {
            Ok(Some(Json::String(format!("{subject}{}", text(0)))))
        }
        (Json::String(subject), "equals") => {
            Ok(Some(Json::Bool(Some(subject.as_str()) == args.first().and_then(Json::as_str))))
        }
        (Json::String(subject), "equalsIgnoreCase") => Ok(Some(Json::Bool(
            args.first()
                .and_then(Json::as_str)
                .is_some_and(|other| subject.eq_ignore_ascii_case(other)),
        ))),
        (Json::String(subject), "charAt") => {
            let at = index_of(&arg(0), subject.chars().count())?;
            Ok(Some(
                subject
                    .chars()
                    .nth(at)
                    .map_or(Json::Null, |character| Json::String(character.to_string())),
            ))
        }
        // Every value renders as text, which is what `toString` answers.
        (value, "toString") => Ok(Some(Json::String(rendered(value)))),
        (value, _) => Err(format!(
            "{NO_SUCH_METHOD}{name} on {}",
            kind_of(value)
        )),
    }
}

/// Whether `name` is a method this can call on `target` with no arguments, which is what a
/// property reference falls back to: `$list.size` is the `size()` of a list, as Velocity's
/// own property lookup reads it.
#[must_use]
pub fn is_zero_argument_method(target: &Json, name: &str) -> bool {
    matches!(
        (target, name),
        (
            Json::Object(_) | Json::Array(_) | Json::String(_),
            "size" | "isEmpty" | "keySet" | "values" | "entrySet" | "length"
        ) | (Json::String(_), "toUpperCase" | "toLowerCase" | "trim")
    )
}

/// A collection's size as the value a template carries.
fn count(size: usize) -> Json {
    Json::Number(i64::try_from(size).unwrap_or(i64::MAX).into())
}

/// An index a search answers with, or `-1` for the search that found nothing — which is
/// what `indexOf` answers in the language this emulates.
fn count_or_absent(found: Option<usize>) -> Json {
    found.map_or(Json::Number((-1).into()), count)
}

/// One index argument as an offset into a collection of `len` elements.
fn index_of(value: &Json, len: usize) -> Result<usize, String> {
    let Some(index) = value.as_i64() else {
        return Err(format!("{NOT_AN_INDEX} {}", rendered(value)));
    };
    usize::try_from(index)
        .ok()
        .filter(|at| *at <= len)
        .ok_or_else(|| format!("{NOT_AN_INDEX} {index}"))
}

/// What a value IS, as a fault names it.
fn kind_of(value: &Json) -> &'static str {
    match value {
        Json::Null => "null",
        Json::Bool(_) => "a boolean",
        Json::Number(_) => "a number",
        Json::String(_) => "a string",
        Json::Array(_) => "a list",
        Json::Object(_) => "a map",
    }
}

/// The sentences a value fault states. The three the language's own OPERATORS state are
/// the tree's one reading of them ([`::service_common::velocity_template`]).
const OUTSIDE_THE_STRING: &str = "This substring is outside the string:";
const NOT_AN_INDEX: &str = "This is not an index into the value:";
const NO_SUCH_METHOD: &str = "No such method: ";
