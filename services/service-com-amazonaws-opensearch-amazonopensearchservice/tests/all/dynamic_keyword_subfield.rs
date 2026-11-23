// SPDX-License-Identifier: CC0-1.0

//! The IMPLICIT `keyword` sub-field of a field no mapping covers, driven over the real wire
//! path: a signed request to the endpoint a domain publishes, through the multi-service
//! router, into the mounted service.
//!
//! > a string field no mapping and no template covers is mapped by the write that introduced
//! > it — as `text` with a `keyword` sub-field named `keyword` holding the field's exact
//! > unanalysed value — so `<field>.keyword` addresses another indexing OF that field rather
//! > than a member the document holds under it: a request naming that dotted path reads the
//! > string values its parent holds, and only those short enough for the sub-field's
//! > `ignore_above`, while a path the document really holds a value at is read as the value
//! > it is.
//!
//! Which matters because `<field>.keyword` is how a client looks a string up by its exact
//! value when nothing declared a mapping for it, and that lookup is the read half of every
//! index-then-verify sequence: answered empty, it is indistinguishable from the document
//! never having been written.
//!
//! The rule is read over a table whose rows differ along what it generalises over — where the
//! field sits (top level or inside an object), what the parent holds (one string, an array of
//! them, a number, a boolean, an object member of that very name), the form of the value
//! (plain token, capitals and spaces) and its LENGTH (exactly the `ignore_above` the dynamic
//! template carries, and one character past it) — and over the request forms that share the
//! one resolution: a `term`, a `terms`, an `exists` and an aggregation.
//!
//! The control-plane harness is [`super::domain_lifecycle`]'s and the data-plane request
//! helper is [`super::domain_rest_data_plane`]'s; this suite adds no second copy of either.

use serde_json::{json, Value};
use service_com_amazonaws_opensearch_amazonopensearchservice::state::analysis::DYNAMIC_KEYWORD_IGNORE_ABOVE;

use super::data_source_registry::created_domain;
use super::domain_lifecycle::{Answer, Emulator, DEFAULT_ACCESS_KEY_ID};
use super::domain_rest_data_plane::{endpoint_of, rest};

/// The domain whose published endpoint every request below is addressed to.
const DOMAIN: &str = "dynamic-keyword";

/// The index the documents land in. Nothing creates it: a write to an index the cluster does
/// not hold creates it with no mappings of its own, which is what makes every field below a
/// dynamically mapped one.
const INDEX: &str = "events";

/// `CreateDomain`, answering the endpoint host it published.
async fn given_domain(emulator: &Emulator) -> String {
    created_domain(emulator, DOMAIN).await;
    endpoint_of(emulator, DOMAIN)
}

