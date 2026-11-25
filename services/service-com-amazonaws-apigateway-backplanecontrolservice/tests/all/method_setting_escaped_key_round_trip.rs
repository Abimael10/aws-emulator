// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the `methodSettings` key a client that writes the method path with
//! ESCAPED separators reads back, and for the entry a second such patch addresses.
//!
//! The RULE under test, one level above any one call:
//!
//! > `Stage$methodSettings` is "a map that defines the method settings for a Stage resource.
//! > Keys (designated as /{method_setting_key) are method paths defined as
//! > {resource_path}/{http_method} for an individual method override, or /\*/\* for
//! > overriding all methods in the stage". A stage holds ONE entry per method path a patch
//! > pointer spelled, under that spelling VERBATIM: a pointer whose resource half escapes its
//! > separators `~1` — the spelling `PatchOperation$path` requires of a separator inside a
//! > path name — is reported under the escaped method path by every reader, with nothing
//! > decoded out of it; a method no pointer named gets no entry, and neither does the
//! > whole-stage path; a later pointer spelled the same way addresses the entry already there
//! > rather than a second one; and reading the stage adds no entry at all.
//!
//! This is the half of the key rule a client that ENABLES a setting and then looks the method
//! up in the map it read depends on: a reader that decoded the escape would report a key no
//! lookup of the patched path finds, and a second patch of that path could not be correlated
//! with the entry already present.
//!
//! The rows span what the rule generalises over: the DEPTH of the resource path (the root, a
//! child, a nested one, a deeper one), a part that is a path parameter, the HTTP verb, and the
//! wildcard method path, whose halves carry no separator to escape and so must come back
//! carrying no escape either. Each row derives its pointer, its key and the decoded spelling
//! that must NOT appear from the method itself rather than remembering a string, and runs on a
//! stage of its own, so the key set each assertion sees is the whole map of that stage.
//!
//! Every case drives the real wire path through [`Gateway`], this crate's one wire-test
//! harness, and reuses the stage, patch and collection-read helpers of the suites beside it.

use serde_json::{json, Value};

use crate::patch_updates::{patch_ok, replace};
use crate::rest_api_lifecycle::{create_api, Gateway};
use crate::stage_lifecycle::{create_deployed_stage, get_stage};
use crate::stage_method_settings_key::listed_method_settings;

/// One row: the method a pointer addresses a setting of, as the method path's two halves.
struct Row {
    case: &'static str,
    /// The resource path, as `Resource$path` spells one ("begins with a '/'"), or the
    /// wildcard that stands for every resource of the stage.
    resource_path: &'static str,
    /// The HTTP method, as `PutMethod$httpMethod` takes one, or the wildcard for every method.
    verb: &'static str,
}

const ROWS: &[Row] = &[
    Row {
        case: "the root resource",
        resource_path: "/",
        verb: "GET",
    },
    Row {
        case: "a child resource",
        resource_path: "/ok",
        verb: "GET",
    },
    Row {
        case: "a child resource at another verb",
        resource_path: "/orders",
        verb: "POST",
    },
    Row {
        case: "a nested resource path whose last part is a path parameter",
        resource_path: "/pets/{petId}",
        verb: "DELETE",
    },
    Row {
        case: "a deeper resource path",
        resource_path: "/a/b/c",
        verb: "PUT",
    },
    Row {
        case: "every method of the stage",
        resource_path: "*",
        verb: "*",
    },
];

/// The resource half of the pointer (and of the key): the resource path as ONE reference
/// token, every separator in it — its leading one included — escaped `~1` the way
/// `PatchOperation$path` requires of a path name. The wildcard names no path, so it carries
/// no separator to escape and stands as it is.
fn escaped_resource(resource_path: &str) -> String {
    resource_path.replace('/', "~1")
}

/// The `methodSettings` key a row's pointer spells: its escaped resource half, then the one
/// separator a key writes literally, then the HTTP method.
fn key_of(row: &Row) -> String {
    format!("{}/{}", escaped_resource(row.resource_path), row.verb)
}

/// The method path with its escapes DECODED — what the key would be if a reader reported the
/// path the key denotes instead of the path the pointer spelled. No reader may report this
/// spelling for a pointer that escaped its separators, so it is asserted ABSENT.
fn decoded_spelling(row: &Row) -> String {
    format!(
        "{}/{}",
        row.resource_path.trim_start_matches('/'),
        row.verb
    )
}

/// The pointer that writes `setting` of the row's method: the method path as the row spells
/// it, then the setting's published group-and-name path.
fn pointer(row: &Row, setting: &str) -> String {
    format!("/{}/{setting}", key_of(row))
}

/// The keys one reader reports, sorted.
fn keys_of(settings: &Value) -> Vec<String> {
    let mut keys: Vec<String> = settings
        .as_object()
        .map(|map| map.keys().cloned().collect())
        .unwrap_or_default();
    keys.sort();
    keys
}

/// The whole-stage method path, which a per-method pointer never writes
/// (`Stage$methodSettings` documents it as `/*/*`, which is `*/*` as a key).
const WHOLE_STAGE_KEY: &str = "*/*";

