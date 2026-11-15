// SPDX-License-Identifier: CC0-1.0

//! What an ingestion job PUTS into the OpenSearch Serverless collection a knowledge base is
//! bound to, read back out of that collection itself.
//!
//! RULE: a knowledge base keeps its chunks in the store its `storageConfiguration` names, so
//! every document an ingestion job reports indexed is backed by a document the OWNING store
//! answers for from its own read. For an `OPENSEARCH_SERVERLESS` configuration that store is
//! the collection: the chunk is written into the vector index the configuration names, in the
//! fields its `fieldMapping` maps ("the field in which Amazon Bedrock stores the vector
//! embeddings for your data sources", the raw text and the metadata), with as many values as
//! that index's own `knn_vector` mapping declares — and writing it there is the knowledge
//! base's ROLE acting on the index, so the collection's `data` access policies are what admit
//! it. A data source's `dataDeletionPolicy` then decides what becomes of its documents when
//! it is deleted.
//!
//! The rows differ along what the rule generalises over: how many source documents the
//! ingested data source holds, how many values the bound index declares (including an index
//! that declares FEWER than the knowledge base's embedding model publishes, so the index is
//! what decides), and which `dataDeletionPolicy` the data source declares — `RETAIN` and
//! `DELETE`, the two values the modelled `DataDeletionPolicy` admits. Every row also runs a
//! SIBLING data source of the same knowledge base into the same index, so a delete that took
//! more than the deleted data source's own documents fails the row.
//!
//! Every call goes over the wire of the service that owns it, on ONE engine: Amazon S3 holds
//! the source documents, this service runs the job, and the collection holds the documents and
//! answers for them — the index is created through that service's own `CreateIndex` and read
//! back through the OpenSearch REST surface it serves for the collection, which is the same
//! index record either plane reaches. Every wait is a poll for the CONDITION under a generous
//! deadline; nothing asserts how long a step took.
//!
//! The ADDRESS a stock OpenSearch client uses — the host the collection publishes as its
//! `collectionEndpoint` — is where the cross-service suite
//! `core/router/tests/all/bedrock_agent_ingestion_populates_its_aoss_index.rs` drives the same
//! rule; here the collection's data plane is addressed by the reserved path namespace, so the
//! table below needs no listener of the collection's own.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every call of this suite is signed for.
const REGION: &str = "us-east-1";

/// The access key id every call presents. It names no IAM record, which is the position an
/// account's own published credential is in: the collection's data access policies do not
/// bound it, so what this suite's own reads of the collection are admitted by is the network
/// policy alone. The knowledge base's ROLE is a different principal, and the policies do bound
/// that one.
const ACCESS_KEY_ID: &str = "AKIDAMBIENT";

/// The IAM role every knowledge base here acts as, and the principal its collection's `data`
/// access policy grants the index permissions to. Matches the modelled `KnowledgeBaseRoleArn`
/// pattern.
const KB_ROLE: &str =
    "arn:aws:iam::000000000000:role/service-role/AmazonBedrockExecutionRoleForKnowledgeBase";

/// A second role, which the revoked-grant case narrows the policy to.
const OTHER_ROLE: &str = "arn:aws:iam::000000000000:role/service-role/AnotherRole";

/// The `X-Amz-Target` prefix the OpenSearch Serverless control plane routes on.
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

/// How long a poll waits for a transition the clock owes (a collection reaching `ACTIVE`, an
/// ingestion job reaching `COMPLETE`): many times the spans the owning services owe, so a
/// loaded machine still converges, and bounded so a hung poll fails this test rather than
/// hanging the suite.
const POLL_CAP: Duration = Duration::from_secs(60);

/// How long one poll attempt waits before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// This service, Amazon S3 and OpenSearch Serverless on ONE engine — the shape
/// `core/router/src/lib.rs` mounts them in. Request authentication is off, as in this crate's
/// other suites: what a call carries is a credential scope, which is what routes it and what
/// each handler reads its Region and its caller from.
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

