// SPDX-License-Identifier: CC0-1.0

//! What a document delete takes out of the knowledge base's BOUND VECTOR INDEX,
//! end to end over HTTP.
//!
//! THE RULE UNDER TEST: a document whose delete `DeleteKnowledgeBaseDocuments`
//! accepts is no longer part of what the knowledge base holds, so the chunk its
//! ingestion wrote into the index the knowledge base's `storageConfiguration`
//! binds it to goes with it — and NOTHING else in that index moves: every
//! document the delete did not name keeps the chunk and the vector it had,
//! whether it belongs to the same data source or to a sibling data source of the
//! same knowledge base, and an identifier the data source holds nothing for
//! takes nothing out of the index.
//!
//! This is what makes a delete stick where a retrieval reads: the Bedrock Agent
//! RUNTIME plane answers a retrieval out of that index, so a document still
//! holding a vector there would keep being retrieved after it had left every
//! listing of its data source.
//!
//! The rows differ along what the rule generalises over: the kind of content the
//! data source holds (each value of the modelled `ContentDataSourceType` a
//! document can be pushed as), how many documents the data source holds, which
//! of them one delete names (the first, the last ones, all of them) and whether
//! the delete also names an identifier the data source never held.
//!
//! Every call goes through the router, so the real wire path (route matching,
//! model constraint validation, serialization) is exercised rather than a
//! handler; the index is read in the service that OWNS it, which is the only
//! place a vector really exists.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_bedrockagent_amazonbedrockagentbuildtimelambda::state::{
    document_vector_key, DocumentKey,
};
use service_com_amazonaws_bedrockagent_amazonbedrockagentbuildtimelambda::EmulatedService;
use service_com_amazonaws_s3_amazons3::state::{service_state as s3_state, Object};
use smithy_server_runtime::MultiServiceRouter;

/// A SigV4 `Authorization` header scoped to the `bedrock` signing name: the
/// routing key this service claims on.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/eu-west-1/bedrock/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=0123456789abcdef";

/// A role ARN matching the modelled `KnowledgeBaseRoleArn` pattern.
const KB_ROLE: &str =
    "arn:aws:iam::000000000000:role/service-role/AmazonBedrockExecutionRoleForKnowledgeBase";

/// An embedding model ARN matching the modelled `BedrockEmbeddingModelArn`.
const EMBEDDING_MODEL: &str =
    "arn:aws:bedrock:eu-west-1::foundation-model/amazon.titan-embed-text-v2:0";

/// The vector bucket and index every knowledge base of this suite binds to.
const VECTOR_BUCKET: &str = "arn:aws:s3vectors:::bucket/document-unindex-vectors";
const VECTOR_INDEX: &str = "document-unindex-index";

/// The object bucket the S3 rows' documents live in, and the region it is in.
const SOURCE_BUCKET: &str = "document-unindex-docs";
const REGION: &str = "eu-west-1";

/// How long a poll keeps looking for a change the delete chain owes the index
/// before giving up.
///
/// Deliberately far past the spans this store owes a delete: what is checked is
/// that the chunk leaves the index at all, never how fast, so a loaded host
/// converges here instead of failing.
const POLL_CAP: Duration = Duration::from_secs(120);

/// How often a poll looks again while it waits for the condition.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The statuses the model documents for a delete in flight: the submitted delete
/// and the delete under way.
///
/// Source: the `status` member of the `KnowledgeBaseDocumentDetail` shape of
/// this service's Smithy model
/// (`smithy_query(query="doc",shape="KnowledgeBaseDocumentDetail",member="status")`).
const DELETION_IN_FLIGHT: [&str; 2] = ["DELETING", "DELETE_IN_PROGRESS"];

/// The status that same member documents for a document the service holds
/// nothing for ("The document wasn't found").
const NOT_FOUND: &str = "NOT_FOUND";

/// One HTTP answer from the emulator, parsed.
#[derive(Debug)]
struct Answer {
    status: u16,
    body: Value,
}

