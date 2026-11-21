// SPDX-License-Identifier: CC0-1.0

//! The VALUES a mapping template operates on, and what the language does with them:
//! how one is written into the rendered text, whether it is true, how two of them compare,
//! and the methods a reference may call on one.
//!
//! A template's values are the ones its context carries, which arrive as JSON, so JSON is
//! the value model here. The Java types Velocity itself works over map onto it directly: an
//! object is a `Map`, an array a `List`, and a string, a number and a boolean their own
//! scalars — which is why the methods a reference may call on one are the `Map`, `List` and
//! `String` members.
//!
//! Every rule below is the LANGUAGE's rather than this service's, so each is the tree's one
//! reading of it ([`::service_common::velocity_template`]) and this module is where this
//! service's own POLICY over them lives: a method that reading does not answer is a FAULT
//! rather than a silently empty value — the field that asked for it is reported as one that did
//! not resolve, instead of being answered with something nothing computed.

use ::serde_json::Value as Json;

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
///
/// The METHODS, like the operators above, are the LANGUAGE's over its value model, so they are
/// the tree's one reading of them ([`::service_common::velocity_template`]). What stays here is
/// this service's own policy: a method that reading does not answer is a FAULT, reported as a
/// field that did not resolve.
pub fn method(target: &mut Json, name: &str, args: &[Json]) -> Result<Option<Json>, String> {
    ::service_common::velocity_template::method(target, name, args)
}

/// Whether `name` is a method this can call on `target` with no arguments, which is what a
/// property reference falls back to: `$list.size` is the `size()` of a list, as Velocity's
/// own property lookup reads it.
#[must_use]
pub fn is_zero_argument_method(target: &Json, name: &str) -> bool {
    ::service_common::velocity_template::is_zero_argument_method(target, name)
}
