// SPDX-License-Identifier: CC0-1.0

//! The domain REST data plane's PARTIAL-DOCUMENT UPDATE, driven over the real wire path: a
//! signed request to the endpoint `DescribeDomain` published, through the multi-service
//! router, into the mounted service. What is asserted is what a client sees.
//!
//! The rule, over a SET of inputs rather than one recorded call: a partial-document update
//! writes the members its `doc` object names over the stored document, leaves every member it
//! does not name standing, stores the result as the document's next revision and answers the
//! revision it landed at; a merge that changes nothing writes nothing; and an update of an id
//! the index does not hold is refused — as a missing document where the index exists, and as
//! a missing index where it does not — unless the request asks for the partial, or the
//! `upsert` document beside it, to be indexed instead.
//!
//! The rows span what the rule generalises over: which member is named (one already held,
//! one the document does not carry, a member nested inside an object, an array, a value of
//! another JSON type), how many are named (none, one, several), and whether the merge
//! changes anything at all. The last test drives the one rule through BOTH requests that ask
//! for it — the update API and the `update` action of a bulk stream — so neither can merge,
//! upsert or refuse differently.
//!
//! The control-plane harness comes from [`super::domain_lifecycle`] and the REST harness
//! from [`super::domain_rest_data_plane`]; this suite adds no second copy of either.

use serde_json::{json, Map, Value};

use super::data_source_registry::created_domain;
use super::domain_lifecycle::{Answer, Emulator, DEFAULT_ACCESS_KEY_ID};
use super::domain_rest_bulk_and_index_apis::{bulk, created_index};
use super::domain_rest_data_plane::{endpoint_of, error_type, rest};

/// The document id every case below addresses: one document per index, so no row can be
/// read as another's.
const ID: &str = "1";

/// The `_primary_term` every document of this cluster reports: one run of a cluster takes
/// every write in one term.
const PRIMARY_TERM: i64 = 1;

/// Store `source` as the document [`ID`] of `index`, which auto-creates the index.
async fn stored(emulator: &Emulator, host: &str, index: &str, source: Value) {
    let answer = rest(
        emulator,
        DEFAULT_ACCESS_KEY_ID,
        host,
        "PUT",
        &format!("/{index}/_doc/{ID}?refresh=true"),
        Some(source),
    )
    .await;
    assert_eq!(
        (answer.status, answer.body["result"].as_str()),
        (201, Some("created")),
        "case \"PUT /{index}/_doc/{ID}\": expected the document to be created, got {} with \
         {:?}",
        answer.status,
        answer.body
    );
}

/// One partial update of that document, as the API takes it.
async fn update(emulator: &Emulator, host: &str, index: &str, body: Value) -> Answer {
    rest(
        emulator,
        DEFAULT_ACCESS_KEY_ID,
        host,
        "POST",
        &format!("/{index}/_update/{ID}?refresh=true"),
        Some(body),
    )
    .await
}

/// The document the index holds, as its own read API reports it, or `None` where it holds
/// none of that id.
async fn read(emulator: &Emulator, host: &str, index: &str) -> Option<Value> {
    let answer = rest(
        emulator,
        DEFAULT_ACCESS_KEY_ID,
        host,
        "GET",
        &format!("/{index}/_doc/{ID}"),
        None,
    )
    .await;
    (answer.status == 200).then(|| answer.body.clone())
}

/// One row of the merge table.
struct Merge {
    case: &'static str,
    /// The document the index holds before the update.
    stored: Value,
    /// The update request's own body.
    body: Value,
    /// The `result` the update answers with.
    result: &'static str,
    /// The `_version` the document is at afterwards.
    version: i64,
    /// The document the index then holds.
    merged: Value,
}

