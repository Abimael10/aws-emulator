// SPDX-License-Identifier: CC0-1.0

//! Pushing a document into a knowledge base whose data source is run BY the service, end to
//! end over both Amazon Bedrock Agent planes.
//!
//! RULE: the kind of document content a data source takes is the kind of SOURCE its stored
//! configuration resolves to — for a data source the service connects on the knowledge
//! base's behalf that is the connector its connector parameters name, and for every other
//! data source its own type — so `IngestKnowledgeBaseDocuments` accepts a document whose
//! `content.dataSourceType` is that kind (202) and refuses any other with the modelled
//! `ValidationException` (400); a data source of a kind no document content names takes
//! none. A document the ingestion accepted is the knowledge base's, so the runtime plane's
//! `GetDocumentContent` hands out an address for exactly the bytes that travelled in the
//! ingestion.
//!
//! A data source's own `DataSourceType` and a document's `ContentDataSourceType` are two
//! different enums (`smithy_query(query="shape",shape="DataSourceType,ContentDataSourceType")`,
//! the Bedrock Agent model): the content enum names only the kinds of source a document can
//! be pushed in from, so a configuration whose type names a connector the service runs is
//! resolved through that connector rather than compared with the content enum.
//!
//! Two crates have to agree for the rule to hold — the knowledge base, the data source and
//! the ingestion are the Bedrock Agent build-time plane's, the content read is the Bedrock
//! Agent runtime plane's — so the suite lives here rather than in either of them. The
//! crate-local halves are those crates' own suites: which documents a data source takes in
//! `services/service-com-amazonaws-bedrockagent-amazonbedrockagentbuildtimelambda/tests/\
//! all/knowledge_base_document_kinds_and_deletion.rs`, and what a content read answers in
//! `services/service-com-amazonaws-bedrockagentruntime-amazonbedrockagentruntimeservice/\
//! tests/all/document_content.rs`.
//!
//! The rows span what the rule generalises over: whether the data source is run by the
//! service or carries its own connection details, which connector a service-run one runs,
//! each value of the modelled `ContentDataSourceType` presented to each data source, the
//! kinds that hold no directly ingestible content at all, and — for the read — the bytes a
//! document carries and how many documents one request pushes in.
//!
//! The one wait here is for a transition the CLOCK owes (a knowledge base reaching its
//! active status, a document reaching its indexed one): each is polled for the condition
//! under a deadline generous enough for any host, and nothing asserts how long it took.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every call of this suite is signed for.
const REGION: &str = "us-west-2";

/// The signing name every Bedrock-family endpoint shares, which is what routes both Bedrock
/// Agent planes (each claims its own path space).
const BEDROCK_SIGNING_NAME: &str = "bedrock";

/// The signing name of the vector store a self-managed knowledge base binds to.
const VECTORS_SIGNING_NAME: &str = "s3vectors";

/// The access key id every call here presents. It names no IAM record, which is the position
/// an account's own published credential is in, so the identity side authorizes every call
/// and what is under test is the service's own decision.
const ACCESS_KEY_ID: &str = "AKIDAMBIENT";

/// A role ARN matching the modelled `KnowledgeBaseRoleArn` pattern.
const KB_ROLE: &str =
    "arn:aws:iam::000000000000:role/service-role/AmazonBedrockExecutionRoleForKnowledgeBase";

/// The embedding model the self-managed knowledge base here declares, and how many values
/// its vectors carry: Titan Text Embeddings V2 produces 1024, so the index it binds to holds
/// vectors of exactly that length.
const EMBEDDING_MODEL: &str =
    "arn:aws:bedrock:us-west-2::foundation-model/amazon.titan-embed-text-v2:0";
const EMBEDDING_DIMENSION: u32 = 1024;

/// The vector bucket and the index inside it the self-managed knowledge base binds to.
const VECTOR_BUCKET: &str = "managed-connector-vectors";
const VECTOR_INDEX: &str = "managed-connector-index";

