// SPDX-License-Identifier: CC0-1.0

//! The KEY one entry of a stage's `methodSettings` map is held under, and the METHOD that key
//! denotes.
//!
//! `Stage$methodSettings` is "a map that defines the method settings for a Stage resource.
//! Keys (designated as /{method_setting_key) are method paths defined as
//! `{resource_path}/{http_method}` for an individual method override, or `/*/*` for
//! overriding all methods in the stage". So one key is a METHOD PATH: a resource path, then a
//! separator, then the HTTP method.
//!
//! The key is the method path AS THE PATCH POINTER SPELLED IT — `UpdateStage` keys the entry
//! by the reference tokens its path carried before the setting, written exactly as the request
//! wrote them (see `crate::ops::update_stage`) — so a client reads an entry back under the
//! very key it patched. A pointer that carries the whole method path in ONE reference token is
//! the exception, because such a token has to be DECODED to be read as a method path at all:
//! what it decodes to is keyed as the documented method path, the resource path without the
//! leading separator a resource path is stated with. Two pointers that spell one method
//! differently therefore leave two entries, which is a stage API Gateway refuses to UPDATE
//! until one of them is taken away.
//!
//! What a key DENOTES is a different question from how it is spelled: the resource path with
//! its escapes decoded and its leading separator restored, and the HTTP method after the last
//! literal separator ([`method_of_settings_key`]). That is the method identity the `execute-api`
//! data plane resolves a served call's settings by ([`method_settings_in_force`]) and the one
//! two keys of a stage are compared on ([`same_method`]), so one spelling of a method cannot
//! write a setting a served call resolves differently.
//!
//! Nothing here reads or writes this service's records: a key is read as the values it carries.

use std::collections::BTreeMap;

use super::patches::unescape_pointer_token;
use crate::ops::types::MethodSetting;

/// The resource half of the key that stands for EVERY resource — and, with the same token as
/// the method half, for every method — of the stage: `Stage$methodSettings` documents that
/// whole-stage key as `/*/*`, which is `*/*` as a key.
pub const EVERY_RESOURCE: &str = "*";

/// The value either THROTTLE limit of a method setting takes to mean that the method is not
/// throttled at all.
///
/// `MethodSetting$throttlingBurstLimit` is a count of requests and
/// `MethodSetting$throttlingRateLimit` a requests-per-second rate, and neither carries a range
/// in the model; API Gateway takes `-1` on either as the ask to meter the method by no limit
/// of its own, answers it with 200 and reports it back, so it is a legal, persisted value of
/// the setting rather than a count or a rate. It is the ONE negative value either limit takes:
/// any other negative number is neither a count nor a rate, and is refused
/// ([`accepts_method_throttle_burst`]).
pub const UNTHROTTLED_LIMIT: i32 = -1;

/// Whether `burst_limit` is a burst a method setting takes: a count of requests, so it cannot
/// be negative — except for the one value that asks for NO throttle
/// ([`UNTHROTTLED_LIMIT`]).
///
/// The per-method setting is the surface that takes the sentinel; the throttle a usage plan
/// carries is judged by the count rule alone ([`super::accepts_throttle_burst`]).
#[must_use]
pub fn accepts_method_throttle_burst(burst_limit: i32) -> bool {
    burst_limit == UNTHROTTLED_LIMIT || super::accepts_throttle_burst(burst_limit)
}

/// Whether `rate_limit` is a rate a method setting takes: a requests-per-second rate, so it has
/// to be finite and cannot be negative — except for the sentinel
/// [`accepts_method_throttle_burst`] admits on the burst, which is the same value here.
#[must_use]
pub fn accepts_method_throttle_rate(rate_limit: f64) -> bool {
    rate_limit == f64::from(UNTHROTTLED_LIMIT) || super::accepts_throttle_rate(rate_limit)
}

/// The `methodSettings` key a method path DECODED out of one reference token is reported under:
/// the method path `Stage$methodSettings` documents, which is the resource path without the
/// leading separator a resource path is stated with, then a literal separator and the HTTP
/// method (`pets/GET`, `*/*`).
///
/// A pointer that carries the whole method path in one token has to be decoded to be read as a
/// method path at all, so what it writes is keyed by the method path itself rather than by the
/// token's spelling. A pointer that spells the method in its OWN tokens is keyed by those
/// tokens as written instead (`crate::ops::update_stage`).
#[must_use]
pub fn documented_method_path(method_path: &str) -> String {
    method_path
        .strip_prefix('/')
        .map_or_else(|| method_path.to_string(), ToString::to_string)
}

