// SPDX-License-Identifier: CC0-1.0

//! What the COLLECTION of a data source's documents reports for a deletion that has run
//! to its end, end to end over HTTP.
//!
//! One rule is pinned here: a document on its way out of a data source is still one of the
//! documents `ListKnowledgeBaseDocuments` retrieves — a deletion in flight under the
//! statuses that say it is going, and a deletion that has COMPLETED under its terminal
//! status — so a client confirms from the collection alone which document a delete removed
//! and that the removal finished; once that deletion has settled the identity is
//! enumerated no more, while its outcome stays reportable through a targeted
//! `GetKnowledgeBaseDocuments` of the identifier.
//!
//! Every assertion goes through the router, so it exercises the real wire path (route
//! matching, model constraint validation, serialization) rather than calling a handler
//! directly.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_bedrockagent_amazonbedrockagentbuildtimelambda::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// A SigV4 `Authorization` header scoped to the `bedrock` signing name: the routing key
/// this service claims on.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/eu-west-1/bedrock/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=0123456789abcdef";

/// A role ARN matching the modelled `KnowledgeBaseRoleArn` pattern.
const KB_ROLE: &str =
    "arn:aws:iam::000000000000:role/service-role/AmazonBedrockExecutionRoleForKnowledgeBase";

/// An embedding model ARN matching the modelled `BedrockEmbeddingModelArn`.
const EMBEDDING_MODEL: &str =
    "arn:aws:bedrock:eu-west-1::foundation-model/amazon.titan-embed-text-v2:0";

/// The vector bucket and index the knowledge base here binds to.
const VECTOR_BUCKET: &str = "arn:aws:s3vectors:::bucket/deletion-settling-vectors";
const VECTOR_INDEX: &str = "deletion-settling-index";

/// The bucket an S3 data source of this suite reads its source documents from.
const SOURCE_BUCKET: &str = "deletion-settling-docs";

/// How long a poll keeps looking for a transition the CLOCK owes before giving up.
///
/// Deliberately far past every span the store owes a document, because what is asserted is
/// that the collection reports the transition at all, never how fast — so a loaded host
/// converges here instead of failing.
const POLL_CAP: Duration = Duration::from_secs(120);

/// How often a poll looks again while it waits for the condition.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The terminal status of an ingestion and of a deletion.
///
/// Source: the `status` member of the `KnowledgeBaseDocumentDetail` shape of this service's
/// Smithy model
/// (`smithy_query(query="doc",shape="KnowledgeBaseDocumentDetail",member="status")`), which
/// documents `INDEXED` as the indexed document and `NOT_FOUND` as "The document wasn't
/// found".
const INDEXED: &str = "INDEXED";
const NOT_FOUND: &str = "NOT_FOUND";

/// One HTTP answer from the emulator, parsed.
#[derive(Debug)]
struct Answer {
    status: u16,
    body: Value,
}

impl Answer {
    /// The response's `documentDetails`, which every document operation of this service
    /// answers with.
    fn details(&self) -> &Vec<Value> {
        self.body["documentDetails"]
            .as_array()
            .unwrap_or_else(|| panic!("documentDetails is not a list in {:?}", self.body))
    }

    /// The detail reported for the document `identifier` names, or `None` when the answer
    /// carries none.
    fn detail_of(&self, identifier: &Value) -> Option<&Value> {
        self.details()
            .iter()
            .find(|detail| &detail["identifier"] == identifier)
    }

    /// The identifiers the answer reports details for, in the order reported.
    fn identifiers(&self) -> Vec<Value> {
        self.details()
            .iter()
            .map(|detail| detail["identifier"].clone())
            .collect()
    }

    /// Whether `identifier` is reported with the status `status`.
    fn reports(&self, identifier: &Value, status: &str) -> bool {
        self.detail_of(identifier)
            .is_some_and(|detail| detail["status"].as_str() == Some(status))
    }
}

