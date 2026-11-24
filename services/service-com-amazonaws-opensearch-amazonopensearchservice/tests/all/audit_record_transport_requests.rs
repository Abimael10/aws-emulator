// SPDX-License-Identifier: CC0-1.0

//! What a TRANSPORT-layer audit record says the cluster's decision was ABOUT, read back out
//! of the CloudWatch Logs group the domain publishes `AUDIT_LOGS` to through that service's
//! own `FilterLogEvents` — so what is asserted is what a reader of the log group sees.
//!
//! The RULE the rows assert, one level above any single route:
//!
//! > A decision of the cluster's transport layer is about an ACTION, which the cluster
//! > carries in a request of its own, so the security plugin's record of that decision names
//! > the TYPE of that request beside the privilege the action asked for and the indexes it
//! > resolved to — and nothing of the request's OUTCOME. A reader of the group can therefore
//! > tell WHICH operation was decided on, which the privilege alone does not say (more than
//! > one route asks for one privilege), and cannot tell how the request was answered: two
//! > requests the cluster decided alike leave the same record whether each was served or
//! > refused.
//!
//! The rows span what that rule generalises over: WHICH operation the request drives (a
//! document read, a document write, a document delete, a search, a count, an index create, a
//! mapping read, the cluster's own health), whether the operation FOUND what it addressed, and
//! whether the cluster GRANTED the action or withheld it — the two categories of the layer.
//!
//! The harness is the control-plane and data-plane one the sibling suites use
//! ([`crate::domain_lifecycle`], [`crate::domain_rest_data_plane`]) and the record reader is
//! the log-publishing suite's ([`crate::log_publishing_delivery::events`]); this suite adds no
//! second copy of either. What the audit CONFIGURATION gates, and that a record carries no
//! response status at all, is
//! `tests/all/domain_rest_data_plane/security_audit_configuration.rs`'s; here the subject is
//! the member that names the decision's own subject.

use serde_json::{json, Value};

use crate::domain_lifecycle::{log_group_arn, Emulator, DEFAULT_ACCESS_KEY_ID};
use crate::domain_rest_data_plane::{
    delivery_session_arn, endpoint_of, error_type, given_data_plane_policy, given_session, rest,
    SESSION_KEY_ID,
};
use crate::log_publishing_delivery::events;

/// The log group the audited domain publishes `AUDIT_LOGS` to.
const AUDIT_GROUP: &str = "/aws/opensearch/domains/transport-audit";

/// The index every row reads and writes.
const INDEX: &str = "orders";

/// The document that index is seeded with.
const DOCUMENT: &str = "first";

/// An id the index does not hold, which a read addresses to show the record is the same.
const ABSENT: &str = "absent";

/// The resource the audit configuration is enabled through, under the prefix the plugin's API
/// carried in the Open Distro era — the spelling a client's own setup step uses.
const AUDIT_PATH: &str = "/_opendistro/_security/api/audit";

/// The master user of the audited domain: the principal this emulator's own long-term
/// credential speaks for, which is what every row below signs as.
fn master_user_arn() -> String {
    ::service_common::caller_principal::ambient_user_arn()
}

/// A domain with fine-grained access control switched on, publishing `AUDIT_LOGS` to a log
/// group CloudWatch Logs itself holds, with auditing enabled through the plugin's own API and
/// one index holding one document.
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
    let seeded = as_master(
        &emulator,
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
    // Auditing starts switched off, so the step a client takes once the domain names the
    // group is the plugin's own patch of its audit configuration: both layers on, no category
    // excluded.
    let enabled = as_master(
        &emulator,
        &host,
        "PATCH",
        AUDIT_PATH,
        Some(json!([
            { "op": "replace", "path": "/config/enabled", "value": true },
            { "op": "replace", "path": "/config/audit/enable_rest", "value": true },
            { "op": "replace", "path": "/config/audit/enable_transport", "value": true },
            { "op": "replace", "path": "/config/audit/disabled_rest_categories", "value": [] },
            {
                "op": "replace",
                "path": "/config/audit/disabled_transport_categories",
                "value": [],
            },
        ])),
    )
    .await;
    assert_eq!(
        enabled.status, 200,
        "case \"enabling auditing through the plugin's API\": expected 200, got {} with {:?}",
        enabled.status, enabled.body
    );
    (emulator, host)
}