/// Dispatch one request and parse its JSON answer (`Value::Null` for a body that is not JSON,
/// which is what an empty one is).
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

/// One OpenSearch REST request to the collection `collection_id`'s DATA plane, addressed by
/// the reserved path namespace that names this plane and the collection — the addressing that
/// needs no listener of the collection's own, and which answers out of the same records the
/// published endpoint does.
async fn data_plane(
    router: &MultiServiceRouter,
    collection_id: &str,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method(method)
        .uri(format!("/_aws/aoss/{collection_id}{path}"))
        .header("host", format!("aoss.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("aoss"))
        .body(Bytes::from(
            body.map(|body| body.to_string()).unwrap_or_default(),
        ))
        .expect("a well-formed data-plane request");
    dispatch(router, request).await
}

/// One Amazon Bedrock Agent call, over the `restJson1` wire this model declares, on the method
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

/// The `s3://` URI of one object, as `S3Location$uri` spells it and as the chunk written for it
/// is keyed by.
fn uri(bucket: &str, key: &str) -> String {
    format!("s3://{bucket}/{key}")
}

/// The `encryption` and `network` security policies every collection of this suite needs: a
/// collection cannot be created without the first, and a collection no network policy covers
/// answers its endpoint to nobody.
async fn admit_collections(router: &MultiServiceRouter, case: &str) {
    for (kind, name, policy) in [
        (
            "encryption",
            "enc-every-collection",
            json!({
                "Rules": [{ "ResourceType": "collection", "Resource": ["collection/*"] }],
                "AWSOwnedKey": true,
            }),
        ),
        (
            "network",
            "net-every-collection",
            json!({
                "Rules": [{ "ResourceType": "collection", "Resource": ["collection/*"] }],
                "AllowFromPublic": true,
            }),
        ),
    ] {
        let (status, answer) = aoss(
            router,
            "CreateSecurityPolicy",
            json!({ "type": kind, "name": name, "policy": policy.to_string() }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the {kind} policy accepted, got {status} with {answer}"
        );
    }
}

/// The document of a `data` access policy: the index and collection rules one principal is
/// granted.
fn data_policy(collection: &str, principal: &str) -> Value {
    json!([{
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
        "Principal": [principal],
    }])
}

/// A `data` access policy granting `principal` every `aoss` permission on one collection and
/// its indexes — the grant AWS's "Create a vector index in Amazon OpenSearch Service" has a
/// caller make for the knowledge-base role. Answers the policy's own version token, which an
/// update of it has to carry.
async fn grant_data_access(
    router: &MultiServiceRouter,
    case: &str,
    collection: &str,
    principal: &str,
) -> String {
    let (status, created) = aoss(
        router,
        "CreateAccessPolicy",
        json!({
            "type": "data",
            "name": format!("{collection}-data"),
            "policy": data_policy(collection, principal).to_string(),
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the data access policy accepted, got {status} with {created}"
    );
    text(&created, "accessPolicyDetail.policyVersion")
}

/// Create a `VECTORSEARCH` collection, poll until it reports `ACTIVE`, and create the vector
/// index inside it through this service's own `CreateIndex` — with a `knn_vector` field of
/// `dimension` values, the index a knowledge base's storage configuration requires to exist
/// before it can name it. Answers the collection's id and its ARN.
async fn collection_with_index(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    index: &str,
    dimension: usize,
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
    let arn = {
        let deadline = Instant::now() + POLL_CAP;
        loop {
            let (status, read) = aoss(router, "BatchGetCollection", json!({ "ids": [&id] })).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected BatchGetCollection({id}) to answer, got {status} with \
                 {read}"
            );
            let detail = at(&read, "collectionDetails")
                .as_array()
                .and_then(|details| details.first())
                .cloned()
                .unwrap_or(Value::Null);
            if text(&detail, "status") == "ACTIVE" {
                break text(&detail, "arn");
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: the collection {name} must reach ACTIVE, last read {read}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    };
    let (status, index_created) = aoss(
        router,
        "CreateIndex",
        json!({
            "id": &id,
            "indexName": index,
            "indexSchema": {
                "settings": { "index": { "knn": true } },
                "mappings": { "properties": {
                    VECTOR_FIELD: { "type": "knn_vector", "dimension": dimension },
                    TEXT_FIELD: { "type": "text" },
                    METADATA_FIELD: { "type": "text", "index": false },
                } },
            },
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected CreateIndex({index}) accepted, got {status} with {index_created}"
    );
    (id, arn)
}

/// Create a knowledge base bound to `index` of the collection `collection_arn` names, answering
/// its minted id.
async fn knowledge_base(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    model: &str,
    collection_arn: &str,
    index: &str,
) -> String {
    let (status, created) = bedrock(
        router,
        "PUT",
        "/knowledgebases/",
        Some(json!({
            "name": name,
            "roleArn": KB_ROLE,
            "knowledgeBaseConfiguration": {
                "type": "VECTOR",
                "vectorKnowledgeBaseConfiguration": {
                    "embeddingModelArn":
                        format!("arn:aws:bedrock:{REGION}::foundation-model/{model}"),
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
        "case {case:?}: expected CreateKnowledgeBase({name:?}) accepted, got {status} with \
         {created}"
    );
    text(&created, "knowledgeBase.knowledgeBaseId")
}

/// Create an S3 data source over `prefix` of `bucket` under `policy`, answering its minted id.
async fn data_source(
    router: &MultiServiceRouter,
    case: &str,
    knowledge_base_id: &str,
    name: &str,
    bucket: &str,
    prefix: &str,
    policy: &str,
) -> String {
    let (status, created) = bedrock(
        router,
        "PUT",
        &format!("/knowledgebases/{knowledge_base_id}/datasources/"),
        Some(json!({
            "name": name,
            "dataDeletionPolicy": policy,
            "dataSourceConfiguration": {
                "type": "S3",
                "s3Configuration": {
                    "bucketArn": format!("arn:aws:s3:::{bucket}"),
                    "inclusionPrefixes": [prefix],
                },
            },
        })),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected CreateDataSource({name:?}) accepted, got {status} with {created}"
    );
    text(&created, "dataSource.dataSourceId")
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

/// How many documents the collection's own count reports for `index`.
async fn counted(
    router: &MultiServiceRouter,
    case: &str,
    collection_id: &str,
    index: &str,
) -> u64 {
    let (status, answer) = data_plane(
        router,
        collection_id,
        "POST",
        &format!("/{index}/_count"),
        Some(&json!({ "query": { "match_all": {} } })),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the collection's own count to be served, got {status} with \
         {answer}"
    );
    at(&answer, "count")
        .as_u64()
        .unwrap_or_else(|| panic!("case {case:?}: expected a numeric count, got {answer}"))
}

/// Every document `index` holds, as the collection's own search reports it: the id and the
/// source, in id order.
async fn held_documents(
    router: &MultiServiceRouter,
    case: &str,
    collection_id: &str,
    index: &str,
) -> Vec<(String, Value)> {
    let (status, answer) = data_plane(
        router,
        collection_id,
        "POST",
        &format!("/{index}/_search"),
        Some(&json!({ "query": { "match_all": {} }, "size": 100 })),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the collection's own search to be served, got {status} with \
         {answer}"
    );
    let mut hits: Vec<(String, Value)> = at(&answer, "hits.hits")
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: expected a list of hits, got {answer}"))
        .iter()
        .map(|hit| (text(hit, "_id"), at(hit, "_source")))
        .collect();
    hits.sort_by(|left, right| left.0.cmp(&right.0));
    hits
}

/// One ingestion, with the index it writes into and the data-deletion policy its data source
/// declares.
struct Row {
    case: &'static str,
    /// How many source documents the ingested data source holds.
    documents: usize,
    /// The embedding model the knowledge base declares: Titan Text Embeddings V2 publishes
    /// 1024-value vectors and Titan Embeddings G1 — Text 1536.
    model: &'static str,
    /// How many values the BOUND INDEX declares for its `knn_vector` field, which is what the
    /// chunks written into it carry — a row where it differs from the model's own dimension is
    /// what pins that.
    dimension: usize,
    /// The data source's `dataDeletionPolicy` — one of the two values the modelled
    /// `DataDeletionPolicy` admits (`smithy_query(query="shape",shape="DataDeletionPolicy")`).
    policy: &'static str,
    /// Whether that data source's documents are still in the collection's index once it is
    /// deleted, which is what the policy decides.
    survives_deletion: bool,
}

/// RULE: every document an ingestion job reports indexed is backed by a document the bound
/// OpenSearch Serverless collection answers for from its own read, written into the fields the
/// storage configuration maps and carrying one value per dimension the bound index declares;
/// the data source's `dataDeletionPolicy` then decides whether those documents outlive it.
#[tokio::test]
async fn an_ingestion_job_writes_the_bound_collection_index() {
    let rows = [
        Row {
            case: "two documents, an index of 1024 values, RETAIN",
            documents: 2,
            model: "amazon.titan-embed-text-v2:0",
            dimension: 1024,
            policy: "RETAIN",
            survives_deletion: true,
        },
        Row {
            case: "three documents, an index of 1536 values, DELETE",
            documents: 3,
            model: "amazon.titan-embed-text-v1",
            dimension: 1536,
            policy: "DELETE",
            survives_deletion: false,
        },
        Row {
            case: "one document, an index of 512 values under a 1024-value model, DELETE",
            documents: 1,
            model: "amazon.titan-embed-text-v2:0",
            dimension: 512,
            policy: "DELETE",
            survives_deletion: false,
        },
    ];

    for (position, row) in rows.iter().enumerate() {
        let case = row.case;
        let router = build_router();
        let collection = format!("kb-aoss-{position}");
        let index = format!("bedrock-knowledge-base-{position}");
        let bucket = format!("kb-aoss-source-{position}");

        admit_collections(&router, case).await;
        grant_data_access(&router, case, &collection, KB_ROLE).await;
        let (collection_id, collection_arn) =
            collection_with_index(&router, case, &collection, &index, row.dimension).await;

        // The source documents, written through Amazon S3's own PutObject: the ingested data
        // source's under one prefix, and one more under a second prefix that a SIBLING data
        // source of the same knowledge base ingests into the same index.
        assert_eq!(
            s3(&router, "PUT", &format!("/{bucket}"), "").await,
            200,
            "case {case:?}: CreateBucket({bucket:?}) must succeed"
        );
        let mut ingested: Vec<(String, String)> = Vec::new();
        for document in 0..row.documents {
            let key = format!("ingested/document-{document}.txt");
            let body = format!("the text of document {document} of {case}");
            assert_eq!(
                s3(&router, "PUT", &format!("/{bucket}/{key}"), &body).await,
                200,
                "case {case:?}: PutObject({key:?}) must succeed"
            );
            ingested.push((uri(&bucket, &key), body));
        }
        let sibling_key = "retained/sibling.txt";
        let sibling_body = "the sibling data source own document";
        assert_eq!(
            s3(
                &router,
                "PUT",
                &format!("/{bucket}/{sibling_key}"),
                sibling_body
            )
            .await,
            200,
            "case {case:?}: PutObject({sibling_key:?}) must succeed"
        );
        let sibling_uri = uri(&bucket, sibling_key);

        let knowledge_base_id = knowledge_base(
            &router,
            case,
            &format!("kb-aoss-{position}"),
            row.model,
            &collection_arn,
            &index,
        )
        .await;
        let subject = data_source(
            &router,
            case,
            &knowledge_base_id,
            &format!("kb-aoss-subject-{position}"),
            &bucket,
            "ingested/",
            row.policy,
        )
        .await;
        let sibling = data_source(
            &router,
            case,
            &knowledge_base_id,
            &format!("kb-aoss-sibling-{position}"),
            &bucket,
            "retained/",
            "RETAIN",
        )
        .await;

        // Nothing has been ingested yet, so the collection holds nothing: what the counts
        // below report is the job's own work.
        assert_eq!(
            counted(&router, case, &collection_id, &index).await,
            0,
            "case {case:?}: expected the freshly created index to hold no document"
        );

        // The job's own accounting of what it did.
        let statistics = run_ingestion_job(&router, case, &knowledge_base_id, &subject).await;
        let documents = i64::try_from(row.documents).expect("a small count");
        for (member, expected) in [
            ("numberOfDocumentsScanned", documents),
            ("numberOfNewDocumentsIndexed", documents),
            ("numberOfModifiedDocumentsIndexed", 0),
            ("numberOfDocumentsDeleted", 0),
            ("numberOfDocumentsFailed", 0),
            ("numberOfDocumentsSkipped", 0),
        ] {
            assert_eq!(
                at(&statistics, member).as_i64().unwrap_or_default(),
                expected,
                "case {case:?}: expected statistics.{member} to be {expected}, got {statistics}"
            );
        }
        let sibling_statistics =
            run_ingestion_job(&router, case, &knowledge_base_id, &sibling).await;
        assert_eq!(
            at(&sibling_statistics, "numberOfNewDocumentsIndexed")
                .as_i64()
                .unwrap_or_default(),
            1,
            "case {case:?}: expected the sibling data source's one document indexed as new, got \
             {sibling_statistics}"
        );

        // ...and every document either job reported indexed is one the COLLECTION answers
        // for, in the fields the storage configuration maps.
        assert_eq!(
            counted(&router, case, &collection_id, &index).await,
            u64::try_from(documents + 1).expect("a small count"),
            "case {case:?}: expected the collection's own count to report one document per \
             indexed document"
        );
        let held = held_documents(&router, case, &collection_id, &index).await;
        let mut expected: Vec<(String, String)> = ingested.clone();
        expected.push((sibling_uri.clone(), sibling_body.to_string()));
        expected.sort();
        assert_eq!(
            held.iter().map(|(id, _)| id.clone()).collect::<Vec<String>>(),
            expected
                .iter()
                .map(|(source, _)| source.clone())
                .collect::<Vec<String>>(),
            "case {case:?}: expected the collection to hold one document per source document, \
             keyed by where that document lives, got {held:?}"
        );
        for ((id, source), (source_uri, body)) in held.iter().zip(expected.iter()) {
            assert_eq!(
                text(source, TEXT_FIELD),
                *body,
                "case {case:?}: expected the chunk of {id:?} to carry its source document's own \
                 text under the mapped text field, got {source}"
            );
            assert_eq!(
                text(source, SOURCE_URI_METADATA_KEY),
                *source_uri,
                "case {case:?}: expected the chunk of {id:?} to record where its source \
                 document lives, got {source}"
            );
            assert!(
                text(source, METADATA_FIELD).contains(source_uri.as_str()),
                "case {case:?}: expected the mapped metadata field to carry the chunk's \
                 metadata, got {source}"
            );
            let values = at(source, VECTOR_FIELD).as_array().cloned().unwrap_or_default();
            assert_eq!(
                values.len(),
                row.dimension,
                "case {case:?}: expected the embedding of {id:?} to carry one value per \
                 dimension the bound index declares, got {} values",
                values.len()
            );

            // ...and the embedding is one the index can be SEARCHED by: a
            // k-nearest-neighbour query for a stored vector answers that very document.
            let (status, neighbours) = data_plane(
                &router,
                &collection_id,
                "POST",
                &format!("/{index}/_search"),
                Some(&json!({
                    "query": { "knn": { VECTOR_FIELD: { "vector": values, "k": 1 } } },
                    "size": 1,
                })),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected the k-NN search to be served, got {status} with \
                 {neighbours}"
            );
            assert_eq!(
                text(&at(&neighbours, "hits.hits")[0], "_id"),
                *id,
                "case {case:?}: expected the nearest neighbour of {id:?}'s own vector to be \
                 {id:?}, got {neighbours}"
            );
        }

        // The data source's policy then decides what becomes of its documents — and of its
        // sibling's, which are never this delete's to take.
        let (status, deleted) = bedrock(
            &router,
            "DELETE",
            &format!("/knowledgebases/{knowledge_base_id}/datasources/{subject}"),
            None,
        )
        .await;
        assert_eq!(
            status, 202,
            "case {case:?}: expected DeleteDataSource accepted, got {status} with {deleted}"
        );
        let mut after: Vec<String> = vec![sibling_uri.clone()];
        if row.survives_deletion {
            after.extend(ingested.iter().map(|(source, _)| source.clone()));
        }
        after.sort();
        let remaining = held_documents(&router, case, &collection_id, &index).await;
        assert_eq!(
            remaining
                .iter()
                .map(|(id, _)| id.clone())
                .collect::<Vec<String>>(),
            after,
            "case {case:?}: under dataDeletionPolicy {} expected the collection to hold \
             {after:?} after the delete, got {remaining:?}",
            row.policy
        );
    }
}

/// RULE, the refusal side: writing a knowledge base's chunks into the collection's index is
/// its ROLE acting on that index, so a role the collection's `data` access policies no longer
/// grant the index write writes nothing — the job reports those documents failed rather than
/// indexed, and the collection's own read agrees.
#[tokio::test]
async fn an_ingestion_whose_role_lost_its_grant_writes_no_document() {
    let case = "the data access policy is narrowed to another principal after the create";
    let router = build_router();
    let collection = "kb-aoss-revoked";
    let index = "bedrock-knowledge-base-revoked";
    let bucket = "kb-aoss-revoked-source";

    admit_collections(&router, case).await;
    let version = grant_data_access(&router, case, collection, KB_ROLE).await;
    let (collection_id, collection_arn) =
        collection_with_index(&router, case, collection, index, 1024).await;

    assert_eq!(
        s3(&router, "PUT", &format!("/{bucket}"), "").await,
        200,
        "case {case:?}: CreateBucket({bucket:?}) must succeed"
    );
    assert_eq!(
        s3(
            &router,
            "PUT",
            &format!("/{bucket}/ingested/only.txt"),
            "the one source document"
        )
        .await,
        200,
        "case {case:?}: PutObject must succeed"
    );

    // The knowledge base is created while the grant holds — without it the create itself is
    // refused, which is that operation's own rule.
    let knowledge_base_id = knowledge_base(
        &router,
        case,
        "kb-aoss-revoked",
        "amazon.titan-embed-text-v2:0",
        &collection_arn,
        index,
    )
    .await;
    let subject = data_source(
        &router,
        case,
        &knowledge_base_id,
        "kb-aoss-revoked-subject",
        bucket,
        "ingested/",
        "RETAIN",
    )
    .await;

    // ...and the grant is then narrowed to a principal that is not the knowledge base's role,
    // so that role may no longer write the index.
    let (status, updated) = aoss(
        &router,
        "UpdateAccessPolicy",
        json!({
            "type": "data",
            "name": format!("{collection}-data"),
            "policyVersion": version,
            "policy": data_policy(collection, OTHER_ROLE).to_string(),
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected UpdateAccessPolicy accepted, got {status} with {updated}"
    );

    let statistics = run_ingestion_job(&router, case, &knowledge_base_id, &subject).await;
    for (member, expected) in [
        ("numberOfDocumentsScanned", 1),
        ("numberOfNewDocumentsIndexed", 0),
        ("numberOfDocumentsFailed", 1),
    ] {
        assert_eq!(
            at(&statistics, member).as_i64().unwrap_or_default(),
            expected,
            "case {case:?}: expected statistics.{member} to be {expected}, got {statistics}"
        );
    }
    assert_eq!(
        counted(&router, case, &collection_id, index).await,
        0,
        "case {case:?}: expected the collection to hold no document for a role whose policies \
         do not grant it the write"
    );
}
