// SPDX-License-Identifier: CC0-1.0

//! What a Bedrock Agent ingestion writes into its knowledge base's OpenSearch Serverless
//! collection, read back at the address that collection PUBLISHES.
//!
//! RULE: a knowledge base keeps its chunks in the store its `storageConfiguration` names, so
//! every document an ingestion job reports indexed is backed by a document the OWNING store
//! answers for from its own read — and for a collection that read is the OpenSearch REST
//! surface served at the address it publishes as `collectionEndpoint`, "the
//! collection-specific endpoint used to submit index, search, and data upload requests"
//! (`CollectionDetail$collectionEndpoint`), which names no RPC target.
//!
//! What this suite adds to the crate-local one is the ADDRESS: both the index create a caller
//! makes before the knowledge base can name the index and the read-back of what the ingestion
//! wrote go to the host the collection published, which is the address a stock OpenSearch
//! client is configured with. Three crates have to agree for that to hold (Amazon S3 holds the
//! source documents, Amazon Bedrock Agent runs the job, the collection holds the documents and
//! answers for them), so the suite lives in this cross-service target. The rest of the rule —
//! which fields the chunk is written into, how many values it carries, what the `data` access
//! policies decide about the knowledge base's role, and what a data source's
//! `dataDeletionPolicy` does to those documents — is a table in that crate's own suite,
//! `services/service-com-amazonaws-bedrockagent-amazonbedrockagentbuildtimelambda/tests/all/\
//! ingestion_writes_the_bound_collection_index.rs`, and the collection's REST surface itself is
//! `services/service-com-amazonaws-opensearchserverless-opensearchserverless/tests/all/\
//! collection_data_plane.rs`.
//!
//! Every call goes over the wire of the service that owns it, and every wait is a poll for the
//! CONDITION under a deadline generous enough for any host; nothing asserts how long a step
//! took.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every call of this suite is signed for.
const REGION: &str = "us-east-1";

/// The access key id every call presents. It names no IAM record, which is the position an
/// account's own published credential is in: the collection's data access policies do not
/// bound it, so the index create a caller makes before any knowledge base exists is decided by
/// the network policy alone.
const ACCESS_KEY_ID: &str = "AKIDAMBIENT";

/// The IAM role the knowledge base acts as, and the principal the collection's `data` access
/// policy grants the index permissions to. Matches the modelled `KnowledgeBaseRoleArn` pattern.
const KB_ROLE: &str =
    "arn:aws:iam::000000000000:role/service-role/AmazonBedrockExecutionRoleForKnowledgeBase";

/// The `X-Amz-Target` prefix the OpenSearch Serverless CONTROL plane routes on, from that
/// crate's `OPERATIONS.md` wire-protocol section. The collection's REST data plane carries no
/// such header.
const AOSS_TARGET_PREFIX: &str = "OpenSearchServerless";

/// The field names AWS's "Create a vector index in Amazon OpenSearch Service" gives a
/// knowledge base's index, which are what the storage configuration's `fieldMapping` names and
/// what the index's own mapping declares.
const VECTOR_FIELD: &str = "bedrock-knowledge-base-default-vector";
const TEXT_FIELD: &str = "AMAZON_BEDROCK_TEXT_CHUNK";
const METADATA_FIELD: &str = "AMAZON_BEDROCK_METADATA";

/// The metadata field a knowledge base records a chunk's source location under, as the
/// published knowledge-base metadata fields name it.
const SOURCE_URI_METADATA_KEY: &str = "x-amz-bedrock-kb-source-uri";

/// The embedding model the knowledge base declares and the number of values the index is
/// created with: Titan Embeddings G1 — Text publishes 1536-value vectors.
const EMBEDDING_MODEL: &str = "amazon.titan-embed-text-v1";
const DIMENSION: usize = 1536;

/// How long a poll waits for a transition the clock owes: many times the spans the owning
/// services owe, so a loaded machine still converges, and bounded so a hung poll fails this
/// test rather than hanging the suite.
const POLL_CAP: Duration = Duration::from_secs(60);

