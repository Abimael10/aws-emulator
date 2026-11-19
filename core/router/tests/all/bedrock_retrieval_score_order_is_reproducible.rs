// SPDX-License-Identifier: CC0-1.0

//! The ORDER and the REPRODUCIBILITY of the relevance `Retrieve` reports for a knowledge
//! base whose vector index lives in an OpenSearch Serverless collection: the OpenSearch
//! Serverless data plane, the Bedrock Agent build-time plane and the Bedrock Agent runtime
//! plane, over one engine.
//!
//! RULE: a retrieval's `score` is the relevance the bound index's own geometry gives the
//! query — the query is turned into a vector the way the ingestion turned each chunk into
//! one and the distance is the index's own — so `retrievalResults` arrives ordered by
//! DESCENDING score, the chunk whose text the query's words occur in ranks above the chunks
//! they do not, and two identical retrievals agree on every score and on every result. The
//! relevance is a function of the chunk, the query and the index alone: nothing about when a
//! call is made, how many calls were made before it, or the order the documents were pushed
//! in may change it.
//!
//! Three crates have to agree for that to hold — the index and the documents it holds are
//! served by `service-com-amazonaws-opensearchserverless-opensearchserverless`, the knowledge
//! base and the ingestion that writes the chunk vectors by the Bedrock Agent build-time
//! plane, the ranking by the Bedrock Agent runtime plane — so the suite lives here rather
//! than in any one of them, and it stands its collection up with the setup helpers the
//! sibling suites share (`bedrock_knowledge_base_over_an_aoss_collection.rs`).
//!
//! What the siblings pin, and this one does not repeat: that such a knowledge base can be
//! BUILT and read at all, and that an identifier naming no knowledge base is refused 404
//! (`bedrock_retrieve_over_a_published_collection_endpoint_index.rs`); and WHERE the chunks
//! are read from — the collection that holds the index, chunk by chunk, with a gone index
//! reported as the failed dependency it is
//! (`bedrock_retrieval_reads_the_bound_collection_index.rs`). What is pinned HERE is the
//! `score` those answers carry: its ordering and its reproducibility, which no suite drives.
//!
//! The rows span what the rule generalises over: the embedding model and with it the number
//! of values the index holds, the query asked and which chunk it is nearest to, and the
//! `numberOfResults` the request bounds the page by — from one result to more than the
//! knowledge base holds. Every wait is a poll for the CONDITION; nothing here asserts how
//! long a step took.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::bedrock_knowledge_base_over_an_aoss_collection::{
    active_collection, admit_network_access, at, await_active_knowledge_base,
    await_indexed_document, bedrock, build_router, encrypt_collections,
    grant_the_role_data_access, inline_byte_document, opensearch_storage, rest, text, KB_ROLE,
    METADATA_FIELD, REGION, TEXT_FIELD, VECTOR_FIELD,
};

/// The passages every knowledge base here ingests: four documents whose distinguishing
/// words occur in exactly one of them, so which chunk a query is nearest to is decided by
/// the TEXT and not by the order the documents were pushed in.
const PASSAGES: [(&str, &str); 4] = [
    (
        "refunds",
        "Refund policy: a customer may return any item within thirty days of delivery.",
    ),
    (
        "shipping",
        "Shipping schedule: orders leave the warehouse within two days by ground courier.",
    ),
    (
        "warranty",
        "Warranty terms: a device is covered against manufacturing defects for twenty four \
         months.",
    ),
    (
        "privacy",
        "Privacy notice: a customer record is purged ninety days after an account is closed.",
    ),
];

/// One row of the ranking table: the query, the passage it is nearest to, and the
/// `numberOfResults` the request carries (the model bounds that member at `range 1..100`,
/// and a bound above what the knowledge base holds reports everything it holds).
const QUERIES: [(&str, &str, usize); 5] = [
    ("refund", "refunds", 10),
    ("warranty defects", "warranty", 10),
    ("warehouse courier", "shipping", 4),
    ("privacy account", "privacy", 2),
    ("refund delivery", "refunds", 1),
];

