// SPDX-License-Identifier: CC0-1.0

//! What a SEARCH BODY the engine cannot read is refused with, driven over the real wire path:
//! a signed request to the endpoint a domain publishes, through the multi-service router, into
//! the mounted service.
//!
//! The rule, over the set of bodies one search may send:
//!
//! > a search body the engine's reader cannot read is refused HTTP 400 with the type and the
//! > reason that reader states — a clause name nothing is registered for as the parse failure
//! > of the query, caused by the named-object lookup that failed; a member a reader has no
//! > place for as a parse failure naming that member; a page count below zero as the illegal
//! > argument it is — and where the reader reports a POSITION it is the line and column of the
//! > body AS SENT, so the same query in a differently laid-out body names a different place.
//!
//! The message is the contract: clients log it, match on it and show it to people, so each row
//! asserts the wording and, where the reader names one, the position and the failure under it.
//!
//! Every body below is sent as the BYTES the row states ([`sent`]) rather than as a value this
//! suite re-serialises, because the position a refusal names is a property of those bytes.
//!
//! The harness is [`super::domain_lifecycle`]'s and the data-plane request helper is
//! [`super::domain_rest_data_plane`]'s; this suite adds no second copy of either.

use serde_json::{json, Value};

use super::domain_lifecycle::{Answer, Emulator, DEFAULT_ACCESS_KEY_ID};
use super::domain_rest_data_plane::{error_type, rest_payload, rest_prefixed};

/// The domain whose published endpoint every request below is addressed to.
const DOMAIN: &str = "parse-domain";

/// The index the bodies are sent to.
const INDEX: &str = "ledger";

/// The host the prefixed addressing reaches this emulator's own endpoint under, which is the
/// addressing [`super::domain_rest_data_plane::rest_prefixed`] uses.
const HOST: &str = "es.us-east-1.amazonaws.com";

/// `CreateDomain`, then the index and the three documents a served search answers from.
async fn given_indexed(emulator: &Emulator) {
    let created = emulator
        .create(json!({
            "DomainName": DOMAIN,
            "ClusterConfig": { "InstanceType": "t3.small.search", "InstanceCount": 1 },
            "EBSOptions": { "EBSEnabled": true, "VolumeType": "gp3", "VolumeSize": 10 },
        }))
        .await;
    assert_eq!(
        created.status, 200,
        "case CreateDomain({DOMAIN:?}): expected 200, got {} with {:?}",
        created.status, created.body
    );
    let index = rest_prefixed(
        emulator,
        DOMAIN,
        "PUT",
        &format!("/{INDEX}"),
        Some(json!({
            "settings": { "number_of_shards": 1, "number_of_replicas": 0 },
            "mappings": { "properties": {
                "name": { "type": "keyword" },
                "rank": { "type": "integer" },
            } },
        })),
    )
    .await;
    assert_eq!(
        index.status, 200,
        "case \"PUT /{INDEX}\": expected 200, got {} with {:?}",
        index.status, index.body
    );
    for (id, name, rank) in [("d1", "alpha", 1), ("d2", "beta", 2), ("d3", "gamma", 3)] {
        let written = rest_prefixed(
            emulator,
            DOMAIN,
            "PUT",
            &format!("/{INDEX}/_doc/{id}"),
            Some(json!({ "name": name, "rank": rank })),
        )
        .await;
        assert!(
            written.status == 200 || written.status == 201,
            "case \"write {INDEX}/{id}\": expected 200 or 201, got {} with {:?}",
            written.status,
            written.body
        );
    }
}

/// One `POST /{INDEX}/_search` carrying `payload` as the bytes the client wrote.
async fn sent(emulator: &Emulator, payload: &str) -> Answer {
    rest_payload(
        emulator,
        DEFAULT_ACCESS_KEY_ID,
        HOST,
        "POST",
        &format!("/_aws/es/{DOMAIN}/{INDEX}/_search"),
        payload.to_string(),
    )
    .await
}

/// One row of the refusal table: the body as sent, and what the engine answers it with.
struct RefusalRow {
    case: &'static str,
    /// The request body, as the bytes the client wrote.
    payload: &'static str,
    /// The error type the refusal carries.
    error_type: &'static str,
    /// The reason it states, in full.
    reason: String,
    /// The failure UNDER it, where the refusal carries one.
    caused_by: Option<Value>,
}