/// The bucket an S3 data source of this suite names, and the one an S3 document lives in.
const SOURCE_BUCKET: &str = "managed-connector-docs";

/// Both values of the modelled `ContentDataSourceType`
/// (`smithy_query(query="shape",shape="ContentDataSourceType")`), each of which is presented
/// to every data source of the table: so each row asserts both the acceptance of the kind
/// that data source holds and the refusal of the kind it does not.
const CONTENT_KINDS: [&str; 2] = ["CUSTOM", "S3"];

/// The terminal status of an ingested document, from the `status` member of the modelled
/// `KnowledgeBaseDocumentDetail`, which documents it as successfully indexed.
const INDEXED: &str = "INDEXED";

/// The status that member gives an identifier the data source holds nothing for.
const NOT_FOUND: &str = "NOT_FOUND";

/// The media type a content read reports for text that travelled in the ingestion.
const TEXT_MEDIA_TYPE: &str = "text/plain";

/// How many times a read looks again for a transition the clock owes. Generous: every wait
/// here is for the CONDITION and nothing asserts how long it took.
const SETTLE_ATTEMPTS: usize = 1200;

/// How long one look waits before the next.
const SETTLE_PAUSE: Duration = Duration::from_millis(25);

/// One HTTP answer from the emulator, parsed.
#[derive(Debug)]
struct Answer {
    status: u16,
    /// The restJson1 error shape name, from `x-amzn-errortype` or the body's `__type`;
    /// `None` on a success.
    error_type: Option<String>,
    body: Value,
}

impl Answer {
    /// The string at a dotted path of the body, or the empty string where there is none.
    fn text(&self, path: &str) -> String {
        let mut here = &self.body;
        for segment in path.split('.') {
            match here.get(segment) {
                Some(next) => here = next,
                None => return String::new(),
            }
        }
        here.as_str().unwrap_or_default().to_string()
    }

    /// The `documentDetails` every document operation of the build-time plane answers with.
    fn details(&self) -> Vec<Value> {
        self.body["documentDetails"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    /// The detail reported for the document `identifier` names, whatever position it was
    /// reported in.
    fn detail_of(&self, case: &str, identifier: &Value) -> Value {
        self.details()
            .into_iter()
            .find(|detail| &detail["identifier"] == identifier)
            .unwrap_or_else(|| {
                panic!(
                    "case {case}: expected a documentDetails entry for {identifier}, got {}",
                    self.body
                )
            })
    }
}

/// The emulator under test: the two Bedrock Agent planes and the vector store a self-managed
/// knowledge base binds to, mounted on ONE engine exactly as `core/router/src/lib.rs` mounts
/// them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_s3vectors_s3vectors::EmulatedService::new(Arc::clone(&engine)),
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

/// A SigV4 `Authorization` header whose credential scope names `signing_name` in [`REGION`]
/// and whose credential is [`ACCESS_KEY_ID`].
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={ACCESS_KEY_ID}/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// Dispatch one request and parse its answer.
async fn dispatch(router: &MultiServiceRouter, request: http::Request<Bytes>) -> Answer {
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let header_error = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(ToString::to_string);
    let raw = response.into_body();
    let body: Value = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw).unwrap_or(Value::Null)
    };
    let error_type = header_error.or_else(|| {
        body.get("__type")
            .and_then(Value::as_str)
            .map(ToString::to_string)
    });
    Answer {
        status,
        error_type,
        body,
    }
}

