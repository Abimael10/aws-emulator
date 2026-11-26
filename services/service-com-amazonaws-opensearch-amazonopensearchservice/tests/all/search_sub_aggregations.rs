// SPDX-License-Identifier: CC0-1.0

//! The SUB-AGGREGATIONS a bucket aggregation carries, driven over the real wire path: a signed
//! request to the endpoint a domain publishes, through the multi-service router, into the
//! mounted service.
//!
//! The rule, over the set of sections one search may ask for:
//!
//! > a BUCKET aggregation may carry an `aggs` section of its own, and each of its buckets
//! > reports what that section computes over the documents that fell in THAT bucket, beside
//! > its own `key` and `doc_count` — nested as deeply as the body nests it, with a document
//! > falling in a bucket ONCE however many of its values map to that bucket's key — while an
//! > aggregation that opens no bucket has no subset for such a section to be computed over and
//! > is refused rather than answered with it dropped.
//!
//! A `size: 0` search is the usual way to ask for one, so the `aggregations` section is
//! asserted beside hits the request did not ask for: a facet request that came back with no
//! section at all would look to a client like a domain holding no data.
//!
//! The harness is [`super::domain_lifecycle`]'s and the data-plane request helper is
//! [`super::domain_rest_data_plane`]'s; this suite adds no second copy of either.

use serde_json::{json, Value};

use super::domain_lifecycle::{Answer, Emulator};
use super::domain_rest_data_plane::{error_type, rest_prefixed};

/// The domain whose published endpoint every request below is addressed to.
const DOMAIN: &str = "facet-domain";

/// The index the table is read over: a `keyword` to bucket by, a second one to bucket a
/// bucket's documents by, a MULTI-VALUED one a document can fall in several buckets of, and a
/// number to measure.
const INDEX: &str = "catalogue";