/// How long one poll attempt waits before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The three services this rule spans, over ONE engine, as `core/router/src/lib.rs` mounts
/// them. Request authentication is off, as in every other suite here: what each call carries is
/// a credential scope, which is what routes it and what each handler reads its Region and its
/// caller from.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_opensearchserverless_opensearchserverless::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_bedrockagent_amazonbedrockagentbuildtimelambda::EmulatedService::new(
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

/// One OpenSearch Serverless CONTROL-plane call, over the `awsJson1_0` wire its model declares.
async fn aoss(router: &MultiServiceRouter, operation: &str, body: Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("aoss.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("{AOSS_TARGET_PREFIX}.{operation}"))
        .header("authorization", authorization("aoss"))
        .body(Bytes::from(
            serde_json::to_vec(&body).expect("the request body serializes"),
        ))
        .expect("a well-formed control-plane request");
    dispatch(router, request).await
}

/// One OpenSearch REST request a client sends to a collection's PUBLISHED endpoint, as
/// `opensearch-py` sends it: addressed to the authority that endpoint names, signed for the
/// `aoss` signing name, and carrying NO `x-amz-target`.
async fn rest(
    router: &MultiServiceRouter,
    endpoint: &str,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let (authority, prefix) = addressed(endpoint);
    let request = http::Request::builder()
        .method(method)
        .uri(format!("{prefix}{path}"))
        .header("host", authority)
        .header("content-type", "application/json")
        .header("authorization", authorization("aoss"))
        .body(Bytes::from(
            body.map(|body| body.to_string()).unwrap_or_default(),
        ))
        .expect("a well-formed data-plane request");
    dispatch(router, request).await
}

/// The authority a published endpoint is dialled at and the path prefix it carries, so a REST
/// request is addressed exactly where the collection said to address it — whether the published
/// address names the collection in its HOST or in its PATH.
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

/// One Amazon Bedrock Agent call, over the `restJson1` wire its model declares, on the method
/// and URI the operation's own `@http` binding names.
async fn bedrock(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let payload = body
        .map(|value| Bytes::from(serde_json::to_vec(&value).expect("the request body serializes")))
        .unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("bedrock.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("bedrock"))
        .body(payload)
        .expect("a well-formed request");
    dispatch(router, request).await
}

/// One request against Amazon S3's REST surface, path-style; answers the status alone.
async fn s3(router: &MultiServiceRouter, method: &str, path: &str, body: &str) -> u16 {
    let host = format!("s3.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host.clone())
        .header("authorization", authorization("s3"))
        .header("content-type", "application/octet-stream")
        .body(Bytes::from(body.to_owned()))
        .expect("a well-formed request");
    router.dispatch(request).await.status().as_u16()
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

/// The `encryption` and `network` security policies a collection needs, and the `data` access
/// policy that grants the knowledge base's own role the index permissions a knowledge base
/// performs.
async fn admit_collection(router: &MultiServiceRouter, case: &str, collection: &str) {
    for (operation, body) in [
        (
            "CreateSecurityPolicy",
            json!({
                "type": "encryption",
                "name": "enc-every-collection",
                "policy": json!({
                    "Rules": [{ "ResourceType": "collection", "Resource": ["collection/*"] }],
                    "AWSOwnedKey": true,
                })
                .to_string(),
            }),
        ),
        (
            "CreateSecurityPolicy",
            json!({
                "type": "network",
                "name": "net-every-collection",
                "policy": json!({
                    "Rules": [{ "ResourceType": "collection", "Resource": ["collection/*"] }],
                    "AllowFromPublic": true,
                })
                .to_string(),
            }),
        ),
        (
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
        ),
    ] {
        let (status, answer) = aoss(router, operation, body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected {operation} accepted, got {status} with {answer}"
        );
    }
}

/// Create a `VECTORSEARCH` collection and poll until it reports `ACTIVE`, answering the address
/// it publishes and its ARN.
async fn active_collection(router: &MultiServiceRouter, case: &str, name: &str) -> (String, String) {
    let (status, created) = aoss(
        router,
        "CreateCollection",
        json!({ "name": name, "type": "VECTORSEARCH" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected CreateCollection({name}) accepted, got {status} with {created}"
    );
    let id = text(&created, "createCollectionDetail.id");
    let deadline = Instant::now() + POLL_CAP;
    loop {
        let (status, read) = aoss(router, "BatchGetCollection", json!({ "ids": [&id] })).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected BatchGetCollection({id}) to answer, got {status} with {read}"
        );
        let detail = at(&read, "collectionDetails")
            .as_array()
            .and_then(|details| details.first())
            .cloned()
            .unwrap_or(Value::Null);
        if text(&detail, "status") == "ACTIVE" {
            let endpoint = text(&detail, "collectionEndpoint");
            let arn = text(&detail, "arn");
            assert!(
                !endpoint.is_empty() && !arn.is_empty(),
                "case {case:?}: expected an ACTIVE collection to publish an endpoint and an ARN, \
                 got {detail}"
            );
            return (endpoint, arn);
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the collection {name} must reach ACTIVE, last read {read}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Start an ingestion job for one data source and POLL `GetIngestionJob` until the clock has
/// completed it, answering the completed job's statistics.
async fn run_ingestion_job(
    router: &MultiServiceRouter,
    case: &str,
    knowledge_base_id: &str,
    data_source_id: &str,
) -> Value {
    let jobs =
        format!("/knowledgebases/{knowledge_base_id}/datasources/{data_source_id}/ingestionjobs");
    let (status, started) = bedrock(router, "PUT", &format!("{jobs}/"), Some(json!({}))).await;
    assert_eq!(
        status, 202,
        "case {case:?}: expected StartIngestionJob accepted, got {status} with {started}"
    );
    let job_id = text(&started, "ingestionJob.ingestionJobId");
    let deadline = Instant::now() + POLL_CAP;
    loop {
        let (status, read) = bedrock(router, "GET", &format!("{jobs}/{job_id}"), None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected GetIngestionJob({job_id}) to answer, got {status} with {read}"
        );
        if text(&read, "ingestionJob.status") == "COMPLETE" {
            return at(&read, "ingestionJob.statistics");
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the clock must complete job {job_id}, last read {read}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// RULE: the index a knowledge base's vector store needs is created at the address the
/// collection publishes, and what the ingestion then writes into it is what that same address
/// counts, searches and reports — the documents the job accounted for, in the fields the
/// storage configuration maps.
#[tokio::test]
async fn an_ingestion_is_visible_at_the_published_collection_endpoint() {
    let case = "an S3 data source of a knowledge base bound to a published collection endpoint";
    let router = build_router();
    let collection = "kb-published-endpoint";
    let index = "bedrock-knowledge-base-published";
    let bucket = "kb-published-endpoint-source";
    let documents = ["first.txt", "second.txt"];

    admit_collection(&router, case, collection).await;
    let (endpoint, collection_arn) = active_collection(&router, case, collection).await;

    // The index create a knowledge base's vector store needs, exactly as a client sends it: no
    // RPC target, at the root of the address the collection published.
    let (status, created) = rest(
        &router,
        &endpoint,
        "PUT",
        &format!("/{index}"),
        Some(&json!({
            "settings": { "index": { "knn": true } },
            "mappings": { "properties": {
                VECTOR_FIELD: { "type": "knn_vector", "dimension": DIMENSION },
                TEXT_FIELD: { "type": "text" },
                METADATA_FIELD: { "type": "text", "index": false },
            } },
        })),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the index create at the published endpoint to be served, got \
         {status} with {created}"
    );

    // The source documents the ingestion will crawl.
    assert_eq!(
        s3(&router, "PUT", &format!("/{bucket}"), "").await,
        200,
        "case {case:?}: CreateBucket({bucket:?}) must succeed"
    );
    let mut expected: Vec<(String, String)> = Vec::new();
    for key in documents {
        let body = format!("the text of {key}");
        assert_eq!(
            s3(&router, "PUT", &format!("/{bucket}/ingested/{key}"), &body).await,
            200,
            "case {case:?}: PutObject({key:?}) must succeed"
        );
        expected.push((format!("s3://{bucket}/ingested/{key}"), body));
    }
    expected.sort();

    // The knowledge base over that index, and a data source over those objects.
    let (status, knowledge_base) = bedrock(
        &router,
        "PUT",
        "/knowledgebases/",
        Some(json!({
            "name": collection,
            "roleArn": KB_ROLE,
            "knowledgeBaseConfiguration": {
                "type": "VECTOR",
                "vectorKnowledgeBaseConfiguration": {
                    "embeddingModelArn":
                        format!("arn:aws:bedrock:{REGION}::foundation-model/{EMBEDDING_MODEL}"),
                },
            },
            "storageConfiguration": {
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
            },
        })),
    )
    .await;
    assert_eq!(
        status, 202,
        "case {case:?}: expected CreateKnowledgeBase over the created index accepted, got \
         {status} with {knowledge_base}"
    );
    let knowledge_base_id = text(&knowledge_base, "knowledgeBase.knowledgeBaseId");
    let (status, data_source) = bedrock(
        &router,
        "PUT",
        &format!("/knowledgebases/{knowledge_base_id}/datasources/"),
        Some(json!({
            "name": format!("{collection}-source"),
            "dataSourceConfiguration": {
                "type": "S3",
                "s3Configuration": {
                    "bucketArn": format!("arn:aws:s3:::{bucket}"),
                    "inclusionPrefixes": ["ingested/"],
                },
            },
        })),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected CreateDataSource accepted, got {status} with {data_source}"
    );
    let data_source_id = text(&data_source, "dataSource.dataSourceId");

    let statistics = run_ingestion_job(&router, case, &knowledge_base_id, &data_source_id).await;
    let indexed = i64::try_from(documents.len()).expect("a small count");
    assert_eq!(
        at(&statistics, "numberOfNewDocumentsIndexed")
            .as_i64()
            .unwrap_or_default(),
        indexed,
        "case {case:?}: expected the job to account for every source document as newly \
         indexed, got {statistics}"
    );
    assert_eq!(
        at(&statistics, "numberOfDocumentsFailed")
            .as_i64()
            .unwrap_or_default(),
        0,
        "case {case:?}: expected no document reported failed, got {statistics}"
    );

    // ...and the PUBLISHED address is what answers for them: the count a client of that
    // endpoint runs, and the search that reports each chunk in the mapped fields.
    let (status, counted) = rest(
        &router,
        &endpoint,
        "POST",
        &format!("/{index}/_count"),
        Some(&json!({ "query": { "match_all": {} } })),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the count at the published endpoint to be served, got {status} \
         with {counted}"
    );
    assert_eq!(
        at(&counted, "count").as_u64().unwrap_or_default(),
        u64::try_from(indexed).expect("a small count"),
        "case {case:?}: expected the published endpoint to count one document per indexed \
         document, got {counted}"
    );

    let (status, searched) = rest(
        &router,
        &endpoint,
        "POST",
        &format!("/{index}/_search"),
        Some(&json!({ "query": { "match_all": {} }, "size": 100 })),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the search at the published endpoint to be served, got {status} \
         with {searched}"
    );
    let mut held: Vec<(String, Value)> = at(&searched, "hits.hits")
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: expected a list of hits, got {searched}"))
        .iter()
        .map(|hit| (text(hit, "_id"), at(hit, "_source")))
        .collect();
    held.sort_by(|left, right| left.0.cmp(&right.0));
    assert_eq!(
        held.iter().map(|(id, _)| id.clone()).collect::<Vec<String>>(),
        expected
            .iter()
            .map(|(uri, _)| uri.clone())
            .collect::<Vec<String>>(),
        "case {case:?}: expected the published endpoint to report one document per source \
         document, keyed by where that document lives, got {held:?}"
    );
    for ((id, source), (uri, body)) in held.iter().zip(expected.iter()) {
        assert_eq!(
            text(source, TEXT_FIELD),
            *body,
            "case {case:?}: expected the chunk of {id:?} to carry its source document's own text \
             under the mapped text field, got {source}"
        );
        assert_eq!(
            text(source, SOURCE_URI_METADATA_KEY),
            *uri,
            "case {case:?}: expected the chunk of {id:?} to record where its source document \
             lives, got {source}"
        );
        assert_eq!(
            at(source, VECTOR_FIELD)
                .as_array()
                .map(Vec::len)
                .unwrap_or_default(),
            DIMENSION,
            "case {case:?}: expected the embedding of {id:?} to carry one value per declared \
             dimension, got {source}"
        );
    }
}
