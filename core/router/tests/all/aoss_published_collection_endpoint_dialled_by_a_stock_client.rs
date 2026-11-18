// SPDX-License-Identifier: CC0-1.0

//! What a STOCK HTTPS client is served when it DIALS the address an OpenSearch Serverless
//! collection publishes — the address itself, over a socket, not the emulator's front door.
//!
//! RULE: every endpoint an emulated resource publishes for a caller to dial names a
//! `host:port` THIS emulator listens on, so a client that only has the published value —
//! `CollectionDetail$collectionEndpoint`, the "Collection-specific endpoint used to submit
//! index, search, and data upload requests to an OpenSearch Serverless collection" — reaches
//! the collection's OpenSearch REST surface at the ROOT of that address and is answered out
//! of what the collection holds. A production `<id>.<region>.aoss.amazonaws.com` host
//! resolves nowhere on the machine the emulator runs on, so publishing one leaves a client
//! that follows AWS's documented flow unable to send anything at all.
//!
//! The suite lives in the cross-service target because two planes have to agree for the rule
//! to be worth anything: the address and the REST surface behind it are
//! `service-com-amazonaws-opensearchserverless-opensearchserverless`'s, while the knowledge
//! base that writes its vectors into the index created through that address is the Bedrock
//! Agent build-time plane's, and the second test below reads what that plane wrote back out
//! at the dialled address.
//!
//! What the sibling suites pin, and this one does not repeat: the VALUE of the published
//! members and the surface reached through the router's front door
//! (`services/service-com-amazonaws-opensearchserverless-opensearchserverless/tests/all/\
//! announced_collection_endpoints.rs` and `collection_data_plane.rs`), the retrieval a
//! knowledge base over such an index serves
//! (`bedrock_knowledge_base_over_an_aoss_collection.rs`) and the ingestion's own accounting
//! (`bedrock_agent_ingestion_populates_its_aoss_index.rs`). Here every request travels over
//! a real TLS connection to the published port, which is the one thing a value in a field
//! cannot show.
//!
//! The first test's rows span the routes a knowledge-base build and search drives at that
//! address — the index create, the two reads of its schema, its settings, the document write
//! and read, both verbs of a search and the count — each with the status and the members the
//! answer must carry. They are driven in the written order, because a read is only worth
//! asserting after the write it reports.
//!
//! Every wait is a poll for the CONDITION; nothing here asserts how long a step took.

use std::time::Duration;

use serde_json::{json, Value};

use crate::bedrock_knowledge_base_over_an_aoss_collection::{
    active_collection, admit_network_access, at, await_active_knowledge_base,
    await_indexed_document, bedrock, build_router, encrypt_collections, grant_the_role_data_access,
    inline_byte_document, opensearch_storage, text, KB_ROLE, METADATA_FIELD, REGION, TEXT_FIELD,
    VECTOR_FIELD,
};

/// The access key id every dialled request presents. It names no IAM record, which is the
/// position an account's own published credential is in: the collection's `data` access
/// policies do not bound it, so the `network` policy alone admits the call.
const ACCESS_KEY_ID: &str = "AKIDDIALLED";

/// The OpenSearch Serverless signing name, which is the credential scope a client of a
/// collection endpoint signs for (`smithy_query(query="service_info")` -> `sigv4Name`, the
/// `aoss` model). The address names no service in its host, so this is what says which plane
/// the request speaks to.
const SIGNING_NAME: &str = "aoss";

/// How long one dial of the published address waits for its answer. Generous: nothing here
/// measures the wait, only what came back.
const DIAL_DEADLINE: Duration = Duration::from_secs(60);

/// How many values the vectors of the index hold: `amazon.titan-embed-text-v2:0` publishes
/// 1024-value embeddings, so an index created for it declares that dimension.
const DIMENSION: usize = 1024;

/// The embedding model the knowledge base of the second test names.
const MODEL: &str = "amazon.titan-embed-text-v2:0";

