// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the `methodSettings` KEY one method is held under, whichever of the
//! spellings an `UpdateStage` patch path may address that method by was used.
//!
//! The RULE under test, one level above any one call:
//!
//! > `Stage$methodSettings` is "a map that defines the method settings for a Stage resource.
//! > Keys (designated as /{method_setting_key) are method paths defined as
//! > {resource_path}/{http_method} for an individual method override, or /\*/\* for
//! > overriding all methods in the stage". A key therefore names a METHOD, as the method
//! > path itself: the resource path without its leading separator and with every other
//! > separator literal, then a separator, then the HTTP method; and `*/*` for the
//! > whole-stage path `/*/*`, which names no resource path. The `~1` escape of
//! > `PatchOperation$path` belongs to the POINTER that addresses an entry and never to the
//! > key the entry is reported under. A patch may spell that method
//! > several ways (the resource path's leading separator escaped or left to the pointer's
//! > own, the separator before the HTTP method escaped too so the whole method path rides in
//! > one reference token), and every one of them addresses the one method, so a stage holds
//! > exactly ONE entry per method however many spellings wrote it.
//!
//! The rows span what the rule generalises over: the DEPTH of the resource path (the root, a
//! child, a nested one), the wildcard that stands for every method, and each spelling of the
//! separators — so the setting a client writes under one spelling is the setting it reads
//! back under the documented key, and a settings-diffing client sees no entry AWS would not
//! report.
//!
//! Every case drives the real wire path through [`Gateway`], this crate's one wire-test
//! harness, and reuses the stage and patch helpers of the suites beside it. Each spelling is
//! sent in a request of its OWN: a single request carrying two spellings of one method is the
//! duplicate-method-settings refusal, which is a rule of its own and not this one.

use serde_json::{json, Value};

use crate::patch_updates::{op, patch_ok, replace};
use crate::rest_api_lifecycle::{create_api, Gateway};
use crate::stage_lifecycle::{create_deployed_stage, get_stage};

/// One row: a method, the single `methodSettings` key it is held under, and the patch paths
/// that address a setting of it — each paired with the `MethodSetting` member it writes.
///
/// The setting names are the published group-and-name paths and the member names of the
/// `MethodSetting` shape (`smithy_query(query="shape", shape="MethodSetting")`); a one-token
/// method path is followed by the member name, which is the only spelling one token can carry.
struct Row {
    case: &'static str,
    key: &'static str,
    /// `(patch path, the member the stage must then report as `true`)`.
    spellings: &'static [(&'static str, &'static str)],
}

const ROWS: &[Row] = &[
    Row {
        case: "a child resource",
        key: "pets/GET",
        spellings: &[
            // The resource path's leading separator escaped into the first token.
            ("/~1pets/GET/metrics/enabled", "metricsEnabled"),
            // The same resource path with that separator left to the pointer's own, the
            // spelling `MethodSetting$ResourcePath` states a path in.
            ("/pets/GET/caching/enabled", "cachingEnabled"),
            // The whole method path inside ONE reference token.
            ("/~1pets~1GET/cacheDataEncrypted", "cacheDataEncrypted"),
        ],
    },
    Row {
        case: "a nested resource path",
        key: "pets/{petId}/DELETE",
        spellings: &[
            (
                "/~1pets~1{petId}/DELETE/metrics/enabled",
                "metricsEnabled",
            ),
            ("/pets~1{petId}/DELETE/caching/enabled", "cachingEnabled"),
            (
                "/~1pets~1{petId}~1DELETE/cacheDataEncrypted",
                "cacheDataEncrypted",
            ),
        ],
    },
    Row {
        case: "the root resource",
        key: "/GET",
        spellings: &[
            ("/~1/GET/metrics/enabled", "metricsEnabled"),
            ("//GET/caching/enabled", "cachingEnabled"),
            ("/~1~1GET/cacheDataEncrypted", "cacheDataEncrypted"),
        ],
    },
    Row {
        case: "every method of the stage",
        key: "*/*",
        // The wildcard names no resource path, so the spellings are its own two tokens, both
        // of them in one, and the leading separator `MethodSetting$ResourcePath` states a
        // path with — every one of them the whole-stage key.
        spellings: &[
            ("/*/*/metrics/enabled", "metricsEnabled"),
            ("/*~1*/cachingEnabled", "cachingEnabled"),
            ("/~1*~1*/cacheDataEncrypted", "cacheDataEncrypted"),
        ],
    },
];