/// The refusal table: one row per way a search body can be unreadable, and \u2014 for the two
/// readers that report a position \u2014 the same body laid out differently, so what the position
/// follows is the text and not the query.
fn refusal_rows() -> Vec<RefusalRow> {
    vec![
        RefusalRow {
            case: "a clause name nothing is registered for",
            payload: r#"{"query": {"no_such_query": {"name": "alpha"}}}"#,
            error_type: "parsing_exception",
            reason: "unknown query [no_such_query]".to_string(),
            caused_by: Some(json!({
                "type": "named_object_not_found_exception",
                "reason": "[1:29] unknown field [no_such_query]",
            })),
        },
        RefusalRow {
            case: "that same clause in a body written without spacing",
            payload: r#"{"query":{"no_such_query":{"name":"alpha"}}}"#,
            error_type: "parsing_exception",
            reason: "unknown query [no_such_query]".to_string(),
            caused_by: Some(json!({
                "type": "named_object_not_found_exception",
                "reason": "[1:27] unknown field [no_such_query]",
            })),
        },
        RefusalRow {
            case: "that same clause in a body broken over lines",
            payload: "{\n  \"query\": {\n    \"no_such_query\": {}\n  }\n}",
            error_type: "parsing_exception",
            reason: "unknown query [no_such_query]".to_string(),
            caused_by: Some(json!({
                "type": "named_object_not_found_exception",
                "reason": "[3:22] unknown field [no_such_query]",
            })),
        },
        RefusalRow {
            case: "a bool carrying a member its reader has no place for",
            payload: r#"{"query": {"bool": {"must_sometimes": [{"match_all": {}}]}}}"#,
            error_type: "x_content_parse_exception",
            reason: "[1:21] [bool] unknown field [must_sometimes]".to_string(),
            caused_by: None,
        },
        RefusalRow {
            case: "a term clause naming two fields",
            payload: r#"{"query": {"term": {"name": "alpha", "rank": 1}}}"#,
            error_type: "parsing_exception",
            reason: "[term] query doesn't support multiple fields, found [name] and [rank]"
                .to_string(),
            caused_by: None,
        },
        RefusalRow {
            case: "a match clause naming two fields",
            payload: r#"{"query": {"match": {"name": "alpha", "rank": 1}}}"#,
            error_type: "parsing_exception",
            reason: "[match] query doesn't support multiple fields, found [name] and [rank]"
                .to_string(),
            caused_by: None,
        },
        RefusalRow {
            case: "a range clause naming two fields",
            payload: r#"{"query": {"range": {"name": {"gte": "a"}, "rank": {"gte": 1}}}}"#,
            error_type: "parsing_exception",
            reason: "[range] query doesn't support multiple fields, found [name] and [rank]"
                .to_string(),
            caused_by: None,
        },
        RefusalRow {
            case: "an exists clause naming no field",
            payload: r#"{"query": {"exists": {}}}"#,
            error_type: "parsing_exception",
            reason: "[exists] must be provided with a [field]".to_string(),
            caused_by: None,
        },
        RefusalRow {
            case: "a misspelled top-level member whose value is an object",
            payload: r#"{"querry": {"match_all": {}}}"#,
            error_type: "parsing_exception",
            reason: "Unknown key for a START_OBJECT in [querry].".to_string(),
            caused_by: None,
        },
        RefusalRow {
            case: "a misspelled top-level member whose value is a number",
            payload: r#"{"sizee": 5}"#,
            error_type: "parsing_exception",
            reason: "Unknown key for a VALUE_NUMBER in [sizee].".to_string(),
            caused_by: None,
        },
        RefusalRow {
            case: "a misspelled top-level member whose value is an array",
            payload: r#"{"sorts": [{"rank": "asc"}]}"#,
            error_type: "parsing_exception",
            reason: "Unknown key for a START_ARRAY in [sorts].".to_string(),
            caused_by: None,
        },
        RefusalRow {
            case: "a size below zero",
            payload: r#"{"size": -5, "query": {"match_all": {}}}"#,
            error_type: "illegal_argument_exception",
            reason: "[size] parameter cannot be negative, found [-5]".to_string(),
            caused_by: None,
        },
        RefusalRow {
            case: "a from below zero",
            payload: r#"{"from": -1, "query": {"match_all": {}}}"#,
            error_type: "illegal_argument_exception",
            reason: "[from] parameter cannot be negative, found [-1]".to_string(),
            caused_by: None,
        },
        RefusalRow {
            case: "a sort direction outside the engine's own order",
            payload: r#"{"sort": [{"rank": {"order": "sideways"}}]}"#,
            error_type: "x_content_parse_exception",
            reason: "[1:30] [field_sort] failed to parse field [order]".to_string(),
            caused_by: Some(json!({
                "type": "illegal_argument_exception",
                "reason": "No enum constant org.opensearch.search.sort.SortOrder.SIDEWAYS",
            })),
        },
        RefusalRow {
            case: "a sort mode outside the ones the engine picks with",
            payload: r#"{"sort": [{"rank": {"order": "asc", "mode": "mediann"}}]}"#,
            error_type: "x_content_parse_exception",
            reason: "[1:45] [field_sort] failed to parse field [mode]".to_string(),
            caused_by: None,
        },
    ]
}

