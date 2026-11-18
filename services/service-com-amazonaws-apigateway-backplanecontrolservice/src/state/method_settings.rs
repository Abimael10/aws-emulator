// SPDX-License-Identifier: CC0-1.0

//! The KEY one entry of a stage's `methodSettings` map is held under, derived from the
//! method it names rather than from the spelling that addressed it.
//!
//! `Stage$methodSettings` is "a map that defines the method settings for a Stage resource.
//! Keys (designated as /{method_setting_key) are method paths defined as
//! `{resource_path}/{http_method}` for an individual method override, or `/*/*` for
//! overriding all methods in the stage". So one key names one method: the resource path, a
//! real separator, then the HTTP method — and because the key is the `{method_setting_key}`
//! of the patch PATH that addresses it, every separator INSIDE the resource path is written
//! the way `PatchOperation$path` requires ("Any slash ("/") character appearing in path names
//! must be escaped with "~1""), the resource path's own LEADING separator included. The
//! documented whole-stage path `/*/*` names no resource path, so it is keyed `*/*`.
//!
//! A `UpdateStage` patch may spell the same method several ways — the escape may stand for
//! the leading separator or be left to the pointer's own, and the resource path and the HTTP
//! method may ride in one reference token or two — and every one of them addresses THAT
//! method, so every one of them resolves here to the single key the map holds the method's
//! settings under: a setting written under any spelling reads back under the documented one,
//! and one method cannot hold two entries.
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

/// Write one reference token the way a patch `path` spells it: the inverse of decoding one
/// ([`super::unescape_pointer_token`]), which is the one spelling of that escape this tree
/// keeps ([`::service_common::json_pointer::escaped_segment`]).
fn escape_pointer_token(token: &str) -> String {
    ::service_common::json_pointer::escaped_segment(token)
}

/// The `methodSettings` key the method `http_method` of `resource_path` is held under.
///
/// `resource_path` is the resource path as a reader of it has it — decoded, so its separators
/// are real ones. A resource path "begins with a '/'" (`Resource$path`), and the key carries
/// that whole path as ONE reference token of the method path: every separator in it, the
/// leading one included, is escaped `~1` as `PatchOperation$path` spells a separator inside a
/// path name, and only the separator BEFORE the HTTP method is a real one. A caller that left
/// the leading separator to the pointer's own therefore addressed the same resource as one
/// that escaped it, and both are keyed the one way.
///
/// The wildcard is the one key that names no resource path: `Stage$methodSettings` documents
/// the whole-stage method path `/*/*` and keys it `*/*`, so the resource half stays the bare
/// token however a caller spelled its leading separator — which is how
/// `MethodSetting$ResourcePath` states it ("the initial slash must include a forward slash").
#[must_use]
pub fn method_settings_key(resource_path: &str, http_method: &str) -> String {
    let unprefixed = resource_path.strip_prefix('/').unwrap_or(resource_path);
    if unprefixed == EVERY_RESOURCE {
        return format!("{EVERY_RESOURCE}/{http_method}");
    }
    let escaped = escape_pointer_token(&format!("/{unprefixed}"));
    format!("{escaped}/{http_method}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a key is the method path `{resource_path}/{http_method}` with the resource
    /// path carried as one reference token — every separator in it escaped `~1`, the leading
    /// one included — whatever spelling of the resource path it is derived from, and `*/*`
    /// for every method of the stage.
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
