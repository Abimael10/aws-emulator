// SPDX-License-Identifier: CC0-1.0

//! WHICH fields of its OpenSearch Serverless index a Bedrock Agent ingestion writes a chunk
//! into, and which fields a retrieval reads it back out of.
//!
//! RULE: for an `OPENSEARCH_SERVERLESS` storage configuration the three fields a knowledge
//! base's chunks occupy are the ones that configuration's own `fieldMapping` NAMES — any
//! `FieldName`, which the model bounds only by `length 0..2048` and `pattern ^.*$`
//! (`smithy_query(query="shape",shape="OpenSearchServerlessFieldMapping")`, the bedrock-agent
//! model) — never a fixed set of names: `vectorField` holds the embedding, one value per value
//! the bound index declares, `textField` the raw text of the source document the chunk was read
//! from, and `metadataField` the metadata about the vector store, which records where that
//! source lives. No field this configuration does not name is written, and a retrieval reads
//! the chunk's content back out of that same configured text field — so a knowledge base whose
//! mapping names fields of the caller's own choosing is as queryable as one on the default
//! names, and a `COMPLETE` ingestion job is backed by a document the collection itself answers
//! for.
//!
//! Four crates have to agree for that to hold — Amazon S3 holds the source document, the
//! Bedrock Agent build-time plane runs the job, the collection holds the documents and answers
//! for them at the address it publishes, and the Bedrock Agent runtime plane serves the
//! retrieval — so the suite lives in this cross-service target. What the sibling suites pin is
//! the other half of the same rule on the DEFAULT names:
//! `bedrock_agent_ingestion_populates_its_aoss_index.rs` (the published address) and
//! `bedrock_retrieve_over_a_published_collection_endpoint_index.rs` (the retrieval and its
//! refusal). This suite varies the one thing those hold fixed: the names themselves.
//!
//! The rows differ along what the rule generalises over — the FORM the three mapped names take:
//! the names AWS's own vector-index documentation gives a knowledge base, the bare names a
//! hardcoded implementation would use, lower_snake_case names of the caller's own, names
//! carrying capitals and spaces, and names of the maximum length the model admits. Each row
//! also names a different embedding model, so the number of values the index declares differs
//! with it, and each row asserts that NO other row's names are present in what was written.
//!
//! Every wait is a poll for the CONDITION under a deadline generous enough for any host;
//! nothing asserts how long a step took.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every call of this suite is signed for.
const REGION: &str = "us-east-1";

/// The access key id every call presents. It names no IAM record, which is the position an
/// account's own published credential is in: the collection's data access policies do not bound
/// it, so the index create a caller makes before any knowledge base exists is decided by the
/// network policy alone.
const ACCESS_KEY_ID: &str = "AKIDAMBIENT";

/// The IAM role every knowledge base here acts as, and the principal the collection's `data`
/// access policy grants the index permissions to. Matches the modelled `KnowledgeBaseRoleArn`
/// pattern.
const KB_ROLE: &str =
    "arn:aws:iam::000000000000:role/service-role/AmazonBedrockExecutionRoleForKnowledgeBase";

/// The `X-Amz-Target` prefix the OpenSearch Serverless CONTROL plane routes on. The collection's
/// REST data plane carries no such header.
const AOSS_TARGET_PREFIX: &str = "OpenSearchServerless";

/// The metadata field a knowledge base records a chunk's source location under, as the published
/// knowledge-base metadata fields name it. It is recorded BESIDE the three mapped fields, so it
/// is not one of the names a row varies.
const SOURCE_URI_METADATA_KEY: &str = "x-amz-bedrock-kb-source-uri";

/// The metadata a knowledge base records ABOUT every chunk it ingests, under the published
/// knowledge-base metadata field names, whatever the storage configuration maps: the chunk's own
/// text, its chunk id, the data source it came from, the modality of its source file and where
/// that source lives. They are written BESIDE the three mapped fields, so a row's absence check
/// skips them — another row happening to map one of these names says nothing about whether the
/// MAPPED fields are fixed.
const RECORDED_CHUNK_METADATA_KEYS: [&str; 5] = [
    "AMAZON_BEDROCK_TEXT_CHUNK",
    "x-amz-bedrock-kb-chunk-id",
    "x-amz-bedrock-kb-data-source-id",
    "x-amz-bedrock-kb-source-file-modality",
    SOURCE_URI_METADATA_KEY,
];

