// SPDX-License-Identifier: CC0-1.0

//! What a data source reports once a document delete has run to completion, end
//! to end over HTTP.
//!
//! One rule is pinned here: a delete the service has carried to the end of its
//! chain leaves the data source HOLDING nothing for that document while the
//! deletion's terminal status stays reportable, so
//! `ListKnowledgeBaseDocuments` keeps one detail for the deleted document
//! carrying the status the model gives a document that was not found — page by
//! page as well as in one page — beside the documents that were not deleted, a
//! targeted `GetKnowledgeBaseDocuments` answers that identifier with the same
//! status, and the identifier is free for a fresh ingestion to take again.
//!
//! The rows span what the rule generalises over: the kind of content the data
//! source holds (each value of the modelled `ContentDataSourceType`), how many
//! documents the data source holds, which of them one delete names (the first,
//! the last, several at once, all of them) and therefore how many survive.
//!
//! Every assertion goes through the router, so it exercises the real wire path
//! (route matching, model constraint validation, serialization) rather than
//! calling a handler directly.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_bedrockagent_amazonbedrockagentbuildtimelambda::EmulatedService;
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

/// The vector bucket and index the knowledge base here binds to.
const VECTOR_BUCKET: &str = "arn:aws:s3vectors:::bucket/document-retirement-vectors";
const VECTOR_INDEX: &str = "document-retirement-index";

/// How long a poll keeps looking for a transition the CLOCK owes before giving
/// up.
///
/// Deliberately far past the spans the store owes a delete: what is checked is
/// that the document leaves the listing at all, never how fast, so a loaded host
/// converges here instead of failing.
const POLL_CAP: Duration = Duration::from_secs(120);

/// How often a poll looks again while it waits for the condition.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The `status` the model gives a document the service holds nothing for.
///
/// Source: the `status` member of the `KnowledgeBaseDocumentDetail` shape of
/// this service's Smithy model
/// (`smithy_query(query="doc",shape="KnowledgeBaseDocumentDetail",member="status")`),
/// which documents `NOT_FOUND` as "The document wasn't found".
const NOT_FOUND: &str = "NOT_FOUND";

/// The statuses that same member documents for a delete in flight: the submitted
/// delete and the delete under way.
const DELETION_IN_FLIGHT: [&str; 2] = ["DELETING", "DELETE_IN_PROGRESS"];

/// The statuses that member documents for an ingestion: submitted, under way and
/// indexed.
const INGESTION_STATUSES: [&str; 3] = ["STARTING", "IN_PROGRESS", "INDEXED"];

/// The terminal ingestion status of that same member: the document was
/// successfully indexed.
const INDEXED: &str = "INDEXED";

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

    /// The identifiers the response reports details for, in the order reported.
    fn identifiers(&self) -> Vec<Value> {
        self.details()
            .iter()
            .map(|detail| detail["identifier"].clone())
            .collect()
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

    /// The `nextToken` the response hands out, when more documents remain.
    fn next_token(&self) -> Option<String> {
        self.body["nextToken"].as_str().map(ToString::to_string)
    }
}

