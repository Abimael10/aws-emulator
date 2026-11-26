// SPDX-License-Identifier: CC0-1.0

//! The `$util.list` and `$util.map` helpers a mapping template may call — the list and map
//! families of the AppSync resolver mapping-template utility reference.
//!
//! Both are COLLECTION families, and both answer a COPY: "returns a copy of the list [map]"
//! is what every one of them states, so the value the template was handed is never changed
//! by a call — a helper here reads its arguments and builds a new value, which is what lets
//! a template narrow a filter it was given without disturbing the context it came from.
//!
//! Membership is the LANGUAGE's own equality ([`super::values::equal`]), decided through a key
//! equal values share ([`membership_key`]) so the work of a call is proportional to the
//! arguments together rather than to their product; a key is named by its text. A value of the
//! wrong kind — a list helper handed something that is no list — is a FAULT naming it rather
//! than an empty collection, as everywhere else in this language.

use ::serde_json::{Map as JsonMap, Value as Json};

use super::values;

/// What one `$util.list.<helper>` call came to, or `None` when `helper` names no helper of
/// the list family — which the registry ([`super::utilities`]) reports as the fault naming
/// it.
///
/// The family is `copyAndRetainAll` ("returns a copy of the list that retains only the
/// elements specified"), `copyAndRemoveAll` ("returns a copy of the list with the elements
/// specified removed") and `sortList` ("sorts a list of objects … given the desired
/// ordering and a property to sort on").
pub fn list_call(helper: &str, args: &[Json]) -> Result<Option<Json>, String> {
    let produced = match helper {
        "copyAndRetainAll" => filtered(args, true)?,
        "copyAndRemoveAll" => filtered(args, false)?,
        "sortList" => sorted(args)?,
        _ => return Ok(None),
    };
    // The copy holds as much as what it was made from, so it is weighed against the same
    // two memory bounds the rest of an evaluation is.
    super::weigh(&produced, 0)?;
    Ok(Some(produced))
}

/// What one `$util.map.<helper>` call came to, or `None` when `helper` names no helper of
/// the map family.
///
/// The family is `copyAndRetainAllKeys` ("returns a copy of the map that retains only the
/// keys specified") and `copyAndRemoveAllKeys` ("returns a copy of the map with the keys
/// specified removed").
pub fn map_call(helper: &str, args: &[Json]) -> Result<Option<Json>, String> {
    let produced = match helper {
        "copyAndRetainAllKeys" => keyed(args, true)?,
        "copyAndRemoveAllKeys" => keyed(args, false)?,
        _ => return Ok(None),
    };
    super::weigh(&produced, 0)?;
    Ok(Some(produced))
}

/// A copy of the list the call was handed holding the elements the second argument names —
/// or, for `retain` false, the ones it does not.
///
/// Membership is decided by a LOOKUP rather than by a scan: the named list is indexed once
/// under the key equal values share ([`membership_key`]), so the work is proportional to the
/// two lists together rather than to their product — neither of which the caller bounds.
fn filtered(args: &[Json], retain: bool) -> Result<Json, String> {
    let elements = list_at(args, 0)?;
    let named = list_at(args, 1)?;
    let index: ::std::collections::BTreeSet<String> = named
        .iter()
        .map(|candidate| membership_key(candidate, 0))
        .collect();
    Ok(Json::Array(
        elements
            .iter()
            .filter(|element| index.contains(&membership_key(element, 0)) == retain)
            .cloned()
            .collect(),
    ))
}

/// A copy of the map the call was handed holding the members the second argument names by
/// key — or, for `retain` false, the ones it does not. Indexed the same way [`filtered`] is,
/// for the same reason.
fn keyed(args: &[Json], retain: bool) -> Result<Json, String> {
    let Some(Json::Object(members)) = args.first() else {
        return Err(format!("{NOT_A_MAP} {}", rendered_at(args, 0)));
    };
    let named = list_at(args, 1)?;
    let names: ::std::collections::BTreeSet<String> = named
        .iter()
        .map(|key| match key {
            Json::String(text) => text.clone(),
            other => values::rendered(other),
        })
        .collect();
    let mut copied = JsonMap::new();
    for (name, member) in members {
        if names.contains(name) == retain {
            copied.insert(name.clone(), member.clone());
        }
    }
    Ok(Json::Object(copied))
}