/// One Bedrock call, over the `restJson1` wire both Bedrock Agent planes declare, on the
/// method and URI the operation's own `@http` binding names.
async fn bedrock(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> Answer {
    let payload = body
        .map(|value| Bytes::from(serde_json::to_vec(&value).expect("the request body serializes")))
        .unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("bedrock.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization(BEDROCK_SIGNING_NAME))
        .body(payload)
        .expect("a well-formed Bedrock request");
    dispatch(router, request).await
}

/// One Amazon S3 Vectors operation over its restJson1 wire: a POST to the operation's own
/// URI, which is how that model binds every one of them.
async fn s3vectors(router: &MultiServiceRouter, operation: &str, body: Value) -> Answer {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("/{operation}"))
        .header("host", format!("s3vectors.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization(VECTORS_SIGNING_NAME))
        .body(Bytes::from(
            serde_json::to_vec(&body).expect("the request body serializes"),
        ))
        .expect("a well-formed S3 Vectors request");
    dispatch(router, request).await
}

/// Create the vector bucket and the index a self-managed knowledge base binds to, answering
/// the bucket's ARN as the owning service minted it — the store a caller sets up before
/// naming it.
async fn vector_store(router: &MultiServiceRouter) -> String {
    let created = s3vectors(
        router,
        "CreateVectorBucket",
        json!({ "vectorBucketName": VECTOR_BUCKET }),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "case setup: expected CreateVectorBucket -> 200, got {created:?}"
    );
    let bucket_arn = created.text("vectorBucketArn");
    let index = s3vectors(
        router,
        "CreateIndex",
        json!({
            "vectorBucketName": VECTOR_BUCKET,
            "indexName": VECTOR_INDEX,
            "dataType": "float32",
            "dimension": EMBEDDING_DIMENSION,
            "distanceMetric": "cosine",
        }),
    )
    .await;
    assert_eq!(
        index.status, 200,
        "case setup: expected CreateIndex -> 200, got {index:?}"
    );
    bucket_arn
}

/// Create the knowledge base `request` describes, wait for the status the clock owes it and
/// answer its minted `knowledgeBaseId`.
async fn knowledge_base(router: &MultiServiceRouter, request: Value) -> String {
    let created = bedrock(router, "PUT", "/knowledgebases/", Some(request)).await;
    assert_eq!(
        created.status, 202,
        "case setup: expected CreateKnowledgeBase -> 202, got {created:?}"
    );
    let knowledge_base_id = created.text("knowledgeBase.knowledgeBaseId");
    assert!(
        !knowledge_base_id.is_empty(),
        "case setup: expected a minted knowledgeBaseId, got {created:?}"
    );
    for _ in 0..SETTLE_ATTEMPTS {
        let read = bedrock(
            router,
            "GET",
            &format!("/knowledgebases/{knowledge_base_id}"),
            None,
        )
        .await;
        assert_eq!(
            read.status, 200,
            "case setup: expected GetKnowledgeBase -> 200, got {read:?}"
        );
        if read.text("knowledgeBase.status") == "ACTIVE" {
            return knowledge_base_id;
        }
        tokio::time::sleep(SETTLE_PAUSE).await;
    }
    panic!("case setup: the knowledge base {knowledge_base_id} never reached ACTIVE");
}

/// A knowledge base the SERVICE runs the connectors of: the kind a service-run data source
/// belongs to.
fn service_managed_knowledge_base(name: &str) -> Value {
    json!({
        "name": name,
        "roleArn": KB_ROLE,
        "knowledgeBaseConfiguration": {
            "type": "MANAGED",
            "managedKnowledgeBaseConfiguration": { "embeddingModelType": "MANAGED" },
        },
    })
}

/// A knowledge base whose data sources carry their own connection details, bound to the
/// vector index `bucket_arn` names.
fn self_managed_knowledge_base(name: &str, bucket_arn: &str) -> Value {
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
                "vectorBucketArn": bucket_arn,
                "indexName": VECTOR_INDEX,
            },
        },
    })
}

/// A data source the service connects on the knowledge base's behalf, running the connector
/// `connector` names with the connection details that connector needs.
///
/// Source: the `DataSourceConfiguration` structure of the Bedrock Agent model
/// (`smithy_query(query="shape",shape="DataSourceConfiguration")`), whose managed-connector
/// member is the one to specify for a data source of that type, and whose
/// `connectorParameters` document names the connector in its `type`.
fn service_run_data_source(name: &str, connector: &str, connection: Value) -> Value {
    let mut parameters = json!({ "type": connector, "version": "1", "aclEnabled": false });
    if let Value::Object(connection) = connection {
        for (member, value) in connection {
            parameters[member] = value;
        }
    }
    json!({
        "name": name,
        "dataSourceConfiguration": {
            "type": "MANAGED_KNOWLEDGE_BASE_CONNECTOR",
            "managedKnowledgeBaseConnectorConfiguration": {
                "connectorParameters": parameters,
            },
        },
    })
}