/// The longest field name `FieldName` admits (`length 0..2048`), which one row's names are built
/// to.
const MAX_FIELD_NAME_LENGTH: usize = 2048;

/// How long a poll waits for a transition the clock owes (a collection reaching `ACTIVE`, an
/// ingestion job reaching `COMPLETE`): many times the spans the owning services owe, so a loaded
/// machine still converges, and bounded so a hung poll fails this test rather than hanging the
/// suite.
const POLL_CAP: Duration = Duration::from_secs(60);

/// How long one poll attempt waits before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// One case of the table: the resources it provisions, the three names its storage
/// configuration maps, and the embedding model whose vectors the index is created for.
struct Row {
    case: &'static str,
    /// The collection, the index inside it, the source bucket and the knowledge base's name.
    collection: &'static str,
    index: &'static str,
    bucket: &'static str,
    /// The three names the `fieldMapping` carries, which are also what the index declares.
    vector_field: String,
    text_field: String,
    metadata_field: String,
    /// The embedding model the knowledge base names and the number of values its vectors carry,
    /// which is the dimension the index is created with
    /// (`state::vector_store::EMBEDDING_MODEL_DIMENSIONS` of the bedrock-agent crate, read off
    /// each model's published output length).
    model: &'static str,
    dimension: usize,
}

impl Row {
    /// The three names this row maps, in the order the mapping names them.
    fn mapped(&self) -> [&str; 3] {
        [&self.vector_field, &self.text_field, &self.metadata_field]
    }
}

/// A field name of exactly [`MAX_FIELD_NAME_LENGTH`] bytes ending in `suffix`, so the three
/// names of that row are distinct and each is at the published bound.
fn longest_name(suffix: &str) -> String {
    let mut name = "f".repeat(MAX_FIELD_NAME_LENGTH - suffix.len());
    name.push_str(suffix);
    name
}

/// The table: one row per FORM the three mapped names take.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the names AWS's own vector-index documentation gives a knowledge base",
            collection: "fieldmap-documented",
            index: "bedrock-knowledge-base-documented",
            bucket: "fieldmap-documented-source",
            vector_field: "bedrock-knowledge-base-default-vector".to_string(),
            text_field: "AMAZON_BEDROCK_TEXT_CHUNK".to_string(),
            metadata_field: "AMAZON_BEDROCK_METADATA".to_string(),
            model: "amazon.titan-embed-text-v1",
            dimension: 1536,
        },
        Row {
            case: "the bare names a hardcoded implementation would use",
            collection: "fieldmap-bare",
            index: "bedrock-knowledge-base-bare",
            bucket: "fieldmap-bare-source",
            vector_field: "vector".to_string(),
            text_field: "text".to_string(),
            metadata_field: "metadata".to_string(),
            model: "cohere.embed-english-v3",
            dimension: 1024,
        },
        Row {
            case: "lower_snake_case names of the caller's own",
            collection: "fieldmap-snake",
            index: "bedrock-knowledge-base-snake",
            bucket: "fieldmap-snake-source",
            vector_field: "kb_embedding".to_string(),
            text_field: "kb_passage".to_string(),
            metadata_field: "kb_source_meta".to_string(),
            model: "amazon.titan-embed-text-v2:0",
            dimension: 1024,
        },
        Row {
            case: "names carrying capitals and spaces",
            collection: "fieldmap-spaced",
            index: "bedrock-knowledge-base-spaced",
            bucket: "fieldmap-spaced-source",
            vector_field: "Chunk Embedding".to_string(),
            text_field: "Chunk Passage".to_string(),
            metadata_field: "Chunk Store Metadata".to_string(),
            model: "amazon.titan-embed-g1-text-02",
            dimension: 1536,
        },
        Row {
            case: "names of the maximum length the model admits",
            collection: "fieldmap-longest",
            index: "bedrock-knowledge-base-longest",
            bucket: "fieldmap-longest-source",
            vector_field: longest_name("-vector"),
            text_field: longest_name("-text"),
            metadata_field: longest_name("-metadata"),
            model: "cohere.embed-multilingual-v3",
            dimension: 1024,
        },
    ]
}