/// RULE: a partial-document update writes the members its `doc` names over the stored
/// document, leaves every member it does not name standing, and stores the result as the
/// document's next revision — and a merge that changes nothing writes nothing at all.
#[tokio::test]
async fn a_partial_update_merges_the_members_its_doc_names_and_leaves_the_rest_standing() {
    let emulator = Emulator::new();
    created_domain(&emulator, "update-target").await;
    let host = endpoint_of(&emulator, "update-target");

    let rows: &[Merge] = &[
        Merge {
            case: "members the document already carries, beside one it keeps",
            stored: json!({ "status": "pending", "attempts": 0, "owner": "x" }),
            body: json!({ "doc": { "status": "done", "attempts": 1 } }),
            result: "updated",
            version: 2,
            merged: json!({ "status": "done", "attempts": 1, "owner": "x" }),
        },
        Merge {
            case: "a member the document does not carry at all",
            stored: json!({ "status": "pending" }),
            body: json!({ "doc": { "retries": 3 } }),
            result: "updated",
            version: 2,
            merged: json!({ "status": "pending", "retries": 3 }),
        },
        Merge {
            case: "a member nested inside an object, whose sibling stays",
            stored: json!({ "meta": { "region": "eu", "tier": "gold" }, "n": 1 }),
            body: json!({ "doc": { "meta": { "tier": "silver" } } }),
            result: "updated",
            version: 2,
            merged: json!({ "meta": { "region": "eu", "tier": "silver" }, "n": 1 }),
        },
        Merge {
            case: "a nested object the document does not carry",
            stored: json!({ "n": 1 }),
            body: json!({ "doc": { "meta": { "region": "eu" } } }),
            result: "updated",
            version: 2,
            merged: json!({ "n": 1, "meta": { "region": "eu" } }),
        },
        Merge {
            case: "an array, which is replaced and not appended to",
            stored: json!({ "tags": ["a", "b"], "n": 1 }),
            body: json!({ "doc": { "tags": ["c"] } }),
            result: "updated",
            version: 2,
            merged: json!({ "tags": ["c"], "n": 1 }),
        },
        Merge {
            case: "a value of another JSON type",
            stored: json!({ "count": "1", "n": 1 }),
            body: json!({ "doc": { "count": 2 } }),
            result: "updated",
            version: 2,
            merged: json!({ "count": 2, "n": 1 }),
        },
        Merge {
            case: "a member set to null, which stands as the value null",
            stored: json!({ "owner": "x", "n": 1 }),
            body: json!({ "doc": { "owner": null } }),
            result: "updated",
            version: 2,
            merged: json!({ "owner": null, "n": 1 }),
        },
        Merge {
            case: "the values the document already holds, which changes nothing",
            stored: json!({ "status": "done", "attempts": 1 }),
            body: json!({ "doc": { "status": "done" } }),
            result: "noop",
            version: 1,
            merged: json!({ "status": "done", "attempts": 1 }),
        },
        Merge {
            case: "no member at all, which changes nothing either",
            stored: json!({ "status": "done" }),
            body: json!({ "doc": {} }),
            result: "noop",
            version: 1,
            merged: json!({ "status": "done" }),
        },
        Merge {
            case: "an object naming no member, at a member the document does not carry",
            stored: json!({ "n": 1 }),
            body: json!({ "doc": { "meta": {} } }),
            result: "updated",
            version: 2,
            merged: json!({ "n": 1, "meta": {} }),
        },
        Merge {
            case: "an object naming no member, at one the document does carry",
            stored: json!({ "meta": { "region": "eu" }, "n": 1 }),
            body: json!({ "doc": { "meta": {} } }),
            result: "noop",
            version: 1,
            merged: json!({ "meta": { "region": "eu" }, "n": 1 }),
        },
        Merge {
            case: "the same values with the no-op comparison switched off",
            stored: json!({ "status": "done" }),
            body: json!({ "doc": { "status": "done" }, "detect_noop": false }),
            result: "updated",
            version: 2,
            merged: json!({ "status": "done" }),
        },
    ];

    for (at, row) in rows.iter().enumerate() {
        let index = format!("merged-{at}");
        stored(&emulator, &host, &index, row.stored.clone()).await;
        let answered = update(&emulator, &host, &index, row.body.clone()).await;
        assert_eq!(
            (answered.status, answered.body["result"].as_str()),
            (200, Some(row.result)),
            "case {:?}: expected 200 with result {:?}, got {} with {:?}",
            row.case,
            row.result,
            answered.status,
            answered.body
        );
        assert_eq!(
            (
                answered.body["_index"].as_str(),
                answered.body["_id"].as_str(),
                answered.body["_version"].as_i64(),
                answered.body["_primary_term"].as_i64(),
            ),
            (
                Some(index.as_str()),
                Some(ID),
                Some(row.version),
                Some(PRIMARY_TERM)
            ),
            "case {:?}: expected the answer to name the document at its revision, got {:?}",
            row.case,
            answered.body
        );
        assert!(
            answered.body["_seq_no"].as_i64().is_some(),
            "case {:?}: expected the sequence number the document is at, got {:?}",
            row.case,
            answered.body
        );
        // A merge that wrote nothing counts no shard copy as having taken a write; one
        // that wrote reports the copies the cluster has.
        let copies = answered.body["_shards"]["total"].as_i64().unwrap_or_default();
        assert_eq!(
            copies >= 1,
            row.result != "noop",
            "case {:?}: expected a merge that stored nothing to count no shard copy, got \
             {:?}",
            row.case,
            answered.body
        );
        let held = read(&emulator, &host, &index)
            .await
            .unwrap_or_else(|| panic!("case {:?}: expected the index to hold the document", row.case));
        assert_eq!(
            (held["_source"].clone(), held["_version"].as_i64()),
            (row.merged.clone(), Some(row.version)),
            "case {:?}: expected the merged document at version {}, got {held:?}",
            row.case,
            row.version
        );
    }
}

