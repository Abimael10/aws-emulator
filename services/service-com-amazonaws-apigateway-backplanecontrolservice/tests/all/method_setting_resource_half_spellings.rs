// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the RESOURCE half of a method-setting pointer: how a patch may spell
//! the resource path of `/{resource_path}/{http_method}/{setting}`, and the
//! `methodSettings` key the stage then reports the setting under.
//!
//! The RULE under test, one level above any one call:
//!
//! > A `methodSettings` key is the METHOD PATH the patch pointer SPELLED — "method paths
//! > defined as {resource_path}/{http_method} for an individual method override, or /\*/\* for
//! > overriding all methods in the stage" (`Stage$methodSettings`) — taken from the pointer's
//! > reference tokens before the setting and written exactly as the request wrote them, so a
//! > client reads an entry back under the path it patched. Inside the pointer every separator
//! > INSIDE the resource path is escaped `~1` the way `PatchOperation$path` spells a separator
//! > in a path name, while the path's LEADING separator may be escaped too or left to the
//! > pointer's own (`//pets/GET/…`, the spelling `MethodSetting$resourcePath` states a path
//! > in). A resource half carrying an unescaped separator of its OWN names no method, because
//! > nothing in the pointer then says where the resource path stops and the method begins, so
//! > it is the modelled `BadRequestException`.
//! >
//! > Two pointers that spell one method differently are two entries, and a stage cannot be
//! > UPDATED while it carries two: the update is refused with "Update Stage failed due to
//! > different method settings with the same resource path and http method", naming the
//! > colliding method paths, the account and the API, and the settings the refused update
//! > wrote stay there to be removed — so every later update of that stage earns the same
//! > sentence until one of the two is gone.
//!
//! The rows span what the rule generalises over: the DEPTH of the resource path (the root, a
//! child, a nested one), the wildcard that stands for every method, and each spelling of the
//! leading separator — the escaped one `PatchOperation$path` documents, the pointer's own
//! (which is how a CloudFormation-managed stage is patched), and the half that carries none.
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

/// One row: a spelling of the pointer that addresses a setting of one method, and the
/// `methodSettings` key the stage must report that setting under — the method path the pointer
/// spelled.
struct Row {
    case: &'static str,
    path: &'static str,
    key: &'static str,
}

const ROWS: &[Row] = &[
    Row {
        case: "a child resource, its leading separator escaped",
        path: "/~1pets/GET/caching/enabled",
        key: "~1pets/GET",
    },
    Row {
        case: "a child resource, its leading separator the pointer's own",
        path: "//pets/POST/caching/enabled",
        key: "/pets/POST",
    },
    Row {
        case: "a child resource, no leading separator at all",
        path: "/pets/PUT/caching/enabled",
        key: "pets/PUT",
    },
    Row {
        case: "a nested resource path, its leading separator escaped",
        path: "/~1pets~1{petId}/GET/caching/enabled",
        key: "~1pets~1{petId}/GET",
    },
    Row {
        case: "a nested resource path, its leading separator the pointer's own",
        path: "//pets~1{petId}/POST/caching/enabled",
        key: "/pets~1{petId}/POST",
    },
    Row {
        case: "the root resource, its separator escaped",
        path: "/~1/GET/caching/enabled",
        key: "~1/GET",
    },
    Row {
        case: "the root resource, its separator the pointer's own",
        path: "//POST/caching/enabled",
        key: "/POST",
    },
    Row {
        case: "the method path carried whole in one token, which is decoded",
        path: "/~1pets~1HEAD/cachingEnabled",
        key: "pets/HEAD",
    },
    Row {
        case: "every method of the stage",
        path: "/*/*/caching/enabled",
        key: "*/*",
    },
];

/// The pointers whose resource half carries an unescaped separator of its own: a nested path
/// written with literal separators, and the same under the wildcard verb. Neither says where
/// the resource path stops, so neither names a method.
const UNREADABLE: &[&str] = &[
    "/pets/{petId}/GET/caching/enabled",
    "/pets/{petId}/*/caching/enabled",
];

/// The `methodSettings` keys a stage reports, sorted.
fn reported_keys(body: &serde_json::Value) -> Vec<String> {
    let mut keys: Vec<String> = body["methodSettings"]
        .as_object()
        .map(|map| map.keys().cloned().collect())
        .unwrap_or_default();
    keys.sort();
    keys
}