/// One case of the outer table: the resources it stands up and the geometry of the index its
/// chunks live in.
struct Row {
    case: &'static str,
    /// The collection, the index inside it and the knowledge base's name.
    collection: &'static str,
    index: &'static str,
    knowledge_base: &'static str,
    /// The embedding model the knowledge base names, and the number of values its vectors
    /// carry — which is the dimension the index is created with.
    model: &'static str,
    dimension: usize,
}

/// The index body a Bedrock knowledge base's vector index is created with: a `knn_vector`
/// field of `dimension` values beside the text and metadata fields the storage configuration
/// maps, which is the body `opensearch-py`'s `indices.create` carries for one.
fn knn_index_body(dimension: usize) -> Value {
    json!({
        "settings": { "index": { "knn": true } },
        "mappings": { "properties": {
            VECTOR_FIELD: { "type": "knn_vector", "dimension": dimension },
            TEXT_FIELD: { "type": "text" },
            METADATA_FIELD: { "type": "text", "index": false },
        } },
    })
}

/// One `Retrieve` of at most `keep` results, as the operation's `@http` binding places it
/// (`POST /knowledgebases/{knowledgeBaseId}/retrieve`), asserting the call was served and
/// answering the `retrievalResults` list.
async fn retrieve(
    router: &MultiServiceRouter,
    case: &str,
    knowledge_base_id: &str,
    query: &str,
    keep: usize,
) -> Vec<Value> {
    let (status, answer) = bedrock(
        router,
        "POST",
        &format!("/knowledgebases/{knowledge_base_id}/retrieve"),
        Some(&json!({
            "retrievalQuery": { "text": query },
            "retrievalConfiguration": {
                "vectorSearchConfiguration": { "numberOfResults": keep },
            },
        })),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the retrieval to be served, got {status} with {answer}"
    );
    at(&answer, "retrievalResults")
        .as_array()
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected retrievalResults, got {answer}"))
}

/// The `score` of each result, asserting every one of them reports the member.
fn scores(case: &str, results: &[Value]) -> Vec<f64> {
    results
        .iter()
        .map(|result| {
            at(result, "score")
                .as_f64()
                .unwrap_or_else(|| panic!("case {case:?}: expected a score, got {result}"))
        })
        .collect()
}

/// The id each result cites its document under, which is how a document of a CUSTOM data
/// source is addressed.
fn cited(results: &[Value]) -> Vec<String> {
    results
        .iter()
        .map(|result| text(result, "location.customDocumentLocation.id"))
        .collect()
}