/// The four services this rule spans, over ONE engine, as `core/router/src/lib.rs` mounts them.
/// Request authentication is off, as in every other suite here: what each call carries is a
/// credential scope, which is what routes it and what each handler reads its Region and its
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

/// One Bedrock call, over the `restJson1` wire both Bedrock Agent planes declare, on the method
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
        .expect("a well-formed Bedrock request");
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

/// The string one MEMBER of a JSON object carries, looked up by its exact name — a mapped field
/// name may carry a dot or a space, so it is never walked as a path.
fn member(document: &Value, name: &str) -> Value {
    document.get(name).cloned().unwrap_or(Value::Null)
}

/// The `encryption` and `network` security policies a collection needs, and the `data` access
/// policy granting the knowledge base's own role the index permissions a knowledge base
/// performs.
async fn admit_collection(router: &MultiServiceRouter, case: &str, collection: &str) {
    for (operation, body) in [
        (
            "CreateSecurityPolicy",
            json!({
                "type": "encryption",
                "name": format!("{collection}-enc"),
                "policy": json!({
                    "Rules": [{
                        "ResourceType": "collection",
                        "Resource": [format!("collection/{collection}")],
                    }],
                    "AWSOwnedKey": true,
                })
                .to_string(),
            }),
        ),
        (
            "CreateSecurityPolicy",
            json!({
                "type": "network",
                "name": format!("{collection}-net"),
                "policy": json!({
                    "Rules": [{
                        "ResourceType": "collection",
                        "Resource": [format!("collection/{collection}")],
                    }],
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
async fn active_collection(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
) -> (String, String) {
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

/// The `storageConfiguration` one row's knowledge base is created with.
fn storage_configuration(row: &Row, collection_arn: &str) -> Value {
    json!({
        "type": "OPENSEARCH_SERVERLESS",
        "opensearchServerlessConfiguration": {
            "collectionArn": collection_arn,
            "vectorIndexName": row.index,
            "fieldMapping": {
                "vectorField": row.vector_field,
                "textField": row.text_field,
                "metadataField": row.metadata_field,
            },
        },
    })
}

/// RULE: an ingestion writes each chunk into the fields the knowledge base's own `fieldMapping`
/// names — the embedding under `vectorField` with one value per value the bound index declares,
/// the source document's text under `textField`, the metadata about the vector store under
/// `metadataField` — writes no field that mapping does not name, and a retrieval reads the
/// chunk's content back out of that same configured text field.
#[tokio::test]
async fn an_ingestion_writes_and_a_retrieval_reads_the_configured_fields() {
    let rows = rows();
    // Every name any row maps, so each row can assert that nothing it did NOT configure was
    // written: a handler that wrote a fixed set of names would leave another row's names behind.
    let every_name: Vec<String> = rows
        .iter()
        .flat_map(|row| row.mapped().map(ToString::to_string))
        .collect();

    for row in &rows {
        let case = row.case;
        let router = build_router();
        let key = "corpus/passage.txt";
        let body = format!("the passage ingested for {case}");
        let uri = format!("s3://{}/{key}", row.bucket);

        admit_collection(&router, case, row.collection).await;
        let (endpoint, collection_arn) = active_collection(&router, case, row.collection).await;

        // The index the knowledge base's vector store needs, declared in the row's own names
        // and for the number of values its embedding model publishes.
        let (status, created) = rest(
            &router,
            &endpoint,
            "PUT",
            &format!("/{}", row.index),
            Some(&json!({
                "settings": { "index": { "knn": true } },
                "mappings": { "properties": {
                    row.vector_field.clone():
                        { "type": "knn_vector", "dimension": row.dimension },
                    row.text_field.clone(): { "type": "text" },
                    row.metadata_field.clone(): { "type": "text", "index": false },
                } },
            })),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the index create at the published endpoint to be served, \
             got {status} with {created}"
        );

        // The source document the ingestion crawls.
        assert_eq!(
            s3(&router, "PUT", &format!("/{}", row.bucket), "").await,
            200,
            "case {case:?}: CreateBucket({:?}) must succeed",
            row.bucket
        );
        assert_eq!(
            s3(&router, "PUT", &format!("/{}/{key}", row.bucket), &body).await,
            200,
            "case {case:?}: PutObject({key:?}) must succeed"
        );

        // The knowledge base over that index, in the row's own field names.
        let (status, knowledge_base) = bedrock(
            &router,
            "PUT",
            "/knowledgebases/",
            Some(json!({
                "name": row.collection,
                "roleArn": KB_ROLE,
                "knowledgeBaseConfiguration": {
                    "type": "VECTOR",
                    "vectorKnowledgeBaseConfiguration": {
                        "embeddingModelArn":
                            format!("arn:aws:bedrock:{REGION}::foundation-model/{}", row.model),
                    },
                },
                "storageConfiguration": storage_configuration(row, &collection_arn),
            })),
        )
        .await;
        assert_eq!(
            status, 202,
            "case {case:?}: expected CreateKnowledgeBase over the created index accepted, got \
             {status} with {knowledge_base}"
        );
        let knowledge_base_id = text(&knowledge_base, "knowledgeBase.knowledgeBaseId");
        let mapping = at(
            &knowledge_base,
            "knowledgeBase.storageConfiguration.opensearchServerlessConfiguration.fieldMapping",
        );
        assert_eq!(
            [
                text(&mapping, "vectorField"),
                text(&mapping, "textField"),
                text(&mapping, "metadataField"),
            ],
            row.mapped().map(ToString::to_string),
            "case {case:?}: expected the created knowledge base to carry the mapping the \
             request named, got {mapping}"
        );

        let (status, data_source) = bedrock(
            &router,
            "PUT",
            &format!("/knowledgebases/{knowledge_base_id}/datasources/"),
            Some(json!({
                "name": format!("{}-source", row.collection),
                "dataSourceConfiguration": {
                    "type": "S3",
                    "s3Configuration": {
                        "bucketArn": format!("arn:aws:s3:::{}", row.bucket),
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

        let statistics =
            run_ingestion_job(&router, case, &knowledge_base_id, &data_source_id).await;
        assert_eq!(
            at(&statistics, "numberOfNewDocumentsIndexed")
                .as_i64()
                .unwrap_or_default(),
            1,
            "case {case:?}: expected the job to account for the source document as newly \
             indexed, got {statistics}"
        );
        assert_eq!(
            at(&statistics, "numberOfDocumentsFailed")
                .as_i64()
                .unwrap_or_default(),
            0,
            "case {case:?}: expected no document reported failed, got {statistics}"
        );

        // ...and the collection itself answers for that document, in the row's own fields.
        let (status, searched) = rest(
            &router,
            &endpoint,
            "POST",
            &format!("/{}/_search", row.index),
            Some(&json!({ "query": { "match_all": {} }, "size": 10 })),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the search at the published endpoint to be served, got \
             {status} with {searched}"
        );
        assert_eq!(
            at(&searched, "hits.total.value").as_u64().unwrap_or_default(),
            1,
            "case {case:?}: expected the COMPLETE job to be backed by one document the \
             collection answers for, got {searched}"
        );
        let hits = at(&searched, "hits.hits")
            .as_array()
            .cloned()
            .unwrap_or_default();
        let hit = hits
            .first()
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: expected one hit, got {searched}"));
        let source = at(&hit, "_source");

        assert_eq!(
            member(&source, &row.text_field).as_str().unwrap_or_default(),
            body,
            "case {case:?}: expected the chunk's raw text under the mapped text field {:?}, got \
             {source}",
            row.text_field
        );
        let embedding = member(&source, &row.vector_field)
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            embedding.len(),
            row.dimension,
            "case {case:?}: expected the embedding under the mapped vector field {:?} to carry \
             one value per declared dimension, got {} values",
            row.vector_field,
            embedding.len()
        );
        assert!(
            embedding.iter().all(Value::is_number),
            "case {case:?}: expected every value of the mapped vector field {:?} to be a \
             number, got {embedding:?}",
            row.vector_field
        );
        let metadata = member(&source, &row.metadata_field)
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            metadata.contains(&uri),
            "case {case:?}: expected the metadata under the mapped metadata field {:?} to \
             record where the source document lives ({uri}), got {metadata:?}",
            row.metadata_field
        );
        assert_eq!(
            member(&source, SOURCE_URI_METADATA_KEY)
                .as_str()
                .unwrap_or_default(),
            uri,
            "case {case:?}: expected the chunk to record its source location, got {source}"
        );
        for recorded in RECORDED_CHUNK_METADATA_KEYS {
            assert!(
                member(&source, recorded)
                    .as_str()
                    .is_some_and(|value| !value.is_empty()),
                "case {case:?}: expected the chunk to record {recorded:?} beside the mapped \
                 fields, got {source}"
            );
        }
        for name in &every_name {
            if row.mapped().contains(&name.as_str())
                || RECORDED_CHUNK_METADATA_KEYS.contains(&name.as_str())
            {
                continue;
            }
            assert_eq!(
                member(&source, name),
                Value::Null,
                "case {case:?}: expected no field this mapping does not name to be written, \
                 but {name:?} was present in {source}"
            );
        }

        // ...and the retrieval reads the content back out of the SAME configured text field.
        let (status, retrieved) = bedrock(
            &router,
            "POST",
            &format!("/knowledgebases/{knowledge_base_id}/retrieve"),
            Some(json!({ "retrievalQuery": { "text": body } })),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the retrieval to be served, got {status} with {retrieved}"
        );
        let results = at(&retrieved, "retrievalResults")
            .as_array()
            .cloned()
            .unwrap_or_default();
        let cited = results
            .iter()
            .find(|result| text(result, "location.s3Location.uri") == uri)
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected the retrieval to cite {uri}, got {retrieved}")
            });
        assert_eq!(
            text(cited, "content.text"),
            body,
            "case {case:?}: expected the cited content to be the source document's own text, \
             got {cited}"
        );
    }
}

/// RULE: the length `FieldName` admits bounds a mapped name — a name AT the published maximum
/// is served, and one byte past it is the modelled `ValidationException` (HTTP 400), never a
/// knowledge base bound to a name the store cannot hold.
///
/// The two requests differ in ONE BYTE of `vectorField` and in nothing else, so only the bound
/// can decide between them: neither leg can be answered by another validation. Both name a
/// collection this emulator holds no record of, which says nothing about what its policies
/// grant and is therefore not judged on one, so the accepted leg is accepted on the mapping
/// alone.
#[tokio::test]
async fn the_modelled_field_name_length_bounds_the_mapping() {
    let router = build_router();
    for (case, length, expected) in [
        (
            "a vectorField at the modelled maximum",
            MAX_FIELD_NAME_LENGTH,
            202,
        ),
        (
            "a vectorField one byte past the modelled maximum",
            MAX_FIELD_NAME_LENGTH + 1,
            400,
        ),
    ] {
        let (status, answer) = bedrock(
            &router,
            "PUT",
            "/knowledgebases/",
            Some(json!({
                "name": format!("fieldmap-bound-{length}"),
                "roleArn": KB_ROLE,
                "knowledgeBaseConfiguration": {
                    "type": "VECTOR",
                    "vectorKnowledgeBaseConfiguration": {
                        "embeddingModelArn":
                            format!("arn:aws:bedrock:{REGION}::foundation-model/\
                                     amazon.titan-embed-text-v1"),
                    },
                },
                "storageConfiguration": {
                    "type": "OPENSEARCH_SERVERLESS",
                    "opensearchServerlessConfiguration": {
                        "collectionArn": format!(
                            "arn:aws:aoss:{REGION}:000000000000:collection/fieldmap-absent"
                        ),
                        "vectorIndexName": "bedrock-knowledge-base-absent",
                        "fieldMapping": {
                            "vectorField": "v".repeat(length),
                            "textField": "t",
                            "metadataField": "m",
                        },
                    },
                },
            })),
        )
        .await;
        assert_eq!(
            status, expected,
            "case {case:?}: expected CreateKnowledgeBase -> {expected}, got {status} with {answer}"
        );
        if expected == 400 {
            assert!(
                text(&answer, "__type").contains("ValidationException"),
                "case {case:?}: expected the modelled ValidationException, got {answer}"
            );
        } else {
            assert_eq!(
                text(
                    &answer,
                    "knowledgeBase.storageConfiguration.opensearchServerlessConfiguration.\
                     fieldMapping.vectorField"
                )
                .len(),
                length,
                "case {case:?}: expected the created knowledge base to carry the name the \
                 request mapped, got {answer}"
            );
        }
    }
}
