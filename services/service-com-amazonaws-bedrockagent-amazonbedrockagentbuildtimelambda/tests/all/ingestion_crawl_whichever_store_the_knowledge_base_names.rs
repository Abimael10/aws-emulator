// SPDX-License-Identifier: CC0-1.0

//! What a completed ingestion job over an S3 data source accounts for when the
//! knowledge base keeps its vectors in a store whose vector plane this emulator
//! serves no index for.
//!
//! RULE: `StartIngestionJob` "begins a data ingestion job. Data sources are
//! ingested into your knowledge base", and `IngestionJobStatistics` is the
//! service's own accounting of that run — `numberOfDocumentsScanned` is "the
//! total number of source documents that were scanned. Includes new, updated,
//! and unchanged documents", `numberOfNewDocumentsIndexed` "the number of new
//! source documents in the data source that were successfully indexed", beside
//! the metadata, modified, deleted, failed and skipped counts
//! (`smithy_query(query="shape",shape="IngestionJobStatistics")`). So a job that
//! completes over a data source whose source holds N previously-unseen documents
//! reports N scanned and accounts for each of them, and the documents it took in
//! are documents of that data source — for a knowledge base backed by ANY of the
//! stores the modelled `KnowledgeBaseStorageType` admits, because the counters
//! measure the crawl rather than the store.
//!
//! The suites beside this one drive that rule for the two stores whose vector
//! plane this emulator serves (`tests/all/ingestion_crawl.rs` for an Amazon S3
//! Vectors index, `tests/all/ingestion_writes_the_bound_collection_index.rs` for
//! an OpenSearch Serverless collection), where a document is reported indexed
//! only once the owning store has really taken its vector. This one drives every
//! OTHER value of `KnowledgeBaseStorageType`: the knowledge base writes into a
//! store no API of this emulator reaches, which changes nothing about the crawl —
//! the job still scans the data source's objects, still accounts for them, and
//! the data source still holds them, so a populated bucket is never reported as
//! an empty one. Their `storageConfiguration` differs per row, which is why the
//! row builds its own knowledge base rather than the one the sibling harness
//! fixes to a single store.
//!
//! The rows differ along what the rule generalises over: which store the
//! knowledge base names, how many source documents its data source holds (none,
//! one, two, three, four), and whether a metadata file sits beside them — the one
//! the modelled `numberOfMetadataDocumentsScanned` counts separately.
//!
//! Every assertion goes through the router, so it exercises the real wire path
//! (route matching, model constraint validation, serialization) rather than
//! calling a handler directly, and every wait is a poll for the CONDITION — the
//! job's terminal status, the documents' terminal status — under a deadline
//! generous enough for any host. Nothing asserts how long a step took.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_bedrockagent_amazonbedrockagentbuildtimelambda::EmulatedService;
use service_com_amazonaws_s3_amazons3::state::{service_state as s3_state, Object};
use smithy_server_runtime::MultiServiceRouter;

/// A SigV4 `Authorization` header scoped to the `bedrock` signing name: the
/// routing key this service claims on.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/eu-west-1/bedrock/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=0123456789abcdef";

/// The Region the source bucket is held in.
const REGION: &str = "eu-west-1";

/// A role ARN matching the modelled `KnowledgeBaseRoleArn` pattern.
const KB_ROLE: &str =
    "arn:aws:iam::000000000000:role/service-role/AmazonBedrockExecutionRoleForKnowledgeBase";

/// An embedding model ARN matching the modelled `BedrockEmbeddingModelArn`, for
/// one of the models the service publishes.
const EMBEDDING_MODEL: &str =
    "arn:aws:bedrock:eu-west-1::foundation-model/amazon.titan-embed-text-v1";

/// The S3 bucket every data source here is crawled from.
const SOURCE_BUCKET: &str = "unserved-store-source-documents";

/// A secret ARN matching the modelled `SecretArn` pattern, for the stores whose
/// configuration requires one.
const CREDENTIALS_SECRET: &str =
    "arn:aws:secretsmanager:eu-west-1:000000000000:secret:bedrock-knowledge-base";

/// The field names a knowledge base maps its chunks onto, as AWS's
/// "Create a vector index" pages give them.
const VECTOR_FIELD: &str = "bedrock-knowledge-base-default-vector";
const TEXT_FIELD: &str = "AMAZON_BEDROCK_TEXT_CHUNK";
const METADATA_FIELD: &str = "AMAZON_BEDROCK_METADATA";

