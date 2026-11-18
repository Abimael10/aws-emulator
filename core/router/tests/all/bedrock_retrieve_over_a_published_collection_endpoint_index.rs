// SPDX-License-Identifier: CC0-1.0

//! `Retrieve` over a vector index created at the address an OpenSearch Serverless collection
//! PUBLISHES — both of the outcomes that one request has, in one sequence over one engine:
//! the retrieval a knowledge base built on that index serves, and the refusal the very same
//! request earns when it names an identifier the Bedrock Agent plane holds no knowledge base
//! for.
//!
//! RULE: the address a collection publishes as its `collectionEndpoint` — "the
//! collection-specific endpoint used to submit index, search, and data upload requests"
//! (`CollectionDetail$collectionEndpoint`) — serves that collection's OpenSearch REST data
//! plane, which has no RPC target, so the vector index a knowledge base's
//! `storageConfiguration` names is created there whatever shape the index body takes; a
//! `Retrieve` addressed at a knowledge base built over that index answers HTTP 200 with the
//! required `retrievalResults` read from what the ingestion recorded, and a `Retrieve` of the
//! same shape addressed at any well-formed identifier that plane holds no knowledge base for
//! answers the modelled `ResourceNotFoundException` (HTTP 404, `Retrieve`'s own error) whose
//! message names the identifier the request carried.
//!
//! Three crates have to agree for that to hold — the collection's published endpoint is served
//! by `service-com-amazonaws-opensearchserverless-opensearchserverless`, the knowledge base and
//! the ingestion by the Bedrock Agent build-time plane, and the retrieval and its refusal by
//! the Bedrock Agent runtime plane — so the suite lives in the cross-service target. The
//! crate-local halves are those crates' own suites: the collection's REST index, document and
//! search surface in `services/service-com-amazonaws-opensearchserverless-\
//! opensearchserverless/tests/all/collection_data_plane.rs`, and what a retrieval reports and
//! refuses in `services/service-com-amazonaws-bedrockagentruntime-\
//! amazonbedrockagentruntimeservice/tests/all/knowledge_base_retrieval.rs`. The round trip of
//! the ingested bytes is pinned by the sibling suite this one shares its setup with,
//! `bedrock_knowledge_base_over_an_aoss_collection.rs`.
//!
//! The rows differ along what the rule generalises over: the FORM the index body takes (the
//! `knn` switch written as a dotted settings key and as a nested object, with and without an
//! explicit `method` on the vector field), the embedding model and with it the number of
//! values the index holds, the bytes the ingested document carries, and the names of the
//! collection, the index, the knowledge base and the document. The identifiers of the refusal
//! leg differ along their form: a bare id nothing created, the served id reversed (which is
//! how the recorded client builds one), and the ARN of an id nothing created.
//!
//! Every wait is a poll for the CONDITION, bounded by how many times it LOOKS; nothing here
//! asserts how long a step took.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::bedrock_knowledge_base_over_an_aoss_collection::{
    active_collection, admit_network_access, at, await_active_knowledge_base,
    await_indexed_document, bedrock, build_router, encrypt_collections,
    grant_the_role_data_access, inline_byte_document, opensearch_storage, rest, text,
    KB_ROLE, METADATA_FIELD, REGION, TEXT_FIELD, VECTOR_FIELD,
};

/// The account every ARN this emulator mints carries (`engine::EMULATOR_ACCOUNT_ID`).
const ACCOUNT: &str = "000000000000";

/// The error `Retrieve` declares for a knowledge base that is not there
/// (`smithy_query(query="operation",operation="Retrieve")` -> `ResourceNotFoundException`,
/// `httpError` 404).
const NOT_FOUND: &str = "ResourceNotFoundException";

/// How long `KnowledgeBaseIdentifier` makes a bare knowledge-base id: the model's
/// `^[0-9a-zA-Z]{10}$` alternative, which is the form the refusal leg's identifiers take.
const BARE_ID_LENGTH: usize = 10;