/// The emulator under test: one router over one shared engine, exactly as the production
/// router mounts this service.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        // A self-managed vector knowledge base binds to an S3 Vectors index, which
        // `CreateKnowledgeBase` resolves in the service that owns it — so the index exists
        // first, as a caller creates it first against AWS.
        crate::vector_store_fixture::seed_index(
            &engine,
            VECTOR_BUCKET,
            VECTOR_INDEX,
            crate::vector_store_fixture::EMBEDDING_DIMENSION,
        );
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(engine));
        Self { router }
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

    /// Create a knowledge base and answer its minted `knowledgeBaseId`.
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

    /// Attach a data source holding documents of the kind `content_kind` names and answer
    /// its minted `dataSourceId`.
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

    async fn get_documents(
        &self,
        knowledge_base_id: &str,
        data_source_id: &str,
        identifiers: Vec<Value>,
    ) -> Answer {
        self.call(
            "POST",
            &format!(
                "/knowledgebases/{knowledge_base_id}/datasources/{data_source_id}\
                 /documents/getDocuments"
            ),
            json!({ "documentIdentifiers": identifiers }),
        )
        .await
    }

    async fn list_documents(&self, knowledge_base_id: &str, data_source_id: &str) -> Answer {
        self.call(
            "POST",
            &format!("/knowledgebases/{knowledge_base_id}/datasources/{data_source_id}/documents"),
            json!({}),
        )
        .await
    }

    /// List the data source's documents until every one of `identifiers` is reported with
    /// `status`, or [`POLL_CAP`] passes: the progression is owed by the CLOCK, so the
    /// CONDITION is waited for rather than a duration.
    async fn poll_until_listed_as(
        &self,
        knowledge_base_id: &str,
        data_source_id: &str,
        identifiers: &[Value],
        status: &str,
    ) -> Answer {
        let deadline = Instant::now() + POLL_CAP;
        loop {
            let listed = self.list_documents(knowledge_base_id, data_source_id).await;
            let reached = listed.status == 200
                && identifiers
                    .iter()
                    .all(|identifier| listed.reports(identifier, status));
            if reached || Instant::now() >= deadline {
                return listed;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// List the data source's documents until none of `identifiers` is among them, or
    /// [`POLL_CAP`] passes.
    async fn poll_until_absent_from_listing(
        &self,
        knowledge_base_id: &str,
        data_source_id: &str,
        identifiers: &[Value],
    ) -> Answer {
        let deadline = Instant::now() + POLL_CAP;
        loop {
            let listed = self.list_documents(knowledge_base_id, data_source_id).await;
            let gone = listed.status == 200
                && identifiers
                    .iter()
                    .all(|identifier| listed.detail_of(identifier).is_none());
            if gone || Instant::now() >= deadline {
                return listed;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
}

/// The id a document of that kind is given: a custom document carries its own id, an S3
/// document is the object at its URI.
fn document_id(content_kind: &str, name: &str) -> String {
    match content_kind {
        "S3" => format!("s3://{SOURCE_BUCKET}/{name}.txt"),
        _ => name.to_string(),
    }
}

/// A document whose content is of the kind `content_kind` names, identified by `id`.
///
/// Source: the `ContentDataSourceType` enum and the `DocumentContent` structure of this
/// service's Smithy model
/// (`smithy_query(query="shape",shape="ContentDataSourceType,DocumentContent")`): the kind
/// names which sibling member carries the document's own content.
fn document_of_kind(content_kind: &str, id: &str) -> Value {
    match content_kind {
        "CUSTOM" => json!({
            "content": {
                "dataSourceType": "CUSTOM",
                "custom": {
                    "customDocumentIdentifier": { "id": id },
                    "sourceType": "IN_LINE",
                    "inlineContent": {
                        "type": "TEXT",
                        "textContent": { "data": "the document's text" },
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

/// The identifier the store holds a document of that kind under.
fn identifier_of_kind(content_kind: &str, id: &str) -> Value {
    match content_kind {
        "CUSTOM" => json!({ "dataSourceType": "CUSTOM", "custom": { "id": id } }),
        "S3" => json!({ "dataSourceType": "S3", "s3": { "uri": id } }),
        other => panic!("no identifier shape for the content kind {other}"),
    }
}

/// `values` sorted by their wire spelling, so two collections are compared without
/// depending on the order either was built in.
fn sorted(values: &[Value]) -> Vec<String> {
    let mut sorted: Vec<String> = values.iter().map(ToString::to_string).collect();
    sorted.sort();
    sorted
}

/// RULE: a document on its way out of a data source is still one of the documents the
/// collection retrieves, so a COMPLETED deletion is reported there under its terminal
/// status — and once that deletion has settled the identity is enumerated no more, while
/// its outcome stays reportable through the targeted read.
///
/// The rows differ along what the rule generalises over: the kind of content the data
/// source holds (each value of the modelled `ContentDataSourceType`), how many documents
/// the data source holds, and how many of them one delete names (one of several, every one
/// of them). The documents a delete does NOT name are the control in every row: they keep
/// the terminal ingestion status throughout.
#[tokio::test]
async fn a_completed_deletion_is_reported_by_the_collection_until_it_settles() {
    let emulator = Emulator::new();
    let knowledge_base_id = emulator.knowledge_base("deletion-settling-kb").await;

    // (case, the kind of content, how many documents are ingested, how many are deleted)
    let rows: Vec<(&str, &str, usize, usize)> = vec![
        ("the only document of a custom data source", "CUSTOM", 1, 1),
        ("one of three custom documents", "CUSTOM", 3, 1),
        ("every document of an S3 data source", "S3", 2, 2),
    ];

    for (case, content_kind, held, doomed_count) in rows {
        let data_source_id = emulator
            .data_source(
                &knowledge_base_id,
                &format!("settling-{}-{held}-{doomed_count}", content_kind.to_lowercase()),
                content_kind,
            )
            .await;
        let ids: Vec<String> = (0..held)
            .map(|position| document_id(content_kind, &format!("settling-{position}")))
            .collect();
        let identifiers: Vec<Value> = ids
            .iter()
            .map(|id| identifier_of_kind(content_kind, id))
            .collect();
        let ingested = emulator
            .ingest(
                &knowledge_base_id,
                &data_source_id,
                ids.iter()
                    .map(|id| document_of_kind(content_kind, id))
                    .collect(),
            )
            .await;
        assert_eq!(
            ingested.status, 202,
            "case {case}: expected IngestKnowledgeBaseDocuments -> 202, got {ingested:?}"
        );
        let listed = emulator
            .poll_until_listed_as(
                &knowledge_base_id,
                &data_source_id,
                &identifiers,
                INDEXED,
            )
            .await;
        assert_eq!(
            listed.details().len(),
            held,
            "case {case}: expected the {held} ingested document(s) listed, got {:?}",
            listed.body
        );

        let doomed: Vec<Value> = identifiers.iter().take(doomed_count).cloned().collect();
        let survivors: Vec<Value> = identifiers.iter().skip(doomed_count).cloned().collect();
        let accepted = emulator
            .delete_documents(&knowledge_base_id, &data_source_id, doomed.clone())
            .await;
        assert_eq!(
            accepted.status, 202,
            "case {case}: expected DeleteKnowledgeBaseDocuments -> 202, got {accepted:?}"
        );

        // The clock carries the deletion to its end, and the COLLECTION is where a client
        // sees that: the deleted document is reported there under the terminal status of
        // its deletion, naming the identifier the delete removed.
        let listed = emulator
            .poll_until_listed_as(&knowledge_base_id, &data_source_id, &doomed, NOT_FOUND)
            .await;
        assert_eq!(
            listed.status, 200,
            "case {case}: expected ListKnowledgeBaseDocuments -> 200, got {listed:?}"
        );
        assert_eq!(
            listed.details().len(),
            held,
            "case {case}: expected the completed deletion(s) reported beside the documents \
             the data source still holds, got {:?}",
            listed.body
        );
        for identifier in &doomed {
            let detail = listed.detail_of(identifier).unwrap_or_else(|| {
                panic!(
                    "case {case}: the collection reports no detail for the deleted document \
                     {identifier:?}, got {:?}",
                    listed.body
                )
            });
            assert_eq!(
                detail["status"].as_str(),
                Some(NOT_FOUND),
                "case {case}: expected {NOT_FOUND} for the completed deletion, got {detail:?}"
            );
            assert_eq!(
                detail["knowledgeBaseId"].as_str(),
                Some(knowledge_base_id.as_str()),
                "case {case}: expected the addressed knowledgeBaseId, got {detail:?}"
            );
            assert_eq!(
                detail["dataSourceId"].as_str(),
                Some(data_source_id.as_str()),
                "case {case}: expected the addressed dataSourceId, got {detail:?}"
            );
            let updated_at = detail["updatedAt"].as_str().unwrap_or_default();
            assert!(
                updated_at.contains('T') && updated_at.ends_with('Z'),
                "case {case}: expected the instant the deletion completed in the modelled \
                 date-time wire form, got {detail:?}"
            );
        }
        // The documents the delete did not name are untouched by it.
        for identifier in &survivors {
            assert!(
                listed.reports(identifier, INDEXED),
                "case {case}: expected the surviving document reported as {INDEXED}, got {:?}",
                listed.detail_of(identifier)
            );
        }

        // Once the deletion has settled the identity is enumerated no more, so a client
        // waiting for it to LEAVE the collection converges too.
        let settled = emulator
            .poll_until_absent_from_listing(&knowledge_base_id, &data_source_id, &doomed)
            .await;
        assert_eq!(
            sorted(&settled.identifiers()),
            sorted(&survivors),
            "case {case}: expected the collection to converge on the documents the data \
             source holds, got {:?}",
            settled.body
        );

        // The deletion's outcome stays reportable through the targeted read either way,
        // while each surviving document keeps the status it stands in.
        let fetched = emulator
            .get_documents(&knowledge_base_id, &data_source_id, identifiers.clone())
            .await;
        for identifier in &doomed {
            assert!(
                fetched.reports(identifier, NOT_FOUND),
                "case {case}: expected the targeted read to report {NOT_FOUND} for the \
                 deleted document, got {:?}",
                fetched.detail_of(identifier)
            );
        }
        for identifier in &survivors {
            assert!(
                fetched.reports(identifier, INDEXED),
                "case {case}: expected the targeted read to report {INDEXED} for the \
                 surviving document, got {:?}",
                fetched.detail_of(identifier)
            );
        }
    }
}