/// The emulator under test: one router over one shared engine, exactly as the
/// production router mounts this service.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        // A vector knowledge base binds to an S3 Vectors index, which
        // `CreateKnowledgeBase` resolves in the service that owns it — so the
        // index exists before the knowledge base names it, as against AWS.
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
            serde_json::from_slice(&raw).unwrap_or_else(|e| {
                panic!(
                    "{method} {uri} -> {status} body is not JSON ({e}): {}",
                    String::from_utf8_lossy(&raw)
                )
            })
        };
        Answer { status, body }
    }

    /// Create a knowledge base and answer its minted `knowledgeBaseId`.
    async fn knowledge_base(&self, request: Value) -> String {
        let created = self.call("PUT", "/knowledgebases/", request).await;
        assert_eq!(
            created.status, 202,
            "case setup: expected CreateKnowledgeBase -> 202, got {created:?}"
        );
        created.body["knowledgeBase"]["knowledgeBaseId"]
            .as_str()
            .expect("a minted knowledgeBaseId")
            .to_string()
    }

    /// Attach a data source to `knowledge_base_id` and answer its minted
    /// `dataSourceId`.
    async fn data_source(&self, knowledge_base_id: &str, request: Value) -> String {
        let created = self
            .call(
                "PUT",
                &format!("/knowledgebases/{knowledge_base_id}/datasources/"),
                request,
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

    /// `ListKnowledgeBaseDocuments`, with or without the paging members.
    async fn list_documents(
        &self,
        knowledge_base_id: &str,
        data_source_id: &str,
        max_results: Option<i64>,
        next_token: Option<&str>,
    ) -> Answer {
        let mut body = json!({});
        if let Some(max_results) = max_results {
            body["maxResults"] = json!(max_results);
        }
        if let Some(next_token) = next_token {
            body["nextToken"] = json!(next_token);
        }
        self.call(
            "POST",
            &format!("/knowledgebases/{knowledge_base_id}/datasources/{data_source_id}/documents"),
            body,
        )
        .await
    }

    /// List the data source's documents until every one of `identifiers` is
    /// reported with the terminal status of a deletion, or [`POLL_CAP`]
    /// passes — the end of the chain a delete starts is owed by the CLOCK, and
    /// this listing is how a client confirms it.
    async fn poll_until_listed_not_found(
        &self,
        knowledge_base_id: &str,
        data_source_id: &str,
        identifiers: &[Value],
    ) -> Answer {
        let deadline = Instant::now() + POLL_CAP;
        loop {
            let listed = self
                .list_documents(knowledge_base_id, data_source_id, None, None)
                .await;
            let gone = listed.status == 200
                && identifiers.iter().all(|identifier| {
                    listed.details().iter().any(|detail| {
                        &detail["identifier"] == identifier
                            && detail["status"].as_str() == Some(NOT_FOUND)
                    })
                });
            if gone || Instant::now() >= deadline {
                return listed;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// Read `identifiers` until every one of them reports the terminal ingestion
    /// status, or [`POLL_CAP`] passes — the progression is owed by the CLOCK, so
    /// a client converges on it by polling exactly this way.
    async fn poll_until_indexed(
        &self,
        knowledge_base_id: &str,
        data_source_id: &str,
        identifiers: &[Value],
    ) -> Answer {
        let deadline = Instant::now() + POLL_CAP;
        loop {
            let fetched = self
                .get_documents(knowledge_base_id, data_source_id, identifiers.to_vec())
                .await;
            let indexed = fetched.status == 200
                && fetched
                    .details()
                    .iter()
                    .all(|detail| detail["status"].as_str() == Some(INDEXED));
            if indexed || Instant::now() >= deadline {
                return fetched;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// Every identifier the listing reports when walked one document at a time,
    /// following the `nextToken` each page hands out.
    async fn walk_listing(&self, knowledge_base_id: &str, data_source_id: &str) -> Vec<Value> {
        let mut walked: Vec<Value> = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let page = self
                .list_documents(knowledge_base_id, data_source_id, Some(1), token.as_deref())
                .await;
            assert_eq!(
                page.status, 200,
                "case paging: expected ListKnowledgeBaseDocuments -> 200, got {page:?}"
            );
            walked.extend(page.identifiers());
            match page.next_token() {
                Some(next) => token = Some(next),
                None => return walked,
            }
        }
    }
}

/// A vector knowledge base over the S3 Vectors index this suite seeds.
fn vector_knowledge_base(name: &str) -> Value {
    json!({
        "name": name,
        "roleArn": KB_ROLE,
        "knowledgeBaseConfiguration": {
            "type": "VECTOR",
            "vectorKnowledgeBaseConfiguration": { "embeddingModelArn": EMBEDDING_MODEL },
        },
        "storageConfiguration": {
            "type": "S3_VECTORS",
            "s3VectorsConfiguration": {
                "vectorBucketArn": VECTOR_BUCKET,
                "indexName": VECTOR_INDEX,
            },
        },
    })
}

/// A data source holding the kind of content `content_type` names, with the
/// connection member that kind needs.
///
/// Source: the `DataSourceType` enum and the `DataSourceConfiguration` structure
/// of this service's Smithy model
/// (`smithy_query(query="shape",shape="DataSourceType,DataSourceConfiguration")`),
/// which pairs each kind with the sibling member carrying its connection details
/// and gives `CUSTOM` none.
fn data_source_of_kind(name: &str, content_type: &str) -> Value {
    let mut configuration = json!({ "type": content_type });
    if content_type == "S3" {
        configuration["s3Configuration"] =
            json!({ "bucketArn": "arn:aws:s3:::document-retirement-docs" });
    }
    json!({ "name": name, "dataSourceConfiguration": configuration })
}

/// A document whose content is of the kind `content_type` names, identified by
/// `id`.
///
/// Source: the `ContentDataSourceType` enum and the `DocumentContent` structure
/// of this service's Smithy model
/// (`smithy_query(query="shape",shape="ContentDataSourceType,DocumentContent")`):
/// the kind names which sibling member carries the document's own content.
fn document_of_kind(content_type: &str, id: &str) -> Value {
    match content_type {
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
fn identifier_of_kind(content_type: &str, id: &str) -> Value {
    match content_type {
        "CUSTOM" => json!({ "dataSourceType": "CUSTOM", "custom": { "id": id } }),
        "S3" => json!({ "dataSourceType": "S3", "s3": { "uri": id } }),
        other => panic!("no identifier shape for the content kind {other}"),
    }
}

/// The id a document of that kind is given: a custom document carries its own
/// id, an S3 document is the object at its URI.
fn document_id(content_type: &str, name: &str) -> String {
    match content_type {
        "S3" => format!("s3://document-retirement-docs/{name}.txt"),
        _ => name.to_string(),
    }
}

/// A document whose delete has run to completion is still listed by its data
/// source, carrying the status the model gives a document that was not found; a
/// targeted read answers it the same way, and every other document of the same
/// data source is left alone.
#[tokio::test]
async fn a_completed_delete_reports_not_found_through_the_listing() {
    let emulator = Emulator::new();
    let knowledge_base_id = emulator
        .knowledge_base(vector_knowledge_base("document-retirement-kb"))
        .await;

    // (case, the kind of content the data source holds, how many documents it
    // takes, which of them one delete names).
    let cases: Vec<(&str, &str, usize, Vec<usize>)> = vec![
        ("the first of three CUSTOM documents", "CUSTOM", 3, vec![0]),
        ("the last of three CUSTOM documents", "CUSTOM", 3, vec![2]),
        (
            "two of four CUSTOM documents in one delete",
            "CUSTOM",
            4,
            vec![1, 2],
        ),
        ("the only S3 document", "S3", 1, vec![0]),
        (
            "every S3 document of the data source",
            "S3",
            2,
            vec![0, 1],
        ),
    ];

    for (row, (case, content_kind, held, doomed_positions)) in cases.into_iter().enumerate() {
        let data_source_id = emulator
            .data_source(
                &knowledge_base_id,
                data_source_of_kind(&format!("retirement-{row}"), content_kind),
            )
            .await;
        let ids: Vec<String> = (0..held)
            .map(|position| document_id(content_kind, &format!("retired-{row}-{position}")))
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

        let doomed: Vec<Value> = doomed_positions
            .iter()
            .map(|position| identifiers[*position].clone())
            .collect();
        let survivors: Vec<Value> = identifiers
            .iter()
            .enumerate()
            .filter(|(position, _)| !doomed_positions.contains(position))
            .map(|(_, identifier)| identifier.clone())
            .collect();

        let accepted = emulator
            .delete_documents(&knowledge_base_id, &data_source_id, doomed.clone())
            .await;
        assert_eq!(
            accepted.status, 202,
            "case {case}: expected DeleteKnowledgeBaseDocuments -> 202, got {accepted:?}"
        );
        for identifier in &doomed {
            let status = accepted.detail_of(identifier)["status"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            assert!(
                DELETION_IN_FLIGHT.contains(&status.as_str()),
                "case {case}: expected one of {DELETION_IN_FLIGHT:?} for the accepted delete, \
                 got {:?}",
                accepted.detail_of(identifier)
            );
        }

        // The clock carries the delete to its end, which is what a client waits
        // for on this very listing.
        let listed = emulator
            .poll_until_listed_not_found(&knowledge_base_id, &data_source_id, &doomed)
            .await;
        assert_eq!(
            listed.identifiers(),
            identifiers,
            "case {case}: expected the listing to report every document of the data source, \
             the deleted ones included, got {:?}",
            listed.body
        );
        for identifier in &doomed {
            let detail = listed.detail_of(identifier);
            assert_eq!(
                detail["status"].as_str(),
                Some(NOT_FOUND),
                "case {case}: expected {NOT_FOUND} listed for the deleted document, \
                 got {detail:?}"
            );
        }
        // The same answer page by page: the listing's cursor runs over the
        // records the data source holds, so walking it one at a time reports
        // them once each and nothing else.
        let walked = emulator
            .walk_listing(&knowledge_base_id, &data_source_id)
            .await;
        assert_eq!(
            walked, identifiers,
            "case {case}: expected the paged listing to report every document of the data \
             source, the deleted ones included, got {walked:?}"
        );

        // A targeted read of a deleted identifier reports it as a document that
        // was not found, at the instant the deletion reached its end — while a
        // survivor still reports its own ingestion.
        let fetched = emulator
            .get_documents(&knowledge_base_id, &data_source_id, identifiers.clone())
            .await;
        for identifier in &doomed {
            let detail = fetched.detail_of(identifier);
            assert_eq!(
                detail["status"].as_str(),
                Some(NOT_FOUND),
                "case {case}: expected {NOT_FOUND} for the deleted document, got {detail:?}"
            );
            assert_eq!(
                detail["updatedAt"],
                listed.detail_of(identifier)["updatedAt"],
                "case {case}: expected the targeted read and the listing to report the same \
                 instant for the deleted document, got {detail:?}"
            );
        }
        for identifier in &survivors {
            let detail = fetched.detail_of(identifier);
            let status = detail["status"].as_str().unwrap_or_default().to_string();
            assert!(
                INGESTION_STATUSES.contains(&status.as_str()),
                "case {case}: expected one of {INGESTION_STATUSES:?} for the surviving \
                 document, got {detail:?}"
            );
        }
        // And each survivor's own ingestion still runs to its end: the delete of
        // another document of the same data source took nothing from it.
        if !survivors.is_empty() {
            let indexed = emulator
                .poll_until_indexed(&knowledge_base_id, &data_source_id, &survivors)
                .await;
            for identifier in &survivors {
                let detail = indexed.detail_of(identifier);
                assert_eq!(
                    detail["status"].as_str(),
                    Some(INDEXED),
                    "case {case}: expected the surviving document to reach {INDEXED}, \
                     got {detail:?}"
                );
            }
        }

        // The identifier is free again: ingesting it is a fresh ingestion, and
        // the data source lists it beside the survivors from there on.
        let first_doomed = doomed_positions[0];
        let reingested = emulator
            .ingest(
                &knowledge_base_id,
                &data_source_id,
                vec![document_of_kind(content_kind, &ids[first_doomed])],
            )
            .await;
        assert_eq!(
            reingested.status, 202,
            "case {case}: expected re-ingesting a retired id -> 202, got {reingested:?}"
        );
        let status = reingested.detail_of(&identifiers[first_doomed])["status"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            INGESTION_STATUSES.contains(&status.as_str()),
            "case {case}: expected a fresh ingestion of the retired id, got {:?}",
            reingested.detail_of(&identifiers[first_doomed])
        );
        let relisted = emulator
            .list_documents(&knowledge_base_id, &data_source_id, None, None)
            .await;
        let mut expected = identifiers.clone();
        let mut reported = relisted.identifiers();
        reported.sort_by_key(|identifier| identifier.to_string());
        expected.sort_by_key(|identifier| identifier.to_string());
        assert_eq!(
            reported, expected,
            "case {case}: expected the re-ingested document listed once, beside the \
             survivors and the deletions still reported, got {:?}",
            relisted.body
        );
        assert!(
            INGESTION_STATUSES
                .contains(&relisted.detail_of(&identifiers[first_doomed])["status"]
                    .as_str()
                    .unwrap_or_default()),
            "case {case}: expected the re-ingested document to replace the deletion reported \
             for its identifier, got {:?}",
            relisted.detail_of(&identifiers[first_doomed])
        );
    }
}
