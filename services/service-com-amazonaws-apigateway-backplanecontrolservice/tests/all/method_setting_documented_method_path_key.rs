// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the `methodSettings` entry a client writes under the method path
//! `Stage$methodSettings` documents a KEY as, and reads back under it.
//!
//! The RULE under test, one level above any one call:
//!
//! > `Stage$methodSettings` is "a map that defines the method settings for a Stage resource.
//! > Keys (designated as /{method_setting_key) are method paths defined as
//! > {resource_path}/{http_method} for an individual method override, or /\*/\* for
//! > overriding all methods in the stage". The key an entry is reported under is the method
//! > path the patch POINTER named, token for token — so a pointer naming the method path in
//! > the very form a key is documented in (the parts separated literally, carrying no
//! > JSON-Pointer escape) is reported under exactly that method path, by every reader of the
//! > stage and with no escape added to it; the entry under it carries the settings the patch
//! > wrote plus the service's own value for every setting it did not name; and that entry —
//! > the one naming the METHOD, not the one naming the stage — is the setting in force for
//! > the method the deployed stage serves.
//!
//! The `~1` escape belongs to `PatchOperation$path`, which requires it of a separator inside
//! a path NAME; a client that writes the method path with no such separator to escape writes
//! no escape, and reads none back. So the form this suite drives is the one a client that
//! READS the map by the documented key writes, and the key it reports carries no escape at
//! all — what the keys of the OTHER spellings of a method path are is the subject of the
//! sibling `method_setting_one_token_method_path` and `method_setting_resource_half_spellings`
//! suites, which drive each escaped spelling and pin the key it writes.
//!
//! The rows span what the rule generalises over: the resource path, the HTTP verb, the
//! setting written and its type (a flag, a count, a span of seconds, an enumerated strategy),
//! and the wildcard that stands for every method of the stage. Each row derives its pointer
//! and its key from the method rather than remembering a string.
//!
//! Every case drives the real wire path through [`Gateway`], this crate's one wire-test
//! harness, and the data-plane case reaches the deployed stage on the api's own
//! `execute-api` host, so what is asserted is what a client of the published invoke URL
//! sees.

use serde_json::{json, Value};

use crate::execute_api_data_plane::{
    deploy, integration_response, invoke, json_template, method_response, put_integration,
    put_method,
};
use crate::patch_updates::{patch_ok, replace};
use crate::rest_api_lifecycle::{create_api, resource_under, Gateway};
use crate::stage_lifecycle::{create_deployed_stage, get_stage};
use crate::stage_method_settings_key::listed_method_settings;
use crate::stage_response_cache::settled_cluster;

/// The stage every control-plane case patches.
const STAGE: &str = "prod";

/// One row: the method whose settings a pointer addresses, written the way a KEY is
/// documented, and the one setting the row writes.
struct Row {
    case: &'static str,
    /// The resource half of the method path, as the documented key form writes it: the
    /// resource path's parts separated literally and no leading separator — or the wildcard
    /// that names every resource of the stage.
    resource: &'static str,
    /// The HTTP method, as `PutMethod$httpMethod` takes one, or the wildcard for every
    /// method.
    verb: &'static str,
    /// The setting's published group-and-name path, the tail the pointer carries after the
    /// method path.
    setting: &'static str,
    /// The `MethodSetting` member that setting writes.
    member: &'static str,
    /// The `value` the patch sends, and the JSON a reader must then report for the member.
    value: &'static str,
    reported: Value,
}

/// The rows, each writing a setting whose value differs from the one an entry carries for a
/// setting nobody configured, so no row can pass on a default.
///
/// `loggingLevel` is left to the `stage_execution_logging_account_role` suite: turning a
/// method's execution logging on is gated on the ACCOUNT carrying a CloudWatch Logs role,
/// which is a precondition of its own rather than of the key form.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a child resource, caching turned on",
            resource: "c",
            verb: "GET",
            setting: "caching/enabled",
            member: "cachingEnabled",
            value: "true",
            reported: json!(true),
        },
        Row {
            case: "the same resource at another verb, a throttle count",
            resource: "c",
            verb: "POST",
            setting: "throttling/burstLimit",
            member: "throttlingBurstLimit",
            value: "77",
            reported: json!(77),
        },
        Row {
            case: "a resource part carrying a hyphen, a span of seconds",
            resource: "order-items",
            verb: "DELETE",
            setting: "caching/ttlInSeconds",
            member: "cacheTtlInSeconds",
            value: "45",
            reported: json!(45),
        },
        Row {
            case: "a resource part that is a path parameter, an enumerated strategy",
            resource: "{petId}",
            verb: "PATCH",
            setting: "caching/unauthorizedCacheControlHeaderStrategy",
            member: "unauthorizedCacheControlHeaderStrategy",
            value: "FAIL_WITH_403",
            reported: json!("FAIL_WITH_403"),
        },
        Row {
            case: "every method of the stage, the authorization of the invalidation ask",
            resource: "*",
            verb: "*",
            setting: "caching/requireAuthorizationForCacheControl",
            member: "requireAuthorizationForCacheControl",
            value: "false",
            reported: json!(false),
        },
    ]
}