/// A data source carrying its own connection details, of the kind `data_source_type` names.
///
/// Source: the `DataSourceType` enum and the `DataSourceConfiguration` structure of the
/// Bedrock Agent model (`smithy_query(query="shape",shape="DataSourceType,\
/// DataSourceConfiguration")`), which pairs each kind with the sibling member carrying its
/// connection details and gives `CUSTOM` and `REDSHIFT_METADATA` none.
fn self_run_data_source(name: &str, data_source_type: &str) -> Value {
    let mut configuration = json!({ "type": data_source_type });
    match data_source_type {
        "S3" => {
            configuration["s3Configuration"] =
                json!({ "bucketArn": format!("arn:aws:s3:::{SOURCE_BUCKET}") });
        }
        "WEB" => {
            configuration["webConfiguration"] = json!({
                "sourceConfiguration": {
                    "urlConfiguration": { "seedUrls": [{ "url": "https://example.com/docs" }] },
                },
            });
        }
        _ => {}
    }
    json!({ "name": name, "dataSourceConfiguration": configuration })
}

/// Attach the data source `request` describes to `knowledge_base_id` and answer its minted
/// `dataSourceId`.
async fn data_source(
    router: &MultiServiceRouter,
    case: &str,
    knowledge_base_id: &str,
    request: Value,
) -> String {
    let created = bedrock(
        router,
        "PUT",
        &format!("/knowledgebases/{knowledge_base_id}/datasources/"),
        Some(request),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "case {case}: expected CreateDataSource -> 200, got {created:?}"
    );
    let data_source_id = created.text("dataSource.dataSourceId");
    assert!(
        !data_source_id.is_empty(),
        "case {case}: expected a minted dataSourceId, got {created:?}"
    );
    data_source_id
}

/// `IngestKnowledgeBaseDocuments` over one data source.
async fn ingest(
    router: &MultiServiceRouter,
    knowledge_base_id: &str,
    data_source_id: &str,
    documents: Vec<Value>,
) -> Answer {
    bedrock(
        router,
        "PUT",
        &format!("/knowledgebases/{knowledge_base_id}/datasources/{data_source_id}/documents"),
        Some(json!({ "documents": documents })),
    )
    .await
}

/// `GetKnowledgeBaseDocuments` over the identifiers of one data source.
async fn get_documents(
    router: &MultiServiceRouter,
    knowledge_base_id: &str,
    data_source_id: &str,
    identifiers: Vec<Value>,
) -> Answer {
    bedrock(
        router,
        "POST",
        &format!(
            "/knowledgebases/{knowledge_base_id}/datasources/{data_source_id}\
             /documents/getDocuments"
        ),
        Some(json!({ "documentIdentifiers": identifiers })),
    )
    .await
}

/// Read `identifier` until the data source reports it indexed, answering that detail: the
/// progression is owed by the CLOCK, so a client converges on it by polling exactly this
/// way.
async fn await_indexed(
    router: &MultiServiceRouter,
    case: &str,
    knowledge_base_id: &str,
    data_source_id: &str,
    identifier: &Value,
) -> Value {
    for _ in 0..SETTLE_ATTEMPTS {
        let read =
            get_documents(router, knowledge_base_id, data_source_id, vec![identifier.clone()])
                .await;
        assert_eq!(
            read.status, 200,
            "case {case}: expected GetKnowledgeBaseDocuments -> 200, got {read:?}"
        );
        let detail = read.detail_of(case, identifier);
        if detail["status"].as_str() == Some(INDEXED) {
            return detail;
        }
        tokio::time::sleep(SETTLE_PAUSE).await;
    }
    panic!("case {case}: {identifier} never reached {INDEXED}");
}