/// The `cacheTtlInSeconds` the second patch of each row writes: a span of seconds that is not
/// the value an entry carries for a setting nobody configured, so the assertion cannot pass on
/// a default.
const SECOND_TTL: &str = "45";

#[tokio::test]
async fn an_escaped_method_path_is_the_only_key_every_reader_reports() {
    let gateway = Gateway::new();
    let (api, _root) = create_api(&gateway, "escaped-method-path-key").await;

    for (index, row) in ROWS.iter().enumerate() {
        // A stage of its own per row, so the keys a reader reports ARE this row's whole map.
        let stage = format!("escaped{index}");
        create_deployed_stage(&gateway, &api, &stage).await;
        let uri = format!("/restapis/{api}/stages/{stage}");
        let key = key_of(row);
        let turn_on = pointer(row, "metrics/enabled");
        let case = format!("{} as {turn_on:?}", row.case);

        let answered = patch_ok(&gateway, &case, &uri, vec![replace(&turn_on, "true")]).await;
        let read = get_stage(&gateway, &api, &stage).await.body;
        let listed = listed_method_settings(&gateway, &api, &stage, &case).await;

        for (source, settings) in [
            ("the UpdateStage answer", &answered["methodSettings"]),
            ("GetStage", &read["methodSettings"]),
            ("GetStages", &listed),
        ] {
            // The map is exactly the one method the pointer named: no entry for a method
            // nobody patched, and no whole-stage entry beside it.
            let keys = keys_of(settings);
            assert_eq!(
                keys,
                vec![key.clone()],
                "case {case}: expected {source} to report the one key {key:?}, got {keys:?}"
            );
            assert_eq!(
                settings[&key]["metricsEnabled"],
                json!(true),
                "case {case}: expected {source} to report metricsEnabled under {key:?}, got \
                 {settings:?}"
            );
            // The spelling the pointer did NOT write: a reader that decoded the escape would
            // report this one, and then no lookup of the patched path would find the entry.
            let decoded = decoded_spelling(row);
            if decoded != key {
                assert!(
                    settings.get(&decoded).is_none(),
                    "case {case}: expected {source} to report no entry under the decoded \
                     spelling {decoded:?}, got {settings:?}"
                );
            }
            if key != WHOLE_STAGE_KEY {
                assert!(
                    settings.get(WHOLE_STAGE_KEY).is_none(),
                    "case {case}: expected {source} to report no whole-stage entry, got \
                     {settings:?}"
                );
            }
        }

        // Reading the stage changes nothing a later caller sees: the same read again reports
        // the very same map.
        let again = get_stage(&gateway, &api, &stage).await.body;
        assert_eq!(
            again["methodSettings"], read["methodSettings"],
            "case {case}: a second GetStage must report the same map, got {:?} after {:?}",
            again["methodSettings"], read["methodSettings"]
        );
    }
}

#[tokio::test]
async fn a_later_patch_of_the_escaped_key_writes_the_entry_already_there() {
    let gateway = Gateway::new();
    let (api, _root) = create_api(&gateway, "escaped-method-path-correlation").await;

    for (index, row) in ROWS.iter().enumerate() {
        let stage = format!("again{index}");
        create_deployed_stage(&gateway, &api, &stage).await;
        let uri = format!("/restapis/{api}/stages/{stage}");
        let key = key_of(row);
        let turn_on = pointer(row, "metrics/enabled");
        let case = format!("{} patched twice as {turn_on:?}", row.case);

        patch_ok(&gateway, &case, &uri, vec![replace(&turn_on, "true")]).await;
        // A second update of the SAME method path, in a request of its own and after the
        // stage has been read: it addresses the entry already there, so the stage carries one
        // entry carrying both settings rather than a second entry beside it.
        let read_between = get_stage(&gateway, &api, &stage).await;
        assert_eq!(
            read_between.status, 200,
            "case {case}: expected 200 from GetStage, got {} with {:?}",
            read_between.status, read_between.body
        );
        let ttl = pointer(row, "caching/ttlInSeconds");
        let answered = patch_ok(&gateway, &case, &uri, vec![replace(&ttl, SECOND_TTL)]).await;
        let read = get_stage(&gateway, &api, &stage).await.body;

        for (source, settings) in [
            ("the UpdateStage answer", &answered["methodSettings"]),
            ("GetStage", &read["methodSettings"]),
        ] {
            let keys = keys_of(settings);
            assert_eq!(
                keys,
                vec![key.clone()],
                "case {case}: expected {source} to report the one key {key:?} after both \
                 patches, got {keys:?}"
            );
            let entry = &settings[&key];
            assert_eq!(
                entry["metricsEnabled"],
                json!(true),
                "case {case}: expected {source} to keep the setting the first patch wrote, got \
                 {entry:?}"
            );
            assert_eq!(
                entry["cacheTtlInSeconds"],
                json!(SECOND_TTL.parse::<i64>().expect("a span of seconds")),
                "case {case}: expected {source} to report the TTL the second patch wrote, got \
                 {entry:?}"
            );
        }
    }
}
