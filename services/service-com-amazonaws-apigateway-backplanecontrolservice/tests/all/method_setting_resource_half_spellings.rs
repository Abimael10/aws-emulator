// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the RESOURCE half of a method-setting pointer: how a patch may spell
//! the resource path of `/{resource_path}/{http_method}/{setting}`, and the ONE
//! `methodSettings` key the stage then reports the setting under.
//!
//! The RULE under test, one level above any one call:
//!
//! > A `methodSettings` key is the METHOD the patch addressed — "method paths defined
//! > as {resource_path}/{http_method} for an individual method override, or /\*/\* for
//! > overriding all methods in the stage" (`Stage$methodSettings`), written as the method
//! > path itself: the resource path without its leading separator, every other separator of
//! > it literal, then the separator before the HTTP method. Inside the PATCH PATH that
//! > resource path is escaped `~1` as `PatchOperation$path` spells a separator inside a path
//! > name, so a pointer that spells its leading separator either way — escaped into its
//! > first reference token, or left to the pointer's own leading
//! > `/` — names the one method and is keyed the one way. A resource half carrying an
//! > unescaped separator of its own names no method, because nothing in the pointer says where
//! > the resource path stops and the method begins, so it is the modelled
//! > `BadRequestException`.
//!
//! The rows span what the rule generalises over: the DEPTH of the resource path (the root,
//! a child, a nested one), the wildcard that stands for every method, and the two spellings
//! of the leading separator — the escaped one `PatchOperation$path` documents and the one
//! `MethodSetting$ResourcePath` states a path in ("the initial slash must include a forward
//! slash"), which is how a CloudFormation-managed stage is patched.
//!
//! What a stage does with the key it stored is the data plane's half of the same rule, which
//! `stage_response_cache` asserts by invoking a method whose settings were stored under each
//! spelling.
//!
//! Every case drives the real wire path through [`Gateway`], this crate's one wire-test
//! harness, and reuses the stage and patch helpers of the suites beside it.

use serde_json::json;

use crate::patch_updates::{op, patch, patch_ok, replace};
use crate::rest_api_lifecycle::{assert_failure, create_api, Gateway};
use crate::stage_lifecycle::{create_deployed_stage, get_stage};

/// One row: the resource path a client means, each spelling of the pointer that addresses a
/// setting of its `GET` method, and the ONE `methodSettings` key the stage must report that
/// setting under whichever spelling wrote it.
struct Row {
    case: &'static str,
    /// The key both spellings resolve to.
    key: &'static str,
    /// The pointer whose resource half escapes the path's leading separator.
    escaped: &'static str,
    /// The pointer that leaves that leading separator to the pointer's own — `None` for the
    /// wildcard, whose single spelling is already that one.
    unprefixed: Option<&'static str>,
}

const ROWS: &[Row] = &[
    Row {
        case: "a child resource",
        key: "pets/GET",
        escaped: "/~1pets/GET/caching/enabled",
        unprefixed: Some("/pets/GET/caching/enabled"),
    },
    Row {
        case: "a nested resource path",
        key: "pets/{petId}/GET",
        escaped: "/~1pets~1{petId}/GET/caching/enabled",
        unprefixed: Some("/pets~1{petId}/GET/caching/enabled"),
    },
    Row {
        case: "the root resource",
        key: "/GET",
        escaped: "/~1/GET/caching/enabled",
        unprefixed: Some("//GET/caching/enabled"),
    },
    Row {
        case: "every method of the stage",
        key: "*/*",
        escaped: "/*/*/caching/enabled",
        unprefixed: None,
    },
];

/// The pointers whose resource half carries an unescaped separator of its own: a nested path
/// written with literal separators, and the same under the wildcard verb. Neither says where
/// the resource path stops, so neither names a method.
const UNREADABLE: &[&str] = &[
    "/pets/{petId}/GET/caching/enabled",
    "/pets/{petId}/*/caching/enabled",
];

#[tokio::test]
async fn every_spelling_of_a_method_path_keys_the_method_it_names() {
    let gateway = Gateway::new();
    let (api, _root) = create_api(&gateway, "method-setting-resource-half").await;
    create_deployed_stage(&gateway, &api, "prod").await;
    let uri = format!("/restapis/{api}/stages/prod");

    let mut written: Vec<String> = Vec::new();
    for row in ROWS {
        written.push(row.key.to_string());
        written.sort();
        let spellings = [Some(row.escaped), row.unprefixed];
        for path in spellings.iter().flatten() {
            let case = format!("{} as {path:?}", row.case);
            let body = patch_ok(&gateway, &case, &uri, vec![replace(path, "true")]).await;
            assert_eq!(
                body["methodSettings"][row.key]["cachingEnabled"],
                json!(true),
                "case {case}: expected cachingEnabled under {:?}, got {:?}",
                row.key,
                body["methodSettings"]
            );

            // The stage holds exactly one entry per method named so far: a second spelling of
            // one method writes the entry the first one wrote rather than one of its own.
            let read = get_stage(&gateway, &api, "prod").await;
            let mut reported: Vec<String> = read.body["methodSettings"]
                .as_object()
                .map(|map| map.keys().cloned().collect())
                .unwrap_or_default();
            reported.sort();
            assert_eq!(
                reported, written,
                "case {case}: expected GetStage to report the keys {written:?}, got {:?}",
                read.body["methodSettings"]
            );
        }
    }
}

#[tokio::test]
async fn a_resource_half_carrying_its_own_separator_names_no_method() {
    let gateway = Gateway::new();
    let (api, _root) = create_api(&gateway, "method-setting-unreadable-half").await;
    create_deployed_stage(&gateway, &api, "prod").await;
    let uri = format!("/restapis/{api}/stages/prod");

    for path in UNREADABLE {
        let case = format!("UpdateStage(replace {path})");
        let answer = patch(&gateway, &uri, vec![op("replace", path, Some("true"))]).await;
        assert_failure(&answer, 400, "BadRequestException", &case);
    }

    // And nothing was stored for any of them: a pointer that names no method leaves the map
    // exactly as it was.
    let read = get_stage(&gateway, &api, "prod").await;
    let reported: Vec<String> = read.body["methodSettings"]
        .as_object()
        .map(|map| map.keys().cloned().collect())
        .unwrap_or_default();
    assert!(
        reported.is_empty(),
        "case \"after every refusal\": expected no method settings stored, got {reported:?}"
    );
}