/// The value an entry carries for each setting the patch did NOT name: `MethodSetting`'s
/// members (`smithy_query(query="shape", shape="MethodSetting")`) with API Gateway's own
/// values — the documented account-level burst of 5,000 requests and steady-state rate of
/// 10,000 requests per second, the documented default cache TTL of 300 seconds, and cache
/// invalidation requiring authorization and answering an unauthorized ask with the response
/// header. `loggingLevel` is the one member that stays absent until a patch sets it, so it is
/// not among the members a reader reports.
fn unconfigured_entry() -> Vec<(&'static str, Value)> {
    vec![
        ("metricsEnabled", json!(false)),
        ("dataTraceEnabled", json!(false)),
        ("throttlingBurstLimit", json!(5_000)),
        ("throttlingRateLimit", json!(10_000.0)),
        ("cachingEnabled", json!(false)),
        ("cacheTtlInSeconds", json!(300)),
        ("cacheDataEncrypted", json!(false)),
        ("requireAuthorizationForCacheControl", json!(true)),
        (
            "unauthorizedCacheControlHeaderStrategy",
            json!("SUCCEED_WITH_RESPONSE_HEADER"),
        ),
    ]
}

/// The whole entry a row's method must read back as: every member of an unconfigured entry,
/// with the one member the row wrote carrying the row's value.
fn expected_entry(row: &Row) -> Vec<(&'static str, Value)> {
    unconfigured_entry()
        .into_iter()
        .map(|(member, unconfigured)| {
            if member == row.member {
                (member, row.reported.clone())
            } else {
                (member, unconfigured)
            }
        })
        .collect()
}

/// The keys (or members) of a JSON object, sorted.
fn sorted_keys(value: &Value) -> Vec<String> {
    let mut keys: Vec<String> = value
        .as_object()
        .map(|map| map.keys().cloned().collect())
        .unwrap_or_default();
    keys.sort();
    keys
}

#[tokio::test]
async fn a_documented_method_path_is_the_key_every_reader_reports() {
    let gateway = Gateway::new();
    let (api, _root) = create_api(&gateway, "documented-method-path-key").await;
    create_deployed_stage(&gateway, &api, STAGE).await;
    let uri = format!("/restapis/{api}/stages/{STAGE}");

    let mut expected_keys: Vec<String> = Vec::new();
    for row in rows() {
        // The key the pointer names, and the pointer that writes that setting of it: the
        // method path, then the setting's published path.
        let key = format!("{}/{}", row.resource, row.verb);
        let pointer = format!("/{key}/{}", row.setting);
        let case = format!("{} as {pointer:?}", row.case);

        let answered = patch_ok(&gateway, &case, &uri, vec![replace(&pointer, row.value)]).await;
        expected_keys.push(key.clone());
        expected_keys.sort();
        let read = get_stage(&gateway, &api, STAGE).await.body;
        // The COLLECTION reader too, through the sibling suite's one copy of that read.
        let listed = listed_method_settings(&gateway, &api, STAGE, &case).await;

        for (source, settings) in [
            ("the UpdateStage answer", &answered["methodSettings"]),
            ("GetStage", &read["methodSettings"]),
            ("GetStages", &listed),
        ] {
            // Every method written so far, under the key its own pointer named, and not one
            // key more: a row writes its own entry and rewrites no other.
            let keys = sorted_keys(settings);
            assert_eq!(
                keys, expected_keys,
                "case {case}: expected {source} to report the keys {expected_keys:?}, got \
                 {keys:?}"
            );
            // And no key carries the escape that belongs to the pointer: none of these
            // pointers spelled one, so no reader may introduce one.
            let escaped: Vec<&String> = keys.iter().filter(|key| key.contains('~')).collect();
            assert!(
                escaped.is_empty(),
                "case {case}: expected {source} to report method paths carrying no pointer \
                 escape, got {escaped:?}"
            );

            let entry = &settings[key.as_str()];
            for (member, expected) in expected_entry(&row) {
                assert_eq!(
                    entry[member], expected,
                    "case {case}: expected {source} to report {member} as {expected:?} under \
                     {key:?}, got {entry:?}"
                );
            }
            let members = sorted_keys(entry);
            let mut expected_members: Vec<String> = unconfigured_entry()
                .into_iter()
                .map(|(member, _)| member.to_string())
                .collect();
            expected_members.sort();
            assert_eq!(
                members, expected_members,
                "case {case}: expected {source} to report the members {expected_members:?} \
                 under {key:?}, got {members:?}"
            );
        }
    }
}