/// `CreateDomain`, then the index and the documents the buckets are counted out of.
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
                "category": { "type": "keyword" },
                "shelf": { "type": "keyword" },
                "tags": { "type": "keyword" },
                "price": { "type": "integer" },
            } },
        })),
    )
    .await;
    assert_eq!(
        index.status, 200,
        "case \"PUT /{INDEX}\": expected 200, got {} with {:?}",
        index.status, index.body
    );
    // Each document's `tags` hold SEVERAL values, and two of them hold one value twice, which
    // is what makes "a document falls in a bucket once" observable.
    let documents: [(&str, &str, &str, i64, Value); 5] = [
        ("d1", "books", "east", 10, json!(["red", "red", "blue"])),
        ("d2", "books", "west", 20, json!(["blue"])),
        ("d3", "books", "east", 30, json!([])),
        ("d4", "tools", "west", 40, json!(["red"])),
        ("d5", "tools", "west", 50, json!(["blue", "blue"])),
    ];
    for (id, category, shelf, price, tags) in documents {
        let written = rest_prefixed(
            emulator,
            DOMAIN,
            "PUT",
            &format!("/{INDEX}/_doc/{id}"),
            Some(json!({
                "category": category,
                "shelf": shelf,
                "price": price,
                "tags": tags,
            })),
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

/// One `POST /{INDEX}/_search`.
async fn search(emulator: &Emulator, body: Value) -> Answer {
    rest_prefixed(
        emulator,
        DOMAIN,
        "POST",
        &format!("/{INDEX}/_search"),
        Some(body),
    )
    .await
}

/// RULE: each bucket reports what its own `aggs` section computes over the documents that fell
/// in it.
#[tokio::test]
async fn a_bucket_reports_what_its_own_aggregations_compute_over_its_documents() {
    let emulator = Emulator::new();
    given_indexed(&emulator).await;

    // (case, the section the request names, the section it must be answered with) — rows
    // spanning what a sub-aggregation may be: two metrics beside each other, a count, a
    // bucket aggregation of its own (so the section nests), a bucket aggregation over a
    // MULTI-VALUED field (where one document falls in several buckets, and in each of them
    // once however often it repeats that bucket's value), and none at all.
    let rows: [(&str, Value, Value); 5] = [
        (
            "two metrics under a terms aggregation",
            json!({ "by_category": {
                "terms": { "field": "category", "size": 10 },
                "aggs": {
                    "avg_price": { "avg": { "field": "price" } },
                    "sum_price": { "sum": { "field": "price" } },
                },
            } }),
            json!({ "by_category": {
                "doc_count_error_upper_bound": 0,
                "sum_other_doc_count": 0,
                "buckets": [
                    {
                        "key": "books",
                        "doc_count": 3,
                        "avg_price": { "value": 20.0 },
                        "sum_price": { "value": 60.0 },
                    },
                    {
                        "key": "tools",
                        "doc_count": 2,
                        "avg_price": { "value": 45.0 },
                        "sum_price": { "value": 90.0 },
                    },
                ],
            } }),
        ),
        (
            "a count of the bucket's own values",
            json!({ "by_category": {
                "terms": { "field": "category" },
                "aggs": { "priced": { "value_count": { "field": "price" } } },
            } }),
            json!({ "by_category": {
                "doc_count_error_upper_bound": 0,
                "sum_other_doc_count": 0,
                "buckets": [
                    { "key": "books", "doc_count": 3, "priced": { "value": 3 } },
                    { "key": "tools", "doc_count": 2, "priced": { "value": 2 } },
                ],
            } }),
        ),
        (
            "a terms aggregation under a terms aggregation",
            json!({ "by_category": {
                "terms": { "field": "category" },
                "aggregations": {
                    "by_shelf": {
                        "terms": { "field": "shelf" },
                        "aggs": { "most": { "max": { "field": "price" } } },
                    },
                },
            } }),
            json!({ "by_category": {
                "doc_count_error_upper_bound": 0,
                "sum_other_doc_count": 0,
                "buckets": [
                    {
                        "key": "books",
                        "doc_count": 3,
                        "by_shelf": {
                            "doc_count_error_upper_bound": 0,
                            "sum_other_doc_count": 0,
                            "buckets": [
                                {
                                    "key": "east",
                                    "doc_count": 2,
                                    "most": { "value": 30.0 },
                                },
                                {
                                    "key": "west",
                                    "doc_count": 1,
                                    "most": { "value": 20.0 },
                                },
                            ],
                        },
                    },
                    {
                        "key": "tools",
                        "doc_count": 2,
                        "by_shelf": {
                            "doc_count_error_upper_bound": 0,
                            "sum_other_doc_count": 0,
                            "buckets": [
                                {
                                    "key": "west",
                                    "doc_count": 2,
                                    "most": { "value": 50.0 },
                                },
                            ],
                        },
                    },
                ],
            } }),
        ),
        (
            "a bucket aggregation carrying no section of its own",
            json!({ "by_category": { "terms": { "field": "category" } } }),
            json!({ "by_category": {
                "doc_count_error_upper_bound": 0,
                "sum_other_doc_count": 0,
                "buckets": [
                    { "key": "books", "doc_count": 3 },
                    { "key": "tools", "doc_count": 2 },
                ],
            } }),
        ),
        (
            "a multi-valued field, where a document falls in each of its buckets once",
            json!({ "by_tag": {
                "terms": { "field": "tags" },
                "aggs": { "sum_price": { "sum": { "field": "price" } } },
            } }),
            json!({ "by_tag": {
                "doc_count_error_upper_bound": 0,
                "sum_other_doc_count": 0,
                "buckets": [
                    // d1, d2 and d5 carry `blue`, and d5 carries it twice: the bucket counts
                    // three documents and its metric reads each of them once.
                    { "key": "blue", "doc_count": 3, "sum_price": { "value": 80.0 } },
                    // d1 carries `red` twice and d4 once.
                    { "key": "red", "doc_count": 2, "sum_price": { "value": 50.0 } },
                ],
            } }),
        ),
    ];
    for (case, asked, expected) in rows {
        let answered = search(&emulator, json!({ "size": 0, "aggs": asked })).await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: expected 200, got {} with {:?}",
            answered.status, answered.body
        );
        assert_eq!(
            answered.body["aggregations"], expected,
            "case {case:?}: expected {expected:?}, got {:?}",
            answered.body["aggregations"]
        );
        // A `size: 0` search asks for the summaries and for no hit, and is answered with the
        // total it counted and an empty hit list — never with the section left out.
        assert_eq!(
            (
                answered.body["hits"]["total"]["value"].as_u64(),
                answered.body["hits"]["hits"].as_array().map(Vec::len),
            ),
            (Some(5), Some(0)),
            "case {case:?}: expected the total beside no hit, got {:?}",
            answered.body["hits"]
        );
    }
}

/// RULE: the sub-aggregations are computed over the documents the QUERY selected, so a query
/// that narrows the documents narrows every bucket and every metric under it.
#[tokio::test]
async fn sub_aggregations_are_computed_over_the_documents_the_query_selected() {
    let emulator = Emulator::new();
    given_indexed(&emulator).await;

    let answered = search(
        &emulator,
        json!({
            "size": 0,
            "query": { "term": { "shelf": "west" } },
            "aggs": { "by_category": {
                "terms": { "field": "category" },
                "aggs": { "sum_price": { "sum": { "field": "price" } } },
            } },
        }),
    )
    .await;
    assert_eq!(
        answered.status, 200,
        "case \"a narrowed facet\": expected 200, got {} with {:?}",
        answered.status, answered.body
    );
    assert_eq!(
        answered.body["aggregations"],
        json!({ "by_category": {
            "doc_count_error_upper_bound": 0,
            "sum_other_doc_count": 0,
            "buckets": [
                { "key": "tools", "doc_count": 2, "sum_price": { "value": 90.0 } },
                { "key": "books", "doc_count": 1, "sum_price": { "value": 20.0 } },
            ],
        } }),
        "case \"a narrowed facet\": expected the buckets of the selected documents, got {:?}",
        answered.body["aggregations"]
    );
}

/// RULE (the other side of the same rule): an aggregation that opens no bucket cannot carry a
/// section computed per bucket, so one carrying it is refused rather than answered with the
/// section dropped.
#[tokio::test]
async fn an_aggregation_that_opens_no_bucket_refuses_sub_aggregations() {
    let emulator = Emulator::new();
    given_indexed(&emulator).await;

    // (case, the aggregation type the request nests under, the two spellings of the section)
    let rows: [(&str, &str); 4] = [
        ("an average", "avg"),
        ("a sum", "sum"),
        ("a maximum", "max"),
        ("a value count", "value_count"),
    ];
    for (case, kind) in rows {
        for member in ["aggs", "aggregations"] {
            let refused = search(
                &emulator,
                json!({ "size": 0, "aggs": { "measured": {
                    kind: { "field": "price" },
                    member: { "by_category": { "terms": { "field": "category" } } },
                } } }),
            )
            .await;
            assert_eq!(
                (refused.status, error_type(&refused)),
                (400, "aggregation_initialization_exception".to_string()),
                "case {case:?} with [{member}]: expected the engine's own refusal, got {} \
                 with {:?}",
                refused.status,
                refused.body
            );
            let reason = refused.body["error"]["reason"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            assert_eq!(
                reason,
                format!("Aggregator [measured] of type [{kind}] cannot accept sub-aggregations"),
                "case {case:?} with [{member}]: expected the engine's own reason, got {reason:?}"
            );
        }
    }
}
