// SPDX-License-Identifier: CC0-1.0

//! The terminal state of an accepted document deletion, as a client observes it
//! through `ListKnowledgeBaseDocuments`, end to end over HTTP.
//!
//! One rule is pinned here: a deletion the data source accepted and the clock
//! has carried to its end stays REPORTABLE through the data source's document
//! listing — the listing keeps one detail for that document, carrying the
//! identifier the delete named, the addressed knowledge base and data source,
//! the terminal `DocumentStatus` the model gives a document that was not found
//! and the instant it reached that status — so a client polling the listing
//! observes the deletion land on the document it named. An identifier the data
//! source never held is the one case that leaves nothing in the listing: that
//! is what makes the two distinguishable.
//!
//! The rows span what the rule generalises over: the kind of content the data
//! source holds (each value of the modelled `ContentDataSourceType`), how many
//! documents it holds, how many of them one delete names (one, several, all of
//! them) and whether that delete also names an identifier nothing was ever
//! stored under.
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
const VECTOR_BUCKET: &str = "arn:aws:s3vectors:::bucket/document-terminal-vectors";
const VECTOR_INDEX: &str = "document-terminal-index";

/// How long a poll keeps looking for a transition the CLOCK owes before giving
/// up.
///
/// Deliberately far past the spans the store owes a deletion: what is checked
/// is that the terminal status is reported at all, never how fast, so a loaded
/// host converges here instead of failing.
const POLL_CAP: Duration = Duration::from_secs(120);

/// How often a poll looks again while it waits for the condition.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The terminal `status` of a deletion.
///
/// Source: the `status` member of the `KnowledgeBaseDocumentDetail` shape of
/// this service's Smithy model
/// (`smithy_query(query="doc",shape="KnowledgeBaseDocumentDetail",member="status")`),
/// which documents `NOT_FOUND` as "The document wasn't found".
const NOT_FOUND: &str = "NOT_FOUND";

/// The statuses that same member documents for a delete still in flight: the
/// submitted delete and the delete under way.
const DELETION_IN_FLIGHT: [&str; 2] = ["DELETING", "DELETE_IN_PROGRESS"];

/// The statuses that member documents for an ingestion: submitted, under way
/// and indexed.
const INGESTION_STATUSES: [&str; 3] = ["STARTING", "IN_PROGRESS", "INDEXED"];

/// Every kind of content a document of this service carries, which is the kind
/// of data source that holds it.
///
/// Source: the `ContentDataSourceType` enum of this service's Smithy model
/// (`smithy_query(query="shape",shape="ContentDataSourceType")`).
const CONTENT_KINDS: [&str; 2] = ["CUSTOM", "S3"];

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

    /// The detail reported for the document `identifier` names, or `None` when
    /// the answer carries none.
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

    /// The `nextToken` the answer hands out, when more documents remain.
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
    /// reported with the terminal status of a deletion, or [`POLL_CAP`] passes.
    ///
    /// This is exactly how a client confirms a delete landed: the chain is owed
    /// by the CLOCK, so the condition is waited for rather than a duration.
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
            let settled = listed.status == 200
                && identifiers.iter().all(|identifier| {
                    listed
                        .detail_of(identifier)
                        .is_some_and(|detail| detail["status"].as_str() == Some(NOT_FOUND))
                });
            if settled || Instant::now() >= deadline {
                return listed;
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

/// A data source holding the kind of content `content_kind` names, with the
/// connection member that kind needs.
///
/// Source: the `DataSourceType` enum and the `DataSourceConfiguration`
/// structure of this service's Smithy model
/// (`smithy_query(query="shape",shape="DataSourceType,DataSourceConfiguration")`),
/// which pairs each kind with the sibling member carrying its connection
/// details and gives `CUSTOM` none.
fn data_source_of_kind(name: &str, content_kind: &str) -> Value {
    let mut configuration = json!({ "type": content_kind });
    if content_kind == "S3" {
        configuration["s3Configuration"] =
            json!({ "bucketArn": "arn:aws:s3:::document-terminal-docs" });
    }
    json!({ "name": name, "dataSourceConfiguration": configuration })
}

/// A document whose content is of the kind `content_kind` names, identified by
/// `id`.
///
/// Source: the `ContentDataSourceType` enum and the `DocumentContent` structure
/// of this service's Smithy model
/// (`smithy_query(query="shape",shape="ContentDataSourceType,DocumentContent")`):
/// the kind names which sibling member carries the document's own content.
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
///
/// Source: the `DocumentIdentifier` structure of this service's Smithy model
/// (`smithy_query(query="shape",shape="DocumentIdentifier")`).
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
        "S3" => format!("s3://document-terminal-docs/{name}.txt"),
        _ => name.to_string(),
    }
}

/// `identifiers` ordered the same way whatever order they were reported in, so
/// two answers can be compared as sets.
fn sorted(identifiers: &[Value]) -> Vec<Value> {
    let mut ordered = identifiers.to_vec();
    ordered.sort_by_key(|identifier| identifier.to_string());
    ordered
}