/// A SigV4 `Authorization` header naming [`ACCESS_KEY_ID`] in the collection's Region under
/// the OpenSearch Serverless signing name — what a stock `aoss` client sends. What this plane
/// reads off it is the CREDENTIAL SCOPE: which signing name the request speaks for and which
/// access key id made it, since the address names no service in its host. The signature value
/// itself is not checked here, so nothing in this suite computes one.
fn authorization() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={ACCESS_KEY_ID}/20260101/{REGION}/{SIGNING_NAME}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// The `host:port` a published endpoint addresses — the authority a client dials, with any
/// path dropped.
fn authority_of(endpoint: &str) -> String {
    let after_scheme = endpoint.strip_prefix("https://").unwrap_or(endpoint);
    after_scheme
        .split('/')
        .next()
        .unwrap_or(after_scheme)
        .to_string()
}

/// Assert `endpoint` is an address a client on this machine can DIAL — an HTTPS address
/// naming no AWS host and carrying the port it is served on — and answer its authority.
///
/// Reads a published URL, where `kinesisvideo_discovered_data_plane`'s check of the same shape
/// reads an authority that carries no scheme and no path: the four members under test here are
/// whole URLs, and that the scheme and the port are part of what is published is half of what
/// is being asserted.
fn assert_dialable(case: &str, endpoint: &str) -> String {
    assert!(
        endpoint.starts_with("https://"),
        "case {case:?}: expected an HTTPS address, got {endpoint:?}"
    );
    // AWS's own `<id>.<region>.aoss.amazonaws.com` host resolves nowhere on the machine this
    // emulator runs on, so a published address may not name one.
    assert!(
        !endpoint.contains("amazonaws.com"),
        "case {case:?}: expected an address this emulator serves rather than an AWS host, got \
         {endpoint:?}"
    );
    let authority = authority_of(endpoint);
    let port = authority.rsplit(':').next().unwrap_or_default();
    assert!(
        port.parse::<u16>().is_ok_and(|port| port != 0),
        "case {case:?}: expected the address to carry the port it is served on, got \
         {endpoint:?}"
    );
    authority
}

/// One OpenSearch REST request a stock client sends by DIALLING `endpoint` — a real TLS
/// connection to the published `host:port`, with the path appended to the address exactly as
/// the collection published it and no RPC target, because the OpenSearch REST API has none.
///
/// The exchange runs on a blocking thread, so the listener it addresses — a task of this same
/// runtime — is served while the call waits for the answer. The client does not verify this
/// emulator's self-signed certificate, which is what a client of a local emulator does.
///
/// `cloudfront_edge_generated_headers` dials a published address too, but for what a viewer
/// FETCHES: one GET of a URL, no credential, no body, the answer as text, over the scheme the
/// URL names. An OpenSearch client's request is the other shape — a verb per route, a JSON
/// body on the writes and the searches, a credential scope, a JSON answer, and TLS the
/// emulator terminates with a certificate of its own — so the two are not one helper.
async fn dial(
    case: &str,
    endpoint: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let url = format!("{endpoint}{path}");
    let verb = method.to_string();
    let named = case.to_string();
    let dialled = url.clone();
    ::tokio::task::spawn_blocking(move || {
        let client = ::reqwest::blocking::Client::builder()
            .danger_accept_invalid_certs(true)
            .timeout(DIAL_DEADLINE)
            .build()
            .expect("a TLS client");
        let verb = ::reqwest::Method::from_bytes(verb.as_bytes()).expect("a known HTTP method");
        let mut request = client
            .request(verb, &dialled)
            .header("authorization", authorization())
            .header("content-type", "application/json");
        if let Some(body) = &body {
            request = request.body(body.to_string());
        }
        let answered = request.send().unwrap_or_else(|error| {
            panic!("case {named:?}: the published address {dialled} answered nothing: {error}")
        });
        let status = answered.status().as_u16();
        let text = answered.text().unwrap_or_default();
        (
            status,
            ::serde_json::from_str(&text).unwrap_or(Value::Null),
        )
    })
    .await
    .unwrap_or_else(|error| panic!("case {case:?}: the dial of {url} did not finish: {error}"))
}