/// RULE: an update of an id the index does not hold is the engine's missing-document
/// refusal — its missing-index one where the cluster holds no such index to read the
/// document from — unless the request asks for the partial, or the `upsert` document beside
/// it, to be indexed instead, which creates the document.
#[tokio::test]
async fn an_update_of_a_document_the_index_does_not_hold_is_refused_unless_it_upserts() {
    let emulator = Emulator::new();
    created_domain(&emulator, "upsert-target").await;
    let host = endpoint_of(&emulator, "upsert-target");

    /// One row of the upsert table.
    struct Upsert {
        case: &'static str,
        /// Whether the cluster holds the index before the update — seeded by writing a
        /// document of ANOTHER id, so the index exists and holds none of the id addressed.
        holds_index: bool,
        /// The update request's own body.
        body: Value,
        status: u16,
        /// The `result` the answer carries, absent where it is a refusal.
        result: Option<&'static str>,
        /// The engine's error type, empty where the request is served.
        error: &'static str,
        /// The document the index holds afterwards, `None` where it holds none.
        stored: Option<Value>,
    }
    let rows: &[Upsert] = &[
        Upsert {
            case: "a partial alone, in an index holding no document of that id",
            holds_index: true,
            body: json!({ "doc": { "status": "done" } }),
            status: 404,
            result: None,
            error: "document_missing_exception",
            stored: None,
        },
        Upsert {
            case: "a partial alone, naming an index the cluster does not hold",
            holds_index: false,
            body: json!({ "doc": { "status": "done" } }),
            status: 404,
            result: None,
            error: "index_not_found_exception",
            stored: None,
        },
        Upsert {
            case: "the same partial asked to be indexed, in an index that exists",
            holds_index: true,
            body: json!({ "doc": { "status": "done", "attempts": 1 }, "doc_as_upsert": true }),
            status: 201,
            result: Some("created"),
            error: "",
            stored: Some(json!({ "status": "done", "attempts": 1 })),
        },
        Upsert {
            case: "the same partial asked to be indexed, which creates the index too",
            holds_index: false,
            body: json!({ "doc": { "status": "done", "attempts": 1 }, "doc_as_upsert": true }),
            status: 201,
            result: Some("created"),
            error: "",
            stored: Some(json!({ "status": "done", "attempts": 1 })),
        },
        Upsert {
            case: "a partial with an upsert document beside it",
            holds_index: false,
            body: json!({
                "doc": { "status": "done" },
                "upsert": { "status": "new", "attempts": 0 },
            }),
            status: 201,
            result: Some("created"),
            error: "",
            stored: Some(json!({ "status": "new", "attempts": 0 })),
        },
    ];

    for (at, row) in rows.iter().enumerate() {
        let index = format!("upserted-{at}");
        if row.holds_index {
            // A document of another id, so the index exists and holds none of the id the
            // update addresses.
            let seeded = rest(
                &emulator,
                DEFAULT_ACCESS_KEY_ID,
                &host,
                "PUT",
                &format!("/{index}/_doc/other?refresh=true"),
                Some(json!({ "status": "pending" })),
            )
            .await;
            assert_eq!(
                seeded.status, 201,
                "case {:?}: expected the index to be seeded, got {} with {:?}",
                row.case, seeded.status, seeded.body
            );
        }
        let answered = update(&emulator, &host, &index, row.body.clone()).await;
        assert_eq!(
            (answered.status, answered.body["result"].as_str()),
            (row.status, row.result),
            "case {:?}: expected {} with result {:?}, got {} with {:?}",
            row.case,
            row.status,
            row.result,
            answered.status,
            answered.body
        );
        if !row.error.is_empty() {
            assert_eq!(
                error_type(&answered),
                row.error,
                "case {:?}: expected the engine's error type, got {:?}",
                row.case,
                answered.body
            );
        }
        let held = read(&emulator, &host, &index).await;
        assert_eq!(
            held.as_ref().map(|held| held["_source"].clone()),
            row.stored.clone(),
            "case {:?}: expected the index to hold {:?}, got {held:?}",
            row.case,
            row.stored
        );
    }
}