impl Answer {
    /// The response's `documentDetails`, which every document operation of this
    /// service answers with.
    fn details(&self) -> &Vec<Value> {
        self.body["documentDetails"]
            .as_array()
            .unwrap_or_else(|| panic!("documentDetails is not a list in {:?}", self.body))
    }

    /// The detail reported for the document `identifier` names, whatever
    /// position it was reported in.
    fn detail_of(&self, identifier: &Value) -> &Value {
        self.details()
            .iter()
            .find(|detail| &detail["identifier"] == identifier)
            .unwrap_or_else(|| {
                panic!(
                    "no documentDetails entry for {identifier:?} in {:?}",
                    self.body
                )
            })
    }

    /// The `status` the response reports for `identifier`.
    fn status_of(&self, identifier: &Value) -> String {
        self.detail_of(identifier)["status"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }
}

/// The emulator under test: one router over one shared engine, exactly as the
/// production router mounts this service.
struct Emulator {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        // The vector store the knowledge base binds to and the object bucket the
        // S3 documents live in: both set up before anything names them, as a
        // caller sets them up against AWS.
        crate::vector_store_fixture::seed_index(
            &engine,
            VECTOR_BUCKET,
            VECTOR_INDEX,
            crate::vector_store_fixture::EMBEDDING_DIMENSION,
        );
        {
            let state = s3_state(&*engine).expect("the object store on this engine");
            state.lock().create_bucket(SOURCE_BUCKET, REGION);
        }
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        Self { router, engine }
    }