/// The suffix that makes an object a metadata file rather than a source
/// document: "a metadata file must be named `<source file name>.metadata.json`"
/// in Amazon Bedrock's knowledge-base documentation.
const METADATA_FILE_SUFFIX: &str = ".metadata.json";

/// How long a poll waits for a transition the clock owes (a job reaching
/// `COMPLETE`, a document reaching `INDEXED`): many times the spans the service
/// owes them, so a loaded machine still converges, and bounded so a hung poll
/// fails this test rather than hanging the suite.
const POLL_CAP: Duration = Duration::from_secs(60);

/// How long one poll attempt waits before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Every member of the modelled `IngestionJobStatistics`
/// (`smithy_query(query="shape",shape="IngestionJobStatistics")`), so a row reads
/// the job's WHOLE accounting rather than the few counters an assertion happens
/// to name.
const STATISTICS_MEMBERS: [&str; 8] = [
    "numberOfDocumentsScanned",
    "numberOfMetadataDocumentsScanned",
    "numberOfNewDocumentsIndexed",
    "numberOfModifiedDocumentsIndexed",
    "numberOfMetadataDocumentsModified",
    "numberOfDocumentsDeleted",
    "numberOfDocumentsFailed",
    "numberOfDocumentsSkipped",
];

/// One HTTP answer from the emulator, parsed.
#[derive(Debug)]
struct Answer {
    status: u16,
    body: Value,
}

impl Answer {
    fn member(&self, path: &[&str]) -> &Value {
        let mut at = &self.body;
        for key in path {
            at = &at[*key];
        }
        at
    }

    fn string(&self, path: &[&str]) -> String {
        self.member(path)
            .as_str()
            .unwrap_or_else(|| panic!("{path:?} is not a string in {:?}", self.body))
            .to_string()
    }

    /// Every counter of the job's statistics, as a client reads them.
    fn statistics(&self) -> BTreeMap<&'static str, i64> {
        STATISTICS_MEMBERS
            .into_iter()
            .map(|member| {
                let value = self
                    .member(&["ingestionJob", "statistics", member])
                    .as_i64()
                    .unwrap_or_else(|| {
                        panic!("statistics.{member} is not a number in {:?}", self.body)
                    });
                (member, value)
            })
            .collect()
    }
}

/// The counters a run must report: `scanned` source documents, of which `new`
/// were previously unseen and `modified` had changed, `metadata` metadata files
/// beside them and `deleted` documents the source no longer offers — with every
/// other member at the modelled zero.
fn expected_statistics(
    scanned: i64,
    new: i64,
    modified: i64,
    metadata: i64,
    deleted: i64,
) -> BTreeMap<&'static str, i64> {
    let mut expected: BTreeMap<&'static str, i64> =
        STATISTICS_MEMBERS.into_iter().map(|member| (member, 0)).collect();
    expected.insert("numberOfDocumentsScanned", scanned);
    expected.insert("numberOfNewDocumentsIndexed", new);
    expected.insert("numberOfModifiedDocumentsIndexed", modified);
    expected.insert("numberOfMetadataDocumentsScanned", metadata);
    expected.insert("numberOfDocumentsDeleted", deleted);
    expected
}