/// RULE: the chunks of a collection-backed knowledge base are reported ordered by descending
/// `score`, the chunk the query's words occur in ranks first, every score is a relevance in
/// `[0, 1]`, and a second identical retrieval answers the same scores and the same results —
/// so a client that ranks or thresholds on `score` gets the same answer twice.
#[tokio::test]
async fn a_collection_backed_retrieval_ranks_by_score_and_two_identical_calls_agree() {
    // `amazon.titan-embed-text-v2:0` publishes 1024-value vectors and
    // `amazon.titan-embed-text-v1` 1536, so each row creates its index with the number of
    // values its own model produces.
    let rows: &[Row] = &[
        Row {
            case: "a 1024-value index",
            collection: "score-order-small",
            index: "bedrock-knowledge-base-score-small",
            knowledge_base: "score-order-small",
            model: "amazon.titan-embed-text-v2:0",
            dimension: 1024,
        },
        Row {
            case: "a 1536-value index",
            collection: "score-order-wide",
            index: "bedrock-knowledge-base-score-wide",
            knowledge_base: "score_order_wide",
            model: "amazon.titan-embed-text-v1",
            dimension: 1536,
        },
    ];

    for row in rows {
        let router = build_router();
        encrypt_collections(&router).await;
        admit_network_access(&router).await;
        grant_the_role_data_access(&router, row.collection).await;

        let detail = active_collection(&router, row.collection).await;
        let endpoint = text(&detail, "collectionEndpoint");
        let collection_arn = text(&detail, "arn");

        // The vector index, created at the address the collection publishes, before any
        // knowledge base names it.
        let (status, created) = rest(
            &router,
            &endpoint,
            "PUT",
            &format!("/{}", row.index),
            Some(&knn_index_body(row.dimension)),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: expected the index create at the published endpoint to be served, got \
             {status} with {created}",
            row.case
        );

        let (status, kb) = bedrock(
            &router,
            "PUT",
            "/knowledgebases/",
            Some(&json!({
                "name": row.knowledge_base,
                "roleArn": KB_ROLE,
                "knowledgeBaseConfiguration": {
                    "type": "VECTOR",
                    "vectorKnowledgeBaseConfiguration": {
                        "embeddingModelArn":
                            format!("arn:aws:bedrock:{REGION}::foundation-model/{}", row.model),
                    },
                },
                "storageConfiguration": opensearch_storage(&collection_arn, row.index),
            })),
        )
        .await;
        assert_eq!(
            status, 202,
            "case {:?}: expected CreateKnowledgeBase over the created index to be accepted, got \
             {status} with {kb}",
            row.case
        );
        let knowledge_base_id = text(&kb, "knowledgeBase.knowledgeBaseId");
        await_active_knowledge_base(&router, row.case, &knowledge_base_id).await;

        // A CUSTOM data source and the four passages pushed straight into it, so every chunk
        // the ranking orders is a chunk an ingestion really wrote into that index.
        let (status, source) = bedrock(
            &router,
            "PUT",
            &format!("/knowledgebases/{knowledge_base_id}/datasources/"),
            Some(&json!({
                "name": format!("{}-pushed", row.collection),
                "dataSourceConfiguration": { "type": "CUSTOM" },
            })),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: expected CreateDataSource(CUSTOM) accepted, got {status} with {source}",
            row.case
        );
        let data_source_id = text(&source, "dataSource.dataSourceId");
        let documents: Vec<Value> = PASSAGES
            .iter()
            .map(|(id, passage)| inline_byte_document(id, passage))
            .collect();
        let (status, ingested) = bedrock(
            &router,
            "PUT",
            &format!(
                "/knowledgebases/{knowledge_base_id}/datasources/{data_source_id}/documents"
            ),
            Some(&json!({ "documents": documents })),
        )
        .await;
        assert_eq!(
            status, 202,
            "case {:?}: expected the ingestion accepted, got {status} with {ingested}",
            row.case
        );
        for (id, _) in PASSAGES {
            await_indexed_document(&router, row.case, &knowledge_base_id, &data_source_id, id)
                .await;
        }

        for (query, nearest, keep) in QUERIES {
            let case = format!("{}, query {query:?}, numberOfResults {keep}", row.case);
            let answered = retrieve(&router, &case, &knowledge_base_id, query, keep).await;
            assert_eq!(
                answered.len(),
                keep.min(PASSAGES.len()),
                "case {case:?}: expected one result per chunk the page admits, got {answered:?}"
            );

            let ranked = scores(&case, &answered);
            assert!(
                ranked.iter().all(|score| (0.0..=1.0).contains(score)),
                "case {case:?}: expected every score to be a relevance in [0, 1], got {ranked:?}"
            );
            for (above, below) in ranked.iter().zip(ranked.iter().skip(1)) {
                assert!(
                    above >= below,
                    "case {case:?}: expected the results ordered by descending score, got \
                     {ranked:?}"
                );
            }
            // The order is the index's own geometry: the chunk whose text the query's words
            // occur in is the nearest, and where the page holds more than one result the
            // scores really separate it from the furthest.
            assert_eq!(
                cited(&answered).first().map(String::as_str),
                Some(nearest),
                "case {case:?}: expected the passage the query names to rank first, got {:?}",
                cited(&answered)
            );
            if let [first, .., last] = ranked.as_slice() {
                assert!(
                    first > last,
                    "case {case:?}: expected the scores to separate the nearest chunk from the \
                     furthest, got {ranked:?}"
                );
            }

            // The score is a function of the chunk, the query and the index, so the identical
            // request answers the identical relevance — well inside the float32 precision the
            // stored vectors carry — and the identical page in the identical order.
            let again = retrieve(&router, &case, &knowledge_base_id, query, keep).await;
            assert_eq!(
                scores(&case, &again),
                ranked,
                "case {case:?}: expected two identical retrievals to agree on every score, got \
                 {again:?}"
            );
            assert_eq!(
                again, answered,
                "case {case:?}: expected two identical retrievals to answer the same results, \
                 got {again:?}"
            );
        }
    }
}