/// RULE: the update route is served for `POST` alone, takes only a request that names the
/// partial to merge, is held to the same compare-and-set a document write is, and stores
/// nothing the index's own mappings refuse.
#[tokio::test]
async fn the_update_route_refuses_what_the_engine_refuses_and_stores_nothing_for_it() {
    let emulator = Emulator::new();
    created_domain(&emulator, "update-refusals").await;
    let host = endpoint_of(&emulator, "update-refusals");
    created_index(
        &emulator,
        &host,
        "guarded",
        json!({
            "settings": { "number_of_shards": 1, "number_of_replicas": 0 },
            "mappings": { "dynamic": "strict", "properties": {
                "status": { "type": "keyword" },
                "attempts": { "type": "integer" },
            } },
        }),
    )
    .await;
    stored(
        &emulator,
        &host,
        "guarded",
        json!({ "status": "pending", "attempts": 0 }),
    )
    .await;
    let unchanged = json!({ "status": "pending", "attempts": 0 });

    // A method the route does not take is the engine's wrong-method refusal naming THIS
    // route's own list, not an index route's.
    for method in ["GET", "PUT", "DELETE", "HEAD"] {
        let answered = rest(
            &emulator,
            DEFAULT_ACCESS_KEY_ID,
            &host,
            method,
            &format!("/guarded/_update/{ID}"),
            None,
        )
        .await;
        assert_eq!(
            answered.status, 405,
            "case {method:?}: expected the wrong-method refusal, got {} with {:?}",
            answered.status, answered.body
        );
        if method != "HEAD" {
            assert_eq!(
                answered.body["error"].as_str(),
                Some(
                    format!(
                        "Incorrect HTTP method for uri [/guarded/_update/{ID}] and method \
                         [{method}], allowed: [POST]"
                    )
                    .as_str()
                ),
                "case {method:?}: expected this route's own method list, got {:?}",
                answered.body
            );
        }
    }

    // (the case, the body, the status, the error type)
    let refused: &[(&str, Value, u16, &str)] = &[
        (
            "a body naming neither a partial nor a script",
            json!({}),
            400,
            "illegal_argument_exception",
        ),
        (
            "a partial that is not an object",
            json!({ "doc": "done" }),
            400,
            "illegal_argument_exception",
        ),
        (
            "a scripted update, which this cluster runs no code for",
            json!({ "script": { "source": "ctx._source.attempts += 1" } }),
            400,
            "illegal_argument_exception",
        ),
        (
            "a merged document the index's own mappings refuse",
            json!({ "doc": { "unmapped": "x" } }),
            400,
            "strict_dynamic_mapping_exception",
        ),
    ];
    for (case, body, status, error) in refused {
        let answered = update(&emulator, &host, "guarded", body.clone()).await;
        assert_eq!(
            (answered.status, error_type(&answered)),
            (*status, (*error).to_string()),
            "case {case:?}: expected {status} {error}, got {} with {:?}",
            answered.status,
            answered.body
        );
        let held = read(&emulator, &host, "guarded")
            .await
            .unwrap_or_else(|| panic!("case {case:?}: expected the document to still be held"));
        assert_eq!(
            (held["_source"].clone(), held["_version"].as_i64()),
            (unchanged.clone(), Some(1)),
            "case {case:?}: expected a refused update to change nothing, got {held:?}"
        );
    }

    // The emulator's OWN bounds on the partial, which hold whatever the index's mappings
    // say: a partial past either is refused before anything is merged, and the error type
    // tells it apart from the mapping refusal above.
    let mut deep = json!("too deep");
    for _ in 0..40 {
        deep = json!({ "under": deep });
    }
    let mut many = Map::new();
    for at in 0..10_100 {
        many.insert(format!("field-{at}"), json!(at));
    }
    for (case, body) in [
        (
            "a partial nesting deeper than the merge walks",
            json!({ "doc": deep }),
        ),
        (
            "a partial naming more members than the merge holds",
            json!({ "doc": Value::Object(many) }),
        ),
    ] {
        let answered = update(&emulator, &host, "guarded", body).await;
        assert_eq!(
            (answered.status, error_type(&answered)),
            (400, "illegal_argument_exception".to_string()),
            "case {case:?}: expected the emulator's own bound to refuse it, got {} with {:?}",
            answered.status,
            answered.body
        );
        let held = read(&emulator, &host, "guarded")
            .await
            .unwrap_or_else(|| panic!("case {case:?}: expected the document to still be held"));
        assert_eq!(
            (held["_source"].clone(), held["_version"].as_i64()),
            (unchanged.clone(), Some(1)),
            "case {case:?}: expected a refused update to change nothing, got {held:?}"
        );
    }

    // The compare-and-set: the update proceeds only while the document is still at the
    // revision the request names.
    let held = read(&emulator, &host, "guarded")
        .await
        .expect("the index holds the document");
    let seq_no = held["_seq_no"].as_i64().unwrap_or_default();
    let stale = rest(
        &emulator,
        DEFAULT_ACCESS_KEY_ID,
        &host,
        "POST",
        &format!(
            "/guarded/_update/{ID}?if_seq_no={}&if_primary_term={PRIMARY_TERM}",
            seq_no + 1
        ),
        Some(json!({ "doc": { "status": "done" } })),
    )
    .await;
    assert_eq!(
        (stale.status, error_type(&stale)),
        (409, "version_conflict_engine_exception".to_string()),
        "case \"an update naming a revision the document is not at\": expected the \
         compare-and-set refusal, got {} with {:?}",
        stale.status,
        stale.body
    );
    let current = rest(
        &emulator,
        DEFAULT_ACCESS_KEY_ID,
        &host,
        "POST",
        &format!("/guarded/_update/{ID}?if_seq_no={seq_no}&if_primary_term={PRIMARY_TERM}"),
        Some(json!({ "doc": { "status": "done" } })),
    )
    .await;
    assert_eq!(
        (current.status, current.body["result"].as_str()),
        (200, Some("updated")),
        "case \"an update naming the revision the document is at\": expected it to land, got \
         {} with {:?}",
        current.status,
        current.body
    );
}