/// The emulator under test: one router over one shared engine, exactly as the
/// production router mounts this service. Amazon S3 holds the source documents in
/// its own store on that engine — the one map its `PutObject` writes and its
/// `GetObject` reads.
struct Emulator {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        {
            let state = s3_state(&*engine).expect("the object store on this engine");
            state.lock().create_bucket(SOURCE_BUCKET, REGION);
        }
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        Self { router, engine }
    }

    async fn call(&self, method: &str, uri: &str, body: Option<Value>) -> Answer {
        let bytes = body
            .map(|value| Bytes::from(serde_json::to_vec(&value).expect("serialize request body")))
            .unwrap_or_default();
        let request = http::Request::builder()
            .method(method)
            .uri(uri)
            .header("host", "bedrock-agent.eu-west-1.amazonaws.com")
            .header("authorization", AUTHORIZATION)
            .header("content-type", "application/json")
            .body(bytes)
            .expect("build the request");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let raw = response.into_body();
        let body: Value = if raw.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&raw).unwrap_or_else(|error| {
                panic!(
                    "{method} {uri} -> {status} body is not JSON ({error}): {}",
                    String::from_utf8_lossy(&raw)
                )
            })
        };
        Answer { status, body }
    }

    /// Store `body` at `key` of the source bucket, through Amazon S3's own store.
    fn hold_object(&self, key: &str, body: &str) {
        let state = s3_state(&*self.engine).expect("the object store on this engine");
        let mut store = state.lock();
        let written = store.put_object(
            SOURCE_BUCKET,
            key,
            Object::stored(body.as_bytes().to_vec(), Some("text/plain".to_string())),
        );
        assert!(
            written.is_some(),
            "case setup: the harness must be able to store the source document at {key:?}"
        );
    }

    /// Take the object at `key` out of the source bucket again.
    fn drop_object(&self, key: &str) {
        let state = s3_state(&*self.engine).expect("the object store on this engine");
        let mut store = state.lock();
        let removed = store.remove_object(SOURCE_BUCKET, key);
        assert!(
            removed.is_some(),
            "case setup: the harness must be able to delete the source document at {key:?}"
        );
    }

    /// Create a knowledge base whose vectors live in the store `storage` names,
    /// and answer its minted id.
    async fn knowledge_base(&self, name: &str, storage: &Value) -> String {
        let created = self
            .call(
                "PUT",
                "/knowledgebases/",
                Some(json!({
                    "name": name,
                    "roleArn": KB_ROLE,
                    "knowledgeBaseConfiguration": {
                        "type": "VECTOR",
                        "vectorKnowledgeBaseConfiguration": {
                            "embeddingModelArn": EMBEDDING_MODEL,
                        },
                    },
                    "storageConfiguration": storage,
                })),
            )
            .await;
        assert_eq!(
            created.status, 202,
            "case setup: expected CreateKnowledgeBase -> 202, got {created:?}"
        );
        created.string(&["knowledgeBase", "knowledgeBaseId"])
    }

    /// Attach an S3 data source over `prefix` of the source bucket and answer its
    /// minted id.
    async fn data_source(&self, knowledge_base_id: &str, name: &str, prefix: &str) -> String {
        let created = self
            .call(
                "PUT",
                &format!("/knowledgebases/{knowledge_base_id}/datasources/"),
                Some(json!({
                    "name": name,
                    "dataSourceConfiguration": {
                        "type": "S3",
                        "s3Configuration": {
                            "bucketArn": format!("arn:aws:s3:::{SOURCE_BUCKET}"),
                            "inclusionPrefixes": [prefix],
                        },
                    },
                })),
            )
            .await;
        assert_eq!(
            created.status, 200,
            "case setup: expected CreateDataSource -> 200, got {created:?}"
        );
        created.string(&["dataSource", "dataSourceId"])
    }

    /// Start an ingestion job and POLL `GetIngestionJob` until the clock has
    /// completed it, answering the completed job as that read reports it.
    async fn run_ingestion_job(&self, knowledge_base_id: &str, data_source_id: &str) -> Answer {
        let started = self
            .call(
                "PUT",
                &format!(
                    "/knowledgebases/{knowledge_base_id}/datasources/{data_source_id}\
                     /ingestionjobs/"
                ),
                Some(json!({})),
            )
            .await;
        assert_eq!(
            started.status, 202,
            "case setup: expected StartIngestionJob -> 202, got {started:?}"
        );
        let ingestion_job_id = started.string(&["ingestionJob", "ingestionJobId"]);
        let deadline = Instant::now() + POLL_CAP;
        loop {
            let read = self
                .call(
                    "GET",
                    &format!(
                        "/knowledgebases/{knowledge_base_id}/datasources/{data_source_id}\
                         /ingestionjobs/{ingestion_job_id}"
                    ),
                    None,
                )
                .await;
            assert_eq!(
                read.status, 200,
                "case setup: expected GetIngestionJob -> 200, got {read:?}"
            );
            if read.string(&["ingestionJob", "status"]) == "COMPLETE" {
                return read;
            }
            assert!(
                Instant::now() < deadline,
                "case setup: the clock must complete job {ingestion_job_id}, last read {read:?}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// The source URIs `ListKnowledgeBaseDocuments` reports for the data source
    /// once it holds `expected` documents and every one of them has reached its
    /// terminal `INDEXED` status, in URI order.
    ///
    /// The count is polled for as well as the statuses, so a listing that does
    /// not hold the documents yet is waited for rather than read as a listing of
    /// none.
    async fn indexed_document_uris(
        &self,
        knowledge_base_id: &str,
        data_source_id: &str,
        expected: usize,
    ) -> Vec<String> {
        let deadline = Instant::now() + POLL_CAP;
        loop {
            let listed = self
                .call(
                    "POST",
                    &format!(
                        "/knowledgebases/{knowledge_base_id}/datasources/{data_source_id}\
                         /documents"
                    ),
                    Some(json!({})),
                )
                .await;
            assert_eq!(
                listed.status, 200,
                "case setup: expected ListKnowledgeBaseDocuments -> 200, got {listed:?}"
            );
            let details = listed
                .member(&["documentDetails"])
                .as_array()
                .unwrap_or_else(|| panic!("documentDetails is a list in {:?}", listed.body))
                .clone();
            if details.len() == expected
                && details
                    .iter()
                    .all(|detail| detail["status"] == json!("INDEXED"))
            {
                let mut uris: Vec<String> = details
                    .iter()
                    .map(|detail| {
                        detail["identifier"]["s3"]["uri"]
                            .as_str()
                            .unwrap_or_else(|| panic!("a document identifies an object: {detail:?}"))
                            .to_string()
                    })
                    .collect();
                uris.sort();
                return uris;
            }
            assert!(
                Instant::now() < deadline,
                "case setup: the data source must hold {expected} INDEXED document(s), \
                 last read {listed:?}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
}

/// The `s3://` URI of one source document, as a knowledge-base document names it.
fn uri(key: &str) -> String {
    format!("s3://{SOURCE_BUCKET}/{key}")
}

/// One row: the store a knowledge base keeps its vectors in, and the source its
/// data source crawls.
struct StoreRow {
    /// The `KnowledgeBaseStorageType` value this row names, which is the case.
    case: &'static str,
    /// The whole `storageConfiguration` the knowledge base declares.
    storage: Value,
    /// The `inclusionPrefixes` entry that makes this row's documents its own.
    prefix: &'static str,
    /// The source documents under that prefix: the key beneath it, and the text.
    documents: Vec<(&'static str, &'static str)>,
    /// The metadata files beside them, keyed beneath the same prefix.
    metadata_files: Vec<&'static str>,
}

/// Every value of the modelled `KnowledgeBaseStorageType`
/// (`smithy_query(query="shape",shape="KnowledgeBaseStorageType")`) whose vector
/// plane this emulator serves no index for — that is, all of them but
/// `S3_VECTORS` and `OPENSEARCH_SERVERLESS`, which the sibling suites drive — each
/// with the configuration member its own `type` names
/// (`smithy_query(query="shape",shape="StorageConfiguration")`) and a source of
/// its own.
fn rows() -> Vec<StoreRow> {
    vec![
        StoreRow {
            case: "PINECONE",
            storage: json!({
                "type": "PINECONE",
                "pineconeConfiguration": {
                    "connectionString": "https://kb-index.svc.eu-west-1-aws.pinecone.io",
                    "credentialsSecretArn": CREDENTIALS_SECRET,
                    "namespace": "bedrock-knowledge-base",
                    "fieldMapping": {
                        "textField": TEXT_FIELD,
                        "metadataField": METADATA_FIELD,
                    },
                },
            }),
            prefix: "pinecone/",
            documents: vec![("only.txt", "the one object the crawl must scan")],
            metadata_files: Vec::new(),
        },
        StoreRow {
            case: "RDS",
            storage: json!({
                "type": "RDS",
                "rdsConfiguration": {
                    "resourceArn": "arn:aws:rds:eu-west-1:000000000000:cluster:kb-vectors",
                    "credentialsSecretArn": CREDENTIALS_SECRET,
                    "databaseName": "bedrock_knowledge_base",
                    "tableName": "bedrock_integration.bedrock_kb",
                    "fieldMapping": {
                        "primaryKeyField": "id",
                        "vectorField": "embedding",
                        "textField": "chunks",
                        "metadataField": "metadata",
                    },
                },
            }),
            prefix: "rds/",
            documents: vec![
                ("first.txt", "the first document"),
                ("second.txt", "the second document"),
            ],
            metadata_files: Vec::new(),
        },
        StoreRow {
            case: "MONGO_DB_ATLAS",
            storage: json!({
                "type": "MONGO_DB_ATLAS",
                "mongoDbAtlasConfiguration": {
                    "endpoint": "https://kb-cluster.mongocluster.example.com",
                    "databaseName": "bedrock",
                    "collectionName": "knowledge-base",
                    "vectorIndexName": "kb-vector-index",
                    "credentialsSecretArn": CREDENTIALS_SECRET,
                    "fieldMapping": {
                        "vectorField": VECTOR_FIELD,
                        "textField": TEXT_FIELD,
                        "metadataField": METADATA_FIELD,
                    },
                },
            }),
            prefix: "mongo/",
            documents: vec![
                ("alpha.txt", "alpha"),
                ("beta.txt", "beta"),
                ("gamma.txt", "gamma"),
            ],
            metadata_files: Vec::new(),
        },
        StoreRow {
            case: "REDIS_ENTERPRISE_CLOUD",
            storage: json!({
                "type": "REDIS_ENTERPRISE_CLOUD",
                "redisEnterpriseCloudConfiguration": {
                    "endpoint": "https://kb-database.eu-west-1.ec2.cloud.redislabs.example",
                    "vectorIndexName": "kb-vector-index",
                    "credentialsSecretArn": CREDENTIALS_SECRET,
                    "fieldMapping": {
                        "vectorField": VECTOR_FIELD,
                        "textField": TEXT_FIELD,
                        "metadataField": METADATA_FIELD,
                    },
                },
            }),
            // The empty row: a source that really holds nothing is the one case
            // the modelled zero defaults are the right answer for, so a crawl of
            // a populated source must differ from it.
            prefix: "redis/",
            documents: Vec::new(),
            metadata_files: Vec::new(),
        },
        StoreRow {
            case: "NEPTUNE_ANALYTICS",
            storage: json!({
                "type": "NEPTUNE_ANALYTICS",
                "neptuneAnalyticsConfiguration": {
                    "graphArn": "arn:aws:neptune-graph:eu-west-1:000000000000:graph/g-abcdef0123",
                    "fieldMapping": {
                        "textField": TEXT_FIELD,
                        "metadataField": METADATA_FIELD,
                    },
                },
            }),
            prefix: "neptune/",
            documents: vec![
                ("one.txt", "one"),
                ("two.txt", "two"),
                ("three.txt", "three"),
                ("four.txt", "four"),
            ],
            metadata_files: Vec::new(),
        },
        StoreRow {
            case: "OPENSEARCH_MANAGED_CLUSTER",
            storage: json!({
                "type": "OPENSEARCH_MANAGED_CLUSTER",
                "opensearchManagedClusterConfiguration": {
                    "domainEndpoint": "https://kb-domain.eu-west-1.es.amazonaws.com",
                    "domainArn": "arn:aws:es:eu-west-1:000000000000:domain/kb-domain",
                    "vectorIndexName": "kb-managed-index",
                    "fieldMapping": {
                        "vectorField": VECTOR_FIELD,
                        "textField": TEXT_FIELD,
                        "metadataField": METADATA_FIELD,
                    },
                },
            }),
            // A metadata file beside the source document: the two the modelled
            // statistics count separately.
            prefix: "managed/",
            documents: vec![("described.txt", "the document the metadata file describes")],
            metadata_files: vec!["described.txt.metadata.json"],
        },
    ]
}

/// A completed crawl of a non-empty S3 data source accounts for the documents it
/// scanned and the data source holds them, whichever store the knowledge base
/// keeps its vectors in — so a populated source is never reported as an empty
/// one.
#[tokio::test]
async fn a_completed_crawl_accounts_for_its_source_documents_whichever_store_is_named() {
    let emulator = Emulator::new();
    for (position, row) in rows().iter().enumerate() {
        let mut expected_uris: Vec<String> = Vec::new();
        for (key, text) in &row.documents {
            let key = format!("{}{key}", row.prefix);
            emulator.hold_object(&key, text);
            expected_uris.push(uri(&key));
        }
        for key in &row.metadata_files {
            assert!(
                key.ends_with(METADATA_FILE_SUFFIX),
                "case {}: a metadata file is named <source file name>{METADATA_FILE_SUFFIX}, \
                 got {key:?}",
                row.case
            );
            emulator.hold_object(
                &format!("{}{key}", row.prefix),
                "{\"metadataAttributes\":{\"topic\":\"crawl\"}}",
            );
        }
        expected_uris.sort();

        let knowledge_base_id = emulator
            .knowledge_base(&format!("unserved-store-kb-{position}"), &row.storage)
            .await;
        let data_source_id = emulator
            .data_source(
                &knowledge_base_id,
                &format!("unserved-store-ds-{position}"),
                row.prefix,
            )
            .await;
        let completed = emulator
            .run_ingestion_job(&knowledge_base_id, &data_source_id)
            .await;

        let documents = i64::try_from(row.documents.len()).expect("a small count");
        let metadata = i64::try_from(row.metadata_files.len()).expect("a small count");
        let expected = expected_statistics(documents, documents, 0, metadata, 0);
        let measured = completed.statistics();
        assert_eq!(
            measured, expected,
            "case {}: expected a completed crawl of {documents} source document(s) and \
             {metadata} metadata file(s) to report {expected:?}, got {measured:?}",
            row.case
        );

        // The documents the job accounted for really are documents of the data
        // source, reported with the terminal status the model declares for an
        // ingested one.
        let listed = emulator
            .indexed_document_uris(&knowledge_base_id, &data_source_id, expected_uris.len())
            .await;
        assert_eq!(
            listed, expected_uris,
            "case {}: expected the crawled documents to be the data source's own, got {listed:?}",
            row.case
        );
    }
}

/// A later crawl over the same data source accounts for what CHANGED: an
/// unchanged source document is scanned and nothing more, a rewritten one is
/// modified, and one the source no longer offers is deleted — on this path too,
/// where no store of this emulator holds the knowledge base's vectors.
#[tokio::test]
async fn a_later_crawl_accounts_for_what_changed_since_the_previous_one() {
    let emulator = Emulator::new();
    let storage = json!({
        "type": "PINECONE",
        "pineconeConfiguration": {
            "connectionString": "https://kb-index.svc.eu-west-1-aws.pinecone.io",
            "credentialsSecretArn": CREDENTIALS_SECRET,
            "fieldMapping": {
                "textField": TEXT_FIELD,
                "metadataField": METADATA_FIELD,
            },
        },
    });
    emulator.hold_object("changing/kept.txt", "unchanged across every run");
    emulator.hold_object("changing/rewritten.txt", "the first revision");
    let knowledge_base_id = emulator
        .knowledge_base("unserved-store-changes-kb", &storage)
        .await;
    let data_source_id = emulator
        .data_source(&knowledge_base_id, "unserved-store-changes-ds", "changing/")
        .await;

    let first = emulator
        .run_ingestion_job(&knowledge_base_id, &data_source_id)
        .await;
    let first_expected = expected_statistics(2, 2, 0, 0, 0);
    assert_eq!(
        first.statistics(),
        first_expected,
        "case the first run: expected {first_expected:?}, got {:?}",
        first.statistics()
    );

    // Nothing has changed under the data source, so the second run scans both
    // documents and indexes neither: "includes new, updated, and unchanged
    // documents" is what makes a scan of an unchanged document a scan.
    let unchanged = emulator
        .run_ingestion_job(&knowledge_base_id, &data_source_id)
        .await;
    let unchanged_expected = expected_statistics(2, 0, 0, 0, 0);
    assert_eq!(
        unchanged.statistics(),
        unchanged_expected,
        "case an unchanged source: expected {unchanged_expected:?}, got {:?}",
        unchanged.statistics()
    );

    // One document is rewritten and the other is gone from the source.
    emulator.hold_object("changing/rewritten.txt", "the second revision, longer than the first");
    emulator.drop_object("changing/kept.txt");
    let changed = emulator
        .run_ingestion_job(&knowledge_base_id, &data_source_id)
        .await;
    let changed_expected = expected_statistics(1, 0, 1, 0, 1);
    assert_eq!(
        changed.statistics(),
        changed_expected,
        "case a rewritten and a departed document: expected {changed_expected:?}, got {:?}",
        changed.statistics()
    );

    // And the data source holds exactly the document its source still offers.
    let listed = emulator
        .indexed_document_uris(&knowledge_base_id, &data_source_id, 1)
        .await;
    assert_eq!(
        listed,
        vec![uri("changing/rewritten.txt")],
        "case after the third run: expected the data source to hold only the document its \
         source still offers, got {listed:?}"
    );
}
