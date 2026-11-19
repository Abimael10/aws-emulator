// SPDX-License-Identifier: CC0-1.0

//! The security plugin's AUDIT CONFIGURATION, driven over the real wire path: a signed
//! request to the endpoint host a domain published, through the multi-service router, into
//! the mounted service — and the records it gates read back through the CloudWatch Logs
//! service's OWN `FilterLogEvents`, so what is asserted is what a client of the log group
//! sees.
//!
//! Two rules, each asserted over a SET of inputs rather than one call:
//!
//! > the security plugin's configuration API is reachable under the plugin's current
//! > `_plugins/<plugin>` prefix AND under its legacy `_opendistro/<plugin>` one, both
//! > spellings reaching ONE resource: its audit configuration is READ whole — the paths the
//! > API will not write beside the configuration in force — WRITTEN by an RFC 6902 JSON
//! > Patch over that document or by restating it whole, and reachable by the master user
//! > alone on a domain that runs the plugin at all;
//!
//! > a domain publishing `AUDIT_LOGS` delivers into the log group that option names one
//! > record per decision its cluster took about a request — who the caller is, at the REST
//! > layer, and what that caller may do, at the transport layer — but only for a decision
//! > its own audit configuration records: the whole switch, the layer's switch, the
//! > categories that layer excludes, the users it ignores and the request paths it ignores
//! > each leave the group untouched on their own.
//!
//! The harness, the endpoint resolution and the signed-request helper are [`super`]'s, the
//! log-group harness and the record reader are the log-publishing suite's; this suite adds
//! no second copy of either.

use super::*;

use crate::domain_lifecycle::log_group_arn;
use crate::fgac_internal_user_database::rest as basic_rest;
use crate::log_publishing_delivery::events;

/// The log group the audited domain publishes `AUDIT_LOGS` to.
const AUDIT_GROUP: &str = "/aws/opensearch/domains/audit";

/// The index whose documents every audited request below reads or writes.
const INDEX: &str = "ledger";

/// The document that index holds.
const DOCUMENT: &str = "one";

/// The resource under the prefix the plugin's API carries today.
const CURRENT_PATH: &str = "/_plugins/_security/api/audit";

/// The same resource under the prefix it carried in the Open Distro era, which the tooling
/// and the setup scripts written then still address.
const LEGACY_PATH: &str = "/_opendistro/_security/api/audit";

/// The route that states the whole configuration rather than a patch of it.
const CONFIG_PATH: &str = "/_plugins/_security/api/audit/config";

/// A domain with fine-grained access control switched on, publishing `AUDIT_LOGS` to a log
/// group CloudWatch Logs itself holds, with this emulator's own credential as its master
/// user — and one index with one document on it.
async fn given_audited_domain(domain: &str) -> (Emulator, String) {
    let emulator = Emulator::with_log_groups();
    emulator.created_log_group(AUDIT_GROUP).await;
    let created = emulator
        .create(json!({
            "DomainName": domain,
            "EncryptionAtRestOptions": { "Enabled": true },
            "NodeToNodeEncryptionOptions": { "Enabled": true },
            "DomainEndpointOptions": { "EnforceHTTPS": true },
            "AdvancedSecurityOptions": {
                "Enabled": true,
                "InternalUserDatabaseEnabled": false,
                "MasterUserOptions": { "MasterUserARN": master_user_arn() }
            },
            "LogPublishingOptions": {
                "AUDIT_LOGS": {
                    "Enabled": true,
                    "CloudWatchLogsLogGroupArn": log_group_arn(AUDIT_GROUP),
                }
            }
        }))
        .await;
    assert_eq!(
        created.status, 200,
        "case \"CreateDomain publishing AUDIT_LOGS\": expected 200, got {} with {:?}",
        created.status, created.body
    );
    let host = endpoint_of(&emulator, domain);
    let seeded = rest(
        &emulator,
        DEFAULT_ACCESS_KEY_ID,
        &host,
        "PUT",
        &format!("/{INDEX}/_doc/{DOCUMENT}"),
        Some(json!({ "title": "an audited document" })),
    )
    .await;
    assert_eq!(
        seeded.status, 201,
        "case \"seeding the index\": expected 201, got {} with {:?}",
        seeded.status, seeded.body
    );
    (emulator, host)
}

