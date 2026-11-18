// SPDX-License-Identifier: CC0-1.0

//! What `Retrieve` (`POST /knowledgebases/{knowledgeBaseId}/retrieve`) reports for a
//! knowledge base whose vector store is an index of an OpenSearch Serverless COLLECTION:
//! the OpenSearch Serverless data plane, the Bedrock Agent build-time plane and the Bedrock
//! Agent runtime plane, over one engine.
//!
//! RULE: a retrieval reads a knowledge base's chunks out of the store that OWNS the vector
//! index its `storageConfiguration` binds it to, whenever this emulator holds that index —
//! so what a retrieval reports is what that store still holds, a chunk taken out of the
//! index through the collection's own data plane stops being reported, and an index taken
//! out of a collection this emulator holds is reported as the modelled
//! `DependencyFailedException` (424) naming it, never as an empty success. A binding whose
//! collection no service here holds is the one case read from the ingestion's own record of
//! what it took in, because nothing here writes that store: the STORE TYPE alone is
//! therefore never a failed dependency — `OPENSEARCH_SERVERLESS` is the standard store of a
//! vector knowledge base and is served with 200 either way.
//!
//! Three crates have to agree for that to hold — the collection's index and its document
//! surface are served by
//! `service-com-amazonaws-opensearchserverless-opensearchserverless`, the knowledge base and
//! the ingestion that writes the index by the Bedrock Agent build-time plane, the retrieval
//! that reads it by the Bedrock Agent runtime plane — so the suite lives here rather than in
//! any one of them. The sibling suite
//! `bedrock_knowledge_base_over_an_aoss_collection.rs` pins that such a knowledge base can
//! be BUILT and read at all; what is pinned here is WHERE its chunks are read from.
//!
//! The rows span what the rule generalises over: how much of the binding the owning store
//! holds (the index with every chunk, the index with one chunk taken out, the collection
//! without the index, no collection at all) and the bytes the ingested document carries
//! (ASCII and multi-byte UTF-8). Nothing here waits for a fixed time or asserts how long a
//! step took: every wait is for the CONDITION, under a generous deadline.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every call of this suite is signed for.
const REGION: &str = "us-west-2";

/// The OpenSearch Serverless signing name, which routes a control-plane call to that service
/// and is the name a client signs a request to a collection endpoint with, and the
/// `X-Amz-Target` prefix only its CONTROL plane carries.
const AOSS_SIGNING_NAME: &str = "aoss";
const AOSS_TARGET_PREFIX: &str = "OpenSearchServerless";

/// The signing name every Bedrock-family endpoint shares, which routes both Bedrock Agent
/// planes.
const BEDROCK_SIGNING_NAME: &str = "bedrock";

/// The access key id every call here presents. It names no IAM record, which is the position
/// an account's own published credential is in.
const ACCESS_KEY_ID: &str = "AKIDAMBIENT";

/// The IAM role the knowledge bases here act as, and the principal the collection's `data`
/// access policy grants the index permissions to. Matches the modelled
/// `KnowledgeBaseRoleArn` pattern.
const KB_ROLE: &str =
    "arn:aws:iam::000000000000:role/service-role/AmazonBedrockExecutionRoleForKnowledgeBase";

/// The embedding model every knowledge base here is created with, and the number of values
/// its vectors carry — which is the dimension the vector index is created with.
/// `amazon.titan-embed-text-v2:0` publishes 1024-value vectors.
const EMBEDDING_MODEL: &str = "amazon.titan-embed-text-v2:0";
const DIMENSION: usize = 1024;

/// The field names AWS's "Create a vector index in Amazon OpenSearch Service" gives a
/// knowledge base's index, which are what the `fieldMapping` of the storage configuration
/// names and what the index's own `knn_vector` mapping declares.
const VECTOR_FIELD: &str = "bedrock-knowledge-base-default-vector";
const TEXT_FIELD: &str = "AMAZON_BEDROCK_TEXT_CHUNK";
const METADATA_FIELD: &str = "AMAZON_BEDROCK_METADATA";

/// The metadata fields a retrieval result carries, as the published knowledge-base metadata
/// fields name them: the chunk's own id, the data source it came from and the modality of
/// its source file. The provenance that ADDRESSES the document is reported as the result's
/// `location` instead, so it is not among them.
const RESULT_METADATA_KEYS: [&str; 3] = [
    "x-amz-bedrock-kb-chunk-id",
    "x-amz-bedrock-kb-data-source-id",
    "x-amz-bedrock-kb-source-file-modality",
];

