// SPDX-License-Identifier: CC0-1.0

//! The KEY one entry of a stage's `methodSettings` map is held under, derived from the
//! method it names rather than from the spelling that addressed it.
//!
//! `Stage$methodSettings` is "a map that defines the method settings for a Stage resource.
//! Keys (designated as /{method_setting_key) are method paths defined as
//! `{resource_path}/{http_method}` for an individual method override, or `/*/*` for
//! overriding all methods in the stage". So one key is the METHOD PATH: the resource path
//! without the leading separator a resource path is stated with, then a separator, then the
//! HTTP method — every separator in it a LITERAL one. A method of the resource `/pets` is
//! therefore keyed `pets/GET`, a nested one `pets/{petId}/DELETE`, and the documented
//! whole-stage path `/*/*` — whose resource half names no resource path at all — `*/*`.
//!
//! The `~1` escape belongs to `PatchOperation$path` alone ("Any slash ("/") character
//! appearing in path names must be escaped with "~1""): it is how a patch POINTER spells a
//! key, not part of the key. An `UpdateStage` patch may therefore spell the same method
//! several ways — the escape may stand for the leading separator or be left to the pointer's
//! own, and the resource path and the HTTP method may ride in one reference token or two —
//! and every one of them addresses THAT method, so every one of them resolves here to the
//! single key the map holds the method's settings under: a setting written under any spelling
//! reads back under the documented one, and one method cannot hold two entries.
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
/// are real ones — or that path with its LEADING separator left to a pointer's own, which is
/// how a two-token patch path spells it. A resource path "begins with a '/'"
/// (`Resource$path`), and the KEY carries the path WITHOUT it: `{resource_path}/{http_method}`
/// read off a resource path trimmed of its leading separator, with every separator inside it
/// left literal. The separator that follows carries the HTTP method.
///
/// The wildcard is the one key that names no resource path: `Stage$methodSettings` documents
/// the whole-stage method path `/*/*` and keys it `*/*`, so that half is carried over as it
/// stands.
#[must_use]
pub fn method_settings_key(resource_path: &str, http_method: &str) -> String {
    let resource = if resource_path == EVERY_RESOURCE
        || resource_path.strip_prefix('/') == Some(EVERY_RESOURCE)
    {
        EVERY_RESOURCE
    } else {
        resource_path.strip_prefix('/').unwrap_or(resource_path)
    };
    format!("{resource}/{http_method}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a key is the method path `{resource_path}/{http_method}` — the resource path
    /// without its leading separator, every separator inside it literal, then a separator and
    /// the HTTP method — whatever spelling of the resource path it is derived from, and `*/*`
    /// for every method of the stage.
    #[test]
    fn a_key_names_the_method_however_its_resource_path_was_spelled() {
        // Rows span the DEPTH of the resource path (the root, a child, a nested one), both
        // spellings of its leading separator, and the wildcard that stands for every method
        // — which a caller spells with that separator or without it, as the two readers of a
        // method path hand it over.
        let rows: [(&str, &str, &str, &str); 10] = [
            ("a child resource", "/pets", "GET", "pets/GET"),
            (
                "a child resource, leading separator left out",
                "pets",
                "GET",
                "pets/GET",
            ),
            (
                "a nested resource",
                "/pets/{petId}",
                "DELETE",
                "pets/{petId}/DELETE",
            ),
            (
                "a nested resource, leading separator left out",
                "pets/{petId}",
                "DELETE",
                "pets/{petId}/DELETE",
            ),
            ("the root resource", "/", "GET", "/GET"),
            (
                "the root resource, leading separator left out",
                "",
                "GET",
                "/GET",
            ),
            ("every method of the stage", "*", "*", "*/*"),
            (
                "every method of the stage, leading separator included",
                "/*",
                "*",
                "*/*",
            ),
            ("every resource at one verb", "*", "PUT", "*/PUT"),
            (
                "every resource at one verb, leading separator included",
                "/*",
                "PUT",
                "*/PUT",
            ),
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