/// The METHOD a `methodSettings` key denotes: the resource path it names, and the HTTP method.
///
/// A key is a method path — "method paths defined as `{resource_path}/{http_method}`"
/// (`Stage$methodSettings`) — so the HTTP method is what follows the key's LAST literal
/// separator and the resource half is everything before it. That half may be a path NAME, the
/// spelling `PatchOperation$path` requires ("Any slash ("/") character appearing in path names
/// must be escaped with "~1""), so it is decoded here; and a resource path "begins with a '/'"
/// (`Resource$path`), so a half that was written without that leading separator denotes the
/// same resource path as one that carried it.
///
/// The wildcard is the one half that names no resource path: it is carried over as it stands,
/// with or without the leading separator `Stage$methodSettings` writes the whole-stage path
/// `/*/*` with.
///
/// `None` for a key that is no method path at all — one carrying no separator — which no
/// pointer of this service writes.
#[must_use]
pub fn method_of_settings_key(key: &str) -> Option<(String, String)> {
    let (resource, http_method) = key.rsplit_once('/')?;
    Some((
        denoted_resource_path(&unescape_pointer_token(resource)),
        http_method.to_string(),
    ))
}

/// The resource path a key's (decoded) resource half denotes: the wildcard as it stands, and
/// every other path with the leading separator a resource path begins with.
fn denoted_resource_path(resource: &str) -> String {
    if resource == EVERY_RESOURCE || resource.strip_prefix('/') == Some(EVERY_RESOURCE) {
        return EVERY_RESOURCE.to_string();
    }
    match resource.strip_prefix('/') {
        Some(_) => resource.to_string(),
        None => format!("/{resource}"),
    }
}

/// Whether the two keys `left` and `right` denote the SAME method, which is what makes two
/// method-setting entries of one stage duplicates of each other.
#[must_use]
pub fn same_method(left: &str, right: &str) -> bool {
    match (method_of_settings_key(left), method_of_settings_key(right)) {
        (Some(left), Some(right)) => left == right,
        // A string that is no method path denotes no method, so it collides with nothing but
        // itself.
        _ => left == right,
    }
}

/// The method settings in force for the method `http_method` of `resource_path`: the entry
/// whose key DENOTES that method, else the entry that names every method of the stage, else
/// none.
///
/// Resolved by the method a key denotes rather than by the key's spelling
/// ([`method_of_settings_key`]), so a setting written under any spelling of a method is the
/// setting a served call of that method resolves. The whole-stage entry is the fallback, which
/// is what makes a `/*/*/caching/enabled` patch cache every method of the stage.
#[must_use]
pub fn method_settings_in_force<'m>(
    settings: &'m BTreeMap<String, MethodSetting>,
    resource_path: &str,
    http_method: &str,
) -> Option<&'m MethodSetting> {
    let addressed = (
        denoted_resource_path(resource_path),
        http_method.to_string(),
    );
    let whole_stage = (EVERY_RESOURCE.to_string(), EVERY_RESOURCE.to_string());
    settings_denoting(settings, &addressed).or_else(|| settings_denoting(settings, &whole_stage))
}