/// RULE: each unreadable body is refused with the type, the reason and — where the reader
/// reports one — the position and the cause that reader states.
#[tokio::test]
async fn an_unreadable_search_body_is_refused_with_the_readers_own_wording() {
    let emulator = Emulator::new();
    given_indexed(&emulator).await;

    for row in refusal_rows() {
        let refused = sent(&emulator, row.payload).await;
        assert_eq!(
            (refused.status, error_type(&refused)),
            (400, row.error_type.to_string()),
            "case {:?}: expected 400 {}, got {} with {:?}",
            row.case,
            row.error_type,
            refused.status,
            refused.body
        );
        let reason = refused.body["error"]["reason"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert_eq!(
            reason, row.reason,
            "case {:?}: expected the reason {:?}, got {reason:?}",
            row.case, row.reason
        );
        // The root cause repeats the failure the refusal states, which is what a client that
        // walks `root_cause` reads.
        assert_eq!(
            refused.body["error"]["root_cause"][0]["reason"].as_str(),
            Some(row.reason.as_str()),
            "case {:?}: expected the root cause to repeat the reason, got {:?}",
            row.case,
            refused.body["error"]
        );
        match &row.caused_by {
            Some(expected) => assert_eq!(
                &refused.body["error"]["caused_by"], expected,
                "case {:?}: expected the failure under it {expected:?}, got {:?}",
                row.case, refused.body["error"]["caused_by"]
            ),
            None => assert!(
                refused.body["error"].get("caused_by").is_none(),
                "case {:?}: expected no failure under it, got {:?}",
                row.case,
                refused.body["error"]
            ),
        }
        // A refused search answers no hits at all.
        assert!(
            refused.body.get("hits").is_none(),
            "case {:?}: expected no hits section on a refusal, got {:?}",
            row.case,
            refused.body
        );
    }
}

/// RULE (the other side of the same rule): a body the reader CAN read is served, so the
/// refusals above are about what the engine cannot read and not about what it declines.
#[tokio::test]
async fn a_readable_search_body_is_served() {
    let emulator = Emulator::new();
    given_indexed(&emulator).await;

    // (case, the body as sent, the ids it must answer with) — the members and spellings the
    // rows above misspell, written as the reader reads them.
    let rows: [(&str, &str, Vec<&str>); 5] = [
        (
            "the clause the first refusal misspells",
            r#"{"query": {"match_all": {}}, "sort": [{"rank": "asc"}]}"#,
            vec!["d1", "d2", "d3"],
        ),
        (
            "a bool naming the members its reader does have a place for",
            r#"{"query": {"bool": {"must": [{"term": {"name": "beta"}}], "boost": 1.0}}}"#,
            vec!["d2"],
        ),
        (
            "a term clause naming the one field it asks about",
            r#"{"query": {"term": {"name": "gamma"}}}"#,
            vec!["d3"],
        ),
        (
            "an exists clause naming its field",
            r#"{"query": {"exists": {"field": "rank"}}, "sort": [{"rank": "desc"}]}"#,
            vec!["d3", "d2", "d1"],
        ),
        (
            "a direction written in upper case, which names the same order",
            r#"{"sort": [{"rank": {"order": "DESC"}}]}"#,
            vec!["d3", "d2", "d1"],
        ),
    ];
    for (case, payload, expected) in rows {
        let answered = sent(&emulator, payload).await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: expected 200, got {} with {:?}",
            answered.status, answered.body
        );
        let found: Vec<String> = answered.body["hits"]["hits"]
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .map(|hit| hit["_id"].as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(
            found, expected,
            "case {case:?}: expected the hits {expected:?}, got {found:?}"
        );
    }
}