/// `GetDocumentContent` on the runtime plane, over one document of one data source.
async fn document_content(
    router: &MultiServiceRouter,
    knowledge_base_id: &str,
    data_source_id: &str,
    document_id: &str,
) -> Answer {
    bedrock(
        router,
        "POST",
        &format!(
            "/knowledgebases/{knowledge_base_id}/datasources/{data_source_id}\
             /documents/{document_id}/content"
        ),
        None,
    )
    .await
}

/// Fetch a URL this emulator signed, the way a client does: the whole URL, through the same
/// router, with no credential of its own.
async fn follow(router: &MultiServiceRouter, case: &str, url: &str) -> (u16, Bytes) {
    let request = http::Request::builder()
        .method("GET")
        .uri(url)
        .body(Bytes::new())
        .unwrap_or_else(|error| panic!("case {case}: {url} must be a URL ({error})"));
    let response = router.dispatch(request).await;
    (response.status().as_u16(), response.into_body())
}

/// A document whose content is of the kind `content_kind` names, identified by `id`.
///
/// Source: the `ContentDataSourceType` enum and the `DocumentContent` structure of the
/// Bedrock Agent model (`smithy_query(query="shape",shape="ContentDataSourceType,\
/// DocumentContent")`): the kind names which sibling member carries the document's own
/// content.
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

/// The identifier the store holds a document of that kind under.
fn identifier_of_kind(content_kind: &str, id: &str) -> Value {
    match content_kind {
        "CUSTOM" => json!({ "dataSourceType": "CUSTOM", "custom": { "id": id } }),
        "S3" => json!({ "dataSourceType": "S3", "s3": { "uri": id } }),
        other => panic!("no identifier shape for the content kind {other}"),
    }
}

/// The id a document of that kind is pushed in under: a custom document carries its own id,
/// an S3 document is the object at its URI.
fn document_id(content_kind: &str, name: &str) -> String {
    match content_kind {
        "S3" => format!("s3://{SOURCE_BUCKET}/{name}.txt"),
        _ => name.to_string(),
    }
}

/// The `ValidationException` message the ingestion answers for a document whose kind is not
/// the kind the addressed data source holds, which that operation documents as "The
/// dataSourceType specified in the content for each document must match the type of the data
/// source that you specify in the header".
///
/// The kind the DOCUMENT named is the one variable part of the sentence; the rest is fixed
/// text for this error class.
fn mismatch_message(content_kind: &str) -> String {
    format!(
        "The dataSourceType that you specified in {content_kind} doesn't match the type of \
         the data source specified in the request header. Check that both types match and \
         retry your request."
    )
}