/// The stage the data-plane case deploys to, which carries a cache cluster: "a cache cluster
/// must be enabled on the stage for responses to be cached" (`MethodSetting$cachingEnabled`).
const CACHED_STAGE: &str = "cached";

/// The `cacheTtlInSeconds` both cached methods carry — the documented default, long enough
/// that nothing here waits for an entry to age out.
const TTL: &str = "300";

/// The ask that reloads a cached entry: the header API Gateway documents for it, which needs
/// the `execute-api:InvalidateCache` grant on a method whose settings require authorization
/// for it.
const INVALIDATION_ASK: (&str, &str) = ("cache-control", "max-age=0");

/// What a caller that presented NO credential is refused an unauthorized ask with.
const REFUSED_UNSIGNED_MESSAGE: &str = "Missing Authentication Token";

/// The response template every method of the data-plane case carries: it renders the serial
/// the INTEGRATION answered, so what a client reads says which answer was mapped — the one
/// the cluster holds or a fresh one.
const ECHO_TEMPLATE: &str = "{\"serial\":\"$input.path('$.serial')\"}";

/// The `requestTemplates` of a `MOCK` whose answer carries `serial`: a `MOCK` integration
/// answers its own rendered integration request, and the `statusCode` that rendering names
/// selects the integration response that maps it.
fn answers(serial: &str) -> Value {
    json!({ "application/json": format!("{{\"statusCode\":200,\"serial\":\"{serial}\"}}") })
}

/// The body a client reads for the answer carrying `serial`, mapped through
/// [`ECHO_TEMPLATE`].
fn body_of(serial: &str) -> String {
    format!("{{\"serial\":\"{serial}\"}}")
}

/// The serial the method on `part` answers while the cluster is being filled, and the one it
/// answers after its integration has been rewritten: per resource, so one method's held entry
/// can never stand in for another's.
fn first_serial(part: &str) -> String {
    format!("{part}-1")
}
fn second_serial(part: &str) -> String {
    format!("{part}-2")
}

/// One `GET` resource under `root` on a `MOCK` integration answering [`first_serial`], its
/// answer mapped through [`ECHO_TEMPLATE`]; answers the resource id, for rewriting that
/// answer later.
async fn echo_resource(gateway: &Gateway, api: &str, root: &str, part: &str) -> String {
    let resource = resource_under(gateway, api, root, part).await;
    put_method(
        gateway,
        api,
        &resource,
        "GET",
        json!({ "authorizationType": "NONE" }),
    )
    .await;
    put_integration(
        gateway,
        api,
        &resource,
        "GET",
        json!({ "type": "MOCK", "requestTemplates": answers(&first_serial(part)) }),
    )
    .await;
    method_response(gateway, api, &resource, "GET", "200", json!({})).await;
    integration_response(
        gateway,
        api,
        &resource,
        "GET",
        "200",
        json!({ "responseTemplates": json_template(ECHO_TEMPLATE) }),
    )
    .await;
    resource
}

/// Rewrite what `resource`'s integration ANSWERS to [`second_serial`], so a request the
/// integration answers reads it and one answered from the cluster does not.
async fn rewrite_answer(gateway: &Gateway, api: &str, resource: &str, part: &str, case: &str) {
    let patched = gateway
        .call(
            "PATCH",
            &format!("/restapis/{api}/resources/{resource}/methods/GET/integration"),
            Some(json!({
                "patchOperations": [{
                    "op": "replace",
                    "path": "/requestTemplates/application~1json",
                    "value": answers(&second_serial(part))["application/json"]
                }]
            })),
        )
        .await;
    assert_eq!(
        patched.status, 200,
        "case {case}: expected 200 from UpdateIntegration, got {} with {:?}",
        patched.status, patched.body
    );
}

/// The patch that turns caching on for one method under the key its own method path names,
/// and says whether an invalidation ask of it needs authorization.
fn cached_at(key: &str, requires_authorization: bool) -> Vec<Value> {
    vec![
        replace(&format!("/{key}/caching/enabled"), "true"),
        replace(&format!("/{key}/caching/ttlInSeconds"), TTL),
        replace(
            &format!("/{key}/caching/requireAuthorizationForCacheControl"),
            &requires_authorization.to_string(),
        ),
        replace(
            &format!("/{key}/caching/unauthorizedCacheControlHeaderStrategy"),
            "FAIL_WITH_403",
        ),
    ]
}