/// The index body a Bedrock knowledge base's vector index is created with: a `knn_vector`
/// field of [`DIMENSION`] values beside the text and metadata fields a storage configuration
/// maps, which is the body `opensearch-py`'s `indices.create` carries for one.
fn knn_index_body() -> Value {
    json!({
        "settings": { "index": { "knn": true } },
        "mappings": { "properties": {
            VECTOR_FIELD: { "type": "knn_vector", "dimension": DIMENSION },
            TEXT_FIELD: { "type": "text" },
            METADATA_FIELD: { "type": "text", "index": false },
        } },
    })
}

/// The first hit of a search answer, or `Value::Null` where it carries none.
fn first_hit(answer: &Value) -> Value {
    at(answer, "hits.hits")
        .as_array()
        .and_then(|hits| hits.first())
        .cloned()
        .unwrap_or(Value::Null)
}

/// One case: the request a stock client dials the published address with, the status it must
/// be answered, and the members that answer must carry — those of the answer itself, and
/// those of the first hit of a search.
struct Row {
    case: &'static str,
    method: &'static str,
    path: String,
    body: Option<Value>,
    status: u16,
    expected: Vec<(String, Value)>,
    first_hit: Vec<(String, Value)>,
}

/// RULE: the address a collection publishes is one this emulator listens on, so a stock HTTPS
/// client that dials it is served the collection's OpenSearch REST surface at the root of
/// that address, out of the indexes and documents the collection holds.
///
/// The rows span the routes a knowledge-base build and search drives there.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stock_client_dialling_the_published_endpoint_is_served_the_rest_surface() {
    const COLLECTION: &str = "dialled-collection";
    const INDEX: &str = "bedrock-knowledge-base-dialled";
    const DOCUMENT: &str = "chunk-1";
    const CHUNK: &str = "escalation-runbook-marker";

    let router = build_router();
    encrypt_collections(&router).await;
    admit_network_access(&router).await;
    let detail = active_collection(&router, COLLECTION).await;
    let endpoint = text(&detail, "collectionEndpoint");
    let authority = assert_dialable("an ACTIVE collection's published endpoint", &endpoint);

    let source = json!({ TEXT_FIELD: CHUNK, METADATA_FIELD: "{\"source\":\"runbook\"}" });
    let rows: Vec<Row> = vec![
        Row {
            case: "the vector index create a knowledge base requires",
            method: "PUT",
            path: format!("/{INDEX}"),
            body: Some(knn_index_body()),
            status: 200,
            expected: vec![
                ("acknowledged".to_string(), json!(true)),
                ("index".to_string(), json!(INDEX)),
            ],
            first_hit: Vec::new(),
        },
        Row {
            case: "the existence check a client makes before creating the index again",
            method: "HEAD",
            path: format!("/{INDEX}"),
            body: None,
            status: 200,
            expected: Vec::new(),
            first_hit: Vec::new(),
        },
        Row {
            case: "the whole index, as a client reads its schema back",
            method: "GET",
            path: format!("/{INDEX}"),
            body: None,
            status: 200,
            expected: vec![(
                format!("{INDEX}.mappings.properties.{VECTOR_FIELD}.dimension"),
                json!(DIMENSION),
            )],
            first_hit: Vec::new(),
        },
        Row {
            case: "the field mappings alone",
            method: "GET",
            path: format!("/{INDEX}/_mapping"),
            body: None,
            status: 200,
            expected: vec![
                (
                    format!("{INDEX}.mappings.properties.{TEXT_FIELD}.type"),
                    json!("text"),
                ),
                (
                    format!("{INDEX}.mappings.properties.{VECTOR_FIELD}.type"),
                    json!("knn_vector"),
                ),
            ],
            first_hit: Vec::new(),
        },
        Row {
            case: "the index settings, which carry the knn switch the create asked for",
            method: "GET",
            path: format!("/{INDEX}/_settings"),
            body: None,
            status: 200,
            expected: vec![
                (format!("{INDEX}.settings.index.knn"), json!(true)),
                (
                    format!("{INDEX}.settings.index.provided_name"),
                    json!(INDEX),
                ),
            ],
            first_hit: Vec::new(),
        },
        Row {
            case: "a document written into the index",
            method: "PUT",
            path: format!("/{INDEX}/_doc/{DOCUMENT}"),
            body: Some(source.clone()),
            status: 201,
            expected: vec![
                ("result".to_string(), json!("created")),
                ("_id".to_string(), json!(DOCUMENT)),
                ("_index".to_string(), json!(INDEX)),
            ],
            first_hit: Vec::new(),
        },
        Row {
            case: "that document read back by its id",
            method: "GET",
            path: format!("/{INDEX}/_doc/{DOCUMENT}"),
            body: None,
            status: 200,
            expected: vec![
                ("found".to_string(), json!(true)),
                ("_source".to_string(), source.clone()),
            ],
            first_hit: Vec::new(),
        },
        Row {
            case: "a search written as GET with a query body, as opensearch-py sends one",
            method: "GET",
            path: format!("/{INDEX}/_search"),
            body: Some(json!({ "query": { "match_all": {} } })),
            status: 200,
            expected: vec![("hits.total.value".to_string(), json!(1))],
            first_hit: vec![
                ("_id".to_string(), json!(DOCUMENT)),
                ("_index".to_string(), json!(INDEX)),
                ("_source".to_string(), source.clone()),
            ],
        },
        Row {
            case: "the same search written as POST, matching the chunk's own text",
            method: "POST",
            path: format!("/{INDEX}/_search"),
            body: Some(json!({ "query": { "match": { TEXT_FIELD: CHUNK } } })),
            status: 200,
            expected: vec![("hits.total.value".to_string(), json!(1))],
            first_hit: vec![("_id".to_string(), json!(DOCUMENT))],
        },
        Row {
            case: "the document count of the index",
            method: "GET",
            path: format!("/{INDEX}/_count"),
            body: None,
            status: 200,
            expected: vec![("count".to_string(), json!(1))],
            first_hit: Vec::new(),
        },
    ];

    for Row {
        case,
        method,
        path,
        body,
        status: expected_status,
        expected,
        first_hit: expected_hit,
    } in rows
    {
        let (status, answer) = dial(case, &endpoint, method, &path, body).await;
        assert_eq!(
            status, expected_status,
            "case {case:?}: expected {method} {path} dialled at {authority} to answer \
             {expected_status}, got {status} with {answer}"
        );
        for (member, value) in expected {
            assert_eq!(
                at(&answer, &member),
                value,
                "case {case:?}: expected {member} to be {value} in the answer to {method} \
                 {path}, got {answer}"
            );
        }
        let hit = first_hit(&answer);
        for (member, value) in expected_hit {
            assert_eq!(
                at(&hit, &member),
                value,
                "case {case:?}: expected the first hit's {member} to be {value} in the answer \
                 to {method} {path}, got {answer}"
            );
        }
    }

    // A route this surface serves nothing for is still answered BY the collection at the
    // dialled address — the request was delivered, which is the whole point — rather than
    // leaving the client with no answer at all.
    let (status, answer) = dial(
        "an index the collection does not hold",
        &endpoint,
        "GET",
        "/no-such-index",
        None,
    )
    .await;
    assert_eq!(
        status, 404,
        "case \"an index the collection does not hold\": expected the dialled address to \
         answer 404, got {status} with {answer}"
    );
}