/// RULE: a data source takes the kind of document content its configuration RESOLVES to —
/// the connector a service-run data source runs, the type a self-run one declares — and
/// refuses every other kind with the modelled `ValidationException`, writing nothing.
#[tokio::test]
async fn a_data_source_takes_the_document_kind_its_configuration_resolves_to() {
    let router = build_router();
    let bucket_arn = vector_store(&router).await;
    let service_managed =
        knowledge_base(&router, service_managed_knowledge_base("connector-managed")).await;
    let self_managed = knowledge_base(
        &router,
        self_managed_knowledge_base("connector-self-managed", &bucket_arn),
    )
    .await;

    // (case, the knowledge base, the data source to create, the content kind it takes —
    // `None` for a data source that takes none).
    let cases: Vec<(&str, &str, Value, Option<&str>)> = vec![
        (
            "a service-run data source running the CUSTOM connector",
            service_managed.as_str(),
            service_run_data_source("pairing-connector-custom", "CUSTOM", json!({})),
            Some("CUSTOM"),
        ),
        (
            "a service-run data source running the S3 connector",
            service_managed.as_str(),
            service_run_data_source(
                "pairing-connector-s3",
                "S3",
                json!({ "connectionConfiguration": { "bucketName": SOURCE_BUCKET } }),
            ),
            Some("S3"),
        ),
        (
            "a service-run data source whose connector crawls a source of its own",
            service_managed.as_str(),
            service_run_data_source(
                "pairing-connector-web",
                "WEB",
                json!({ "connectionConfiguration": { "url": "https://example.com/docs" } }),
            ),
            None,
        ),
        (
            "a service-run data source running a connector no document content names",
            service_managed.as_str(),
            service_run_data_source("pairing-connector-sharepoint", "SHAREPOINT", json!({})),
            None,
        ),
        (
            "a self-run CUSTOM data source",
            self_managed.as_str(),
            self_run_data_source("pairing-custom", "CUSTOM"),
            Some("CUSTOM"),
        ),
        (
            "a self-run S3 data source",
            self_managed.as_str(),
            self_run_data_source("pairing-s3", "S3"),
            Some("S3"),
        ),
        (
            "a self-run data source crawling a source of its own",
            self_managed.as_str(),
            self_run_data_source("pairing-web", "WEB"),
            None,
        ),
        (
            "a self-run data source of a kind no document content names",
            self_managed.as_str(),
            self_run_data_source("pairing-redshift", "REDSHIFT_METADATA"),
            None,
        ),
    ];

    for (case, knowledge_base_id, request, held) in cases {
        let data_source_id = data_source(&router, case, knowledge_base_id, request).await;

        for content_kind in CONTENT_KINDS {
            let case = &format!("{case}, {content_kind} content");
            let id = document_id(content_kind, "pairing-document");
            let identifier = identifier_of_kind(content_kind, &id);
            let answered = ingest(
                &router,
                knowledge_base_id,
                &data_source_id,
                vec![document_of_kind(content_kind, &id, "the document's text")],
            )
            .await;

            if held == Some(content_kind) {
                assert_eq!(
                    answered.status, 202,
                    "case {case}: expected IngestKnowledgeBaseDocuments -> 202, got {answered:?}"
                );
                let detail = answered.detail_of(case, &identifier);
                assert_eq!(
                    detail["dataSourceId"].as_str(),
                    Some(data_source_id.as_str()),
                    "case {case}: expected the document of the data source the URI named, got \
                     {detail}"
                );
                // The document the ingestion accepted is the one the data source now holds,
                // which is what the read reports.
                let fetched = get_documents(
                    &router,
                    knowledge_base_id,
                    &data_source_id,
                    vec![identifier.clone()],
                )
                .await;
                assert_eq!(
                    fetched.status, 200,
                    "case {case}: expected GetKnowledgeBaseDocuments -> 200, got {fetched:?}"
                );
                let held_detail = fetched.detail_of(case, &identifier);
                assert_ne!(
                    held_detail["status"].as_str(),
                    Some(NOT_FOUND),
                    "case {case}: an accepted document is held by the data source, got \
                     {held_detail}"
                );
                continue;
            }

            assert_eq!(
                answered.status, 400,
                "case {case}: expected IngestKnowledgeBaseDocuments -> 400, got {answered:?}"
            );
            assert_eq!(
                answered.error_type.as_deref(),
                Some("ValidationException"),
                "case {case}: expected ValidationException, got {answered:?}"
            );
            assert_eq!(
                answered.text("message"),
                mismatch_message(content_kind),
                "case {case}: expected the mismatch naming the kind the document declared, \
                 got {answered:?}"
            );
            // A refused request writes nothing, so the data source holds no such document.
            let fetched =
                get_documents(&router, knowledge_base_id, &data_source_id, vec![identifier])
                    .await;
            let detail = fetched.details().first().cloned().unwrap_or(Value::Null);
            assert_eq!(
                detail["status"].as_str(),
                Some(NOT_FOUND),
                "case {case}: a refused request persists no document, got {detail}"
            );
        }
    }
}