/// One request to the audit resource as the MASTER user, which every read and write below
/// is taken by.
async fn as_master(
    emulator: &Emulator,
    host: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Answer {
    rest(emulator, DEFAULT_ACCESS_KEY_ID, host, method, path, body).await
}

/// The configuration read off the resource.
async fn configuration(emulator: &Emulator, host: &str, case: &str) -> Value {
    let read = as_master(emulator, host, "GET", CURRENT_PATH, None).await;
    assert_eq!(
        read.status, 200,
        "case {case}: expected the audit config to be readable, got {} with {:?}",
        read.status, read.body
    );
    read.body["config"].clone()
}

/// The audit records the group holds, each parsed back from the line it was delivered as.
async fn audit_records(emulator: &Emulator) -> Vec<Value> {
    events(emulator, AUDIT_GROUP)
        .await
        .iter()
        .map(|message| {
            ::serde_json::from_str(message).unwrap_or_else(|failed| {
                panic!("expected an audit record to be one JSON document, got {message:?}: {failed}")
            })
        })
        .collect()
}

/// The (category, layer) pair each of `records` states.
fn decisions(records: &[Value]) -> Vec<(String, String)> {
    records
        .iter()
        .map(|record| {
            (
                record["audit_category"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                record["audit_request_layer"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect()
}

/// The whole configuration, every layer and category on, as the resource's own `PUT` takes
/// it — the state each row of the gating table starts from.
///
/// It restates the read-only paths at the values in force, which is the only way an API
/// that will not write them takes a body that names them at all.
fn every_decision_recorded() -> Value {
    json!({
        "enabled": true,
        "audit": {
            "enable_rest": true,
            "enable_transport": true,
            "disabled_rest_categories": [],
            "disabled_transport_categories": [],
            "ignore_users": [],
            "ignore_requests": [],
            "log_request_body": true,
            "resolve_indices": true,
            "resolve_bulk_requests": false,
            "exclude_sensitive_headers": true,
        },
        "compliance": {
            "enabled": true,
            "internal_config": true,
            "external_config": false,
        },
    })
}

/// Put that configuration in force, through the route that states the document whole.
async fn given_every_decision_recorded(emulator: &Emulator, host: &str, case: &str) {
    let stated = as_master(
        emulator,
        host,
        "PUT",
        CONFIG_PATH,
        Some(every_decision_recorded()),
    )
    .await;
    assert_eq!(
        stated.status, 200,
        "case {case}: expected the whole configuration to be taken, got {} with {:?}",
        stated.status, stated.body
    );
}

/// RULE: the plugin's audit-configuration resource is read and written under both of the
/// plugin's prefixes, and both spellings reach ONE configuration.
#[tokio::test]
async fn the_audit_configuration_is_read_and_written_under_both_plugin_prefixes() {
    let (emulator, host) = given_audited_domain("audit-config").await;

    // Read under either prefix, the resource answers the document the plugin's API answers:
    // the paths it will not write, and the configuration in force.
    for path in [CURRENT_PATH, LEGACY_PATH] {
        let read = as_master(&emulator, &host, "GET", path, None).await;
        assert_eq!(
            read.status, 200,
            "case GET {path}: expected 200, got {} with {:?}",
            read.status, read.body
        );
        assert!(
            read.body["_readonly"].is_array(),
            "case GET {path}: expected the read-only paths to be listed, got {:?}",
            read.body
        );
        assert!(
            read.body["config"]["enabled"].is_boolean(),
            "case GET {path}: expected the whole switch to be reported, got {:?}",
            read.body
        );
        for member in ["enable_rest", "enable_transport"] {
            assert!(
                read.body["config"]["audit"][member].is_boolean(),
                "case GET {path}: expected {member} to be reported, got {:?}",
                read.body
            );
        }
        for member in ["disabled_rest_categories", "disabled_transport_categories"] {
            assert!(
                read.body["config"]["audit"][member].is_array(),
                "case GET {path}: expected {member} to be reported, got {:?}",
                read.body
            );
        }
    }

    // The patch an idempotent setup step sends, under each prefix in turn: the second
    // application changes nothing, so a step that re-runs reads the same configuration back.
    let patch = json!([
        { "op": "replace", "path": "/config/enabled", "value": true },
        { "op": "replace", "path": "/config/audit/enable_rest", "value": true },
        { "op": "replace", "path": "/config/audit/enable_transport", "value": true },
        { "op": "replace", "path": "/config/audit/disabled_rest_categories", "value": [] },
        { "op": "replace", "path": "/config/audit/disabled_transport_categories", "value": [] },
    ]);
    for path in [CURRENT_PATH, LEGACY_PATH, CURRENT_PATH] {
        let patched = as_master(&emulator, &host, "PATCH", path, Some(patch.clone())).await;
        assert_eq!(
            patched.status, 200,
            "case PATCH {path}: expected 200, got {} with {:?}",
            patched.status, patched.body
        );
        // Re-read under BOTH prefixes: one configuration, two spellings of the resource.
        for read_path in [CURRENT_PATH, LEGACY_PATH] {
            let read = as_master(&emulator, &host, "GET", read_path, None).await;
            assert_eq!(
                (
                    read.body["config"]["enabled"].clone(),
                    read.body["config"]["audit"]["enable_rest"].clone(),
                    read.body["config"]["audit"]["enable_transport"].clone(),
                    read.body["config"]["audit"]["disabled_rest_categories"].clone(),
                    read.body["config"]["audit"]["disabled_transport_categories"].clone(),
                ),
                (json!(true), json!(true), json!(true), json!([]), json!([])),
                "case \"PATCH {path} then GET {read_path}\": expected the patched \
                 configuration, got {:?}",
                read.body["config"]
            );
        }
    }

    // One member patched on its own leaves every other member of the document as it was.
    let before = configuration(&emulator, &host, "\"before a single-member patch\"").await;
    let single = as_master(
        &emulator,
        &host,
        "PATCH",
        LEGACY_PATH,
        Some(json!([{
            "op": "replace",
            "path": "/config/audit/disabled_transport_categories",
            "value": ["GRANTED_PRIVILEGES"],
        }])),
    )
    .await;
    assert_eq!(
        single.status, 200,
        "case \"a single-member patch\": expected 200, got {} with {:?}",
        single.status, single.body
    );
    let after = configuration(&emulator, &host, "\"after a single-member patch\"").await;
    assert_eq!(
        after["audit"]["disabled_transport_categories"],
        json!(["GRANTED_PRIVILEGES"]),
        "case \"a single-member patch\": expected the named member to change, got {after:?}"
    );
    assert_eq!(
        after["audit"]["disabled_rest_categories"],
        before["audit"]["disabled_rest_categories"],
        "case \"a single-member patch\": expected every other member to be left as it was, \
         got {after:?}"
    );

    // A patch naming a path the API does not write is refused, and the document is left as
    // it was. The path is read off the resource's own `_readonly` list rather than written
    // here, so the refusal covers each path that list names.
    let readonly: Vec<String> = as_master(&emulator, &host, "GET", CURRENT_PATH, None)
        .await
        .body["_readonly"]
        .as_array()
        .map(|paths| {
            paths
                .iter()
                .map(|path| path.as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default();
    assert!(
        !readonly.is_empty(),
        "case \"the read-only paths\": expected the resource to name some, got none"
    );
    for path in &readonly {
        let before = configuration(&emulator, &host, path).await;
        // The value under test is the NEGATION of the one in force, so each row really
        // changes the read-only property rather than restating it.
        let changed = match before.pointer(path).and_then(Value::as_bool) {
            Some(flag) => json!(!flag),
            None => Value::Null,
        };
        // The path named directly, and the OBJECT that holds it restated with that value:
        // a write cannot reach a path the API does not write by naming its parent either.
        let mut operations = vec![json!({
            "op": "replace",
            "path": format!("/config{path}"),
            "value": changed.clone(),
        })];
        if let Some((section, member)) = path.rsplit_once('/').filter(|(s, _)| !s.is_empty()) {
            operations.push(json!({
                "op": "replace",
                "path": format!("/config{section}"),
                "value": { member: changed.clone() },
            }));
        }
        for operation in operations {
            let refused = as_master(
                &emulator,
                &host,
                "PATCH",
                CURRENT_PATH,
                Some(json!([operation.clone()])),
            )
            .await;
            assert_eq!(
                (refused.status, refused.body["message"].clone()),
                (400, json!("Attempted to update read-only property.")),
                "case \"a patch of the read-only path {path} through {operation}\": got {} with \
                 {:?}",
                refused.status,
                refused.body
            );
            assert_eq!(
                configuration(&emulator, &host, path).await,
                before,
                "case \"a patch of the read-only path {path} through {operation}\": expected \
                 the configuration to be left as it was"
            );
        }
    }

    // A body that states no patch at all is refused, and so is a method the resource does
    // not take.
    let unreadable = as_master(
        &emulator,
        &host,
        "PATCH",
        CURRENT_PATH,
        Some(json!({ "enabled": true })),
    )
    .await;
    assert_eq!(
        (unreadable.status, unreadable.body["message"].clone()),
        (400, json!("Could not parse content of request.")),
        "case \"a PATCH body that is not a JSON patch\": got {} with {:?}",
        unreadable.status,
        unreadable.body
    );
    let wrong = as_master(&emulator, &host, "DELETE", CURRENT_PATH, None).await;
    assert_eq!(
        wrong.status, 405,
        "case \"DELETE on the audit resource\": expected the method refusal, got {} with {:?}",
        wrong.status, wrong.body
    );

    // A patch this resource will not apply, each refused with the configuration left as it
    // was: one that duplicates the whole document until it is past the bound on how much
    // configuration a domain holds, and one carrying more operations than it applies.
    let before = configuration(&emulator, &host, "\"before an unapplied patch\"").await;
    let duplicating: Vec<Value> = (0..32)
        .map(|step| {
            json!({ "op": "copy", "from": "/config", "path": format!("/config/copy{step}") })
        })
        .collect();
    let crowding: Vec<Value> = (0..65)
        .map(|_| json!({ "op": "replace", "path": "/config/enabled", "value": true }))
        .collect();
    for (case, operations) in [
        ("a patch that duplicates the configuration", duplicating),
        ("a patch of more operations than are applied", crowding),
    ] {
        let refused = as_master(
            &emulator,
            &host,
            "PATCH",
            CURRENT_PATH,
            Some(Value::Array(operations)),
        )
        .await;
        assert_eq!(
            refused.status, 400,
            "case {case:?}: expected the write to be refused, got {} with {:?}",
            refused.status, refused.body
        );
        assert_eq!(
            configuration(&emulator, &host, case).await,
            before,
            "case {case:?}: expected the configuration to be left as it was"
        );
    }

    // And the whole document restated through the resource's own `PUT`.
    given_every_decision_recorded(&emulator, &host, "\"the whole document restated\"").await;
    let restated = configuration(&emulator, &host, "\"the whole document restated\"").await;
    assert_eq!(
        restated,
        every_decision_recorded(),
        "case \"the whole document restated\": expected the document that was stated"
    );
}

/// RULE: the configuration is the security configuration, so only the master user reads or
/// writes it — and a domain that runs no security plugin serves the route at all.
#[tokio::test]
async fn the_audit_configuration_is_the_master_users_alone() {
    let (emulator, host) = given_audited_domain("audit-guarded").await;
    given_session(&emulator);
    given_data_plane_policy(&emulator);

    // Every method of the resource, refused for a principal that is not the master user
    // however much IAM allows it.
    let requests: [(&str, &str, Option<Value>); 4] = [
        ("GET", CURRENT_PATH, None),
        ("GET", LEGACY_PATH, None),
        (
            "PATCH",
            CURRENT_PATH,
            Some(json!([{ "op": "replace", "path": "/config/enabled", "value": false }])),
        ),
        ("PUT", CONFIG_PATH, Some(every_decision_recorded())),
    ];
    for (method, path, body) in requests {
        let refused = rest(&emulator, SESSION_KEY_ID, &host, method, path, body).await;
        assert_eq!(
            (refused.status, error_type(&refused)),
            (403, "security_exception".to_string()),
            "case \"{method} {path} as an unmapped principal\": got {} with {:?}",
            refused.status,
            refused.body
        );
    }

    // A domain WITHOUT fine-grained access control runs no security plugin, so the resource
    // is not registered on it at all — answered the way the engine answers a path it has no
    // handler for, under either prefix.
    let plain = Emulator::new();
    let created = plain
        .create(json!({ "DomainName": "audit-plain" }))
        .await;
    assert_eq!(
        created.status, 200,
        "case \"CreateDomain without fine-grained access control\": got {} with {:?}",
        created.status, created.body
    );
    let plain_host = endpoint_of(&plain, "audit-plain");
    for path in [CURRENT_PATH, LEGACY_PATH] {
        let unserved = rest(&plain, DEFAULT_ACCESS_KEY_ID, &plain_host, "GET", path, None).await;
        assert_eq!(
            (unserved.status, unserved.body["error"].clone()),
            (
                400,
                json!(format!("no handler found for uri [{path}] and method [GET]"))
            ),
            "case \"GET {path} on a domain without the plugin\": got {} with {:?}",
            unserved.status,
            unserved.body
        );
    }
}

/// One row of the gating table: the configuration in force, and the decisions a document
/// read then leaves in the log group.
struct Gate {
    case: &'static str,
    /// The patch that puts the row's configuration in force, over the configuration that
    /// records every decision.
    patch: Value,
    /// The (category, layer) pairs the read leaves, in the order they are delivered.
    expected: Vec<(String, String)>,
}

/// The two decisions a request the cluster admits and permits leaves.
fn admitted_and_permitted() -> Vec<(String, String)> {
    vec![
        ("AUTHENTICATED".to_string(), "REST".to_string()),
        ("GRANTED_PRIVILEGES".to_string(), "TRANSPORT".to_string()),
    ]
}

/// RULE: a decision reaches the `AUDIT_LOGS` group only where the domain's own audit
/// configuration records it.
#[tokio::test]
async fn the_audit_configuration_decides_which_decisions_reach_the_log_group() {
    let (emulator, host) = given_audited_domain("audit-gating").await;

    // Before anything configures auditing, the group the domain publishes to stays empty
    // however much work the domain does: naming the group is not enabling the log.
    let read = as_master(
        &emulator,
        &host,
        "GET",
        &format!("/{INDEX}/_doc/{DOCUMENT}"),
        None,
    )
    .await;
    assert_eq!(
        read.status, 200,
        "case \"a document read before auditing is configured\": got {} with {:?}",
        read.status, read.body
    );
    assert!(
        audit_records(&emulator).await.is_empty(),
        "case \"a document read before auditing is configured\": expected no record, got {:?}",
        audit_records(&emulator).await
    );

    let rows = vec![
        Gate {
            case: "every decision recorded",
            patch: json!([]),
            expected: admitted_and_permitted(),
        },
        Gate {
            case: "auditing switched off",
            patch: json!([{ "op": "replace", "path": "/config/enabled", "value": false }]),
            expected: Vec::new(),
        },
        Gate {
            case: "the REST layer switched off",
            patch: json!([{ "op": "replace", "path": "/config/audit/enable_rest", "value": false }]),
            expected: vec![("GRANTED_PRIVILEGES".to_string(), "TRANSPORT".to_string())],
        },
        Gate {
            case: "the transport layer switched off",
            patch: json!([{
                "op": "replace",
                "path": "/config/audit/enable_transport",
                "value": false,
            }]),
            expected: vec![("AUTHENTICATED".to_string(), "REST".to_string())],
        },
        Gate {
            case: "the REST layer's own category excluded",
            patch: json!([{
                "op": "replace",
                "path": "/config/audit/disabled_rest_categories",
                "value": ["AUTHENTICATED"],
            }]),
            expected: vec![("GRANTED_PRIVILEGES".to_string(), "TRANSPORT".to_string())],
        },
        Gate {
            case: "the transport layer's own category excluded",
            patch: json!([{
                "op": "replace",
                "path": "/config/audit/disabled_transport_categories",
                "value": ["GRANTED_PRIVILEGES"],
            }]),
            expected: vec![("AUTHENTICATED".to_string(), "REST".to_string())],
        },
        Gate {
            // A category the read does not produce is excluded, which changes nothing: the
            // exclusion is per category and not a switch for the layer.
            case: "another layer's category excluded",
            patch: json!([{
                "op": "replace",
                "path": "/config/audit/disabled_rest_categories",
                "value": ["FAILED_LOGIN"],
            }]),
            expected: admitted_and_permitted(),
        },
        Gate {
            case: "the caller ignored",
            patch: json!([{
                "op": "replace",
                "path": "/config/audit/ignore_users",
                "value": [master_user_arn()],
            }]),
            expected: Vec::new(),
        },
        Gate {
            case: "the caller ignored by a wildcard",
            patch: json!([{
                "op": "replace",
                "path": "/config/audit/ignore_users",
                "value": ["arn:aws:iam::*"],
            }]),
            expected: Vec::new(),
        },
        Gate {
            case: "the request path ignored",
            patch: json!([{
                "op": "replace",
                "path": "/config/audit/ignore_requests",
                "value": [format!("/{INDEX}/_doc/*")],
            }]),
            expected: Vec::new(),
        },
        Gate {
            // A pattern matching another path leaves the read audited.
            case: "another request path ignored",
            patch: json!([{
                "op": "replace",
                "path": "/config/audit/ignore_requests",
                "value": ["/_cluster/*"],
            }]),
            expected: admitted_and_permitted(),
        },
    ];

    for row in &rows {
        // Every row starts from the configuration that records everything, so what decides
        // the outcome is the one change under test rather than the previous row's.
        given_every_decision_recorded(&emulator, &host, row.case).await;
        let patched = as_master(
            &emulator,
            &host,
            "PATCH",
            CURRENT_PATH,
            Some(row.patch.clone()),
        )
        .await;
        assert_eq!(
            patched.status, 200,
            "case {}: expected the patch to be taken, got {} with {:?}",
            row.case, patched.status, patched.body
        );

        let before = audit_records(&emulator).await.len();
        let read = as_master(
            &emulator,
            &host,
            "GET",
            &format!("/{INDEX}/_doc/{DOCUMENT}"),
            None,
        )
        .await;
        assert_eq!(
            read.status, 200,
            "case {}: expected the document read to be served, got {} with {:?}",
            row.case, read.status, read.body
        );
        let records = audit_records(&emulator).await;
        let produced = decisions(&records[before..]);
        assert_eq!(
            produced, row.expected,
            "case {}: expected {:?} from the document read, got {:?}",
            row.case, row.expected, produced
        );
    }
}

/// RULE: the record states the DECISION and nothing of the answer, so two reads the cluster
/// decided alike are audited alike however each was answered — and a decision the cluster
/// took against the caller is recorded under its own category.
#[tokio::test]
async fn an_audit_record_states_the_decision_and_not_the_answer() {
    let (emulator, host) = given_audited_domain("audit-records").await;
    given_every_decision_recorded(&emulator, &host, "\"the records of a read\"").await;

    // A document the index holds and one it does not: the cluster decided both the same
    // way, so the two reads leave the same decisions, under the same fields, and no field
    // of either names a response status.
    let mut audited: Vec<Vec<Value>> = Vec::new();
    for (case, id, status) in [
        ("a document the index holds", DOCUMENT, 200),
        ("an id the index does not hold", "absent", 404),
    ] {
        let before = audit_records(&emulator).await.len();
        let read = as_master(&emulator, &host, "GET", &format!("/{INDEX}/_doc/{id}"), None).await;
        assert_eq!(
            read.status, status,
            "case {case:?}: expected {status}, got {} with {:?}",
            read.status, read.body
        );
        let records = audit_records(&emulator).await;
        audited.push(records[before..].to_vec());
    }
    let [found, missing] = <[Vec<Value>; 2]>::try_from(audited).expect("two reads were audited");
    assert_eq!(
        decisions(&found),
        decisions(&missing),
        "case \"a found and a missing document\": expected the same decisions to be recorded"
    );
    for (case, records) in [("found", &found), ("missing", &missing)] {
        assert_eq!(
            decisions(records),
            admitted_and_permitted(),
            "case {case:?}: expected both decisions of the read to be recorded"
        );
        for record in records.iter() {
            let members: Vec<&String> = record
                .as_object()
                .map(|members| members.keys().collect())
                .unwrap_or_default();
            assert!(
                members.iter().all(|member| !member.contains("status")),
                "case {case:?}: expected no member of the record to name a response status, \
                 got {members:?}"
            );
            assert_eq!(
                record["audit_request_effective_user"],
                json!(master_user_arn()),
                "case {case:?}: expected the record to name the caller, got {record:?}"
            );
        }
    }
    // The members of the two reads' records are the same, which is what makes them
    // indistinguishable: a reader cannot tell the answer from the record.
    for (left, right) in found.iter().zip(missing.iter()) {
        let names = |record: &Value| -> Vec<String> {
            record
                .as_object()
                .map(|members| members.keys().cloned().collect())
                .unwrap_or_default()
        };
        assert_eq!(
            names(left),
            names(right),
            "case \"a found and a missing document\": expected the same members, got {left:?} \
             and {right:?}"
        );
    }

    // The TRANSPORT record names the privilege the action asked for and the index it
    // resolved to, which is what a reader filters a document read by.
    let transport = found
        .iter()
        .find(|record| record["audit_request_layer"] == json!("TRANSPORT"))
        .expect("the transport decision was recorded");
    assert_eq!(
        (
            transport["audit_request_privilege"].clone(),
            transport["audit_trace_indices"].clone(),
        ),
        (json!("indices:data/read/get"), json!([INDEX])),
        "case \"the transport record of a document read\": got {transport:?}"
    );
    // The REST record names the request itself, and both name the cluster that decided.
    let rest_record = found
        .iter()
        .find(|record| record["audit_request_layer"] == json!("REST"))
        .expect("the REST decision was recorded");
    assert_eq!(
        (
            rest_record["audit_rest_request_path"].clone(),
            rest_record["audit_rest_request_method"].clone(),
        ),
        (json!(format!("/{INDEX}/_doc/{DOCUMENT}")), json!("GET")),
        "case \"the REST record of a document read\": got {rest_record:?}"
    );
    for record in found.iter() {
        assert_eq!(
            record["audit_cluster_name"],
            json!(format!("{}:audit-records", ::engine::emulator_account_id())),
            "case \"the cluster a record came from\": got {record:?}"
        );
    }

    // A decision the cluster took AGAINST the caller is recorded under its own category: an
    // unmapped principal is refused, and the refusal is what the audit log states.
    given_session(&emulator);
    given_data_plane_policy(&emulator);
    let before = audit_records(&emulator).await.len();
    let refused = rest(
        &emulator,
        SESSION_KEY_ID,
        &host,
        "GET",
        &format!("/{INDEX}/_doc/{DOCUMENT}"),
        None,
    )
    .await;
    assert_eq!(
        refused.status, 403,
        "case \"an unmapped principal reading a document\": expected the cluster's refusal, \
         got {} with {:?}",
        refused.status, refused.body
    );
    let records = audit_records(&emulator).await;
    assert_eq!(
        decisions(&records[before..]),
        vec![
            ("AUTHENTICATED".to_string(), "REST".to_string()),
            ("MISSING_PRIVILEGES".to_string(), "TRANSPORT".to_string()),
        ],
        "case \"an unmapped principal reading a document\": expected the refusal to be \
         recorded, got {:?}",
        &records[before..]
    );

    // And a credential the cluster does not authenticate at all is recorded at the REST
    // layer, for the user it claimed to be.
    let before = audit_records(&emulator).await.len();
    let unauthenticated = basic_rest(
        &emulator,
        &host,
        Some(("nobody", "Not-a-password-1!")),
        "GET",
        &format!("/{INDEX}/_doc/{DOCUMENT}"),
        None,
    )
    .await;
    assert_eq!(
        unauthenticated.status, 401,
        "case \"a credential the cluster refuses\": expected 401, got {} with {:?}",
        unauthenticated.status, unauthenticated.body
    );
    let records = audit_records(&emulator).await;
    assert_eq!(
        decisions(&records[before..]),
        vec![("FAILED_LOGIN".to_string(), "REST".to_string())],
        "case \"a credential the cluster refuses\": expected the refused login to be \
         recorded, got {:?}",
        &records[before..]
    );
    assert_eq!(
        records[before..]
            .first()
            .map(|record| record["audit_request_effective_user"].clone()),
        Some(json!("nobody")),
        "case \"a credential the cluster refuses\": expected the record to name the user it \
         claimed to be, got {:?}",
        &records[before..]
    );
}
