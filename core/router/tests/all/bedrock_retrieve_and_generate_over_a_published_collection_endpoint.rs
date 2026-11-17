// SPDX-License-Identifier: CC0-1.0

//! The whole retrieval-augmented-generation sequence, end to end over ONE engine: a vector
//! index created at the address an OpenSearch Serverless collection PUBLISHES, a knowledge
//! base bound to that index, an ingestion job over an Amazon S3 data source, and
//! `RetrieveAndGenerate` answered from what that ingestion recorded.
//!
//! RULE: every `RetrieveAndGenerate` request is answered with one of the BOUNDED outcomes its
//! model declares — either the 200 whose required `sessionId` and `output.text` it carries,
//! with a `citations` entry per quoted passage whose `retrievedReferences[].content.text` is
//! text an ingestion really recorded and whose `location` names the source it was read from,
//! or one of the modelled errors (`ValidationException` 400, `ResourceNotFoundException` 404)
//! with its code and message — and never nothing at all. The index the knowledge base's
//! `storageConfiguration` names is created over the collection's own OpenSearch REST surface,
//! served at the address it publishes as `collectionEndpoint`, "the collection-specific
//! endpoint used to submit index, search, and data upload requests"
//! (`CollectionDetail$collectionEndpoint`), which names no RPC target.
//!
//! Five crates have to agree for that to hold — Amazon S3 holds the source objects, the
//! collection serves its index and holds the chunks, the Bedrock Agent build-time plane owns
//! the knowledge base and runs the ingestion, the Bedrock Agent runtime plane serves the
//! query, and the Bedrock Runtime plane owns the model catalogue a response is generated
//! against — so this suite lives in the cross-service target rather than in any one of them.
//! The crate-local halves are those crates' own suites: the citation geometry of a generated
//! response in `services/service-com-amazonaws-bedrockagentruntime-\
//! amazonbedrockagentruntimeservice/tests/all/retrieve_and_generate.rs`, the collection's REST
//! surface in `services/service-com-amazonaws-opensearchserverless-opensearchserverless/\
//! tests/all/collection_data_plane.rs`, and what an ingestion writes into a bound index in
//! `services/service-com-amazonaws-bedrockagent-amazonbedrockagentbuildtimelambda/tests/all/\
//! ingestion_writes_the_bound_collection_index.rs`.
//!
//! The rows differ along what the rule generalises over: the bytes an ingested object carries
//! (ASCII, two- and three-byte UTF-8, a combining sequence), how many objects one data source
//! holds, the embedding model and with it the dimension the index is created with, the model
//! the response is generated against, how many results the query may cite, and the names of
//! every resource. None of them may change that the call is ANSWERED, that the answer quotes
//! ingested text, or that a refusal arrives as its modelled error.
//!
//! Every wait is a poll for the CONDITION, bounded by how many times it LOOKS rather than by
//! the clock, so a loaded host converges the same way a quiet one does; nothing asserts how
//! long a step took.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every call of this suite is signed for. It is the one Region a `CreateBucket`
/// needs no `LocationConstraint` for, so the source bucket of a data source is created with
/// the one call a client makes there.
const REGION: &str = "us-east-1";

/// The access key id every call presents. It names no IAM record, which is the position an
/// account's own published credential is in.
const ACCESS_KEY_ID: &str = "AKIDAMBIENT";

/// The IAM role the knowledge bases of this suite act as, and the principal the collection's
/// `data` access policy grants the index permissions to. Matches the modelled
/// `KnowledgeBaseRoleArn` pattern.
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

/// How many times a poll looks again for a transition the clock owes. Counted in LOOKS rather
/// than in elapsed time, so a loaded host converges the same way a quiet one does, and bounded
/// so a transition that never arrives fails this test rather than hanging the suite.
const POLL_LOOKS: usize = 2_000;

/// How long one look waits before the next.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The characters the modelled `SessionId` admits, and the length it admits
/// (`smithy_query(query="operation",operation="RetrieveAndGenerate")`: pattern
/// `^[0-9a-zA-Z._:-]+$`, length 2..100).
const SESSION_ID_EXTRA_CHARACTERS: [char; 4] = ['.', '_', ':', '-'];
const SESSION_ID_MIN: usize = 2;
const SESSION_ID_MAX: usize = 100;