/// One request to the domain as the MASTER user, which every row below is taken by unless it
/// says otherwise.
async fn as_master(
    emulator: &Emulator,
    host: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> crate::domain_lifecycle::Answer {
    rest(emulator, DEFAULT_ACCESS_KEY_ID, host, method, path, body).await
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

/// The messages `FilterLogEvents` reports for the group under the filter pattern `pattern` —
/// the way a reader of the group SELECTS the records it is waiting for.
async fn filtered(emulator: &Emulator, pattern: &str) -> Vec<String> {
    let answer = emulator
        .logs_call(
            "FilterLogEvents",
            json!({ "logGroupName": AUDIT_GROUP, "filterPattern": pattern }),
        )
        .await;
    assert_eq!(
        answer.status, 200,
        "case {pattern:?}: expected 200 from FilterLogEvents, got {} with {:?}",
        answer.status, answer.body
    );
    answer.body["events"]
        .as_array()
        .map(|events| {
            events
                .iter()
                .map(|event| event["message"].as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// The TRANSPORT record among `records`, which a decision of that layer leaves exactly one
/// of.
fn transport_record(records: &[Value], case: &str) -> Value {
    let transport: Vec<&Value> = records
        .iter()
        .filter(|record| record["audit_request_layer"] == json!("TRANSPORT"))
        .collect();
    assert_eq!(
        transport.len(),
        1,
        "case {case:?}: expected exactly one transport record, got {records:?}"
    );
    transport[0].clone()
}

/// One row: the request the domain is driven with, the status it must be answered, the
/// privilege its action asks for and the type of the request the cluster carries that action
/// in.
struct Row {
    case: &'static str,
    method: &'static str,
    path: String,
    body: Option<Value>,
    status: u16,
    privilege: &'static str,
    request_type: &'static str,
    /// The index the action resolved to, where it was about one.
    index: Option<&'static str>,
}

/// The rows: one per operation of the data plane the cluster takes a transport decision for,
/// spanning the reads, the writes, the index administration and the cluster's own.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a document the index holds",
            method: "GET",
            path: format!("/{INDEX}/_doc/{DOCUMENT}"),
            body: None,
            status: 200,
            privilege: "indices:data/read/get",
            request_type: "GetRequest",
            index: Some(INDEX),
        },
        Row {
            // The same operation addressing an id the index does not hold: the cluster took
            // the same decision, so it names the same request.
            case: "an id the index does not hold",
            method: "GET",
            path: format!("/{INDEX}/_doc/{ABSENT}"),
            body: None,
            status: 404,
            privilege: "indices:data/read/get",
            request_type: "GetRequest",
            index: Some(INDEX),
        },
        Row {
            case: "a document written",
            method: "PUT",
            path: format!("/{INDEX}/_doc/second"),
            body: Some(json!({ "title": "another audited document" })),
            status: 201,
            privilege: "indices:data/write/index",
            request_type: "IndexRequest",
            index: Some(INDEX),
        },
        Row {
            case: "a document deleted",
            method: "DELETE",
            path: format!("/{INDEX}/_doc/second"),
            body: None,
            status: 200,
            privilege: "indices:data/write/delete",
            request_type: "DeleteRequest",
            index: Some(INDEX),
        },
        Row {
            case: "a search over the index",
            method: "POST",
            path: format!("/{INDEX}/_search"),
            body: Some(json!({})),
            status: 200,
            privilege: "indices:data/read/search",
            request_type: "SearchRequest",
            index: Some(INDEX),
        },
        Row {
            // A count runs the same request a search runs, so it names that request too.
            case: "a count over the index",
            method: "GET",
            path: format!("/{INDEX}/_count"),
            body: None,
            status: 200,
            privilege: "indices:data/read/count",
            request_type: "SearchRequest",
            index: Some(INDEX),
        },
        Row {
            case: "an index created",
            method: "PUT",
            path: "/invoices".to_string(),
            body: None,
            status: 200,
            privilege: "indices:admin/create",
            request_type: "CreateIndexRequest",
            index: Some("invoices"),
        },
        Row {
            case: "an index's mappings read",
            method: "GET",
            path: format!("/{INDEX}/_mapping"),
            body: None,
            status: 200,
            privilege: "indices:admin/mappings/get",
            request_type: "GetMappingsRequest",
            index: Some(INDEX),
        },
        Row {
            // A decision about the CLUSTER rather than an index: the record names the
            // cluster's own request and resolves no index.
            case: "the cluster's health",
            method: "GET",
            path: "/_cluster/health".to_string(),
            body: None,
            status: 200,
            privilege: "cluster:monitor/health",
            request_type: "ClusterHealthRequest",
            index: None,
        },
    ]
}

/// RULE: a transport-layer audit record names the type of the request the cluster carries the
/// decided action in, beside the privilege that action asked for and the indexes it resolved
/// to.
#[tokio::test]
async fn a_transport_audit_record_names_the_request_the_action_was_carried_in() {
    let (emulator, host) = given_audited_domain("audit-transport").await;

    for row in rows() {
        let before = audit_records(&emulator).await.len();
        let answered = as_master(&emulator, &host, row.method, &row.path, row.body.clone()).await;
        assert_eq!(
            answered.status, row.status,
            "case {:?}: expected HTTP {}, got {} with {:?}",
            row.case, row.status, answered.status, answered.body
        );
        let records = audit_records(&emulator).await;
        let produced = &records[before..];
        let record = transport_record(produced, row.case);
        assert_eq!(
            (
                record["audit_category"].clone(),
                record["audit_request_privilege"].clone(),
                record["audit_transport_request_type"].clone(),
            ),
            (
                json!("GRANTED_PRIVILEGES"),
                json!(row.privilege),
                json!(row.request_type),
            ),
            "case {:?}: expected the granted decision to name {} carried in {}, got {record:?}",
            row.case,
            row.privilege,
            row.request_type
        );
        assert_eq!(
            record["audit_trace_resolved_indices"].clone(),
            row.index.map_or(Value::Null, |index| json!([index])),
            "case {:?}: expected the record to resolve {:?}, got {record:?}",
            row.case,
            row.index
        );
        // The record is of the DECISION, so no member of it names what the request was
        // answered.
        let members: Vec<String> = record
            .as_object()
            .map(|members| members.keys().cloned().collect())
            .unwrap_or_default();
        assert!(
            members.iter().all(|member| {
                !member.contains("status") && !member.contains("response") && member != "code"
            }),
            "case {:?}: expected no member of the record to name the answer, got {members:?}",
            row.case
        );
    }
}

/// RULE, read through the log group's own selection: a delivered audit record is one the
/// group's reader can SELECT, so a filter pattern naming the member a record carries reports
/// that record and one naming a member it does not carry reports nothing — which is how a
/// client waiting for the record of one request finds it among everything else the group
/// holds.
#[tokio::test]
async fn a_delivered_audit_record_is_selectable_by_the_groups_own_filter_patterns() {
    let (emulator, host) = given_audited_domain("audit-transport-filtered").await;

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
        "case \"the audited read\": expected 200, got {} with {:?}",
        read.status, read.body
    );

    // (the pattern, whether the group holds a record it selects)
    let patterns: [(&str, bool); 6] = [
        ("{ $.audit_category = \"AUTHENTICATED\" }", true),
        ("{ $.audit_category = \"GRANTED_PRIVILEGES\" }", true),
        ("{ $.audit_transport_request_type = \"GetRequest\" }", true),
        ("{ $.audit_request_layer = \"TRANSPORT\" }", true),
        // A record of a decision no request of this domain took is selected by nothing.
        ("{ $.audit_category = \"FAILED_LOGIN\" }", false),
        ("{ $.audit_transport_request_type = \"BulkRequest\" }", false),
    ];
    for (pattern, selects) in patterns {
        let selected = filtered(&emulator, pattern).await;
        assert_eq!(
            !selected.is_empty(),
            selects,
            "case {pattern:?}: expected the pattern to select {}, got {selected:?}",
            if selects { "a record" } else { "nothing" }
        );
    }
}

/// RULE, the same record for a decision the cluster took AGAINST the caller: a withheld action
/// is recorded under its own category and names the very same request the granted one does, so
/// what the record says the decision was about does not depend on how it went.
#[tokio::test]
async fn a_withheld_action_names_the_same_request_a_granted_one_does() {
    let (emulator, host) = given_audited_domain("audit-transport-refused").await;
    given_session(&emulator);
    given_data_plane_policy(&emulator);

    // The master's own read of the document, and the same read by a principal no role mapping
    // binds: one granted, one withheld, both of the same action.
    let before = audit_records(&emulator).await.len();
    let granted = as_master(
        &emulator,
        &host,
        "GET",
        &format!("/{INDEX}/_doc/{DOCUMENT}"),
        None,
    )
    .await;
    assert_eq!(
        granted.status, 200,
        "case \"the master reading the document\": expected 200, got {} with {:?}",
        granted.status, granted.body
    );
    let records = audit_records(&emulator).await;
    let granted_record = transport_record(&records[before..], "the master reading the document");

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
        (refused.status, error_type(&refused)),
        (403, "security_exception".to_string()),
        "case \"an unmapped principal reading the document\": got {} with {:?}",
        refused.status,
        refused.body
    );
    let records = audit_records(&emulator).await;
    let withheld_record =
        transport_record(&records[before..], "an unmapped principal reading the document");

    assert_eq!(
        (
            withheld_record["audit_category"].clone(),
            withheld_record["audit_transport_request_type"].clone(),
            withheld_record["audit_request_privilege"].clone(),
            withheld_record["audit_request_effective_user"].clone(),
        ),
        (
            json!("MISSING_PRIVILEGES"),
            json!("GetRequest"),
            json!("indices:data/read/get"),
            json!(delivery_session_arn()),
        ),
        "case \"an unmapped principal reading the document\": expected the withheld decision \
         to name the same request for the principal it was taken for, got {withheld_record:?}"
    );
    // The two records differ in the DECISION and the principal it was taken for, never in
    // which request the cluster decided on.
    assert_eq!(
        granted_record["audit_transport_request_type"].clone(),
        withheld_record["audit_transport_request_type"].clone(),
        "case \"a granted and a withheld decision\": expected one request type, got \
         {granted_record:?} and {withheld_record:?}"
    );
}