/// One case of the table: the resources it sets up, the shape it writes the index body in,
/// and the document it pushes through the knowledge base.
struct Row {
    case: &'static str,
    /// The collection, the index inside it, and the knowledge base's name.
    collection: &'static str,
    index: &'static str,
    knowledge_base: &'static str,
    /// The embedding model the knowledge base names, and the number of values its vectors
    /// carry — which is the dimension the index is created with.
    model: &'static str,
    dimension: usize,
    /// Whether the `knn` switch travels as the dotted settings key `index.knn` (the form
    /// `opensearch-py` callers commonly send) rather than as a nested `index` object.
    dotted_knn_setting: bool,
    /// The approximate-nearest-neighbour `method` the vector field declares, where the body
    /// declares one at all.
    method: Option<(&'static str, &'static str, &'static str)>,
    /// The document pushed into the knowledge base: the id it is cited under and its bytes.
    document: (&'static str, &'static str),
}

/// The index body a Bedrock knowledge base's vector index is created with, in the form `row`
/// writes it: a `knn_vector` field of `row.dimension` values beside the text and metadata
/// fields the storage configuration maps, and the `knn` switch in whichever of its two forms
/// the row sends.
fn knn_index_body(row: &Row) -> Value {
    let mut vector = json!({ "type": "knn_vector", "dimension": row.dimension });
    if let Some((name, engine, space_type)) = row.method {
        vector["method"] = json!({ "name": name, "engine": engine, "space_type": space_type });
    }
    let settings = if row.dotted_knn_setting {
        json!({ "index.knn": true })
    } else {
        json!({ "index": { "knn": true } })
    };
    json!({
        "settings": settings,
        "mappings": { "properties": {
            VECTOR_FIELD: vector,
            TEXT_FIELD: { "type": "text" },
            METADATA_FIELD: { "type": "text", "index": false },
        } },
    })
}

/// The wire error code `restJson1` puts in `__type`, without the shape's namespace.
fn error_code(body: &Value) -> String {
    text(body, "__type")
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// Percent-encode an identifier for the non-greedy `@httpLabel` it travels in: a
/// knowledge-base ARN carries `/`, which would otherwise split the path segment.
fn label(identifier: &str) -> String {
    identifier.replace('%', "%25").replace('/', "%2F")
}

/// One `Retrieve` call, as the operation's `@http` binding places it:
/// `POST /knowledgebases/{knowledgeBaseId}/retrieve`.
async fn retrieve(router: &MultiServiceRouter, identifier: &str, query: &str) -> (u16, Value) {
    bedrock(
        router,
        "POST",
        &format!("/knowledgebases/{}/retrieve", label(identifier)),
        Some(&json!({ "retrievalQuery": { "text": query } })),
    )
    .await
}

/// The identifiers of knowledge bases this emulator holds none of, each well-formed under
/// `KnowledgeBaseIdentifier`: a bare id nothing created, the SERVED id written backwards —
/// which is how the recorded client builds an id it never provisioned — and the ARN form of a
/// bare id nothing created.
///
/// A served id that reads the same backwards would name the knowledge base that exists, so
/// such an id contributes no reversed row; every row this answers is checked against `served`.
fn never_provisioned(served: &str) -> Vec<(String, String)> {
    let reversed: String = served.chars().rev().collect();
    let mut identifiers = vec![(
        "a bare id nothing created".to_string(),
        "0OpenSrch0".to_string(),
    )];
    if reversed != served && reversed.len() == BARE_ID_LENGTH {
        identifiers.push(("the served id reversed".to_string(), reversed));
    }
    identifiers.push((
        "the ARN of an id nothing created".to_string(),
        format!("arn:aws:bedrock:{REGION}:{ACCOUNT}:knowledge-base/0OpenSrch0"),
    ));
    identifiers
        .into_iter()
        .filter(|(_, identifier)| identifier != served)
        .collect()
}

/// RULE: an index created at the address a collection publishes as its `collectionEndpoint`
/// backs a knowledge base whose `Retrieve` is SERVED with `retrievalResults`, and a `Retrieve`
/// naming an identifier the Bedrock Agent plane holds no knowledge base for is refused
/// `ResourceNotFoundException` (404) naming that identifier — never a refusal about the
/// request's shape, and never an empty success.
#[tokio::test]
async fn a_published_endpoint_index_backs_retrieve_and_an_unheld_identifier_is_not_found() {
    // `cohere.embed-english-v3` publishes 1024-value vectors and `amazon.titan-embed-text-v1`
    // 1536, so each row creates its index with the number of values its own model produces
    // (`EMBEDDING_MODEL_DIMENSIONS`, read off each model's published output length).
    let rows: &[Row] = &[
        Row {
            case: "a dotted index.knn setting, an hnsw/faiss method, 1024 values",
            collection: "retrieve-dotted",
            index: "bedrock-knowledge-base-dotted",
            knowledge_base: "retrieve-dotted",
            model: "cohere.embed-english-v3",
            dimension: 1024,
            dotted_knn_setting: true,
            method: Some(("hnsw", "faiss", "l2")),
            document: ("doc-dotted", "refund policy: thirty calendar days"),
        },
        Row {
            case: "a nested index object, no method, 1536 values, multi-byte bytes",
            collection: "retrieve-nested",
            index: "bedrock-knowledge-base-nested",
            knowledge_base: "retrieve_nested",
            model: "amazon.titan-embed-text-v1",
            dimension: 1536,
            dotted_knn_setting: false,
            method: None,
            document: ("doc-nested", "r\u{e8}glement de remboursement \u{2014} 30 jours"),
        },
    ];

    for row in rows {
        let case = row.case;
        let (document_id, document_bytes) = row.document;
        let router = build_router();
        encrypt_collections(&router).await;
        admit_network_access(&router).await;
        grant_the_role_data_access(&router, row.collection).await;

        let detail = active_collection(&router, row.collection).await;
        let endpoint = text(&detail, "collectionEndpoint");
        let collection_arn = text(&detail, "arn");
        assert!(
            !endpoint.is_empty() && !collection_arn.is_empty(),
            "case {case:?}: expected an ACTIVE collection to publish an endpoint and an ARN, \
             got {detail}"
        );

        // The index create a knowledge base's vector store needs, exactly as a client of the
        // published endpoint sends it: no RPC target, at the root of the address the
        // collection published.
        let (status, created) = rest(
            &router,
            &endpoint,
            "PUT",
            &format!("/{}", row.index),
            Some(&knn_index_body(row)),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the index create at the published endpoint to be served, \
             got {status} with {created}"
        );
        assert_eq!(
            at(&created, "acknowledged"),
            json!(true),
            "case {case:?}: expected the create to acknowledge the index, got {created}"
        );
        assert_eq!(
            at(&created, "__type"),
            Value::Null,
            "case {case:?}: expected the published endpoint to serve the REST create rather \
             than refuse it as an RPC request, got {created}"
        );

        // ...and the index really holds what the create carried, whichever form the body took.
        let (status, read) = rest(&router, &endpoint, "GET", &format!("/{}", row.index), None)
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the created index to be readable back, got {status} with \
             {read}"
        );
        assert_eq!(
            at(
                &read,
                &format!("{}.mappings.properties.{VECTOR_FIELD}.dimension", row.index)
            ),
            json!(row.dimension),
            "case {case:?}: expected the index to declare the knn_vector dimension the create \
             carried, got {read}"
        );

        // The knowledge base over that index. 202 Accepted, so ACTIVE is owed by the clock.
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
            "case {case:?}: expected CreateKnowledgeBase over the created index to be \
             accepted, got {status} with {kb}"
        );
        let knowledge_base_id = text(&kb, "knowledgeBase.knowledgeBaseId");
        assert_eq!(
            knowledge_base_id.len(),
            BARE_ID_LENGTH,
            "case {case:?}: expected a minted knowledgeBaseId of the modelled length, got {kb}"
        );
        await_active_knowledge_base(&router, case, &knowledge_base_id).await;

        // A CUSTOM data source and one document pushed straight into it, so the chunks the
        // retrieval reports are chunks an ingestion really recorded in that index.
        let (status, source) = bedrock(
            &router,
            "PUT",
            &format!("/knowledgebases/{knowledge_base_id}/datasources/"),
            Some(&json!({
                "name": format!("{}-pushed", row.knowledge_base),
                "dataSourceConfiguration": { "type": "CUSTOM" },
            })),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected CreateDataSource(CUSTOM) accepted, got {status} with \
             {source}"
        );
        let data_source_id = text(&source, "dataSource.dataSourceId");
        let (status, ingested) = bedrock(
            &router,
            "PUT",
            &format!(
                "/knowledgebases/{knowledge_base_id}/datasources/{data_source_id}/documents"
            ),
            Some(&json!({
                "documents": [inline_byte_document(document_id, document_bytes)],
            })),
        )
        .await;
        assert_eq!(
            status, 202,
            "case {case:?}: expected the ingestion accepted, got {status} with {ingested}"
        );
        await_indexed_document(&router, case, &knowledge_base_id, &data_source_id, document_id)
            .await;

        // The SERVED leg, through both forms of the identifier the model admits.
        let arn = format!("arn:aws:bedrock:{REGION}:{ACCOUNT}:knowledge-base/{knowledge_base_id}");
        for identifier in [knowledge_base_id.clone(), arn] {
            let (status, answer) = retrieve(&router, &identifier, document_bytes).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected the retrieval from {identifier:?} to be served, got \
                 {status} with {answer}"
            );
            let results = at(&answer, "retrievalResults")
                .as_array()
                .cloned()
                .unwrap_or_default();
            let cited = results.iter().find(|result| {
                text(result, "location.customDocumentLocation.id") == document_id
            });
            let cited = cited.unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected the retrieval from {identifier:?} to cite \
                     {document_id:?}, got {answer}"
                )
            });
            assert_eq!(
                text(cited, "content.text"),
                document_bytes,
                "case {case:?}: expected the cited content to be the bytes ingested, got {cited}"
            );
            let score = at(cited, "score").as_f64().unwrap_or(f64::NAN);
            assert!(
                (0.0..=1.0).contains(&score),
                "case {case:?}: expected a relevance score in [0, 1], got {score} in {cited}"
            );
        }

        // ...and the REFUSAL leg: the same request, against identifiers this plane holds no
        // knowledge base for, while the one above is still served.
        for (addressed, identifier) in never_provisioned(&knowledge_base_id) {
            let case = format!("{case}, {addressed}");
            let (status, refused) = retrieve(&router, &identifier, document_bytes).await;
            assert_eq!(
                status, 404,
                "case {case:?}: expected 404 for {identifier:?}, got {status} with {refused}"
            );
            assert_eq!(
                error_code(&refused),
                NOT_FOUND,
                "case {case:?}: expected the modelled not-found error for {identifier:?}, got \
                 {refused}"
            );
            assert_eq!(
                text(&refused, "message"),
                format!("Knowledge Base with id {identifier} does not exist"),
                "case {case:?}: expected the refusal to name the identifier the request \
                 carried, got {refused}"
            );
        }
    }
}
