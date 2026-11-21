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
//!
//! The other half of the same rule — what the entry under a key CARRIES, and who reports it —
//! is pinned at the end of this file:
//!
//! > the value under a `methodSettings` key is the `MethodSetting` the patches applied to that
//! > method wrote, and EVERY reader of the stage reports it: the `UpdateStage` answer,
//! > `GetStage` and the `GetStages` collection entry all render one stage record. The
//! > whole-stage `*/*` key is an entry of its own, so writing either scope neither creates nor
//! > changes the other.

use serde_json::{json, Value};

use crate::account_settings::{patch_account, role_arn};
use crate::patch_updates::{op, patch_ok, replace};
use crate::rest_api_lifecycle::{create_api, page_items, Gateway};
use crate::stage_collection::get_stages;
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

/// One setting of a method: the tail a patch path addresses it by (its published group and
/// name), the `MethodSetting` member it writes, the value the patch sends and the JSON a
/// reader must then report.
///
/// Source: the `MethodSetting` shape (`smithy_query(query="shape", shape="MethodSetting")`)
/// for the member names and their types, its `unauthorizedCacheControlHeaderStrategy` value
/// set for the strategy, and `MethodSetting$loggingLevel` ("valid values are OFF, ERROR, and
/// INFO") for the level. Every value differs from the one an entry carries for a setting
/// nobody configured, so no row can pass on a default.
type Setting = (&'static str, &'static str, &'static str, Value);

fn applied_settings() -> Vec<Setting> {
    vec![
        ("metrics/enabled", "metricsEnabled", "true", json!(true)),
        ("logging/loglevel", "loggingLevel", "ERROR", json!("ERROR")),
        ("logging/dataTrace", "dataTraceEnabled", "true", json!(true)),
        (
            "throttling/burstLimit",
            "throttlingBurstLimit",
            "77",
            json!(77),
        ),
        (
            "throttling/rateLimit",
            "throttlingRateLimit",
            "12.5",
            json!(12.5),
        ),
        ("caching/enabled", "cachingEnabled", "true", json!(true)),
        (
            "caching/ttlInSeconds",
            "cacheTtlInSeconds",
            "45",
            json!(45),
        ),
        (
            "caching/dataEncrypted",
            "cacheDataEncrypted",
            "true",
            json!(true),
        ),
        (
            "caching/requireAuthorizationForCacheControl",
            "requireAuthorizationForCacheControl",
            "false",
            json!(false),
        ),
        (
            "caching/unauthorizedCacheControlHeaderStrategy",
            "unauthorizedCacheControlHeaderStrategy",
            "FAIL_WITH_403",
            json!("FAIL_WITH_403"),
        ),
    ]
}

/// The resource half of a method path as a patch pointer SPELLS it, which is the resource half
/// of the KEY the stage then reports: the resource path as ONE reference token, every separator
/// in it — its leading one included — escaped `~1` the way `PatchOperation$path` requires of a
/// path name. The wildcard names no resource path, so it is carried over as it stands.
fn resource_token(resource_path: &str) -> String {
    if resource_path.trim_start_matches('/') == "*" {
        return "*".to_string();
    }
    let path = resource_path.strip_prefix('/').unwrap_or(resource_path);
    format!("/{path}").replace('/', "~1")
}

/// The patch pointer that addresses `setting` of the method `verb` of `resource_path`: the
/// resource half as [`resource_token`] spells it, then the verb in its own reference token,
/// then the setting.
fn setting_pointer(resource_path: &str, verb: &str, setting: &str) -> String {
    format!("/{}/{verb}/{setting}", resource_token(resource_path))
}

/// The `methodSettings` key a [`setting_pointer`] of that method writes: the pointer's
/// reference tokens before the setting, as written — derived here from the method rather than
/// remembered.
fn method_key(resource_path: &str, verb: &str) -> String {
    format!("{}/{verb}", resource_token(resource_path))
}

/// The `methodSettings` the `GetStages` collection reports for `stage_name` of `api` — the
/// collection's own entry for that stage, so the map is asserted at the COLLECTION reader
/// too and not only at the single-stage one.
async fn listed_method_settings(
    gateway: &Gateway,
    api: &str,
    stage_name: &str,
    case: &str,
) -> Value {
    let listed = get_stages(gateway, api, "").await;
    assert_eq!(
        listed.status, 200,
        "case {case}: expected 200 from GetStages, got {} with {:?}",
        listed.status, listed.body
    );
    let entry = page_items(&listed.body, case)
        .into_iter()
        .find(|item| item["stageName"] == json!(stage_name));
    let entry = entry.unwrap_or_else(|| {
        panic!(
            "case {case}: expected GetStages to report the stage {stage_name:?}, got {:?}",
            listed.body
        )
    });
    entry["methodSettings"].clone()
}

/// The `cacheTtlInSeconds` under one `methodSettings` key, as a reader reports it.
fn reported_ttl(settings: &Value, key: &str) -> Value {
    settings[key]["cacheTtlInSeconds"].clone()
}

/// RULE: the value under a `methodSettings` key is the `MethodSetting` the patches applied to
/// that method wrote, and every reader of the stage reports it — the `UpdateStage` answer,
/// `GetStage` and the `GetStages` collection entry render the one stage record.
#[tokio::test]
async fn every_reader_reports_a_per_method_entry_carrying_the_settings_applied_to_it() {
    // Rows differ along what the rule generalises over: the DEPTH of the resource path (the
    // root, a child, a nested one, a deeper one), a part that is a path parameter, and the
    // HTTP verb — the verbs are the ones `PutMethod$httpMethod` takes.
    let methods: [(&str, &str, &str); 4] = [
        ("the root resource", "/", "GET"),
        ("a child resource", "/short", "GET"),
        ("a nested resource", "/items/{id}", "DELETE"),
        ("a deeply nested resource", "/a/b/c", "PUT"),
    ];

    let gateway = Gateway::new();
    let (api, _root) = create_api(&gateway, "method-settings-readback").await;
    create_deployed_stage(&gateway, &api, "prod").await;
    // Two of the settings turn a method's EXECUTION logging on, which API Gateway writes
    // under the ACCOUNT's CloudWatch Logs role, so the account carries one before the table
    // runs — the precondition the sibling `stage_execution_logging_account_role` suite covers.
    patch_account(
        &gateway,
        "replace",
        "/cloudwatchRoleArn",
        Some(&role_arn("method-settings-readback")),
    )
    .await;
    let uri = format!("/restapis/{api}/stages/prod");

    // The members a reader must report for a method every setting was applied to: the whole
    // `MethodSetting` shape, so a reader reports the structure rather than a subset of it.
    let mut expected_members: Vec<String> = applied_settings()
        .iter()
        .map(|(_, member, _, _)| (*member).to_string())
        .collect();
    expected_members.sort();

    for (depth, resource_path, verb) in methods {
        let key = method_key(resource_path, verb);
        // Every setting of the one method in ONE request, the way a client configures a
        // method's caching and logging in a single call.
        let operations: Vec<Value> = applied_settings()
            .iter()
            .map(|(published, _, value, _)| {
                replace(&setting_pointer(resource_path, verb, published), value)
            })
            .collect();
        let case = format!("{depth} as {key:?}");
        let answered = patch_ok(&gateway, &case, &uri, operations).await;
        let read = get_stage(&gateway, &api, "prod").await.body;
        let listed = listed_method_settings(&gateway, &api, "prod", &case).await;

        for (source, settings) in [
            ("the UpdateStage answer", &answered["methodSettings"]),
            ("GetStage", &read["methodSettings"]),
            ("GetStages", &listed),
        ] {
            for (published, member, _, expected) in applied_settings() {
                let actual = &settings[key.as_str()][member];
                assert_eq!(
                    actual, &expected,
                    "case {case}: expected {source} to report {member} (written as \
                     {published:?}) as {expected:?}, got {:?}",
                    settings[key.as_str()]
                );
            }
            let mut reported: Vec<String> = settings[key.as_str()]
                .as_object()
                .map(|map| map.keys().cloned().collect())
                .unwrap_or_default();
            reported.sort();
            assert_eq!(
                reported, expected_members,
                "case {case}: expected {source} to report the members {expected_members:?} \
                 under {key:?}, got {reported:?}"
            );
        }
    }
}

/// RULE: the whole-stage `*/*` key is an entry of its own, so writing either scope neither
/// creates nor changes the other — a per-method override and the stage-wide settings stand
/// side by side, each carrying its own values.
#[tokio::test]
async fn the_whole_stage_entry_and_a_per_method_entry_are_independent() {
    // The TTL the stage-wide entry carries, and the strategy with it: both differ from every
    // row's own values below, so an entry that was overwritten by the other scope fails the
    // assertion rather than passing on a shared value. Inside `caching/ttlInSeconds`'
    // published range.
    const STAGE_WIDE_TTL: &str = "600";
    const STAGE_WIDE_STRATEGY: &str = "SUCCEED_WITHOUT_RESPONSE_HEADER";
    // The TTL the stage-wide entry is moved to after every per-method entry is written, which
    // is the other direction of the same independence.
    const STAGE_WIDE_RETTL: &str = "1200";

    // Rows differ along what the rule generalises over: the depth of the method's resource
    // path, the verb, and each row's OWN TTL — so the entries are told apart by value as well
    // as by key. `3600` is the published upper bound of `caching/ttlInSeconds`, `0` its lower
    // one.
    let methods: [(&str, &str, &str, &str); 4] = [
        ("a child resource", "/short", "GET", "7"),
        ("the root resource", "/", "POST", "45"),
        ("a nested resource", "/count/{id}", "GET", "3600"),
        ("a deeply nested resource", "/a/b/c", "DELETE", "0"),
    ];

    let gateway = Gateway::new();
    let (api, _root) = create_api(&gateway, "method-settings-scopes").await;
    create_deployed_stage(&gateway, &api, "prod").await;
    let uri = format!("/restapis/{api}/stages/prod");

    // The stage-wide entry first, so each per-method write below lands on a stage that
    // already carries one.
    patch_ok(
        &gateway,
        "the stage-wide entry",
        &uri,
        vec![
            replace("/*/*/caching/enabled", "true"),
            replace("/*/*/caching/ttlInSeconds", STAGE_WIDE_TTL),
            replace(
                "/*/*/caching/unauthorizedCacheControlHeaderStrategy",
                STAGE_WIDE_STRATEGY,
            ),
        ],
    )
    .await;

    let mut written: Vec<(String, String, Value)> = Vec::new();
    for (depth, resource_path, verb, ttl) in methods {
        let key = method_key(resource_path, verb);
        let case = format!("{depth} as {key:?}");
        patch_ok(
            &gateway,
            &case,
            &uri,
            vec![
                replace(&setting_pointer(resource_path, verb, "caching/enabled"), "true"),
                replace(
                    &setting_pointer(resource_path, verb, "caching/ttlInSeconds"),
                    ttl,
                ),
                replace(
                    &setting_pointer(
                        resource_path,
                        verb,
                        "caching/unauthorizedCacheControlHeaderStrategy",
                    ),
                    "FAIL_WITH_403",
                ),
            ],
        )
        .await;
        written.push((case.clone(), key.clone(), json!(ttl.parse::<i64>().unwrap())));

        let settings = get_stage(&gateway, &api, "prod").await.body["methodSettings"].clone();
        // The method's own entry carries the row's values.
        assert_eq!(
            settings[key.as_str()]["unauthorizedCacheControlHeaderStrategy"],
            json!("FAIL_WITH_403"),
            "case {case}: expected the method's own strategy, got {:?}",
            settings[key.as_str()]
        );
        // The stage-wide entry is a separate entry, untouched by the per-method write.
        assert_eq!(
            reported_ttl(&settings, "*/*"),
            json!(STAGE_WIDE_TTL.parse::<i64>().unwrap()),
            "case {case}: expected the stage-wide entry to keep its own TTL, got {:?}",
            settings["*/*"]
        );
        assert_eq!(
            settings["*/*"]["unauthorizedCacheControlHeaderStrategy"],
            json!(STAGE_WIDE_STRATEGY),
            "case {case}: expected the stage-wide entry to keep its own strategy, got {:?}",
            settings["*/*"]
        );
        // And every method written before this one still carries its own TTL: one method's
        // entry is not the next method's.
        for (earlier, earlier_key, earlier_ttl) in &written {
            assert_eq!(
                &reported_ttl(&settings, earlier_key),
                earlier_ttl,
                "case {case}: expected {earlier} to keep its own TTL, got {:?}",
                settings[earlier_key.as_str()]
            );
        }
    }

    // The other direction: moving the stage-wide entry leaves every per-method entry as it
    // was.
    let settings = patch_ok(
        &gateway,
        "the stage-wide entry moved",
        &uri,
        vec![replace("/*/*/caching/ttlInSeconds", STAGE_WIDE_RETTL)],
    )
    .await["methodSettings"]
        .clone();
    assert_eq!(
        reported_ttl(&settings, "*/*"),
        json!(STAGE_WIDE_RETTL.parse::<i64>().unwrap()),
        "case the stage-wide entry moved: expected the new TTL, got {:?}",
        settings["*/*"]
    );
    for (case, key, ttl) in &written {
        assert_eq!(
            &reported_ttl(&settings, key),
            ttl,
            "case the stage-wide entry moved: expected {case} to keep its own TTL, got {:?}",
            settings[key.as_str()]
        );
    }

    // Taking one method's entry away takes that entry and no other: the stage-wide entry and
    // every other method's are still there, with their own values.
    let (removed_case, removed_key, _) = written.remove(0);
    let settings = patch_ok(
        &gateway,
        &format!("{removed_case} removed"),
        &uri,
        vec![op("remove", &format!("/{removed_key}"), None)],
    )
    .await["methodSettings"]
        .clone();
    assert_eq!(
        settings[removed_key.as_str()],
        Value::Null,
        "case {removed_case} removed: expected the entry gone, got {:?}",
        settings[removed_key.as_str()]
    );
    assert_eq!(
        reported_ttl(&settings, "*/*"),
        json!(STAGE_WIDE_RETTL.parse::<i64>().unwrap()),
        "case {removed_case} removed: expected the stage-wide entry untouched, got {:?}",
        settings["*/*"]
    );
    for (case, key, ttl) in &written {
        assert_eq!(
            &reported_ttl(&settings, key),
            ttl,
            "case {removed_case} removed: expected {case} to keep its own TTL, got {:?}",
            settings[key.as_str()]
        );
    }
    let read = get_stage(&gateway, &api, "prod").await.body;
    assert_eq!(
        read["methodSettings"], settings,
        "case {removed_case} removed: GetStage must report what UpdateStage answered, got {:?} \
         after {:?}",
        read["methodSettings"], settings
    );
}
