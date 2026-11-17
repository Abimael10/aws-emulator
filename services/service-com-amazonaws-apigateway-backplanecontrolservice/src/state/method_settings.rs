// SPDX-License-Identifier: CC0-1.0

//! The KEY one entry of a stage's `methodSettings` map is held under, derived from the
//! method it names rather than from the spelling that addressed it.
//!
//! `Stage$methodSettings` is "a map that defines the method settings for a Stage resource.
//! Keys … are method paths defined as `{resource_path}/{http_method}` for an individual
//! method override, or `/*/*` for overriding all methods in the stage". So one key names one
//! method: the resource path, a real separator, then the HTTP method — written the way the
//! documented whole-stage key `/*/*` is written as the key `*/*`, with the resource path's
//! own LEADING separator dropped and every separator inside it left a real one.
//!
//! A `UpdateStage` patch may spell the same method several ways — the JSON Pointer escape
//! `~1` stands for a separator of the resource path inside one reference token, and the
//! resource path and the HTTP method may ride in one token or two — and every one of them
//! addresses THAT method, so every one of them resolves here to the single key the map holds
//! the method's settings under. The escape belongs to the patch PATH alone: it never reaches
//! a key, so a setting written under any spelling reads back under the documented one, and
//! one method cannot hold two entries.
//!
//! The same key is what a reader of the stage reports and what the `execute-api` data plane
//! resolves a served call's settings by, so one spelling of a method cannot write a setting
//! another read cannot find.
//!
//! Nothing here reads or writes this service's records: the key is a function of two values.

/// The resource half of the key that stands for EVERY resource — and, with the same token as
/// the method half, for every method — of the stage: `Stage$methodSettings` documents that
/// whole-stage key as `/*/*`, which is `*/*` as a key.
pub const EVERY_RESOURCE: &str = "*";

/// The `methodSettings` key the method `http_method` of `resource_path` is held under.
///
/// `resource_path` is the resource path as a reader of it has it — decoded, so its separators
/// are real ones. A resource path "begins with a '/'" (`Resource$path`) and the key drops that
/// leading separator, exactly as the documented whole-stage method path `/*/*` is keyed `*/*`:
/// a caller that left the leading separator to the pointer's own addressed the same resource
/// as one that escaped it, and both are keyed the one way. The wildcard needs no case of its
/// own — dropping the leading separator carries `*` and `/*` to the same whole-stage resource,
/// which is how `MethodSetting$ResourcePath` states it ("the initial slash must include a
/// forward slash").
#[must_use]
pub fn method_settings_key(resource_path: &str, http_method: &str) -> String {
    let unprefixed = resource_path.strip_prefix('/').unwrap_or(resource_path);
    format!("{unprefixed}/{http_method}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a key is the method path `{resource_path}/{http_method}` with the resource
    /// path's separators real and its leading one dropped, whatever spelling of the resource
    /// path it is derived from, and `*/*` for every method of the stage.
    #[test]
    fn a_key_names_the_method_however_its_resource_path_was_spelled() {
        // Rows span the DEPTH of the resource path (the root, a child, a nested one), both
        // spellings of its leading separator, and the wildcard that stands for every method
        // — which a caller spells with that separator or without it, as the two readers of a
        // method path hand it over.
        let rows: [(&str, &str, &str, &str); 10] = [
            ("a child resource", "/pets", "GET", "pets/GET"),
            ("a child resource, leading separator left out", "pets", "GET", "pets/GET"),
            ("a nested resource", "/pets/{petId}", "DELETE", "pets/{petId}/DELETE"),
            (
                "a nested resource, leading separator left out",
                "pets/{petId}",
                "DELETE",
                "pets/{petId}/DELETE",
            ),
            ("the root resource", "/", "GET", "/GET"),
            ("the root resource, leading separator left out", "", "GET", "/GET"),
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