#[tokio::test]
async fn each_methods_own_entry_under_its_method_path_key_is_the_setting_in_force() {
    // Two methods of ONE stage whose entries — each under the method path that names it —
    // differ in the ONE setting that decides an unauthorized invalidation ask, so the answer
    // each serves says which entry was resolved for it. A key resolved by anything but the
    // method it names would answer both the same way.
    let rows: [(&str, &str, bool, u16); 2] = [
        (
            "the method whose entry requires no authorization for the ask",
            "open",
            false,
            200,
        ),
        (
            "the method whose entry requires authorization for it",
            "locked",
            true,
            403,
        ),
    ];

    let gateway = Gateway::new();
    let (api, root) = create_api(&gateway, "per-method-cache-control").await;
    let mut resources: Vec<String> = Vec::new();
    for (_, part, _, _) in rows {
        resources.push(echo_resource(&gateway, &api, &root, part).await);
    }
    let deployed = gateway
        .call(
            "POST",
            &format!("/restapis/{api}/deployments"),
            Some(json!({
                "stageName": CACHED_STAGE,
                "cacheClusterEnabled": true,
                "cacheClusterSize": "0.5"
            })),
        )
        .await;
    assert_eq!(
        deployed.status, 201,
        "case CreateDeployment({CACHED_STAGE}): expected 201, got {} with {:?}",
        deployed.status, deployed.body
    );
    settled_cluster(&gateway, &api, CACHED_STAGE, "the cached stage").await;

    // Every method's settings in one request, each under the key its own method path names.
    let uri = format!("/restapis/{api}/stages/{CACHED_STAGE}");
    let operations: Vec<Value> = rows
        .iter()
        .flat_map(|(_, part, requires, _)| cached_at(&format!("{part}/GET"), *requires))
        .collect();
    let answered = patch_ok(&gateway, "UpdateStage(per-method caching)", &uri, operations).await;

    // The control-plane half: each method's entry is readable under the method path the
    // patch named, and the two differ only in the setting the rows differ in.
    let read = get_stage(&gateway, &api, CACHED_STAGE).await.body;
    for (case, part, requires, _) in rows {
        let key = format!("{part}/GET");
        for (source, settings) in [
            ("the UpdateStage answer", &answered["methodSettings"]),
            ("GetStage", &read["methodSettings"]),
        ] {
            let entry = &settings[key.as_str()];
            assert_eq!(
                entry["cachingEnabled"],
                json!(true),
                "case {case}: expected {source} to report caching on under {key:?}, got \
                 {settings:?}"
            );
            assert_eq!(
                entry["requireAuthorizationForCacheControl"],
                json!(requires),
                "case {case}: expected {source} to report \
                 requireAuthorizationForCacheControl {requires} under {key:?}, got {entry:?}"
            );
        }
    }

    // Fill each method's entry, then rewrite what its integration answers and redeploy, so
    // the body a client reads says whether the ask reloaded the entry.
    for (index, (case, part, _, _)) in rows.iter().enumerate() {
        let path = format!("/{CACHED_STAGE}/{part}");
        let filled = invoke(&gateway, &api, "GET", &path, &[]).await;
        assert_eq!(
            filled.status, 200,
            "case {case}: expected 200 from the first invoke, got {} with {:?}",
            filled.status, filled.body
        );
        assert_eq!(
            filled.body,
            body_of(&first_serial(part)),
            "case {case}: expected the integration's own answer first, got {:?}",
            filled.body
        );
        rewrite_answer(&gateway, &api, &resources[index], part, case).await;
    }
    deploy(&gateway, &api, CACHED_STAGE).await;

    for (case, part, _, status) in rows {
        let path = format!("/{CACHED_STAGE}/{part}");
        let asked = invoke(&gateway, &api, "GET", &path, &[INVALIDATION_ASK]).await;
        assert_eq!(
            asked.status, status,
            "case {case}: expected {status} for the invalidation ask, got {} with {:?}",
            asked.status, asked.body
        );
        if status == 200 {
            // The entry its own settings let the ask reload: the integration answered afresh.
            assert_eq!(
                asked.body,
                body_of(&second_serial(part)),
                "case {case}: expected the reloaded answer, got {:?}",
                asked.body
            );
        } else {
            // A caller that presented no credential authenticated nobody, so the ask its
            // settings require authorization for earns that refusal — and the entry it could
            // not reload is still the one the method is answered from.
            assert_eq!(
                asked.message().as_deref(),
                Some(REFUSED_UNSIGNED_MESSAGE),
                "case {case}: expected the missing-token refusal, got {:?}",
                asked.body
            );
            let held = invoke(&gateway, &api, "GET", &path, &[]).await;
            assert_eq!(
                held.body,
                body_of(&first_serial(part)),
                "case {case}: expected the entry the refused ask left held, got {:?}",
                held.body
            );
        }
    }
}