/// The keys the stage reports, sorted.
fn reported_keys(body: &Value) -> Vec<String> {
    let mut keys: Vec<String> = body["methodSettings"]
        .as_object()
        .map(|map| map.keys().cloned().collect())
        .unwrap_or_default();
    keys.sort();
    keys
}

#[tokio::test]
async fn every_spelling_of_one_method_writes_the_one_documented_key() {
    let gateway = Gateway::new();
    let (api, _root) = create_api(&gateway, "method-settings-key").await;
    create_deployed_stage(&gateway, &api, "prod").await;
    let uri = format!("/restapis/{api}/stages/prod");

    let mut expected_keys: Vec<String> = Vec::new();
    for row in ROWS {
        expected_keys.push(row.key.to_string());
        expected_keys.sort();
        for (path, member) in row.spellings {
            let case = format!("{} as {path:?}", row.case);
            let body = patch_ok(&gateway, &case, &uri, vec![replace(path, "true")]).await;
            assert_eq!(
                body["methodSettings"][row.key][*member],
                json!(true),
                "case {case}: expected {member} under {:?}, got {:?}",
                row.key,
                body["methodSettings"]
            );
            // The whole map: one entry per method named so far and not one more, so a second
            // spelling of a method is the SAME entry rather than one of its own.
            let keys = reported_keys(&body);
            assert_eq!(
                keys, expected_keys,
                "case {case}: expected the keys {expected_keys:?}, got {keys:?}"
            );
            // No key carries a pointer escape: that escape is the patch path's, while a key
            // is the method path with its separators literal.
            assert!(
                keys.iter().all(|key| !key.contains("~1")),
                "case {case}: a methodSettings key spells its separators literally, got {keys:?}"
            );
            let read = get_stage(&gateway, &api, "prod").await;
            assert_eq!(
                read.body, body,
                "case {case}: GetStage must report what UpdateStage answered, got {:?} after {:?}",
                read.body, body
            );
        }

        // Every spelling wrote into the one entry, so each setting they addressed is still
        // there together with the others.
        let read = get_stage(&gateway, &api, "prod").await;
        for (path, member) in row.spellings {
            assert_eq!(
                read.body["methodSettings"][row.key][*member],
                json!(true),
                "case {} as {path:?}: the entry under {:?} must keep every setting the \
                 spellings wrote, got {:?}",
                row.case,
                row.key,
                read.body["methodSettings"][row.key]
            );
        }
    }
}

#[tokio::test]
async fn a_method_entry_is_removed_by_any_spelling_that_names_it() {
    let gateway = Gateway::new();
    let (api, _root) = create_api(&gateway, "method-settings-key-removal").await;
    create_deployed_stage(&gateway, &api, "prod").await;
    let uri = format!("/restapis/{api}/stages/prod");

    // The other side of the same rule: a `remove` naming the method alone takes away the one
    // entry that method is held under, so a spelling OTHER than the one that wrote the entry
    // still names it. Rows pair each writing spelling with a different removing one, over the
    // same resource-path depths and the wildcard.
    let rows: [(&str, &str, &str); 6] = [
        (
            "a child resource",
            "/~1pets/GET/metrics/enabled",
            "/pets/GET",
        ),
        (
            "a child resource written with the pointer's own separator",
            "/pets/GET/metrics/enabled",
            "/~1pets/GET",
        ),
        (
            "a nested resource path",
            "/~1pets~1{petId}/DELETE/metrics/enabled",
            "/pets~1{petId}/DELETE",
        ),
        ("the root resource", "/~1/GET/metrics/enabled", "//GET"),
        (
            "a method written in one token",
            "/~1pets~1GET/metricsEnabled",
            "/~1pets/GET",
        ),
        ("every method of the stage", "/*/*/metrics/enabled", "/*/*"),
    ];

    for (case, write_path, remove_path) in rows {
        let written = format!("{case} written as {write_path:?}");
        let body = patch_ok(&gateway, &written, &uri, vec![replace(write_path, "true")]).await;
        let keys = reported_keys(&body);
        assert_eq!(
            keys.len(),
            1,
            "case {written}: expected the method's one entry, got {keys:?}"
        );

        let removed = format!("{case} removed as {remove_path:?}");
        let body = patch_ok(
            &gateway,
            &removed,
            &uri,
            vec![op("remove", remove_path, None)],
        )
        .await;
        let keys = reported_keys(&body);
        assert!(
            keys.is_empty(),
            "case {removed}: expected the method's one entry gone, got {keys:?}"
        );
        let read = get_stage(&gateway, &api, "prod").await;
        assert_eq!(
            read.body, body,
            "case {removed}: GetStage must report what UpdateStage answered, got {:?} after {:?}",
            read.body, body
        );
    }
}