/// The key one element is held under while membership is decided: a canonical writing of the
/// value, so whether a list holds an element costs one lookup.
///
/// Equal values share a key, by the same reading of equality the rest of this language has
/// ([`values::equal`]): a number is keyed by its NUMERIC value however it was written, so `1`
/// and `1.0` are one element, and every other value by its own structure, with a map's members
/// written in NAME order so two maps holding the same members share one key. The walk stops at
/// the depth an evaluation may hold a value to, past which nothing of this language reaches it.
fn membership_key(value: &Json, depth: usize) -> String {
    if depth > MAX_MEMBERSHIP_DEPTH {
        return String::new();
    }
    match value {
        Json::Number(_) => format!("n{}", value.as_f64().unwrap_or_default()),
        Json::Null => "z".to_string(),
        Json::Bool(decided) => format!("b{decided}"),
        Json::String(text) => format!("s{text}"),
        Json::Array(elements) => format!(
            "l[{}]",
            elements
                .iter()
                .map(|element| membership_key(element, depth.saturating_add(1)))
                .collect::<Vec<String>>()
                .join(",")
        ),
        Json::Object(members) => {
            let mut written: Vec<String> = members
                .iter()
                .map(|(name, member)| {
                    format!("{name}={}", membership_key(member, depth.saturating_add(1)))
                })
                .collect();
            written.sort();
            format!("m{{{}}}", written.join(","))
        }
    }
}

/// How deeply a membership key is written: at or above the depth an evaluation may hold a
/// value to, so no value of this language reaches the bound and the key is a total one.
const MAX_MEMBERSHIP_DEPTH: usize = 32;

/// A SORTED copy of the list the call was handed: by the property the third argument names
/// where the elements are maps, and by the elements themselves where it names none, in
/// descending order when the second argument is true.
///
/// The order the elements are put in is a TOTAL one, because a list a template built can hold
/// values of different kinds and two of them need not be comparable: the kind decides first
/// ([`kind_rank`]) and the values decide within one kind, so every pair orders one way and no
/// three of them disagree. Two values of one kind this language cannot order at all — two maps
/// — keep the position they were in, which is what makes the sort stable rather than
/// arbitrary.
fn sorted(args: &[Json]) -> Result<Json, String> {
    let elements = list_at(args, 0)?;
    let descending = values::truthy(args.get(1));
    let property = match args.get(2) {
        Some(Json::String(name)) if !name.is_empty() => Some(name.clone()),
        _ => None,
    };
    let sorting_key = |element: &Json| -> Json {
        match &property {
            Some(name) => element.get(name).cloned().unwrap_or(Json::Null),
            None => element.clone(),
        }
    };
    // The keys are computed once, beside the position each element came in at, so the
    // comparison itself reads two values and nothing else.
    let mut keyed: Vec<(usize, Json, Json)> = elements
        .iter()
        .enumerate()
        .map(|(at, element)| (at, sorting_key(element), element.clone()))
        .collect();
    keyed.sort_by(|(left_at, left, _), (right_at, right, _)| {
        let ordered = kind_rank(left).cmp(&kind_rank(right)).then_with(|| {
            values::ordering(Some(left), Some(right)).unwrap_or(::std::cmp::Ordering::Equal)
        });
        let ordered = if descending { ordered.reverse() } else { ordered };
        // The position breaks every remaining tie, so the comparison is a total order over
        // the whole list whatever kinds of value it holds.
        ordered.then(left_at.cmp(right_at))
    });
    Ok(Json::Array(
        keyed.into_iter().map(|(_, _, element)| element).collect(),
    ))
}

/// Which KIND of value this is, as the rank that orders two values of different kinds: the
/// order JSON's own kinds are written in. It decides a pair this language cannot compare
/// ([`values::ordering`] answers only for two numbers or two strings) without leaving the
/// comparison undecided, which a sort needs.
fn kind_rank(value: &Json) -> u8 {
    match value {
        Json::Null => 0,
        Json::Bool(_) => 1,
        Json::Number(_) => 2,
        Json::String(_) => 3,
        Json::Array(_) => 4,
        Json::Object(_) => 5,
    }
}

/// The LIST the argument at `index` carries. An absent argument is the empty list — a call
/// naming nothing to retain retains nothing — and a value that is no list is nothing a
/// collection helper can copy.
fn list_at(args: &[Json], index: usize) -> Result<Vec<Json>, String> {
    match args.get(index) {
        Some(Json::Array(elements)) => Ok(elements.clone()),
        None | Some(Json::Null) => Ok(Vec::new()),
        Some(other) => Err(format!("{NOT_A_LIST} {}", values::rendered(other))),
    }
}

/// The argument at `index` as the text this language renders it as, for a fault that names
/// what it was handed.
fn rendered_at(args: &[Json], index: usize) -> String {
    args.get(index).map_or_else(String::new, values::rendered)
}

/// The sentences a fault of either family states, worded as the marshalling helpers of this
/// language word the same two faults.
const NOT_A_LIST: &str = "This value is not a list:";
const NOT_A_MAP: &str = "This value is not a map:";
