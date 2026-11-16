// SPDX-License-Identifier: CC0-1.0

//! The KEY one entry of a stage's `methodSettings` map is held under, derived from the
//! method it names rather than from the spelling that addressed it.
//!
//! `Stage$methodSettings` is "a map that defines the method settings for a Stage resource.
//! Keys … are method paths defined as `{resource_path}/{http_method}` for an individual
//! method override, or `/*/*` for overriding all methods in the stage". So one key names one
//! method: the resource path, then a real separator, then the HTTP method — with every
//! separator of the RESOURCE path written as the JSON Pointer escape ([`ESCAPED_SEPARATOR`])
//! the patch path that addressed it spells one with, and the wildcard form standing for every
//! method of the stage.
//!
//! A `UpdateStage` patch may spell the same method several ways — the resource path escaped
//! into one reference token or its leading separator left to the pointer's own, the separator
//! before the HTTP method escaped as well so the whole method path rides in ONE token — and
//! every one of them addresses THAT method, so every one of them resolves here to the single
//! key the map holds the method's settings under. That is what makes a setting written one
//! way read back under the documented key, and what keeps one method from holding two
//! entries.
//!
//! The same key is what a reader of the stage reports and what the `execute-api` data plane
//! resolves a served call's settings by, so one spelling of a method cannot write a setting
//! another read cannot find.
//!
//! Nothing here reads or writes this service's records: the key is a function of two values.

/// The JSON Pointer escape a `methodSettings` key spells a separator of the RESOURCE path
/// with: the key for `GET` on `/pets` is `~1pets/GET`, so the one separator that stays a real
/// one is the one before the HTTP method.
const ESCAPED_SEPARATOR: &str = "~1";

/// The resource half of the key that stands for EVERY resource — and, with the same token as
/// the method half, for every method — of the stage: `Stage$methodSettings` documents that
/// whole-stage key as `/*/*`, which is `*/*` as a key.
pub const EVERY_RESOURCE: &str = "*";

/// The `methodSettings` key the method `http_method` of `resource_path` is held under.
///
/// `resource_path` is the resource path as a reader of it has it — decoded, so its separators
/// are real ones — and is made absolute before it is escaped, because a resource path "begins
/// with a '/'" (`Resource$path`): a caller that left that leading separator to the pointer's
/// own addressed the same resource as one that escaped it, and both are keyed the one way. The
/// wildcard is a token rather than a path, so it is carried through as it stands under either
/// spelling of that leading separator — `*` and `/*` are the one whole-stage resource, which is
/// how `MethodSetting$ResourcePath` states it ("the initial slash must include a forward
/// slash").
#[must_use]
pub fn method_settings_key(resource_path: &str, http_method: &str) -> String {
    let unprefixed = resource_path.strip_prefix('/').unwrap_or(resource_path);
    if unprefixed == EVERY_RESOURCE {
        return format!("{EVERY_RESOURCE}/{http_method}");
    }
    let absolute = match resource_path.starts_with('/') {
        true => resource_path.to_string(),
        false => format!("/{resource_path}"),
    };
    format!(
        "{}/{http_method}",
        absolute.replace('/', ESCAPED_SEPARATOR)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a key is `{resource_path}/{http_method}` with every separator of the resource
    /// path escaped and the one before the method left real, whatever spelling of the resource
    /// path it is derived from, and `*/*` for every method of the stage.
    #[test]
    fn a_key_names_the_method_however_its_resource_path_was_spelled() {
        // Rows span the DEPTH of the resource path (the root, a child, a nested one), both
        // spellings of its leading separator, and the wildcard that stands for every method
        // — which a caller spells with that separator or without it, as the two readers of a
        // method path hand it over.
        let rows: [(&str, &str, &str, &str); 10] = [
            ("a child resource", "/pets", "GET", "~1pets/GET"),
            ("a child resource, leading separator left out", "pets", "GET", "~1pets/GET"),
            ("a nested resource", "/pets/{petId}", "DELETE", "~1pets~1{petId}/DELETE"),
            (
                "a nested resource, leading separator left out",
                "pets/{petId}",
                "DELETE",
                "~1pets~1{petId}/DELETE",
            ),
            ("the root resource", "/", "GET", "~1/GET"),
            ("the root resource, leading separator left out", "", "GET", "~1/GET"),
            ("every method of the stage", "*", "*", "*/*"),
            (
                "every method of the stage, leading separator included",
                "/*",
                "*",
                "*/*",
            ),
            ("every resource at one verb", "*", "PUT", "*/PUT"),
            ("every resource at one verb, leading separator included", "/*", "PUT", "*/PUT"),
        ];

        for (case, resource_path, http_method, expected) in rows {
            let actual = method_settings_key(resource_path, http_method);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }
}