/// RULE: the key of a `methodSettings` entry is the METHOD PATH `Stage$methodSettings`
/// documents — `{resource_path}/{http_method}`, the resource path without its leading
/// separator and with every other separator of it literal — and never the `~1`-escaped
/// spelling the patch POINTER addressed the entry by.
///
/// The key is DERIVED here from the resource path and the verb of each row, so the row proves
/// the form rather than one remembered string.
#[tokio::test]
async fn a_key_is_the_documented_method_path_derived_from_the_method_it_names() {
    // Rows differ along what the rule generalises over: the DEPTH of the resource path (the
    // root, a child, a nested one, a deeper one), a path part that is a path parameter, a part
    // carrying a character the pointer leaves alone, the HTTP verb — the verbs are the ones
    // `PutMethod$httpMethod` takes — and the wildcard that stands for every method of the
    // stage.
    let rows: [(&str, &str, &str); 6] = [
        ("the root resource", "/", "GET"),
        ("a child resource", "/pets", "POST"),
        ("a nested resource", "/pets/{petId}", "DELETE"),
        ("a deeply nested resource", "/a/b/c", "PUT"),
        ("a resource part carrying a hyphen", "/order-items", "PATCH"),
        ("every method of the stage", "/*", "*"),
    ];

    let gateway = Gateway::new();
    let (api, _root) = create_api(&gateway, "method-settings-key-form").await;
    create_deployed_stage(&gateway, &api, "prod").await;
    let uri = format!("/restapis/{api}/stages/prod");

    let mut expected_keys: Vec<String> = Vec::new();
    for (case, resource_path, verb) in rows {
        // The method path as a key: the resource path's leading separator is the one the key
        // leaves out.
        let unprefixed = resource_path.strip_prefix('/').unwrap_or(resource_path);
        let expected_key = format!("{unprefixed}/{verb}");
        // The pointer that addresses a setting of that same method: a separator INSIDE the
        // resource path is escaped `~1`, as `PatchOperation$path` requires of a path name.
        let pointer = format!(
            "/{}/{verb}/metrics/enabled",
            unprefixed.replace('/', "~1")
        );

        let case = format!("{case} as {pointer:?}");
        let body = patch_ok(&gateway, &case, &uri, vec![replace(&pointer, "true")]).await;
        let actual = &body["methodSettings"][expected_key.as_str()]["metricsEnabled"];
        assert_eq!(
            actual,
            &json!(true),
            "case {case}: expected metricsEnabled under {expected_key:?}, got {:?}",
            body["methodSettings"]
        );

        expected_keys.push(expected_key);
        expected_keys.sort();
        let keys = reported_keys(&body);
        assert_eq!(
            keys, expected_keys,
            "case {case}: expected the keys {expected_keys:?}, got {keys:?}"
        );
        assert!(
            keys.iter().all(|key| !key.contains("~1")),
            "case {case}: a methodSettings key spells its separators literally, got {keys:?}"
        );

        let read = get_stage(&gateway, &api, "prod").await;
        assert_eq!(
            read.body["methodSettings"], body["methodSettings"],
            "case {case}: GetStage must report the keys UpdateStage answered, got {:?} after \
             {:?}",
            read.body["methodSettings"], body["methodSettings"]
        );
    }
}