/// The terminal `DocumentStatus` of a document the plane has finished ingesting, which the
/// modelled member documents as the successfully indexed state.
const INDEXED: &str = "INDEXED";

/// How many times a read looks again for a transition the clock owes, and how long one look
/// waits before the next. Generous: every wait here is for the CONDITION and nothing
/// asserts how long it took.
const SETTLE_ATTEMPTS: usize = 400;
const SETTLE_PAUSE: Duration = Duration::from_millis(25);

/// The three services this rule spans, over ONE engine, as `core/router/src/lib.rs` mounts
/// them. Request authentication is off, as in every other suite here: what each call carries
/// is a credential scope, which is what routes it and what each handler reads its Region and
/// its caller from.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_opensearchserverless_opensearchserverless::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_bedrockagent_amazonbedrockagentbuildtimelambda::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_bedrockagentruntime_amazonbedrockagentruntimeservice::EmulatedService::new(
            engine,
        ),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name` in [`REGION`].
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={ACCESS_KEY_ID}/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// Dispatch one request and parse its JSON answer.
async fn dispatch(router: &MultiServiceRouter, request: http::Request<Bytes>) -> (u16, Value) {
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// One OpenSearch Serverless CONTROL-plane call, over the `awsJson1_0` wire its model
/// declares.
async fn aoss(router: &MultiServiceRouter, operation: &str, body: Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{AOSS_SIGNING_NAME}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("{AOSS_TARGET_PREFIX}.{operation}"))
        .header("authorization", authorization(AOSS_SIGNING_NAME))
        .body(Bytes::from(
            serde_json::to_vec(&body).expect("the request body serializes"),
        ))
        .expect("a well-formed control-plane request");
    dispatch(router, request).await
}

/// One OpenSearch Serverless control-plane call that must be served.
async fn aoss_ok(router: &MultiServiceRouter, operation: &str, body: Value) -> Value {
    let (status, answered) = aoss(router, operation, body).await;
    assert_eq!(
        status, 200,
        "setup: expected {operation} to be served, got {status} with {answered}"
    );
    answered
}

/// The authority a published endpoint is dialled at and the path prefix it carries, so a
/// REST request is addressed exactly where the collection said to address it — whether the
/// published address names the collection in its HOST or in its PATH.
fn addressed(endpoint: &str) -> (String, String) {
    let after_scheme = endpoint
        .strip_prefix("https://")
        .or_else(|| endpoint.strip_prefix("http://"))
        .unwrap_or(endpoint);
    match after_scheme.find('/') {
        Some(at) => (
            after_scheme[..at].to_string(),
            after_scheme[at..].trim_end_matches('/').to_string(),
        ),
        None => (after_scheme.to_string(), String::new()),
    }
}

/// One OpenSearch REST request a client sends to a collection's published endpoint, as
/// `opensearch-py` sends it: addressed to the authority the endpoint names, signed for the
/// `aoss` signing name, and carrying NO `x-amz-target`, because the OpenSearch REST API has
/// no RPC target. `body` is sent verbatim, so an NDJSON bulk body travels as written.
async fn rest(
    router: &MultiServiceRouter,
    endpoint: &str,
    method: &str,
    path: &str,
    body: &str,
) -> (u16, Value) {
    let (authority, prefix) = addressed(endpoint);
    let request = http::Request::builder()
        .method(method)
        .uri(format!("{prefix}{path}"))
        .header("host", authority)
        .header("content-type", "application/json")
        .header("authorization", authorization(AOSS_SIGNING_NAME))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed data-plane request");
    dispatch(router, request).await
}

/// One Bedrock call, over the `restJson1` wire both Bedrock Agent planes declare, on the
/// method and URI the operation's own `@http` binding names.
async fn bedrock(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("bedrock.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization(BEDROCK_SIGNING_NAME))
        .body(
            body.map(|body| {
                Bytes::from(serde_json::to_vec(body).expect("the request body serializes"))
            })
            .unwrap_or_default(),
        )
        .expect("a well-formed Bedrock request");
    dispatch(router, request).await
}

/// The value at a dotted path of a JSON document, or `Value::Null` where there is none.
fn at(document: &Value, path: &str) -> Value {
    let mut here = document;
    for segment in path.split('.') {
        match here.get(segment) {
            Some(next) => here = next,
            None => return Value::Null,
        }
    }
    here.clone()
}

/// The string at a dotted path, or the empty string where there is none.
fn text(document: &Value, path: &str) -> String {
    at(document, path).as_str().unwrap_or_default().to_string()
}

/// The `encryption` and `network` security policies a collection cannot be created or
/// reached without, and the `data` access policy granting the knowledge base's own role
/// every `aoss` permission on `collection` and its indexes — the grant AWS's "Create a
/// vector index in Amazon OpenSearch Service" has a caller make for the knowledge-base role,
/// and what `CreateKnowledgeBase` resolves the binding against.
async fn collection_policies(router: &MultiServiceRouter, collection: &str) {
    aoss_ok(
        router,
        "CreateSecurityPolicy",
        json!({
            "type": "encryption",
            "name": format!("{collection}-enc"),
            "policy": json!({
                "Rules": [{ "ResourceType": "collection", "Resource": ["collection/*"] }],
                "AWSOwnedKey": true,
            })
            .to_string(),
        }),
    )
    .await;
    aoss_ok(
        router,
        "CreateSecurityPolicy",
        json!({
            "type": "network",
            "name": format!("{collection}-net"),
            "policy": json!({
                "Rules": [{ "ResourceType": "collection", "Resource": ["collection/*"] }],
                "AllowFromPublic": true,
            })
            .to_string(),
        }),
    )
    .await;
    aoss_ok(
        router,
        "CreateAccessPolicy",
        json!({
            "type": "data",
            "name": format!("{collection}-data"),
            "policy": json!([{
                "Rules": [
                    {
                        "ResourceType": "index",
                        "Resource": [format!("index/{collection}/*")],
                        "Permission": ["aoss:*"],
                    },
                    {
                        "ResourceType": "collection",
                        "Resource": [format!("collection/{collection}")],
                        "Permission": ["aoss:*"],
                    },
                ],
                "Principal": [KB_ROLE],
            }])
            .to_string(),
        }),
    )
    .await;
}

/// Create a `VECTORSEARCH` collection, poll until it reports `ACTIVE`, and answer its ARN
/// beside the endpoint it publishes.
async fn active_collection(router: &MultiServiceRouter, name: &str) -> (String, String) {
    let created = aoss_ok(
        router,
        "CreateCollection",
        json!({ "name": name, "type": "VECTORSEARCH" }),
    )
    .await;
    let id = text(&created, "createCollectionDetail.id");
    assert!(
        !id.is_empty(),
        "setup: expected CreateCollection({name}) to mint an id, got {created}"
    );
    // The `CREATING -> ACTIVE` move is owed by the clock, so this looks again until the
    // collection reports ACTIVE or a generous deadline passes.
    for _ in 0..SETTLE_ATTEMPTS {
        let read = aoss_ok(router, "BatchGetCollection", json!({ "ids": [&id] })).await;
        let detail = at(&read, "collectionDetails")
            .as_array()
            .and_then(|details| details.first())
            .cloned()
            .unwrap_or(Value::Null);
        if text(&detail, "status") == "ACTIVE" {
            let arn = text(&detail, "arn");
            let endpoint = text(&detail, "collectionEndpoint");
            assert!(
                !arn.is_empty() && !endpoint.is_empty(),
                "setup: expected an ACTIVE collection to publish an ARN and an endpoint, got \
                 {detail}"
            );
            return (arn, endpoint);
        }
        tokio::time::sleep(SETTLE_PAUSE).await;
    }
    panic!("setup: the collection {name} never reached ACTIVE");
}

/// Create the vector index at the collection's published endpoint, with the `knn_vector`
/// mapping a knowledge base's index declares. `method` is the mapping's own `method` object
/// when the caller declares one, which is where a `space_type` is named.
async fn vector_index_with(
    router: &MultiServiceRouter,
    endpoint: &str,
    index: &str,
    method: Option<Value>,
) {
    let mut vector_mapping = json!({ "type": "knn_vector", "dimension": DIMENSION });
    if let (Some(method), Value::Object(mapping)) = (method, &mut vector_mapping) {
        mapping.insert("method".to_string(), method);
    }
    let body = json!({
        "settings": { "index": { "knn": true } },
        "mappings": { "properties": {
            VECTOR_FIELD: vector_mapping,
            TEXT_FIELD: { "type": "text" },
            METADATA_FIELD: { "type": "text", "index": false },
        } },
    });
    let (status, created) =
        rest(router, endpoint, "PUT", &format!("/{index}"), &body.to_string()).await;
    assert_eq!(
        status, 200,
        "setup: expected the index create at the published endpoint to be served, got {status} \
         with {created}"
    );
}

/// [`vector_index_with`], for an index whose mapping declares no `method` of its own.
async fn vector_index(router: &MultiServiceRouter, endpoint: &str, index: &str) {
    vector_index_with(router, endpoint, index, None).await;
}

/// The `storageConfiguration` of a knowledge base whose vectors live in `index` of the
/// collection `collection_arn` names.
fn opensearch_storage(collection_arn: &str, index: &str) -> Value {
    json!({
        "type": "OPENSEARCH_SERVERLESS",
        "opensearchServerlessConfiguration": {
            "collectionArn": collection_arn,
            "vectorIndexName": index,
            "fieldMapping": {
                "vectorField": VECTOR_FIELD,
                "textField": TEXT_FIELD,
                "metadataField": METADATA_FIELD,
            },
        },
    })
}

/// Create a knowledge base over `storage`, poll until it reports `ACTIVE`, and answer its
/// minted id.
async fn knowledge_base(router: &MultiServiceRouter, name: &str, storage: Value) -> String {
    let (status, created) = bedrock(
        router,
        "PUT",
        "/knowledgebases/",
        Some(&json!({
            "name": name,
            "roleArn": KB_ROLE,
            "knowledgeBaseConfiguration": {
                "type": "VECTOR",
                "vectorKnowledgeBaseConfiguration": {
                    "embeddingModelArn":
                        format!("arn:aws:bedrock:{REGION}::foundation-model/{EMBEDDING_MODEL}"),
                },
            },
            "storageConfiguration": storage,
        })),
    )
    .await;
    assert_eq!(
        status, 202,
        "setup: expected CreateKnowledgeBase({name}) accepted, got {status} with {created}"
    );
    let id = text(&created, "knowledgeBase.knowledgeBaseId");
    assert!(
        !id.is_empty(),
        "setup: expected a minted knowledgeBaseId, got {created}"
    );
    for _ in 0..SETTLE_ATTEMPTS {
        let (status, read) = bedrock(router, "GET", &format!("/knowledgebases/{id}"), None).await;
        assert_eq!(
            status, 200,
            "setup: expected GetKnowledgeBase({id}) to answer, got {status} with {read}"
        );
        if text(&read, "knowledgeBase.status") == "ACTIVE" {
            return id;
        }
        tokio::time::sleep(SETTLE_PAUSE).await;
    }
    panic!("setup: the knowledge base {name} never reached ACTIVE");
}

/// Attach a CUSTOM data source — the kind documents are pushed into rather than crawled from
/// a source of its own — and answer its minted id.
async fn custom_data_source(router: &MultiServiceRouter, knowledge_base_id: &str) -> String {
    let (status, created) = bedrock(
        router,
        "PUT",
        &format!("/knowledgebases/{knowledge_base_id}/datasources/"),
        Some(&json!({
            "name": "pushed",
            "dataSourceConfiguration": { "type": "CUSTOM" },
        })),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: expected CreateDataSource(CUSTOM) accepted, got {status} with {created}"
    );
    let id = text(&created, "dataSource.dataSourceId");
    assert!(
        !id.is_empty(),
        "setup: expected a minted dataSourceId, got {created}"
    );
    id
}

/// A custom document whose content is defined inline IN BYTES, which is how a client pushes
/// bytes it holds straight into a knowledge base: `data` travels base64-encoded, as the
/// modelled `ByteContentDoc$data` blob does.
fn inline_byte_document(id: &str, bytes: &str) -> Value {
    json!({
        "content": {
            "dataSourceType": "CUSTOM",
            "custom": {
                "customDocumentIdentifier": { "id": id },
                "sourceType": "IN_LINE",
                "inlineContent": {
                    "type": "BYTE",
                    "byteContent": {
                        "mimeType": "text/plain",
                        "data": ::aws_smithy_types::base64::encode(bytes),
                    },
                },
            },
        },
    })
}

/// Push `documents` into `data_source_id` and look again until every one of them reports
/// [`INDEXED`] — the CONDITION the retrievals below are about, rather than a fixed wait.
async fn ingest(
    router: &MultiServiceRouter,
    knowledge_base_id: &str,
    data_source_id: &str,
    documents: &[(&str, &str)],
) {
    let (status, accepted) = bedrock(
        router,
        "PUT",
        &format!("/knowledgebases/{knowledge_base_id}/datasources/{data_source_id}/documents"),
        Some(&json!({
            "documents": documents
                .iter()
                .map(|(id, bytes)| inline_byte_document(id, bytes))
                .collect::<Vec<Value>>(),
        })),
    )
    .await;
    assert_eq!(
        status, 202,
        "setup: expected the ingestion accepted, got {status} with {accepted}"
    );
    let uri = format!(
        "/knowledgebases/{knowledge_base_id}/datasources/{data_source_id}/documents/getDocuments"
    );
    let body = json!({
        "documentIdentifiers": documents
            .iter()
            .map(|(id, _)| json!({ "dataSourceType": "CUSTOM", "custom": { "id": id } }))
            .collect::<Vec<Value>>(),
    });
    for _ in 0..SETTLE_ATTEMPTS {
        let (status, read) = bedrock(router, "POST", &uri, Some(&body)).await;
        assert_eq!(
            status, 200,
            "setup: expected GetKnowledgeBaseDocuments to answer, got {status} with {read}"
        );
        let details = at(&read, "documentDetails");
        let details = details.as_array().cloned().unwrap_or_default();
        if details.len() == documents.len()
            && details
                .iter()
                .all(|detail| text(detail, "status") == INDEXED)
        {
            return;
        }
        tokio::time::sleep(SETTLE_PAUSE).await;
    }
    panic!("setup: not every pushed document reached {INDEXED}");
}

/// `Retrieve` over one knowledge base, answering the raw `(status, body)`.
async fn retrieve(
    router: &MultiServiceRouter,
    knowledge_base_id: &str,
    query: &str,
) -> (u16, Value) {
    bedrock(
        router,
        "POST",
        &format!("/knowledgebases/{knowledge_base_id}/retrieve"),
        Some(&json!({
            "retrievalQuery": { "text": query },
            "retrievalConfiguration": { "vectorSearchConfiguration": { "numberOfResults": 10 } },
        })),
    )
    .await
}

/// The `retrievalResults` of a retrieval that must be served.
async fn results(
    router: &MultiServiceRouter,
    case: &str,
    knowledge_base_id: &str,
    query: &str,
) -> Vec<Value> {
    let (status, answered) = retrieve(router, knowledge_base_id, query).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the retrieval to be served, got {status} with {answered}"
    );
    at(&answered, "retrievalResults")
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// The document ids a retrieval cites, as the custom document identifiers the ingestion gave
/// them.
fn cited(results: &[Value]) -> BTreeSet<String> {
    results
        .iter()
        .map(|result| text(result, "location.customDocumentLocation.id"))
        .collect()
}

/// The one result citing `id`, asserting exactly one does.
fn result_for<'results>(case: &str, results: &'results [Value], id: &str) -> &'results Value {
    let found: Vec<&Value> = results
        .iter()
        .filter(|result| text(result, "location.customDocumentLocation.id") == id)
        .collect();
    assert_eq!(
        found.len(),
        1,
        "case {case:?}: expected exactly one result citing {id:?}, got {results:?}"
    );
    found[0]
}

/// The documents the index `index` of the collection at `endpoint` holds, as a client's own
/// `_search` reports them: the id each is held under beside the raw text it carries.
async fn indexed_documents(
    router: &MultiServiceRouter,
    case: &str,
    endpoint: &str,
    index: &str,
) -> Vec<(String, String)> {
    let body = json!({ "query": { "match_all": {} }, "size": 50 });
    let (status, answered) = rest(
        router,
        endpoint,
        "POST",
        &format!("/{index}/_search"),
        &body.to_string(),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the collection's own search to be served, got {status} with \
         {answered}"
    );
    at(&answered, "hits.hits")
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|hit| {
            (
                text(hit, "_id"),
                text(hit, &format!("_source.{TEXT_FIELD}")),
            )
        })
        .collect()
}

/// RULE: the chunks a retrieval reports are the ones the bound collection index HOLDS — the
/// ingestion writes them there, a client's own search finds them there, and a chunk taken
/// out of that index through the collection's own bulk API stops being reported while its
/// siblings are untouched.
///
/// The documents span the bytes the rule generalises over: ASCII, and multi-byte UTF-8 in
/// several scripts including a character outside the basic multilingual plane.
#[tokio::test]
async fn a_retrieval_reports_the_chunks_the_bound_collection_index_holds() {
    let case = "the bound index holds every chunk";
    let router = build_router();
    let collection = "kb-bound-index";
    let index = "bedrock-knowledge-base-bound-index";
    collection_policies(&router, collection).await;
    let (collection_arn, endpoint) = active_collection(&router, collection).await;
    vector_index(&router, &endpoint, index).await;

    let documents: Vec<(&str, &str)> = vec![
        ("manifest", "Manifest: the klystron bay holds forty spare units."),
        (
            "dossier",
            "Dossier: Ωμέγα — 四十の導波管 — ударная секция — 🛰 orbiting relay.",
        ),
        ("ledger", "Ledger: the propulsion team drew nine waveguides."),
    ];
    let knowledge_base_id = knowledge_base(
        &router,
        "bound-index-kb",
        opensearch_storage(&collection_arn, index),
    )
    .await;
    let data_source_id = custom_data_source(&router, &knowledge_base_id).await;
    ingest(&router, &knowledge_base_id, &data_source_id, &documents).await;

    // The chunks are in the COLLECTION's own index: what the retrieval below reports is
    // backed by the store that owns them, as that store's own search reports it.
    let held = indexed_documents(&router, case, &endpoint, index).await;
    let indexed_text: BTreeSet<String> = held.iter().map(|(_, text)| text.clone()).collect();
    assert_eq!(
        indexed_text,
        documents
            .iter()
            .map(|(_, bytes)| (*bytes).to_string())
            .collect::<BTreeSet<String>>(),
        "case {case:?}: expected the ingestion to have written every chunk into the bound \
         index, got {held:?}"
    );

    let answered = results(&router, case, &knowledge_base_id, "waveguide relay").await;
    assert_eq!(
        cited(&answered),
        documents
            .iter()
            .map(|(id, _)| (*id).to_string())
            .collect::<BTreeSet<String>>(),
        "case {case:?}: expected every ingested document to be retrievable, got {answered:?}"
    );
    for (id, bytes) in &documents {
        let result = result_for(case, &answered, id);
        assert_eq!(
            text(result, "content.text"),
            *bytes,
            "case {case:?}: expected the bytes the ingestion took in for {id:?}, got {result:?}"
        );
        assert_eq!(
            text(result, "content.type"),
            "TEXT",
            "case {case:?}: expected the chunk of {id:?} reported as text, got {result:?}"
        );
        assert_eq!(
            text(result, "location.type"),
            "CUSTOM",
            "case {case:?}: expected a document of a custom data source cited as CUSTOM, got \
             {result:?}"
        );
        let metadata = at(result, "metadata");
        let keys: BTreeSet<String> = metadata
            .as_object()
            .map(|members| members.keys().cloned().collect())
            .unwrap_or_default();
        assert_eq!(
            keys,
            RESULT_METADATA_KEYS
                .iter()
                .map(|key| (*key).to_string())
                .collect::<BTreeSet<String>>(),
            "case {case:?}: expected the published chunk metadata fields for {id:?}, got \
             {metadata:?}"
        );
        assert_eq!(
            text(&metadata, "x-amz-bedrock-kb-data-source-id"),
            data_source_id,
            "case {case:?}: expected the data source {id:?} was ingested into, got {metadata:?}"
        );
    }

    // One chunk taken out of the index through the collection's OWN bulk API: the knowledge
    // base no longer holds it, so no retrieval reports it — and nothing else moves.
    let removed = "dossier";
    let removed_text = documents
        .iter()
        .find(|(id, _)| *id == removed)
        .map(|(_, bytes)| (*bytes).to_string())
        .expect("the removed document is one of the rows");
    let key = held
        .iter()
        .find(|(_, text)| *text == removed_text)
        .map(|(key, _)| key.clone())
        .unwrap_or_else(|| panic!("case {case:?}: the bound index holds no chunk of {removed:?}"));
    let body = format!("{}\n", json!({ "delete": { "_id": key } }));
    let (status, deleted) = rest(
        &router,
        &endpoint,
        "POST",
        &format!("/{index}/_bulk"),
        &body,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the bulk delete to be served, got {status} with {deleted}"
    );
    assert_eq!(
        at(&deleted, "errors"),
        json!(false),
        "case {case:?}: expected the bulk delete to take the chunk, got {deleted}"
    );

    let after = "one chunk is gone from the bound index";
    let answered = results(&router, after, &knowledge_base_id, "waveguide relay").await;
    assert_eq!(
        cited(&answered),
        documents
            .iter()
            .map(|(id, _)| (*id).to_string())
            .filter(|id| id != removed)
            .collect::<BTreeSet<String>>(),
        "case {after:?}: expected only the chunks the index still holds, got {answered:?}"
    );
    for (id, bytes) in documents.iter().filter(|(id, _)| *id != removed) {
        let result = result_for(after, &answered, id);
        assert_eq!(
            text(result, "content.text"),
            *bytes,
            "case {after:?}: expected {id:?} untouched by the removal of {removed:?}, got \
             {result:?}"
        );
    }
}

/// RULE, the refusal: a knowledge base bound to an index its collection no longer holds has
/// its content nowhere this plane can read, which is the modelled `DependencyFailedException`
/// (424) naming that index — not an empty success, because "nothing to read it from" is not
/// "nothing was ingested".
#[tokio::test]
async fn a_knowledge_base_whose_bound_index_is_gone_is_refused() {
    let router = build_router();
    let collection = "kb-vanishing-index";
    let index = "bedrock-knowledge-base-vanishing-index";
    collection_policies(&router, collection).await;
    let (collection_arn, endpoint) = active_collection(&router, collection).await;
    vector_index(&router, &endpoint, index).await;
    let knowledge_base_id = knowledge_base(
        &router,
        "vanishing-index-kb",
        opensearch_storage(&collection_arn, index),
    )
    .await;
    let data_source_id = custom_data_source(&router, &knowledge_base_id).await;
    let documents: Vec<(&str, &str)> =
        vec![("manifest", "Manifest: the klystron bay holds forty units.")];
    ingest(&router, &knowledge_base_id, &data_source_id, &documents).await;

    // While the index is there the chunk it holds is retrievable: that is what makes the
    // refusal below about the store and not about the knowledge base.
    let served = results(&router, "the index is there", &knowledge_base_id, "klystron").await;
    assert_eq!(
        served.len(),
        1,
        "case \"the index is there\": expected the ingested document retrievable, got {served:?}"
    );

    let (status, dropped) = rest(&router, &endpoint, "DELETE", &format!("/{index}"), "").await;
    assert_eq!(
        status, 200,
        "setup: expected the index delete at the published endpoint served, got {status} with \
         {dropped}"
    );

    let case = "the bound index is gone from a collection this emulator holds";
    let (status, body) = retrieve(&router, &knowledge_base_id, "klystron").await;
    assert_eq!(
        status, 424,
        "case {case:?}: expected the modelled 424, got {status} with {body}"
    );
    assert_eq!(
        text(&body, "__type"),
        "DependencyFailedException",
        "case {case:?}: expected DependencyFailedException, got {body}"
    );
    let named = format!("{collection_arn}/{index}");
    assert_eq!(
        text(&body, "resourceName"),
        named,
        "case {case:?}: expected the refusal to name the index the knowledge base is bound to, \
         got {body}"
    );
    assert!(
        text(&body, "message").contains(&named),
        "case {case:?}: expected the message to name that index too, got {body}"
    );
}

/// RULE, the other side: a binding whose collection no service of this emulator holds names a
/// vector store living OUTSIDE it — nothing here wrote that index — so the chunks come from
/// the ingestion's own record and the retrieval is served. The store TYPE is never itself a
/// failed dependency: `OPENSEARCH_SERVERLESS` answers 200 whether or not this emulator holds
/// the collection.
#[tokio::test]
async fn a_knowledge_base_over_a_collection_this_emulator_does_not_hold_is_still_retrieved() {
    let case = "the collection lives outside this emulator";
    let router = build_router();
    // A collection ARN matching the modelled `OpenSearchServerlessCollectionArn` pattern
    // that no `CreateCollection` of this engine ever minted.
    let collection_arn = format!("arn:aws:aoss:{REGION}:000000000000:collection/absent-kb-store");
    let knowledge_base_id = knowledge_base(
        &router,
        "external-store-kb",
        opensearch_storage(&collection_arn, "bedrock-knowledge-base-absent-index"),
    )
    .await;
    let data_source_id = custom_data_source(&router, &knowledge_base_id).await;
    let documents: Vec<(&str, &str)> = vec![
        ("manifest", "Manifest: the klystron bay holds forty units."),
        ("dossier", "Dossier: Ωμέγα — 四十の導波管 — 🛰 relay."),
    ];
    ingest(&router, &knowledge_base_id, &data_source_id, &documents).await;

    let answered = results(&router, case, &knowledge_base_id, "klystron relay").await;
    assert_eq!(
        cited(&answered),
        documents
            .iter()
            .map(|(id, _)| (*id).to_string())
            .collect::<BTreeSet<String>>(),
        "case {case:?}: expected every ingested document to be retrievable, got {answered:?}"
    );
    for (id, bytes) in &documents {
        let result = result_for(case, &answered, id);
        assert_eq!(
            text(result, "content.text"),
            *bytes,
            "case {case:?}: expected the bytes the ingestion took in for {id:?}, got {result:?}"
        );
    }
}

/// RULE, whatever the index declares: the chunks are read out of the bound index whichever
/// `space_type` its own `knn_vector` mapping names. That mapping is what a CLIENT's own `knn`
/// query at the collection endpoint is scored under; it does not decide whether the knowledge
/// base's chunks can be read there, and the relevance a retrieval reports is the one metric
/// this emulator ranks a knowledge base's chunks under wherever they live — so the nearest
/// result for a query that IS a document's own text is that document.
#[tokio::test]
async fn a_bound_index_declaring_its_own_space_type_is_read_there_too() {
    let case = "the bound index declares space_type l2";
    let router = build_router();
    let collection = "kb-l2-index";
    let index = "bedrock-knowledge-base-l2-index";
    collection_policies(&router, collection).await;
    let (collection_arn, endpoint) = active_collection(&router, collection).await;
    // The `method` a caller declares for a knowledge base's vector field, naming one of the
    // space types OpenSearch's k-NN plugin defines beside the default.
    vector_index_with(
        &router,
        &endpoint,
        index,
        Some(json!({ "name": "hnsw", "engine": "faiss", "space_type": "l2" })),
    )
    .await;
    let documents: Vec<(&str, &str)> = vec![
        (
            "manifest",
            "Manifest: the klystron bay holds forty spare units.",
        ),
        ("ledger", "Ledger: Ωμέγα drew nine 導波管 for the 🛰 relay."),
    ];
    let knowledge_base_id = knowledge_base(
        &router,
        "l2-index-kb",
        opensearch_storage(&collection_arn, index),
    )
    .await;
    let data_source_id = custom_data_source(&router, &knowledge_base_id).await;
    ingest(&router, &knowledge_base_id, &data_source_id, &documents).await;

    let every_document: BTreeSet<String> = documents
        .iter()
        .map(|(id, _)| (*id).to_string())
        .collect();
    for (id, bytes) in &documents {
        let answered = results(&router, case, &knowledge_base_id, bytes).await;
        assert_eq!(
            cited(&answered),
            every_document,
            "case {case:?}: expected every ingested document to be retrievable, got {answered:?}"
        );
        let nearest = answered
            .first()
            .unwrap_or_else(|| panic!("case {case:?}: expected a retrieval result, got nothing"));
        assert_eq!(
            text(nearest, "location.customDocumentLocation.id"),
            *id,
            "case {case:?}: expected the document whose own text was queried reported first, \
             got {answered:?}"
        );
        assert_eq!(
            text(result_for(case, &answered, id), "content.text"),
            *bytes,
            "case {case:?}: expected the bytes the ingestion took in for {id:?}, got \
             {answered:?}"
        );
    }

    // And those chunks really are read THERE: one taken out of the index stops being reported.
    let held = indexed_documents(&router, case, &endpoint, index).await;
    let (key, _) = held
        .iter()
        .find(|(_, held_text)| held_text == documents[0].1)
        .cloned()
        .unwrap_or_else(|| {
            panic!("case {case:?}: the bound index holds no chunk of the first document")
        });
    let (status, deleted) = rest(
        &router,
        &endpoint,
        "POST",
        &format!("/{index}/_bulk"),
        &format!("{}\n", json!({ "delete": { "_id": key } })),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the bulk delete to be served, got {status} with {deleted}"
    );
    let answered = results(&router, case, &knowledge_base_id, documents[0].1).await;
    assert_eq!(
        cited(&answered),
        BTreeSet::from([documents[1].0.to_string()]),
        "case {case:?}: expected only the chunk the index still holds, got {answered:?}"
    );
}