/// The terminal status of an accepted deletion is reportable through the
/// listing: once the chain has run to its end the data source still carries one
/// detail for the deleted document, naming it and the status the model gives a
/// document that was not found, while an identifier nothing was ever stored
/// under leaves no detail at all.
#[tokio::test]
async fn a_completed_deletion_is_listed_as_not_found() {
    let emulator = Emulator::new();
    let knowledge_base_id = emulator
        .knowledge_base(vector_knowledge_base("document-terminal-kb"))
        .await;

    // (case, the kind of content the data source holds, how many documents it
    // takes, which of them one delete names, whether the delete also names an
    // identifier nothing was ever stored under).
    let cases: Vec<(&str, &str, usize, Vec<usize>, bool)> = vec![
        (
            "the only CUSTOM document of the data source",
            CONTENT_KINDS[0],
            1,
            vec![0],
            false,
        ),
        (
            "the first of three CUSTOM documents",
            CONTENT_KINDS[0],
            3,
            vec![0],
            false,
        ),
        (
            "two of four CUSTOM documents in one delete",
            CONTENT_KINDS[0],
            4,
            vec![1, 2],
            false,
        ),
        (
            "every CUSTOM document, beside an identifier never stored",
            CONTENT_KINDS[0],
            2,
            vec![0, 1],
            true,
        ),
        (
            "the last of three S3 documents",
            CONTENT_KINDS[1],
            3,
            vec![2],
            false,
        ),
        (
            "every S3 document, beside an identifier never stored",
            CONTENT_KINDS[1],
            2,
            vec![0, 1],
            true,
        ),
    ];

    for (row, (case, content_kind, held, doomed_positions, names_an_unknown)) in
        cases.into_iter().enumerate()
    {
        let data_source_id = emulator
            .data_source(
                &knowledge_base_id,
                data_source_of_kind(&format!("terminal-{row}"), content_kind),
            )
            .await;
        let ids: Vec<String> = (0..held)
            .map(|position| document_id(content_kind, &format!("terminal-{row}-{position}")))
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
        let unknown = identifier_of_kind(
            content_kind,
            &document_id(content_kind, &format!("terminal-{row}-never-stored")),
        );
        let mut named = doomed.clone();
        if names_an_unknown {
            named.push(unknown.clone());
        }

        let accepted = emulator
            .delete_documents(&knowledge_base_id, &data_source_id, named.clone())
            .await;
        assert_eq!(
            accepted.status, 202,
            "case {case}: expected DeleteKnowledgeBaseDocuments -> 202, got {accepted:?}"
        );
        for identifier in &doomed {
            let detail = accepted
                .detail_of(identifier)
                .unwrap_or_else(|| panic!("case {case}: no detail for {identifier:?}"));
            let status = detail["status"].as_str().unwrap_or_default().to_string();
            assert!(
                DELETION_IN_FLIGHT.contains(&status.as_str()),
                "case {case}: expected one of {DELETION_IN_FLIGHT:?} for the accepted delete, \
                 got {detail:?}"
            );
        }
        if names_an_unknown {
            let detail = accepted
                .detail_of(&unknown)
                .unwrap_or_else(|| panic!("case {case}: no detail for {unknown:?}"));
            assert_eq!(
                detail["status"].as_str(),
                Some(NOT_FOUND),
                "case {case}: expected {NOT_FOUND} for an identifier nothing is stored under, \
                 got {detail:?}"
            );
        }

        // The clock carries the deletion to its end, and the listing is where
        // the client sees it: the deleted document keeps a detail of its own,
        // carrying the terminal status and the instant it reached it.
        let listed = emulator
            .poll_until_listed_not_found(&knowledge_base_id, &data_source_id, &doomed)
            .await;
        assert_eq!(
            listed.status, 200,
            "case {case}: expected ListKnowledgeBaseDocuments -> 200, got {listed:?}"
        );
        for identifier in &doomed {
            let detail = listed.detail_of(identifier).unwrap_or_else(|| {
                panic!(
                    "case {case}: the listing reports no detail for the deleted document \
                     {identifier:?}, got {:?}",
                    listed.body
                )
            });
            assert_eq!(
                detail["status"].as_str(),
                Some(NOT_FOUND),
                "case {case}: expected the listing to report {NOT_FOUND} for the deleted \
                 document, got {detail:?}"
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
            assert!(
                !detail["updatedAt"].is_null(),
                "case {case}: the deletion reached its terminal status at an instant, which \
                 the detail reports, got {detail:?}"
            );
        }
        // The documents the delete did not name are untouched by it: each is
        // still reported with a status of its own ingestion.
        for (position, identifier) in identifiers.iter().enumerate() {
            if doomed_positions.contains(&position) {
                continue;
            }
            let detail = listed.detail_of(identifier).unwrap_or_else(|| {
                panic!(
                    "case {case}: the listing reports no detail for the surviving document \
                     {identifier:?}, got {:?}",
                    listed.body
                )
            });
            let status = detail["status"].as_str().unwrap_or_default().to_string();
            assert!(
                INGESTION_STATUSES.contains(&status.as_str()),
                "case {case}: expected one of {INGESTION_STATUSES:?} for the surviving \
                 document, got {detail:?}"
            );
        }
        // An identifier nothing was ever stored under is the one case that
        // leaves nothing behind, which is what tells a completed deletion from
        // a document this data source never held.
        assert!(
            listed.detail_of(&unknown).is_none(),
            "case {case}: deleting an identifier nothing is stored under stores nothing, \
             got {:?}",
            listed.body
        );
        assert_eq!(
            sorted(&listed.identifiers()),
            sorted(&identifiers),
            "case {case}: expected the listing to report every document of the data source, \
             the deleted ones included, got {:?}",
            listed.body
        );

        // The same answer page by page: a detail reporting a completed deletion
        // is paged like any other, reported once and in the listing's own
        // order.
        let walked = emulator
            .walk_listing(&knowledge_base_id, &data_source_id)
            .await;
        assert_eq!(
            walked,
            listed.identifiers(),
            "case {case}: expected the paged listing to report the same documents in the same \
             order as one page, got {walked:?}"
        );
    }
}

/// Reading the terminal status changes nothing: the listing answers a completed
/// deletion the same way however many times it is polled, and the identifier is
/// free for a fresh ingestion that replaces the deletion reported for it.
#[tokio::test]
async fn reporting_a_completed_deletion_leaves_it_as_it_stands() {
    let emulator = Emulator::new();
    let knowledge_base_id = emulator
        .knowledge_base(vector_knowledge_base("document-terminal-reads-kb"))
        .await;
    let data_source_id = emulator
        .data_source(
            &knowledge_base_id,
            data_source_of_kind("terminal-reads", "CUSTOM"),
        )
        .await;

    let id = "terminal-read-document";
    let identifier = identifier_of_kind("CUSTOM", id);
    let ingested = emulator
        .ingest(
            &knowledge_base_id,
            &data_source_id,
            vec![document_of_kind("CUSTOM", id)],
        )
        .await;
    assert_eq!(
        ingested.status, 202,
        "case setup: expected IngestKnowledgeBaseDocuments -> 202, got {ingested:?}"
    );
    let accepted = emulator
        .delete_documents(
            &knowledge_base_id,
            &data_source_id,
            vec![identifier.clone()],
        )
        .await;
    assert_eq!(
        accepted.status, 202,
        "case setup: expected DeleteKnowledgeBaseDocuments -> 202, got {accepted:?}"
    );

    let settled = emulator
        .poll_until_listed_not_found(
            &knowledge_base_id,
            &data_source_id,
            std::slice::from_ref(&identifier),
        )
        .await;
    let first = settled
        .detail_of(&identifier)
        .unwrap_or_else(|| {
            panic!(
                "case a settled deletion: the listing reports nothing for it, got {:?}",
                settled.body
            )
        })
        .clone();
    assert_eq!(
        first["status"].as_str(),
        Some(NOT_FOUND),
        "case a settled deletion: expected {NOT_FOUND}, got {first:?}"
    );

    // Three more reads, each answering exactly what the one before it did: a
    // listing reports the deletion, it does not consume it.
    for round in 0..3 {
        let again = emulator
            .list_documents(&knowledge_base_id, &data_source_id, None, None)
            .await;
        let detail = again
            .detail_of(&identifier)
            .unwrap_or_else(|| {
                panic!(
                    "case read {round} of a settled deletion: the listing reports nothing for \
                     it, got {:?}",
                    again.body
                )
            })
            .clone();
        assert_eq!(
            detail, first,
            "case read {round} of a settled deletion: expected the same detail as the read \
             before it, got {detail:?}"
        );
    }

    // The identity is free again: a fresh ingestion of it takes the place of
    // the deletion reported for it, and the data source reports one document.
    let reingested = emulator
        .ingest(
            &knowledge_base_id,
            &data_source_id,
            vec![document_of_kind("CUSTOM", id)],
        )
        .await;
    assert_eq!(
        reingested.status, 202,
        "case re-ingesting a deleted id: expected IngestKnowledgeBaseDocuments -> 202, \
         got {reingested:?}"
    );
    let relisted = emulator
        .list_documents(&knowledge_base_id, &data_source_id, None, None)
        .await;
    assert_eq!(
        relisted.identifiers(),
        vec![identifier.clone()],
        "case re-ingesting a deleted id: expected one detail for the one document, got {:?}",
        relisted.body
    );
    let detail = relisted
        .detail_of(&identifier)
        .expect("a detail for the re-ingested document");
    let status = detail["status"].as_str().unwrap_or_default().to_string();
    assert!(
        INGESTION_STATUSES.contains(&status.as_str()),
        "case re-ingesting a deleted id: expected one of {INGESTION_STATUSES:?}, got {detail:?}"
    );
}