    async fn call(&self, method: &str, uri: &str, body: Value) -> Answer {
        let bytes = Bytes::from(serde_json::to_vec(&body).expect("serialize request body"));
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

    /// Create a knowledge base bound to this suite's index and answer its minted
    /// `knowledgeBaseId`.
    async fn knowledge_base(&self, name: &str) -> String {
        let created = self
            .call(
                "PUT",
                "/knowledgebases/",
                json!({
                    "name": name,
                    "roleArn": KB_ROLE,
                    "knowledgeBaseConfiguration": {
                        "type": "VECTOR",
                        "vectorKnowledgeBaseConfiguration": {
                            "embeddingModelArn": EMBEDDING_MODEL,
                        },
                    },
                    "storageConfiguration": {
                        "type": "S3_VECTORS",
                        "s3VectorsConfiguration": {
                            "vectorBucketArn": VECTOR_BUCKET,
                            "indexName": VECTOR_INDEX,
                        },
                    },
                }),
            )
            .await;
        assert_eq!(
            created.status, 202,
            "case setup: expected CreateKnowledgeBase -> 202, got {created:?}"
        );
        created.body["knowledgeBase"]["knowledgeBaseId"]
            .as_str()
            .expect("a minted knowledgeBaseId")
            .to_string()
    }

    /// Attach a data source holding the kind of content `content_kind` names and
    /// answer its minted `dataSourceId`.
    async fn data_source(
        &self,
        knowledge_base_id: &str,
        name: &str,
        content_kind: &str,
    ) -> String {
        let mut configuration = json!({ "type": content_kind });
        if content_kind == "S3" {
            configuration["s3Configuration"] =
                json!({ "bucketArn": format!("arn:aws:s3:::{SOURCE_BUCKET}") });
        }
        let created = self
            .call(
                "PUT",
                &format!("/knowledgebases/{knowledge_base_id}/datasources/"),
                json!({ "name": name, "dataSourceConfiguration": configuration }),
            )
            .await;
        assert_eq!(
            created.status, 200,
            "case setup: expected CreateDataSource -> 200, got {created:?}"
        );
        created.body["dataSource"]["dataSourceId"]
            .as_str()
            .expect("a minted dataSourceId")
            .to_string()
    }

    async fn ingest(
        &self,
        knowledge_base_id: &str,
        data_source_id: &str,
        documents: Vec<Value>,
    ) -> Answer {
        self.call(
            "PUT",
            &format!("/knowledgebases/{knowledge_base_id}/datasources/{data_source_id}/documents"),
            json!({ "documents": documents }),
        )
        .await
    }

    async fn delete_documents(
        &self,
        knowledge_base_id: &str,
        data_source_id: &str,
        identifiers: Vec<Value>,
    ) -> Answer {
        self.call(
            "POST",
            &format!(
                "/knowledgebases/{knowledge_base_id}/datasources/{data_source_id}\
                 /documents/deleteDocuments"
            ),
            json!({ "documentIdentifiers": identifiers }),
        )
        .await
    }

    /// Store `text` at `key` of the source bucket, through Amazon S3's own
    /// store — the one map its `PutObject` writes and its `GetObject` reads.
    fn hold_object(&self, key: &str, text: &str) {
        let state = s3_state(&*self.engine).expect("the object store on this engine");
        let mut store = state.lock();
        let written = store.put_object(
            SOURCE_BUCKET,
            key,
            Object::stored(text.as_bytes().to_vec(), Some("text/plain".to_string())),
        );
        assert!(
            written.is_some(),
            "the harness must be able to store the source document at {key:?}"
        );
    }

    /// The keys the bound index holds vectors under, as an `s3vectors`
    /// `ListVectors` of it reports them.
    fn index_keys(&self) -> BTreeSet<String> {
        crate::vector_store_fixture::index_vector_keys(&self.engine, VECTOR_BUCKET, VECTOR_INDEX)
            .into_iter()
            .collect()
    }

    /// The vector the bound index holds under `key`, or `None` when it holds
    /// none.
    fn vector_of(&self, key: &str) -> Option<Vec<f32>> {
        crate::vector_store_fixture::indexed_vector(&self.engine, VECTOR_BUCKET, VECTOR_INDEX, key)
    }

    /// Read the bound index until it holds a vector under none of `keys`, or
    /// [`POLL_CAP`] passes — the delete chain is carried by the CLOCK, so this is
    /// how the index converges.
    ///
    /// Answers the keys the index holds and whether the deadline ran out first,
    /// so a delete chain that never got there is reported as that rather than as
    /// a delete that took the wrong keys.
    async fn poll_until_unindexed(&self, keys: &[String]) -> (BTreeSet<String>, bool) {
        let deadline = Instant::now() + POLL_CAP;
        loop {
            let held = self.index_keys();
            if keys.iter().all(|key| !held.contains(key)) {
                return (held, false);
            }
            if Instant::now() >= deadline {
                return (held, true);
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
}

/// A document whose content is of the kind `content_kind` names, identified by
/// `id` and carrying `text`.
///
/// Source: the `ContentDataSourceType` enum and the `DocumentContent` structure
/// of this service's Smithy model
/// (`smithy_query(query="shape",shape="ContentDataSourceType,DocumentContent")`):
/// the kind names which sibling member carries the document's own content.
fn document_of_kind(content_kind: &str, id: &str, text: &str) -> Value {
    match content_kind {
        "CUSTOM" => json!({
            "content": {
                "dataSourceType": "CUSTOM",
                "custom": {
                    "customDocumentIdentifier": { "id": id },
                    "sourceType": "IN_LINE",
                    "inlineContent": {
                        "type": "TEXT",
                        "textContent": { "data": text },
                    },
                },
            },
        }),
        "S3" => json!({
            "content": {
                "dataSourceType": "S3",
                "s3": { "s3Location": { "uri": id } },
            },
        }),
        other => panic!("no document shape for the content kind {other}"),
    }
}

/// The document a request of that kind names `id` with, as the store keys it.
fn key_of_kind(content_kind: &str, id: &str) -> DocumentKey {
    match content_kind {
        "CUSTOM" => DocumentKey::Custom(id.to_string()),
        "S3" => DocumentKey::S3(id.to_string()),
        other => panic!("no document key for the content kind {other}"),
    }
}

/// The modelled `DocumentIdentifier` for that same document.
fn identifier_of_kind(content_kind: &str, id: &str) -> Value {
    match content_kind {
        "CUSTOM" => json!({ "dataSourceType": "CUSTOM", "custom": { "id": id } }),
        "S3" => json!({ "dataSourceType": "S3", "s3": { "uri": id } }),
        other => panic!("no identifier shape for the content kind {other}"),
    }
}

/// The id a document of that kind is given: a custom document carries its own
/// id, an S3 document is the object at its URI.
fn document_id(content_kind: &str, name: &str) -> String {
    match content_kind {
        "S3" => format!("s3://{SOURCE_BUCKET}/{name}.txt"),
        _ => name.to_string(),
    }
}

/// The object key of an S3 document's URI, which is where its bytes are held.
fn object_key(uri: &str) -> String {
    uri.strip_prefix(&format!("s3://{SOURCE_BUCKET}/"))
        .expect("an S3 document of this suite lives in the source bucket")
        .to_string()
}

/// RULE: an accepted document delete takes that document's chunk out of the
/// bound vector index and leaves every other chunk of the index exactly as it
/// was.
#[tokio::test]
async fn an_accepted_delete_takes_the_documents_chunk_out_of_the_bound_index() {
    let emulator = Emulator::new();
    let knowledge_base_id = emulator.knowledge_base("document-unindex-kb").await;

    // A SIBLING data source of the same knowledge base, whose documents no
    // delete here names: its chunks live in the same bound index, so a delete
    // that took more than the documents it named shows up on these.
    let sibling = emulator
        .data_source(&knowledge_base_id, "unindex-sibling", "CUSTOM")
        .await;
    let sibling_ids = ["unindex-sibling-a", "unindex-sibling-b"];
    let ingested = emulator
        .ingest(
            &knowledge_base_id,
            &sibling,
            sibling_ids
                .iter()
                .map(|id| document_of_kind("CUSTOM", id, &format!("the text of {id}")))
                .collect(),
        )
        .await;
    assert_eq!(
        ingested.status, 202,
        "case setup: expected IngestKnowledgeBaseDocuments -> 202, got {ingested:?}"
    );
    let sibling_vectors: Vec<(String, Vec<f32>)> = sibling_ids
        .iter()
        .map(|id| {
            let key = document_vector_key(&key_of_kind("CUSTOM", id));
            let vector = emulator.vector_of(&key).unwrap_or_else(|| {
                panic!("case setup: the bound index holds a vector for {key:?}")
            });
            (key, vector)
        })
        .collect();

    // Every key the bound index is expected to hold, kept up to date row by row:
    // what each ingestion adds, less what each delete is expected to take.
    let mut expected: BTreeSet<String> = sibling_vectors
        .iter()
        .map(|(key, _)| key.clone())
        .collect();

    // (case, the kind of content the data source holds, how many documents it
    // takes, which of them one delete names, whether the delete also names an
    // identifier the data source never held).
    let cases: Vec<(&str, &str, usize, Vec<usize>, bool)> = vec![
        ("the first of three custom documents", "CUSTOM", 3, vec![0], false),
        (
            "the last two of three custom documents",
            "CUSTOM",
            3,
            vec![1, 2],
            false,
        ),
        ("the only custom document", "CUSTOM", 1, vec![0], false),
        ("one of two S3 documents", "S3", 2, vec![0], false),
        (
            "every S3 document of the data source",
            "S3",
            2,
            vec![0, 1],
            false,
        ),
        (
            "a custom document beside an identifier nothing is held for",
            "CUSTOM",
            2,
            vec![1],
            true,
        ),
    ];

    for (row, (case, content_kind, held, doomed_positions, names_an_absent_document)) in
        cases.into_iter().enumerate()
    {
        let data_source_id = emulator
            .data_source(
                &knowledge_base_id,
                &format!("unindex-{row}"),
                content_kind,
            )
            .await;
        let ids: Vec<String> = (0..held)
            .map(|position| document_id(content_kind, &format!("unindexed-{row}-{position}")))
            .collect();
        // An S3 document IS the object it names, so its bytes are in the object
        // store before the ingestion reads them.
        if content_kind == "S3" {
            for id in &ids {
                emulator.hold_object(&object_key(id), &format!("the text of {id}"));
            }
        }

        let ingested = emulator
            .ingest(
                &knowledge_base_id,
                &data_source_id,
                ids.iter()
                    .map(|id| document_of_kind(content_kind, id, &format!("the text of {id}")))
                    .collect(),
            )
            .await;
        assert_eq!(
            ingested.status, 202,
            "case {case}: expected IngestKnowledgeBaseDocuments -> 202, got {ingested:?}"
        );

        // The chunk of every ingested document is in the index: that is what the
        // delete has to take out again.
        let keys: Vec<String> = ids
            .iter()
            .map(|id| document_vector_key(&key_of_kind(content_kind, id)))
            .collect();
        let before: Vec<(String, Vec<f32>)> = keys
            .iter()
            .map(|key| {
                let vector = emulator.vector_of(key).unwrap_or_else(|| {
                    panic!("case {case}: the bound index holds a vector for {key:?}")
                });
                (key.clone(), vector)
            })
            .collect();
        expected.extend(keys.iter().cloned());
        assert_eq!(
            emulator.index_keys(),
            expected,
            "case {case}: expected the ingestion to add exactly its own documents' chunks"
        );

        let doomed_keys: Vec<String> = doomed_positions
            .iter()
            .map(|position| keys[*position].clone())
            .collect();
        let mut identifiers: Vec<Value> = doomed_positions
            .iter()
            .map(|position| identifier_of_kind(content_kind, &ids[*position]))
            .collect();
        let absent = document_id(content_kind, &format!("unindexed-{row}-absent"));
        if names_an_absent_document {
            identifiers.push(identifier_of_kind(content_kind, &absent));
        }

        let accepted = emulator
            .delete_documents(&knowledge_base_id, &data_source_id, identifiers.clone())
            .await;
        assert_eq!(
            accepted.status, 202,
            "case {case}: expected DeleteKnowledgeBaseDocuments -> 202, got {accepted:?}"
        );
        for position in &doomed_positions {
            let identifier = identifier_of_kind(content_kind, &ids[*position]);
            let status = accepted.status_of(&identifier);
            assert!(
                DELETION_IN_FLIGHT.contains(&status.as_str()),
                "case {case}: expected one of {DELETION_IN_FLIGHT:?} for the accepted delete, \
                 got {:?}",
                accepted.detail_of(&identifier)
            );
        }
        if names_an_absent_document {
            let identifier = identifier_of_kind(content_kind, &absent);
            assert_eq!(
                accepted.status_of(&identifier),
                NOT_FOUND,
                "case {case}: expected {NOT_FOUND} for an identifier nothing is held for, \
                 got {:?}",
                accepted.detail_of(&identifier)
            );
        }

        // The chunks of the named documents leave the index, and only those: the
        // whole index is checked, so a delete that took a survivor's chunk or a
        // sibling data source's chunk fails here.
        for key in &doomed_keys {
            expected.remove(key);
        }
        let (held_keys, timed_out) = emulator.poll_until_unindexed(&doomed_keys).await;
        assert!(
            !timed_out,
            "case {case}: expected the chunks of {doomed_keys:?} to leave the bound index, \
             still held after the poll deadline: {held_keys:?}"
        );
        assert_eq!(
            held_keys, expected,
            "case {case}: expected the bound index to hold exactly the chunks of the documents \
             the delete did not name, got {held_keys:?}"
        );
        for (key, vector) in &before {
            if doomed_keys.contains(key) {
                assert!(
                    emulator.vector_of(key).is_none(),
                    "case {case}: expected no vector under {key:?} once its delete was accepted, \
                     got {:?}",
                    emulator.vector_of(key)
                );
            } else {
                assert_eq!(
                    emulator.vector_of(key).as_ref(),
                    Some(vector),
                    "case {case}: expected the chunk of the document the delete did not name to \
                     be untouched under {key:?}"
                );
            }
        }
        // And the sibling data source's chunks, which no delete here named, are
        // the ones its own ingestion wrote.
        for (key, vector) in &sibling_vectors {
            assert_eq!(
                emulator.vector_of(key).as_ref(),
                Some(vector),
                "case {case}: expected the sibling data source's chunk under {key:?} to be \
                 untouched"
            );
        }
    }
}