/// One signed request to that endpoint.
async fn call(
    emulator: &Emulator,
    host: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Answer {
    rest(emulator, DEFAULT_ACCESS_KEY_ID, host, method, path, body).await
}

/// Write one document under `id`, asserting the cluster took it.
async fn written(emulator: &Emulator, host: &str, id: &str, source: Value) {
    let answer = call(
        emulator,
        host,
        "PUT",
        &format!("/{INDEX}/_doc/{id}"),
        Some(source),
    )
    .await;
    assert!(
        answer.status == 200 || answer.status == 201,
        "case write {INDEX}/{id}: expected 200 or 201, got {} with {:?}",
        answer.status,
        answer.body
    );
}

/// Make the writes visible, asserting the refresh was served.
async fn refreshed(emulator: &Emulator, host: &str) {
    let answer = call(emulator, host, "POST", &format!("/{INDEX}/_refresh"), None).await;
    assert_eq!(
        answer.status, 200,
        "case \"POST /{INDEX}/_refresh\": expected 200, got {} with {:?}",
        answer.status, answer.body
    );
}

/// One search of that index.
async fn search(emulator: &Emulator, host: &str, body: Value) -> Answer {
    call(
        emulator,
        host,
        "POST",
        &format!("/{INDEX}/_search"),
        Some(body),
    )
    .await
}

/// The ids a search answered, in the order it answered them.
fn hit_ids(answer: &Answer) -> Vec<String> {
    answer.body["hits"]["hits"]
        .as_array()
        .map(|hits| {
            hits.iter()
                .map(|hit| hit["_id"].as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// A value of `count` repetitions of `ch` — what the rows at the sub-field's length bound are
/// built from.
fn repeated(ch: char, count: usize) -> String {
    ::std::iter::repeat_n(ch, count).collect()
}

/// The documents the table below is read over: one per thing a dynamically mapped field can
/// hold, so what a row asks of `<field>.keyword` is decided by the value rather than by the
/// query.
fn documents() -> Vec<(&'static str, Value)> {
    vec![
        (
            "plain",
            json!({ "title": "encrypted at rest", "marker": "cmk-7716ac54d1f3" }),
        ),
        ("phrase", json!({ "marker": "Encrypted At Rest" })),
        ("nested", json!({ "meta": { "tag": "nested-value" } })),
        ("listed", json!({ "labels": ["alpha", "beta"] })),
        ("counted", json!({ "count": 7 })),
        ("flagged", json!({ "flag": true })),
        (
            "at-bound",
            json!({ "marker": repeated('a', DYNAMIC_KEYWORD_IGNORE_ABOVE) }),
        ),
        (
            "past-bound",
            json!({ "marker": repeated('b', DYNAMIC_KEYWORD_IGNORE_ABOVE + 1) }),
        ),
        (
            "wide-at-bound",
            json!({ "marker": repeated('é', DYNAMIC_KEYWORD_IGNORE_ABOVE) }),
        ),
        ("member", json!({ "nested": { "keyword": "a real member" } })),
    ]
}

/// Those documents, written and made visible.
async fn given_indexed(emulator: &Emulator) -> String {
    let host = given_domain(emulator).await;
    for (id, source) in documents() {
        written(emulator, &host, id, source).await;
    }
    refreshed(emulator, &host).await;
    host
}

/// RULE (the sequence the whole rule serves): a document written into an index nothing mapped
/// and then made visible is found by an exact-term query on the implicit `keyword` sub-field
/// of the string field it introduced, with its own `_source` answered beside it.
#[tokio::test]
async fn an_indexed_and_refreshed_document_is_found_by_its_dynamic_keyword_term() {
    let emulator = Emulator::new();
    let host = given_domain(&emulator).await;
    let marker = "cmk-7716ac54d1f3";

    // No index and no mapping is created beforehand, so `marker` is mapped by this write.
    let indexed = call(
        &emulator,
        &host,
        "PUT",
        &format!("/{INDEX}/_doc/1"),
        Some(json!({ "title": "encrypted at rest", "marker": marker })),
    )
    .await;
    assert_eq!(
        indexed.status, 201,
        "case \"PUT /{INDEX}/_doc/1\": expected the created document, got {} with {:?}",
        indexed.status, indexed.body
    );
    refreshed(&emulator, &host).await;

    let answered = search(
        &emulator,
        &host,
        json!({ "query": { "term": { "marker.keyword": marker } } }),
    )
    .await;
    assert_eq!(
        (answered.status, hit_ids(&answered)),
        (200, vec!["1".to_string()]),
        "case \"a term on marker.keyword after the refresh\": expected exactly the one indexed \
         document, got {} with {:?}",
        answered.status,
        answered.body
    );
    assert_eq!(
        (
            answered.body["hits"]["total"]["value"].as_u64(),
            answered.body["hits"]["hits"][0]["_source"]["marker"].as_str(),
            answered.body["hits"]["hits"][0]["_source"]["title"].as_str(),
        ),
        (Some(1), Some(marker), Some("encrypted at rest")),
        "case \"the hit the term query answered\": expected the total and the source the write \
         carried, got {:?}",
        answered.body
    );

    // The read changed nothing a later caller sees: the same query answers the same hit.
    let again = search(
        &emulator,
        &host,
        json!({ "query": { "term": { "marker.keyword": marker } } }),
    )
    .await;
    assert_eq!(
        (again.status, hit_ids(&again)),
        (200, vec!["1".to_string()]),
        "case \"the same term asked twice\": expected the same hit, got {} with {:?}",
        again.status,
        again.body
    );
}

/// One row of the table: what the exact-term query asks, and which documents hold it.
struct Row {
    case: &'static str,
    query: Value,
    expected: Vec<&'static str>,
}

/// RULE: `<field>.keyword` holds the exact unanalysed string values its parent holds — for a
/// field at the top level and one inside an object, for one value and for every element of an
/// array — and nothing for a parent holding a value dynamic mapping gives no `keyword`
/// sub-field, for a value past the sub-field's `ignore_above`, or for a term the parent's
/// value only contains.
#[tokio::test]
async fn the_implicit_keyword_subfield_holds_the_exact_values_of_its_parent() {
    let emulator = Emulator::new();
    let host = given_indexed(&emulator).await;

    let rows: Vec<Row> = vec![
        Row {
            case: "the exact value of a dynamically mapped string",
            query: json!({ "term": { "marker.keyword": "cmk-7716ac54d1f3" } }),
            expected: vec!["plain"],
        },
        Row {
            case: "a value carrying capitals and spaces, unanalysed",
            query: json!({ "term": { "marker.keyword": "Encrypted At Rest" } }),
            expected: vec!["phrase"],
        },
        Row {
            case: "one token of that same value",
            query: json!({ "term": { "marker.keyword": "Encrypted" } }),
            expected: vec![],
        },
        Row {
            case: "that value in another case",
            query: json!({ "term": { "marker.keyword": "encrypted at rest" } }),
            expected: vec![],
        },
        Row {
            case: "a dynamically mapped field inside an object",
            query: json!({ "term": { "meta.tag.keyword": "nested-value" } }),
            expected: vec!["nested"],
        },
        Row {
            case: "one element of an array of strings",
            query: json!({ "term": { "labels.keyword": "beta" } }),
            expected: vec!["listed"],
        },
        Row {
            case: "either element of that array",
            query: json!({ "terms": { "labels.keyword": ["alpha", "zeta"] } }),
            expected: vec!["listed"],
        },
        Row {
            case: "a value no document holds",
            query: json!({ "term": { "marker.keyword": "cmk-absent" } }),
            expected: vec![],
        },
        Row {
            case: "a number, which dynamic mapping gives no keyword sub-field",
            query: json!({ "term": { "count.keyword": "7" } }),
            expected: vec![],
        },
        Row {
            case: "a boolean, likewise",
            query: json!({ "term": { "flag.keyword": "true" } }),
            expected: vec![],
        },
        Row {
            case: "a value exactly as long as ignore_above admits",
            query: json!({
                "term": { "marker.keyword": repeated('a', DYNAMIC_KEYWORD_IGNORE_ABOVE) }
            }),
            expected: vec!["at-bound"],
        },
        Row {
            case: "a value one character past ignore_above",
            query: json!({
                "term": { "marker.keyword": repeated('b', DYNAMIC_KEYWORD_IGNORE_ABOVE + 1) }
            }),
            expected: vec![],
        },
        Row {
            case: "a value at that bound in characters outside ASCII",
            query: json!({
                "term": { "marker.keyword": repeated('é', DYNAMIC_KEYWORD_IGNORE_ABOVE) }
            }),
            expected: vec!["wide-at-bound"],
        },
        Row {
            case: "a member the document really holds under that name",
            query: json!({ "term": { "nested.keyword": "a real member" } }),
            expected: vec!["member"],
        },
    ];

    for row in &rows {
        let answered = search(&emulator, &host, json!({ "query": row.query })).await;
        let expected: Vec<String> = row.expected.iter().map(|id| (*id).to_string()).collect();
        assert_eq!(
            (answered.status, hit_ids(&answered)),
            (200, expected.clone()),
            "case {:?}: expected {expected:?}, got {} with {:?}",
            row.case,
            answered.status,
            answered.body
        );
        assert_eq!(
            answered.body["hits"]["total"]["value"].as_u64(),
            Some(expected.len() as u64),
            "case {:?}: expected the total to count the same hits, got {:?}",
            row.case,
            answered.body
        );
    }
}

/// RULE: every request form reads that sub-field through the one resolution — an `exists`
/// about the document, and an aggregation over the per-document values — so each answers the
/// documents whose parent holds an indexed string value and no others.
#[tokio::test]
async fn every_request_form_reads_the_implicit_keyword_subfield_the_same_way() {
    let emulator = Emulator::new();
    let host = given_indexed(&emulator).await;

    // An `exists` on the sub-field: the documents whose `marker` holds a string the sub-field
    // indexes — the one past `ignore_above` holds no term, so it holds no value there either.
    let existing = search(
        &emulator,
        &host,
        json!({ "query": { "exists": { "field": "marker.keyword" } }, "size": 20 }),
    )
    .await;
    let mut found = hit_ids(&existing);
    found.sort();
    assert_eq!(
        (existing.status, found),
        (
            200,
            vec![
                "at-bound".to_string(),
                "phrase".to_string(),
                "plain".to_string(),
                "wide-at-bound".to_string(),
            ]
        ),
        "case \"an exists on marker.keyword\": expected the documents holding an indexed value, \
         got {} with {:?}",
        existing.status,
        existing.body
    );

    // An aggregation over the same values: one bucket per distinct value of the sub-field,
    // read off the array its parent holds.
    let bucketed = search(
        &emulator,
        &host,
        json!({ "size": 0, "aggs": { "by_label": { "terms": { "field": "labels.keyword" } } } }),
    )
    .await;
    let keys: Vec<String> = bucketed.body["aggregations"]["by_label"]["buckets"]
        .as_array()
        .map(|buckets| {
            buckets
                .iter()
                .map(|bucket| bucket["key"].as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        (bucketed.status, keys),
        (200, vec!["alpha".to_string(), "beta".to_string()]),
        "case \"a terms aggregation over labels.keyword\": expected one bucket per element, got \
         {} with {:?}",
        bucketed.status,
        bucketed.body
    );

    // The documents are untouched by either read: each still reads back as it was written.
    let read = call(
        &emulator,
        &host,
        "GET",
        &format!("/{INDEX}/_doc/listed"),
        None,
    )
    .await;
    assert_eq!(
        (read.status, &read.body["_source"]["labels"]),
        (200, &json!(["alpha", "beta"])),
        "case \"reading a document behind those requests\": expected the source the write \
         carried, got {} with {:?}",
        read.status,
        read.body
    );
}