/// The first entry of `settings`, in key order, whose key denotes `method`.
fn settings_denoting<'m>(
    settings: &'m BTreeMap<String, MethodSetting>,
    method: &(String, String),
) -> Option<&'m MethodSetting> {
    settings
        .iter()
        .find(|(key, _)| method_of_settings_key(key).as_ref() == Some(method))
        .map(|(_, setting)| setting)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a method setting's throttle limit takes any count (or rate) and the one sentinel
    /// that asks for no throttle at all, and nothing else negative.
    #[test]
    fn a_method_throttle_takes_the_unthrottled_sentinel_and_no_other_negative() {
        let bursts: [(&str, i32, bool); 6] = [
            ("the unthrottled sentinel", UNTHROTTLED_LIMIT, true),
            ("no burst at all", 0, true),
            ("the documented account default", 5_000, true),
            ("the largest count the type holds", i32::MAX, true),
            ("one past the sentinel", -2, false),
            ("a far negative count", -100_000, false),
        ];
        for (case, burst_limit, expected) in bursts {
            let actual = accepts_method_throttle_burst(burst_limit);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected}, got {actual} for {burst_limit}"
            );
        }

        let rates: [(&str, f64, bool); 7] = [
            ("the unthrottled sentinel", -1.0, true),
            ("no rate at all", 0.0, true),
            ("a fractional rate", 0.5, true),
            ("the documented account default", 10_000.0, true),
            ("just past the sentinel", -1.5, false),
            ("an infinite rate", f64::INFINITY, false),
            ("a rate that is not a number", f64::NAN, false),
        ];
        for (case, rate_limit, expected) in rates {
            let actual = accepts_method_throttle_rate(rate_limit);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected}, got {actual} for {rate_limit}"
            );
        }
    }

    /// RULE: a method path decoded out of one reference token is keyed as the method path
    /// `Stage$methodSettings` documents: the resource path without its leading separator, then
    /// the HTTP method.
    #[test]
    fn a_decoded_method_path_is_keyed_as_the_documented_one() {
        let rows: [(&str, &str, &str); 5] = [
            ("a child resource", "/pets/GET", "pets/GET"),
            (
                "a nested resource",
                "/pets/{petId}/DELETE",
                "pets/{petId}/DELETE",
            ),
            ("the root resource", "//GET", "/GET"),
            ("every method of the stage", "*/*", "*/*"),
            (
                "every method of the stage, the separator included",
                "/*/*",
                "*/*",
            ),
        ];

        for (case, method_path, expected) in rows {
            let actual = documented_method_path(method_path);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: a key denotes the method `{resource_path}/{http_method}` — the HTTP method after
    /// the key's last literal separator, and the resource path before it decoded and begun
    /// with the separator a resource path begins with — whatever spelling of that method path
    /// the key carries, and every method of the stage for the wildcard.
    #[test]
    fn a_key_denotes_the_method_its_spelling_names() {
        // Rows span the DEPTH of the resource path (the root, a child, a nested one), each
        // spelling one pointer may key a method by (the leading separator escaped, left to the
        // pointer's own, or carried whole in one decoded token), and the wildcard halves.
        let rows: [(&str, &str, &str, &str); 11] = [
            ("a child resource, escaped", "~1pets/GET", "/pets", "GET"),
            ("a child resource, unescaped", "pets/GET", "/pets", "GET"),
            (
                "a child resource, the pointer's own separator",
                "/pets/GET",
                "/pets",
                "GET",
            ),
            (
                "a nested resource, escaped",
                "~1pets~1{petId}/DELETE",
                "/pets/{petId}",
                "DELETE",
            ),
            (
                "a nested resource, decoded out of one token",
                "pets/{petId}/DELETE",
                "/pets/{petId}",
                "DELETE",
            ),
            ("the root resource, escaped", "~1/GET", "/", "GET"),
            (
                "the root resource, the pointer's own separator",
                "/GET",
                "/",
                "GET",
            ),
            ("every method of the stage", "*/*", "*", "*"),
            (
                "every method of the stage, leading separator included",
                "/*/*",
                "*",
                "*",
            ),
            ("every resource at one verb", "*/PUT", "*", "PUT"),
            (
                "a resource whose name carries a tilde",
                "~1a~0b/GET",
                "/a~b",
                "GET",
            ),
        ];

        for (case, key, resource_path, http_method) in rows {
            let actual = method_of_settings_key(key);
            let expected = Some((resource_path.to_string(), http_method.to_string()));
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: two keys are the same method setting when they DENOTE the same method, however
    /// differently they are spelled.
    #[test]
    fn keys_that_denote_one_method_collide() {
        let rows: [(&str, &str, &str, bool); 6] = [
            (
                "two spellings of one method",
                "~1pets/GET",
                "/pets/GET",
                true,
            ),
            (
                "the same method written whole in one token",
                "~1pets/GET",
                "pets/GET",
                true,
            ),
            (
                "two verbs of one resource",
                "~1pets/GET",
                "~1pets/POST",
                false,
            ),
            (
                "two resources at one verb",
                "~1pets/GET",
                "~1orders/GET",
                false,
            ),
            ("the whole-stage key, both spellings", "*/*", "/*/*", true),
            (
                "the whole-stage key and one method",
                "*/*",
                "~1pets/GET",
                false,
            ),
        ];

        for (case, left, right, expected) in rows {
            let actual = same_method(left, right);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected}, got {actual}"
            );
        }
    }
}