#[tokio::test]
async fn every_spelling_of_a_method_path_keys_the_setting_as_it_was_written() {
    let gateway = Gateway::new();
    let (api, _root) = create_api(&gateway, "method-setting-resource-half").await;
    create_deployed_stage(&gateway, &api, "prod").await;
    let uri = format!("/restapis/{api}/stages/prod");

    let mut written: Vec<String> = Vec::new();
    for row in ROWS {
        let case = format!("{} as {:?}", row.case, row.path);
        let body = patch_ok(&gateway, &case, &uri, vec![replace(row.path, "true")]).await;
        assert_eq!(
            body["methodSettings"][row.key]["cachingEnabled"],
            json!(true),
            "case {case}: expected cachingEnabled under {:?}, got {:?}",
            row.key,
            body["methodSettings"]
        );

        // Each spelling is an entry of its OWN: the key it wrote is there beside the keys
        // every earlier spelling wrote, and no key the request did not spell.
        written.push(row.key.to_string());
        written.sort();
        let read = get_stage(&gateway, &api, "prod").await;
        let reported = reported_keys(&read.body);
        assert_eq!(
            reported, written,
            "case {case}: expected GetStage to report the keys {written:?}, got {:?}",
            read.body["methodSettings"]
        );
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
    let reported = reported_keys(&read.body);
    assert!(
        reported.is_empty(),
        "case \"after every refusal\": expected no method settings stored, got {reported:?}"
    );
}

#[tokio::test]
async fn one_method_spelled_two_ways_cannot_be_updated_until_one_spelling_is_gone() {
    let gateway = Gateway::new();
    let (api, _root) = create_api(&gateway, "method-setting-duplicate-spellings").await;

    // Rows pair two pointers of ONE method whose method paths are spelled differently, over
    // the depths of the resource path, each pairing of the three spellings of its leading
    // separator, the method path carried whole in one token, and the wildcard. Each row runs
    // on a stage of its own, because a stage that reached this state answers every later
    // update with the same sentence.
    let rows: [(&str, &str, &str, &str, &str); 5] = [
        (
            "the leading separator escaped against the pointer's own",
            "/~1pets/GET/caching/enabled",
            "//pets/GET/metrics/enabled",
            "~1pets/GET",
            "/pets/GET",
        ),
        (
            "the leading separator escaped against none at all",
            "/pets/GET/caching/enabled",
            "/~1pets/GET/metrics/enabled",
            "~1pets/GET",
            "pets/GET",
        ),
        (
            "the method path carried whole in one token",
            "/~1pets~1GET/cacheDataEncrypted",
            "/~1pets/GET/metrics/enabled",
            "~1pets/GET",
            "pets/GET",
        ),
        (
            "a nested resource path, both spellings",
            "/~1pets~1{petId}/DELETE/caching/enabled",
            "//pets~1{petId}/DELETE/metrics/enabled",
            "~1pets~1{petId}/DELETE",
            "/pets~1{petId}/DELETE",
        ),
        (
            "every method of the stage, both spellings",
            "/*/*/caching/enabled",
            "//*/*/metrics/enabled",
            "/*/*",
            "*/*",
        ),
    ];

    for (index, (case, first, second, leading, trailing)) in rows.iter().enumerate() {
        let stage = format!("dup{index}");
        create_deployed_stage(&gateway, &api, &stage).await;
        let uri = format!("/restapis/{api}/stages/{stage}");
        // The sentence names the two colliding method paths, the escaped spelling first, the
        // account the stage belongs to and the API it is under — nothing else varies.
        let expected = format!(
            "Update Stage failed due to different method settings with the same resource path \
             and http method. Please remove one of the duplicate method settings for method \
             paths: {leading}, {trailing} for accountId: {} and restApiId: {api}",
            engine::emulator_account_id()
        );

        let case = format!("UpdateStage({first} and {second}) / {case}");
        let answer = patch(
            &gateway,
            &uri,
            vec![replace(first, "true"), replace(second, "true")],
        )
        .await;
        assert_failure(&answer, 400, "BadRequestException", &case);
        assert_eq!(
            answer.body["message"].as_str(),
            Some(expected.as_str()),
            "case {case}: expected the message {expected:?}, got {:?}",
            answer.body["message"]
        );

        // The settings the refused update wrote are there to be taken away, which is what the
        // sentence asks for: the stage reports both entries, and every further update is
        // refused the same way while it carries them.
        let read = get_stage(&gateway, &api, &stage).await;
        let mut expected_keys = vec![(*leading).to_string(), (*trailing).to_string()];
        expected_keys.sort();
        let reported = reported_keys(&read.body);
        assert_eq!(
            reported, expected_keys,
            "case {case}: expected the stage to report both spellings {expected_keys:?}, got \
             {reported:?}"
        );
        let again = patch(&gateway, &uri, vec![replace("/description", "anything")]).await;
        assert_failure(&again, 400, "BadRequestException", &case);
        assert_eq!(
            again.body["message"].as_str(),
            Some(expected.as_str()),
            "case {case}: a later update must earn the same sentence, got {:?}",
            again.body["message"]
        );
        // The refusal is about the stage this update LEAVES rather than about the request, so
        // what the list applied is kept — which is what leaves the two entries there to be
        // removed. The op this one carried is therefore in the stage a later read reports,
        // 400 or not.
        let read = get_stage(&gateway, &api, &stage).await;
        assert_eq!(
            read.body["description"].as_str(),
            Some("anything"),
            "case {case}: the op the refused update applied must be in the stage it left, got \
             {:?}",
            read.body["description"]
        );

        // And it is updatable again once one of the two is gone.
        let removed = patch(
            &gateway,
            &uri,
            vec![op("remove", &format!("/{trailing}"), None)],
        )
        .await;
        assert_eq!(
            removed.status, 200,
            "case {case}: removing one of the two must be accepted, got {} with {:?}",
            removed.status, removed.body
        );
        let reported = reported_keys(&removed.body);
        assert_eq!(
            reported,
            vec![(*leading).to_string()],
            "case {case}: expected only {leading:?} left, got {reported:?}"
        );
        let accepted = patch(&gateway, &uri, vec![replace("/description", "updatable")]).await;
        assert_eq!(
            accepted.status, 200,
            "case {case}: the stage must be updatable again, got {} with {:?}",
            accepted.status, accepted.body
        );
    }
}

#[tokio::test]
async fn a_list_that_takes_one_of_the_two_away_leaves_no_duplicate() {
    let gateway = Gateway::new();
    let (api, _root) = create_api(&gateway, "method-setting-transient-duplicate").await;
    create_deployed_stage(&gateway, &api, "prod").await;
    let uri = format!("/restapis/{api}/stages/prod");

    // The refusal is about the stage the LIST leaves, not about any op of it: a list that
    // writes one method under two spellings and then takes one of the two away leaves a stage
    // carrying one, so it is applied.
    let case = "UpdateStage(two spellings, then a remove of one)";
    let body = patch_ok(
        &gateway,
        case,
        &uri,
        vec![
            replace("/~1pets/GET/caching/enabled", "true"),
            replace("//pets/GET/metrics/enabled", "true"),
            op("remove", "//pets/GET", None),
        ],
    )
    .await;
    let reported = reported_keys(&body);
    assert_eq!(
        reported,
        vec!["~1pets/GET".to_string()],
        "case {case}: expected the surviving spelling's entry alone, got {reported:?}"
    );
    assert_eq!(
        body["methodSettings"]["~1pets/GET"]["cachingEnabled"],
        json!(true),
        "case {case}: expected the setting the surviving spelling wrote, got {:?}",
        body["methodSettings"]["~1pets/GET"]
    );
    let read = get_stage(&gateway, &api, "prod").await;
    assert_eq!(
        read.body, body,
        "case {case}: GetStage must report what UpdateStage answered, got {:?} after {:?}",
        read.body, body
    );
}

#[tokio::test]
async fn two_settings_of_one_spelling_are_one_entry() {
    let gateway = Gateway::new();
    let (api, _root) = create_api(&gateway, "method-setting-one-spelling").await;
    create_deployed_stage(&gateway, &api, "prod").await;
    let uri = format!("/restapis/{api}/stages/prod");

    // The row the duplicate rule does NOT cover: two pointers that spell the method the SAME
    // way name one entry, so both settings are written to it.
    let case = "UpdateStage(two settings of /~1pets/GET)";
    let body = patch_ok(
        &gateway,
        case,
        &uri,
        vec![
            replace("/~1pets/GET/caching/enabled", "true"),
            replace("/~1pets/GET/metrics/enabled", "true"),
        ],
    )
    .await;
    let reported = reported_keys(&body);
    assert_eq!(
        reported,
        vec!["~1pets/GET".to_string()],
        "case {case}: expected the one entry both settings name, got {reported:?}"
    );
    for member in ["cachingEnabled", "metricsEnabled"] {
        assert_eq!(
            body["methodSettings"]["~1pets/GET"][member],
            json!(true),
            "case {case}: expected {member} set, got {:?}",
            body["methodSettings"]["~1pets/GET"]
        );
    }
}