/// The five services this rule spans, over ONE engine, as `core/router/src/lib.rs` mounts
/// them. Request authentication is off, as in every other suite here: what each call carries
/// is a credential scope, which is what routes it and what each handler reads its Region and
/// its caller from.
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
        service_com_amazonaws_bedrockruntime_amazonbedrockfrontendservice::EmulatedService::new(
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
/// and URI the operation's own `@http` binding names. Both planes share the `bedrock` signing
/// name and each claims its own path space.
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

/// The list at a dotted path, or an empty one where there is none.
fn list(document: &Value, path: &str) -> Vec<Value> {
    at(document, path).as_array().cloned().unwrap_or_default()
}

/// The wire error code `restJson1` puts in `__type`, without the shape's namespace.
fn error_code(body: &Value) -> String {
    text(body, "__type")
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// The `encryption` and `network` security policies a collection needs, and the `data` access
/// policy that grants the knowledge base's own role the index permissions it performs.
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
    let mut last = Value::Null;
    for _ in 0..POLL_LOOKS {
        let (status, read) = aoss(router, "BatchGetCollection", json!({ "ids": [&id] })).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected BatchGetCollection({id}) to answer, got {status} with {read}"
        );
        let detail = list(&read, "collectionDetails")
            .first()
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
        last = read;
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!("case {case:?}: the collection {name} must reach ACTIVE, last read {last}");
}

/// Start an ingestion job for one data source and POLL `GetIngestionJob` until the clock has
/// completed it.
async fn run_ingestion_job(
    router: &MultiServiceRouter,
    case: &str,
    knowledge_base_id: &str,
    data_source_id: &str,
) {
    let jobs =
        format!("/knowledgebases/{knowledge_base_id}/datasources/{data_source_id}/ingestionjobs");
    let (status, started) = bedrock(router, "PUT", &format!("{jobs}/"), Some(json!({}))).await;
    assert_eq!(
        status, 202,
        "case {case:?}: expected StartIngestionJob accepted, got {status} with {started}"
    );
    let job_id = text(&started, "ingestionJob.ingestionJobId");
    let mut last = Value::Null;
    for _ in 0..POLL_LOOKS {
        let (status, read) = bedrock(router, "GET", &format!("{jobs}/{job_id}"), None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected GetIngestionJob({job_id}) to answer, got {status} with {read}"
        );
        if text(&read, "ingestionJob.status") == "COMPLETE" {
            assert_eq!(
                at(&read, "ingestionJob.statistics.numberOfDocumentsFailed")
                    .as_i64()
                    .unwrap_or_default(),
                0,
                "case {case:?}: expected no source document reported failed, got {read}"
            );
            return;
        }
        last = read;
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!("case {case:?}: the clock must complete job {job_id}, last read {last}");
}

/// Poll `GetKnowledgeBase` until `knowledge_base_id` reports `ACTIVE`.
async fn await_active_knowledge_base(
    router: &MultiServiceRouter,
    case: &str,
    knowledge_base_id: &str,
) {
    let mut last = Value::Null;
    for _ in 0..POLL_LOOKS {
        let (status, read) = bedrock(
            router,
            "GET",
            &format!("/knowledgebases/{knowledge_base_id}"),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected GetKnowledgeBase to answer, got {status} with {read}"
        );
        if text(&read, "knowledgeBase.status") == "ACTIVE" {
            return;
        }
        last = read;
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!(
        "case {case:?}: the knowledge base {knowledge_base_id} must reach ACTIVE, last read {last}"
    );
}

/// What one case builds: a collection, an index created at its published endpoint, a bucket of
/// source objects, a knowledge base over that index and an ingestion of that bucket.
struct Built {
    knowledge_base_id: String,
    /// Each ingested object as `(s3 uri, the text it carries)`.
    ingested: Vec<(String, String)>,
}

/// What one case's vector store is made of: the collection, the index inside it, the bucket of
/// source objects, and the embedding model and the dimension the index is created with.
struct Plan<'case> {
    collection: &'case str,
    index: &'case str,
    bucket: &'case str,
    embedding_model: &'case str,
    dimension: usize,
    /// The objects the data source holds, as `(key, the text it carries)`.
    documents: &'case [(&'case str, &'case str)],
}

/// Build the whole knowledge base of one case and answer what it holds.
async fn build(router: &MultiServiceRouter, case: &str, plan: &Plan<'_>) -> Built {
    let Plan {
        collection,
        index,
        bucket,
        embedding_model,
        dimension,
        documents,
    } = plan;
    admit_collection(router, case, collection).await;
    let (endpoint, collection_arn) = active_collection(router, case, collection).await;

    // The index create a knowledge base's vector store needs, exactly as a client sends it: no
    // RPC target, at the root of the address the collection published.
    let (status, created) = rest(
        router,
        &endpoint,
        "PUT",
        &format!("/{index}"),
        Some(&json!({
            "settings": { "index": { "knn": true } },
            "mappings": { "properties": {
                VECTOR_FIELD: { "type": "knn_vector", "dimension": dimension },
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

    // The source objects the ingestion will crawl.
    assert_eq!(
        s3(router, "PUT", &format!("/{bucket}"), "").await,
        200,
        "case {case:?}: CreateBucket({bucket:?}) must succeed"
    );
    let mut ingested: Vec<(String, String)> = Vec::new();
    for (key, body) in documents.iter() {
        assert_eq!(
            s3(router, "PUT", &format!("/{bucket}/ingested/{key}"), body).await,
            200,
            "case {case:?}: PutObject({key:?}) must succeed"
        );
        ingested.push((format!("s3://{bucket}/ingested/{key}"), (*body).to_string()));
    }

    let (status, knowledge_base) = bedrock(
        router,
        "PUT",
        "/knowledgebases/",
        Some(json!({
            "name": collection,
            "roleArn": KB_ROLE,
            "knowledgeBaseConfiguration": {
                "type": "VECTOR",
                "vectorKnowledgeBaseConfiguration": {
                    "embeddingModelArn":
                        format!("arn:aws:bedrock:{REGION}::foundation-model/{embedding_model}"),
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
    await_active_knowledge_base(router, case, &knowledge_base_id).await;

    let (status, data_source) = bedrock(
        router,
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
    run_ingestion_job(router, case, &knowledge_base_id, &data_source_id).await;

    Built {
        knowledge_base_id,
        ingested,
    }
}

/// One `RetrieveAndGenerate` call over the runtime plane's own segment.
async fn retrieve_and_generate(router: &MultiServiceRouter, body: Value) -> (u16, Value) {
    bedrock(router, "POST", "/retrieveAndGenerate", Some(body)).await
}

/// The request body of a knowledge-base query: the modelled `KNOWLEDGE_BASE` configuration.
fn query_body(knowledge_base_id: &str, model: &str, query: &str, results: Option<u32>) -> Value {
    let mut knowledge_base = json!({
        "knowledgeBaseId": knowledge_base_id,
        "modelArn": format!("arn:aws:bedrock:{REGION}::foundation-model/{model}"),
    });
    if let Some(results) = results {
        knowledge_base["retrievalConfiguration"] = json!({
            "vectorSearchConfiguration": { "numberOfResults": results },
        });
    }
    json!({
        "input": { "text": query },
        "retrieveAndGenerateConfiguration": {
            "type": "KNOWLEDGE_BASE",
            "knowledgeBaseConfiguration": knowledge_base,
        },
    })
}

/// Every `retrievedReferences[].content.text` of an answer, with the location it was read from.
fn cited(answer: &Value) -> Vec<(String, String)> {
    list(answer, "citations")
        .iter()
        .flat_map(|citation| list(citation, "retrievedReferences"))
        .map(|reference| {
            (
                text(&reference, "content.text"),
                text(&reference, "location.s3Location.uri"),
            )
        })
        .collect()
}

/// One case of the table.
struct Row {
    case: &'static str,
    collection: &'static str,
    index: &'static str,
    bucket: &'static str,
    /// The embedding model the knowledge base declares and the dimension the index is created
    /// with: `amazon.titan-embed-text-v1` publishes 1536-value vectors and
    /// `amazon.titan-embed-text-v2:0` 1024.
    embedding_model: &'static str,
    dimension: usize,
    /// The model the response is generated against, from the catalogue the Bedrock Runtime
    /// plane publishes (`model_catalog`).
    generation_model: &'static str,
    /// The objects the data source holds, as `(key, the text it carries)`.
    documents: &'static [(&'static str, &'static str)],
    /// The query asked, and the key of the object whose text answers it.
    query: &'static str,
    grounded_in: &'static str,
    /// `numberOfResults` the query asks for, which bounds how many passages may be cited; the
    /// member defaults to 5 when a request names none.
    results: Option<u32>,
}

/// RULE: a knowledge base whose vector index was created at the collection's PUBLISHED endpoint
/// serves `RetrieveAndGenerate` — HTTP 200 with the required `sessionId` and `output.text`, and
/// a citation whose retrieved reference carries text an ingestion recorded and names the object
/// it was read from.
///
/// The rows span the bytes an object carries, how many objects one data source holds, the
/// embedding model and the dimension of the index, the generation model, how many results the
/// query may cite, and every resource name.
#[tokio::test]
async fn a_knowledge_base_over_a_published_endpoint_answers_retrieve_and_generate() {
    let rows: &[Row] = &[
        Row {
            case: "ascii objects, two of them, 1536 values, default result count",
            collection: "rag-ascii",
            index: "bedrock-knowledge-base-ascii",
            bucket: "rag-ascii-sources",
            embedding_model: "amazon.titan-embed-text-v1",
            dimension: 1536,
            generation_model: "anthropic.claude-3-5-sonnet-20240620-v1:0",
            documents: &[
                (
                    "pump.txt",
                    "Hydraulic pump P-12 in bay 2 is serviced every 90 days.",
                ),
                (
                    "coolant.txt",
                    "Coolant for the hydraulic system is stored in drum 7.",
                ),
            ],
            query: "How often is hydraulic pump P-12 serviced?",
            grounded_in: "pump.txt",
            results: None,
        },
        Row {
            case: "three-byte UTF-8 bytes, one object, 1024 values, one result",
            collection: "rag-cjk",
            index: "bedrock-knowledge-base-cjk",
            bucket: "rag-cjk-sources",
            embedding_model: "amazon.titan-embed-text-v2:0",
            dimension: 1024,
            generation_model: "amazon.nova-lite-v1:0",
            documents: &[(
                "runbook.txt",
                "\u{30a8}\u{30b9}\u{30ab}\u{30ec}-\u{6587}\u{66f8} bay 4 \u{306f} 30 \u{65e5}",
            )],
            query: "\u{30a8}\u{30b9}\u{30ab}\u{30ec}-\u{6587}\u{66f8}",
            grounded_in: "runbook.txt",
            results: Some(1),
        },
        Row {
            case: "two-byte UTF-8 bytes beside a combining sequence, three objects, 1024 \
                   values, two results",
            collection: "rag-latin",
            index: "bedrock-knowledge-base-latin",
            bucket: "rag-latin-sources",
            embedding_model: "amazon.titan-embed-text-v2:0",
            dimension: 1024,
            generation_model: "anthropic.claude-3-haiku-20240307-v1:0",
            documents: &[
                ("menu.txt", "Canteen service: barley stew on Monday."),
                (
                    "ueberwachung.txt",
                    "\u{dc}berwachung der Anlage \u{df} l\u{e4}uft t\u{e4}glich.",
                ),
                ("echeance.txt", "L'eche\u{301}ance du contrat est en mai."),
            ],
            query: "\u{dc}berwachung der Anlage",
            grounded_in: "ueberwachung.txt",
            results: Some(2),
        },
    ];

    for row in rows {
        let Row {
            case,
            collection,
            index,
            bucket,
            embedding_model,
            dimension,
            generation_model,
            documents,
            query,
            grounded_in,
            results,
        } = row;
        let router = build_router();
        let built = build(
            &router,
            case,
            &Plan {
                collection,
                index,
                bucket,
                embedding_model,
                dimension: *dimension,
                documents,
            },
        )
        .await;

        let (status, answer) = retrieve_and_generate(
            &router,
            query_body(&built.knowledge_base_id, generation_model, query, *results),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the knowledge-base query to be served, got {status} with \
             {answer}"
        );

        // The two REQUIRED members of the modelled response.
        let session_id = text(&answer, "sessionId");
        assert!(
            (SESSION_ID_MIN..=SESSION_ID_MAX).contains(&session_id.chars().count())
                && session_id.chars().all(|character| {
                    character.is_ascii_alphanumeric()
                        || SESSION_ID_EXTRA_CHARACTERS.contains(&character)
                }),
            "case {case:?}: expected a sessionId of the modelled form, got {session_id:?} in \
             {answer}"
        );
        let generated = text(&answer, "output.text");
        assert!(
            !generated.is_empty(),
            "case {case:?}: expected output.text to carry the generated response, got {answer}"
        );

        // ...and the citations, which are what makes the answer GROUNDED: every cited passage
        // is text an ingestion really recorded, read from the object it names.
        let citations = list(&answer, "citations");
        assert!(
            !citations.is_empty(),
            "case {case:?}: expected at least one citation, got {answer}"
        );
        let references = cited(&answer);
        // `numberOfResults` bounds how many source chunks the RETRIEVAL reports, which is what
        // the response may quote: the distinct references cited cannot outnumber it.
        if let Some(allowed) = results {
            let mut distinct: Vec<&(String, String)> = Vec::new();
            for reference in &references {
                if !distinct.contains(&reference) {
                    distinct.push(reference);
                }
            }
            assert!(
                distinct.len() <= usize::try_from(*allowed).expect("a small count"),
                "case {case:?}: expected no more cited references than the {allowed} results the \
                 query asked for, got {distinct:?} in {answer}"
            );
        }
        for (quoted, uri) in &references {
            assert!(
                !quoted.is_empty()
                    && built
                        .ingested
                        .iter()
                        .any(|(source, body)| source == uri && body.contains(quoted.as_str())),
                "case {case:?}: expected every cited reference to be ingested text read from the \
                 object it names, got {quoted:?} at {uri:?} against {:?}",
                built.ingested
            );
        }
        let wanted = format!("s3://{bucket}/ingested/{grounded_in}");
        let fact = documents
            .iter()
            .find(|(key, _)| key == grounded_in)
            .map(|(_, body)| (*body).to_string())
            .expect("the row names one of its own objects");
        assert!(
            references
                .iter()
                .any(|(quoted, uri)| *uri == wanted && fact.contains(quoted.as_str())),
            "case {case:?}: expected a citation of {wanted:?} carrying text ingested from it, got \
             {references:?}"
        );

        // Every citation indexes the response it is reported beside: the segment the span
        // names is the text of the generated response part.
        let characters: Vec<char> = generated.chars().collect();
        for citation in &citations {
            let part = text(citation, "generatedResponsePart.textResponsePart.text");
            let start = at(citation, "generatedResponsePart.textResponsePart.span.start")
                .as_u64()
                .unwrap_or_default() as usize;
            let end = at(citation, "generatedResponsePart.textResponsePart.span.end")
                .as_u64()
                .unwrap_or_default() as usize;
            assert!(
                start < end && end <= characters.len(),
                "case {case:?}: expected the span {start}..{end} to index the {} characters of \
                 the response, got {citation}",
                characters.len()
            );
            assert_eq!(
                characters[start..end].iter().collect::<String>(),
                part,
                "case {case:?}: expected output.text[span] to be the cited segment, got \
                 {citation}"
            );
        }

        // The session the first call opened CONTINUES: a second turn carrying the id it
        // answered is served under that same id.
        let mut continued = query_body(&built.knowledge_base_id, generation_model, query, *results);
        continued["sessionId"] = json!(session_id);
        let (status, second) = retrieve_and_generate(&router, continued).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the second turn of session {session_id:?} to be served, got \
             {status} with {second}"
        );
        assert_eq!(
            text(&second, "sessionId"),
            session_id,
            "case {case:?}: expected the continued turn to report the session it continued, got \
             {second}"
        );
        assert!(
            !list(&second, "citations").is_empty(),
            "case {case:?}: expected the continued turn to cite the same knowledge base, got \
             {second}"
        );
    }
}

/// One refusal case: what the request is refused for, and the modelled outcome it must get.
/// The request body itself is the `Value` this case is paired with.
struct Refusal {
    case: &'static str,
    /// The HTTP status the refusal must carry.
    status: u16,
    /// The modelled error code it must carry.
    code: &'static str,
    /// The message the refusal must carry, when the class fixes it.
    message: Option<&'static str>,
}

/// RULE: a `RetrieveAndGenerate` request the emulator cannot serve is answered with the
/// modelled error of its class — code, status and message — PROMPTLY, never left open and
/// never answered with an empty success.
///
/// The rows span the classes a client branches on: a knowledge base nothing created
/// (`ResourceNotFoundException` 404), a request naming no configuration, a session id this
/// service never issued, and a leg nothing here evaluates (`ValidationException` 400).
#[tokio::test]
async fn an_unservable_knowledge_base_query_is_answered_with_its_modelled_error() {
    let case = "a knowledge base over a published collection endpoint";
    let router = build_router();
    let built = build(
        &router,
        case,
        &Plan {
            collection: "rag-refusals",
            index: "bedrock-knowledge-base-refusals",
            bucket: "rag-refusal-sources",
            embedding_model: "amazon.titan-embed-text-v1",
            dimension: 1536,
            documents: &[("note.txt", "The spare belt lives in cabinet 3.")],
        },
    )
    .await;
    let model = "anthropic.claude-3-5-sonnet-20240620-v1:0";
    let query = "Where is the spare belt?";

    // A knowledge base id nothing created. `knowledgeBaseId` is `^[0-9a-zA-Z]+$` of at most 10
    // characters, so the row names one of that form.
    let absent = "SIM2REAK0B";
    let rows: Vec<(Refusal, Value)> = vec![
        (
            Refusal {
                case: "a knowledge base id nothing created",
                status: 404,
                code: "ResourceNotFoundException",
                message: Some("Knowledge Base with id SIM2REAK0B does not exist"),
            },
            query_body(absent, model, query, None),
        ),
        (
            Refusal {
                case: "a request naming no retrieveAndGenerateConfiguration",
                status: 400,
                code: "ValidationException",
                message: None,
            },
            json!({ "input": { "text": query } }),
        ),
        (
            Refusal {
                case: "a session id this service never issued",
                status: 400,
                code: "ValidationException",
                message: None,
            },
            {
                let mut body = query_body(&built.knowledge_base_id, model, query, None);
                body["sessionId"] = json!("never-issued-session");
                body
            },
        ),
    ];

    for (refusal, body) in rows {
        let Refusal {
            case,
            status: wanted,
            code,
            message,
        } = refusal;
        let (status, answer) = retrieve_and_generate(&router, body).await;
        assert_eq!(
            status, wanted, "case {case:?}: expected HTTP {wanted}, got {status} with {answer}"
        );
        assert_eq!(
            error_code(&answer),
            code,
            "case {case:?}: expected the modelled {code}, got {answer}"
        );
        assert!(
            !text(&answer, "message").is_empty(),
            "case {case:?}: expected the refusal to say why, got {answer}"
        );
        if let Some(message) = message {
            assert_eq!(
                text(&answer, "message"),
                message,
                "case {case:?}: expected the wording of this error class, got {answer}"
            );
        }
    }

    // ...and the knowledge base that DOES exist is still served, which is what proves the
    // refusals above were about what the request carried.
    let (status, answer) = retrieve_and_generate(
        &router,
        query_body(&built.knowledge_base_id, model, query, None),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the query over the built knowledge base to be served, got \
         {status} with {answer}"
    );
    assert!(
        !text(&answer, "output.text").is_empty() && !list(&answer, "citations").is_empty(),
        "case {case:?}: expected a generated response with citations, got {answer}"
    );

    // A query carrying the identity whose authorized documents the response would be narrowed
    // to is served the same way: the ingestion recorded no access control list for any
    // document, and a document no ACL names is withheld from nobody, so the answer is the one
    // the same query carrying no identity is answered with.
    let (scoped_status, scoped) = retrieve_and_generate(&router, {
        let mut body = query_body(&built.knowledge_base_id, model, query, None);
        body["userContext"] = json!({ "userId": "operator-7" });
        body
    })
    .await;
    assert_eq!(
        scoped_status, 200,
        "case {case:?}: expected the query scoped to an identity to be served, got \
         {scoped_status} with {scoped}"
    );
    assert_eq!(
        text(&scoped, "output.text"),
        text(&answer, "output.text"),
        "case {case:?}: expected the identity-scoped query to answer what the unscoped one \
         answers, got {scoped}"
    );
    assert_eq!(
        list(&scoped, "citations").len(),
        list(&answer, "citations").len(),
        "case {case:?}: expected the same passages to be cited, got {scoped}"
    );
}