/// RULE: a document pushed into a data source the service runs for a knowledge base is that
/// knowledge base's, so the runtime plane's content read hands out an address for exactly
/// the bytes the ingestion carried.
#[tokio::test]
async fn a_document_pushed_into_a_service_run_data_source_is_read_back_by_the_runtime_plane() {
    let router = build_router();
    let knowledge_base_id =
        knowledge_base(&router, service_managed_knowledge_base("connector-read-back")).await;
    let data_source_id = data_source(
        &router,
        "setup",
        &knowledge_base_id,
        service_run_data_source("read-back-connector", "CUSTOM", json!({})),
    )
    .await;

    // The rows differ along what the rule generalises over: the bytes the document carries
    // (ASCII, two- and three-byte UTF-8, a four-byte emoji, a combining sequence, and a text
    // far longer than one line) and how many documents one request pushes in — every row
    // below travels in a SINGLE ingestion, within the modelled `documents` bound of 1..10.
    let long = "a long document. ".repeat(256);
    let rows: Vec<(&str, &str)> = vec![
        ("read-back-ascii", "the plainest ASCII text"),
        ("read-back-utf8", "héllo wörld — ünïcode ✓"),
        ("read-back-emoji", "a four-byte character: 😀"),
        ("read-back-combining", "a combining sequence: e\u{301}"),
        ("read-back-long", long.as_str()),
        ("read-back-one-byte", "x"),
    ];

    let documents: Vec<Value> = rows
        .iter()
        .map(|(id, text)| document_of_kind("CUSTOM", id, text))
        .collect();
    let ingested = ingest(&router, &knowledge_base_id, &data_source_id, documents).await;
    assert_eq!(
        ingested.status, 202,
        "case setup: expected IngestKnowledgeBaseDocuments -> 202, got {ingested:?}"
    );
    assert_eq!(
        ingested.details().len(),
        rows.len(),
        "case setup: expected one detail per submitted document, got {:?}",
        ingested.body
    );

    for (id, text) in &rows {
        let case = &format!("the document {id}");
        let identifier = identifier_of_kind("CUSTOM", id);
        let detail =
            await_indexed(&router, case, &knowledge_base_id, &data_source_id, &identifier).await;
        assert_eq!(
            detail["identifier"], identifier,
            "case {case}: expected the detail of the document the request identified, got \
             {detail}"
        );

        let read = document_content(&router, &knowledge_base_id, &data_source_id, id).await;
        assert_eq!(
            read.status, 200,
            "case {case}: expected GetDocumentContent -> 200, got {read:?}"
        );
        assert_eq!(
            read.text("mimeType"),
            TEXT_MEDIA_TYPE,
            "case {case}: expected the media type of the text that was ingested, got {read:?}"
        );
        assert_eq!(
            read.body["documentContentLength"].as_i64(),
            Some(text.len() as i64),
            "case {case}: expected the byte count of the text that was ingested ({}), got \
             {read:?}",
            text.len()
        );

        let url = read.text("presignedUrl");
        assert!(
            !url.is_empty(),
            "case {case}: expected an address for the document's content, got {read:?}"
        );
        let (status, bytes) = follow(&router, case, &url).await;
        assert_eq!(
            status, 200,
            "case {case}: expected the address handed out to be served, got {status} with {}",
            String::from_utf8_lossy(&bytes)
        );
        assert_eq!(
            bytes.as_ref(),
            text.as_bytes(),
            "case {case}: expected the bytes the ingestion carried, got {:?}",
            String::from_utf8_lossy(&bytes)
        );
    }

    // A document of this data source that was never pushed in is the runtime plane's
    // modelled not-found, which is what proves the reads above were served from state the
    // ingestion wrote rather than from the request.
    let missing = document_content(
        &router,
        &knowledge_base_id,
        &data_source_id,
        "read-back-never-ingested",
    )
    .await;
    assert_eq!(
        missing.status, 404,
        "case a document never ingested: expected GetDocumentContent -> 404, got {missing:?}"
    );
    assert_eq!(
        missing.error_type.as_deref(),
        Some("ResourceNotFoundException"),
        "case a document never ingested: expected ResourceNotFoundException, got {missing:?}"
    );
}