/// RULE: the ONE partial-update rule answers both requests that ask for it — the update API
/// and the `update` action of a bulk stream — so a field advanced on its own and one
/// advanced inside a stream leave the same document and report the same result.
#[tokio::test]
async fn the_same_partial_update_rule_answers_the_route_and_a_bulk_item() {
    let emulator = Emulator::new();
    created_domain(&emulator, "shared-rule").await;
    let host = endpoint_of(&emulator, "shared-rule");
    let source = json!({ "status": "pending", "attempts": 0, "owner": "x" });
    for index in ["by-route", "by-bulk"] {
        stored(&emulator, &host, index, source.clone()).await;
    }
    let partial = json!({ "status": "done", "attempts": 1 });
    let merged = json!({ "status": "done", "attempts": 1, "owner": "x" });

    // The route, and the same partial as one item of a bulk stream.
    let routed = update(
        &emulator,
        &host,
        "by-route",
        json!({ "doc": partial.clone() }),
    )
    .await;
    assert_eq!(
        (routed.status, routed.body["result"].as_str()),
        (200, Some("updated")),
        "case \"the update route\": expected 200 updated, got {} with {:?}",
        routed.status,
        routed.body
    );
    let streamed = bulk(
        &emulator,
        &host,
        "/_bulk?refresh=true",
        &[
            json!({ "update": { "_index": "by-bulk", "_id": ID } }),
            json!({ "doc": partial.clone() }),
        ],
    )
    .await;
    assert_eq!(
        (
            streamed.status,
            streamed.body["errors"].as_bool(),
            streamed.body["items"][0]["update"]["status"].as_i64(),
            streamed.body["items"][0]["update"]["result"].as_str(),
        ),
        (200, Some(false), Some(200), Some("updated")),
        "case \"the bulk update action\": expected one item reporting the same merge, got {} \
         with {:?}",
        streamed.status,
        streamed.body
    );

    // Both left the same document at the same revision.
    for index in ["by-route", "by-bulk"] {
        let held = read(&emulator, &host, index)
            .await
            .unwrap_or_else(|| panic!("case {index:?}: expected the index to hold the document"));
        assert_eq!(
            (held["_source"].clone(), held["_version"].as_i64()),
            (merged.clone(), Some(2)),
            "case {index:?}: expected the merged document at version 2, got {held:?}"
        );
    }

    // And both report a merge that changes nothing as one, leaving the revision where it
    // is.
    let again = update(&emulator, &host, "by-route", json!({ "doc": partial.clone() })).await;
    assert_eq!(
        (again.status, again.body["result"].as_str(), again.body["_version"].as_i64()),
        (200, Some("noop"), Some(2)),
        "case \"the route, re-sent\": expected a no-op at the revision the document is at, \
         got {} with {:?}",
        again.status,
        again.body
    );
    let streamed_again = bulk(
        &emulator,
        &host,
        "/_bulk?refresh=true",
        &[
            json!({ "update": { "_index": "by-bulk", "_id": ID } }),
            json!({ "doc": partial.clone() }),
        ],
    )
    .await;
    assert_eq!(
        (
            streamed_again.status,
            streamed_again.body["items"][0]["update"]["result"].as_str(),
            streamed_again.body["items"][0]["update"]["_version"].as_i64(),
        ),
        (200, Some("noop"), Some(2)),
        "case \"the bulk item, re-sent\": expected the same no-op, got {} with {:?}",
        streamed_again.status,
        streamed_again.body
    );
}
