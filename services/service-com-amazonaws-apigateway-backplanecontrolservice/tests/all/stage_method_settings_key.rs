// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the `methodSettings` KEY a method's settings are held under, and what
//! that key DENOTES.
//!
//! The RULE under test, one level above any one call:
//!
//! > `Stage$methodSettings` is "a map that defines the method settings for a Stage resource.
//! > Keys (designated as /{method_setting_key) are method paths defined as
//! > {resource_path}/{http_method} for an individual method override, or /\*/\* for
//! > overriding all methods in the stage". The key API Gateway reports an entry under is the
//! > method path the patch POINTER spelled: the pointer's reference tokens before the setting
//! > it writes, each exactly as the request wrote it — so a client reads a setting back under
//! > the very path it patched. A pointer carrying the whole method path in ONE reference token
//! > is the exception, because such a token has to be DECODED to be read as a method path at
//! > all: it is keyed as the documented method path, the resource path without the leading
//! > separator a resource path is stated with. The method a key denotes (its resource path
//! > decoded, with that leading separator, and the HTTP method) is what a `remove` names, and
//! > a stage cannot be updated while two of its keys denote the one method.
//!
//! The rows span what the rule generalises over: the DEPTH of the resource path (the root, a
//! child, a nested one), the wildcard that stands for every method, each spelling of the
//! separators (escaped, the pointer's own, absent, and the whole method path in one token),
//! and each spelling of the setting — so the key a client patched is the key it reads back,
//! and a settings-diffing client sees no key AWS would not report. Each spelling is driven on
//! a method of its own: two spellings of ONE method are the duplicate-method-settings
//! refusal, which `method_setting_resource_half_spellings` asserts.
//!
//! Every case drives the real wire path through [`Gateway`], this crate's one wire-test
//! harness, and reuses the stage and patch helpers of the suites beside it.

use serde_json::{json, Value};

use crate::patch_updates::{op, patch_ok, replace};
use crate::rest_api_lifecycle::{create_api, Gateway};
use crate::stage_lifecycle::{create_deployed_stage, get_stage};

/// One row: a method, and the patch paths that address a setting of it — each paired with the
/// `methodSettings` key the stage reports it under and the `MethodSetting` member it writes.
///
/// The setting names are the published group-and-name paths and the member names of the
/// `MethodSetting` shape (`smithy_query(query="shape", shape="MethodSetting")`); a one-token
/// method path is followed by the member name, which is the only spelling one token can carry.
struct Row {
    case: &'static str,
    /// `(patch path, the key the setting is reported under, the member it sets to `true`)`.
    spellings: &'static [(&'static str, &'static str, &'static str)],
}