/// RULE: the index a client created by dialling the published address is the one the Bedrock
/// Agent plane writes a knowledge base's vectors into, so a document ingested through that
/// knowledge base is read back AT the dialled address — one store behind the two planes, not
/// two.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_ingested_document_is_read_back_at_the_dialled_endpoint() {
    const COLLECTION: &str = "dialled-ingested";
    const INDEX: &str = "bedrock-knowledge-base-ingested";
    const KNOWLEDGE_BASE: &str = "kb-dialled";
    const DOCUMENT: &str = "doc-dialled";
    const BYTES: &str = "incident-review-marker";
    let case = "a knowledge base over an index created at the dialled address";

    let router = build_router();
    encrypt_collections(&router).await;
    admit_network_access(&router).await;
    grant_the_role_data_access(&router, COLLECTION).await;
    let detail = active_collection(&router, COLLECTION).await;
    let endpoint = text(&detail, "collectionEndpoint");
    let collection_arn = text(&detail, "arn");
    assert_dialable(case, &endpoint);

    // The vector index, created by DIALLING the published address.
    let (status, created) = dial(
        case,
        &endpoint,
        "PUT",
        &format!("/{INDEX}"),
        Some(knn_index_body()),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the index create at the dialled address to be served, got \
         {status} with {created}"
    );

    let (status, created) = bedrock(
        &router,
        "PUT",
        "/knowledgebases/",
        Some(&json!({
            "name": KNOWLEDGE_BASE,
            "roleArn": KB_ROLE,
            "knowledgeBaseConfiguration": {
                "type": "VECTOR",
                "vectorKnowledgeBaseConfiguration": {
                    "embeddingModelArn":
                        format!("arn:aws:bedrock:{REGION}::foundation-model/{MODEL}"),
                },
            },
            "storageConfiguration": opensearch_storage(&collection_arn, INDEX),
        })),
    )
    .await;
    assert_eq!(
        status, 202,
        "case {case:?}: expected CreateKnowledgeBase over the dialled index to be accepted, \
         got {status} with {created}"
    );
    let knowledge_base_id = text(&created, "knowledgeBase.knowledgeBaseId");
    let read = await_active_knowledge_base(&router, case, &knowledge_base_id).await;
    assert_eq!(
        at(&read, "storageConfiguration"),
        opensearch_storage(&collection_arn, INDEX),
        "case {case:?}: expected the knowledge base to report the index it was created over, \
         got {read}"
    );

    let (status, created) = bedrock(
        &router,
        "PUT",
        &format!("/knowledgebases/{knowledge_base_id}/datasources/"),
        Some(&json!({
            "name": format!("{KNOWLEDGE_BASE}-pushed"),
            "dataSourceConfiguration": { "type": "CUSTOM" },
        })),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected CreateDataSource(CUSTOM) accepted, got {status} with {created}"
    );
    let data_source_id = text(&created, "dataSource.dataSourceId");

    let (status, ingested) = bedrock(
        &router,
        "PUT",
        &format!("/knowledgebases/{knowledge_base_id}/datasources/{data_source_id}/documents"),
        Some(&json!({ "documents": [inline_byte_document(DOCUMENT, BYTES)] })),
    )
    .await;
    assert_eq!(
        status, 202,
        "case {case:?}: expected the ingestion accepted, got {status} with {ingested}"
    );
    // The document reaches INDEXED on its own, with nobody calling: the clock owes it.
    await_indexed_document(
        &router,
        case,
        &knowledge_base_id,
        &data_source_id,
        DOCUMENT,
    )
    .await;

    // ...and what that ingestion wrote is in the index AT the address the collection
    // published: the count the dialled client reads is the document the knowledge base took.
    let (status, counted) = dial(case, &endpoint, "GET", &format!("/{INDEX}/_count"), None).await;
    assert_eq!(
        (status, at(&counted, "count")),
        (200, json!(1)),
        "case {case:?}: expected the dialled address to count the ingested document, got \
         {status} with {counted}"
    );
    let (status, found) = dial(
        case,
        &endpoint,
        "POST",
        &format!("/{INDEX}/_search"),
        Some(json!({ "query": { "match_all": {} } })),
    )
    .await;
    assert_eq!(
        (status, at(&found, "hits.total.value")),
        (200, json!(1)),
        "case {case:?}: expected the dialled address to search the ingested document, got \
         {status} with {found}"
    );
    assert_eq!(
        at(&first_hit(&found), &format!("_source.{TEXT_FIELD}")),
        json!(BYTES),
        "case {case:?}: expected the chunk the ingestion carried, got {found}"
    );
}