const ROWS: &[Row] = &[
    Row {
        case: "a child resource",
        spellings: &[
            // The resource path's leading separator escaped into the first token.
            ("/~1pets/GET/metrics/enabled", "~1pets/GET", "metricsEnabled"),
            // The same resource path with that separator left to the pointer's own, the
            // spelling `MethodSetting$resourcePath` states a path in — on another verb, since
            // two spellings of ONE method are the refused duplicate.
            ("//pets/POST/caching/enabled", "/pets/POST", "cachingEnabled"),
            // The resource path with no leading separator at all.
            (
                "/pets/PUT/caching/dataEncrypted",
                "pets/PUT",
                "cacheDataEncrypted",
            ),
            // The whole method path inside ONE reference token, which is decoded and keyed as
            // the documented method path.
            (
                "/~1pets~1DELETE/metricsEnabled",
                "pets/DELETE",
                "metricsEnabled",
            ),
        ],
    },
    Row {
        case: "a nested resource path",
        spellings: &[
            (
                "/~1pets~1{petId}/DELETE/metrics/enabled",
                "~1pets~1{petId}/DELETE",
                "metricsEnabled",
            ),
            (
                "/pets~1{petId}/PATCH/caching/enabled",
                "pets~1{petId}/PATCH",
                "cachingEnabled",
            ),
            (
                "/~1pets~1{petId}~1PUT/cacheDataEncrypted",
                "pets/{petId}/PUT",
                "cacheDataEncrypted",
            ),
        ],
    },
    Row {
        case: "the root resource",
        spellings: &[
            ("/~1/GET/metrics/enabled", "~1/GET", "metricsEnabled"),
            ("//HEAD/caching/enabled", "/HEAD", "cachingEnabled"),
        ],
    },
    Row {
        case: "every method of the stage",
        // The wildcard names no resource path, so the spellings are its own two tokens and
        // both of them in one — which decodes to the same method path, so it is the same key.
        spellings: &[
            ("/*/*/metrics/enabled", "*/*", "metricsEnabled"),
            ("/*~1*/cachingEnabled", "*/*", "cachingEnabled"),
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
async fn every_spelling_writes_the_key_it_spelled() {
    let gateway = Gateway::new();
    let (api, _root) = create_api(&gateway, "method-settings-key").await;
    create_deployed_stage(&gateway, &api, "prod").await;
    let uri = format!("/restapis/{api}/stages/prod");

    let mut expected_keys: Vec<String> = Vec::new();
    for row in ROWS {
        for (path, key, member) in row.spellings {
            let case = format!("{} as {path:?}", row.case);
            let body = patch_ok(&gateway, &case, &uri, vec![replace(path, "true")]).await;
            assert_eq!(
                body["methodSettings"][*key][*member],
                json!(true),
                "case {case}: expected {member} under {key:?}, got {:?}",
                body["methodSettings"]
            );
            // The whole map: the keys every spelling so far wrote and not one more, so a
            // spelling writes its own entry and rewrites no other.
            if !expected_keys.contains(&(*key).to_string()) {
                expected_keys.push((*key).to_string());
                expected_keys.sort();
            }
            let keys = reported_keys(&body);
            assert_eq!(
                keys, expected_keys,
                "case {case}: expected the keys {expected_keys:?}, got {keys:?}"
            );
            let read = get_stage(&gateway, &api, "prod").await;
            assert_eq!(
                read.body, body,
                "case {case}: GetStage must report what UpdateStage answered, got {:?} after {:?}",
                read.body, body
            );
        }

        // Every setting each spelling wrote is still there, under the key that spelling named.
        let read = get_stage(&gateway, &api, "prod").await;
        for (path, key, member) in row.spellings {
            assert_eq!(
                read.body["methodSettings"][*key][*member],
                json!(true),
                "case {} as {path:?}: the entry under {key:?} must keep the setting that \
                 spelling wrote, got {:?}",
                row.case,
                read.body["methodSettings"][*key]
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

    // The other side of the same rule: a `remove` naming the method alone takes away the
    // entry of the METHOD it names — the sentence API Gateway refuses a `remove` it has
    // nothing to take away with says "there is no method setting for this method" — so a
    // spelling OTHER than the one that wrote the entry still names it. Rows pair each writing
    // spelling with a different removing one, over the same resource-path depths and the
    // wildcard.
    let rows: [(&str, &str, &str); 6] = [
        (
            "a child resource",
            "/~1pets/GET/metrics/enabled",
            "/pets/GET",
        ),
        (
            "a child resource written with the pointer's own separator",
            "//pets/GET/metrics/enabled",
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

/// RULE: the key of a `methodSettings` entry is the method path the pointer SPELLED — the
/// pointer's reference tokens before the setting, written exactly as the request wrote them.
///
/// The key is DERIVED here from the pointer of each row, so the row proves the form rather
/// than one remembered string.
#[tokio::test]
async fn a_key_is_the_method_path_its_pointer_spelled() {
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

    /// The setting every row writes, as its published group-and-name path.
    const SETTING: &str = "metrics/enabled";

    let gateway = Gateway::new();
    let (api, _root) = create_api(&gateway, "method-settings-key-form").await;
    create_deployed_stage(&gateway, &api, "prod").await;
    let uri = format!("/restapis/{api}/stages/prod");

    let mut expected_keys: Vec<String> = Vec::new();
    for (case, resource_path, verb) in rows {
        // The pointer that addresses that method's setting: every separator INSIDE the
        // resource path is escaped `~1`, as `PatchOperation$path` requires of a path name, and
        // so is its leading one.
        let pointer = format!(
            "/{}/{verb}/{SETTING}",
            resource_path
                .strip_prefix('/')
                .map_or_else(|| resource_path.to_string(), |rest| format!("/{rest}"))
                .replace('/', "~1")
        );
        // The key is that pointer's method path, as written: the pointer without its leading
        // separator and without the setting it names.
        let expected_key = pointer
            .strip_prefix('/')
            .and_then(|path| path.strip_suffix(&format!("/{SETTING}")))
            .map(str::to_string)
            .unwrap_or_default();

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

        let read = get_stage(&gateway, &api, "prod").await;
        assert_eq!(
            read.body["methodSettings"], body["methodSettings"],
            "case {case}: GetStage must report the keys UpdateStage answered, got {:?} after \
             {:?}",
            read.body["methodSettings"], body["methodSettings"]
        );
    }
}
