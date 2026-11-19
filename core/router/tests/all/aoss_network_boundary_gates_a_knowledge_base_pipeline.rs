// SPDX-License-Identifier: CC0-1.0

//! What an OpenSearch Serverless collection's `network` boundary does to the requests a
//! knowledge-base pipeline makes at the address that collection publishes, and which FIELDS
//! of the bound index the ingestion behind that boundary writes into.
//!
//! Two rules are pinned here, and each spans three crates — OpenSearch Serverless publishes
//! the endpoint, holds the policies and holds the indexes and documents; Amazon S3 holds the
//! source objects; the Amazon Bedrock Agent build-time plane runs the knowledge base and its
//! ingestion — so the suite lives in this cross-service target:
//!
//! * a request no `network` security policy admits is refused at the endpoint's own front
//!   door, BEFORE the collection's records are addressed at all, so it changes nothing the
//!   collection holds: no index exists for a refused create, no document for a refused write,
//!   and a refused delete takes nothing away. Adding a policy that covers the collection —
//!   with everything else unchanged — makes the identical requests serve, and a knowledge
//!   base over that collection and index then reaches its terminal status;
//! * every field an ingestion writes a chunk into is one the knowledge base's own
//!   `fieldMapping` NAMES ("the name of the field in which Amazon Bedrock stores the vector
//!   embeddings for your data sources", the raw text, and the metadata about the vector
//!   store), so the honoured names are the configured ones whatever they are — the embedding
//!   under the configured vector field with one value per dimension the bound index declares,
//!   the raw text under the configured text field, the metadata recording where the source
//!   document lives under the configured metadata field, and neither of those two under a
//!   default name the configuration did not ask for.
//!
//! The rows of the second test span what that rule generalises over: the field names AWS's own
//! vector-index guide gives, names of the caller's own choosing, one-character names, names
//! carrying digits, hyphens, underscores and upper case, and a name at the modelled maximum
//! length of `OpenSearchServerlessFieldMapping`'s members
//! (`smithy_query(query="shape",shape="OpenSearchServerlessFieldMapping")`: `FieldName`,
//! length max 2048, pattern `^.*$`) — each with the dimension its own index declares. The
//! first test states its admitting policy in both source forms a network policy has — public,
//! and through one VPC endpoint — because either covers the collection and so either admits.
//!
//! What the sibling suites pin, and this one does not repeat: the status and the empty body a
//! network refusal carries, per route
//! (`services/service-com-amazonaws-opensearchserverless-opensearchserverless/tests/all/\
//! network_boundary_refusal.rs`), the ingestion's own accounting and a data source's
//! `dataDeletionPolicy` (`services/service-com-amazonaws-bedrockagent-\
//! amazonbedrockagentbuildtimelambda/tests/all/ingestion_writes_the_bound_collection_index.rs`
//! and `bedrock_agent_ingestion_populates_its_aoss_index.rs` beside this file), and a running
//! Lambda handler's own signed request to the published address
//! (`lambda_signed_write_to_an_aoss_collection_endpoint.rs`, also beside this file).
//!
//! Every wait is a poll for the CONDITION under a generous deadline; nothing here asserts how
//! long a step took.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::bedrock_knowledge_base_over_an_aoss_collection::{
    active_collection, addressed, aoss, at, authorization, await_active_knowledge_base, bedrock,
    encrypt_collections, grant_the_role_data_access, rest, text, KB_ROLE, METADATA_FIELD, REGION,
    TEXT_FIELD, VECTOR_FIELD,
};

/// The OpenSearch Serverless signing name, which is the scope a client of a collection
/// endpoint signs for (`smithy_query(query="service_info")` -> `sigv4Name`, the `aoss` model).
const AOSS_SIGNING_NAME: &str = "aoss";

/// Amazon S3's signing name, which routes the object writes the data sources crawl.
const S3_SIGNING_NAME: &str = "s3";

/// The status the endpoint's network front door refuses an inadmissible request with: a client
/// of the endpoint reads it as an authentication failure.
const NETWORK_REFUSAL: u16 = 401;

/// How many times a read looks again for a transition the clock owes, and how long one look
/// waits before the next. Generous — minutes rather than seconds, because a host running every
/// suite of this workspace at once is far slower than one running this file alone: two minutes
/// in all, looked at every 25 ms so a stage that does settle is seen to have settled at once.
/// Every wait here is for the CONDITION and nothing asserts how long it took.
const SETTLE_ATTEMPTS: usize = 4_800;
const SETTLE_PAUSE: Duration = Duration::from_millis(25);

/// The VPC endpoint the second admitting policy of the first test names as its only source: a
/// well-formed `vpce-` id, which is what a client that created one names in a policy.
const VPC_ENDPOINT: &str = "vpce-0123456789abcdef0";

/// The metadata field a knowledge base records a chunk's source location under, as the
/// published knowledge-base metadata fields name it.
const SOURCE_URI_METADATA_KEY: &str = "x-amz-bedrock-kb-source-uri";

/// The three services these rules span, over ONE engine, as `core/router/src/lib.rs` mounts
/// them. Request authentication is off, as in every other suite here: what each call carries
/// is a credential scope, which is what routes it and what each handler reads its Region and
/// its caller from.
///
/// The sibling helper module this file reuses builds a router of the Bedrock planes and the
/// collection alone; the ingestion below crawls Amazon S3 objects, so the router is built
/// here with that service registered beside them.
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

/// One request against Amazon S3's REST surface, path-style; answers the status and the body.
async fn s3(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    content_type: &str,
    body: &str,
) -> (u16, String) {
    let host = format!("{S3_SIGNING_NAME}.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host.clone())
        .header("authorization", authorization(S3_SIGNING_NAME))
        .header("content-type", content_type)
        .body(Bytes::from(body.to_owned()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body().as_ref()).to_string(),
    )
}

/// Create the bucket the source documents live in, in the Region every call here is signed
/// for.
///
/// The create carries a `CreateBucketConfiguration` naming that Region, because "if you don't
/// specify a Region, the bucket is created in the US East (N. Virginia) Region (us-east-1) by
/// default" (`CreateBucketConfiguration$LocationConstraint`) and a regional endpoint cannot
/// place a bucket in another Region.
async fn create_bucket(router: &MultiServiceRouter, case: &str, bucket: &str) {
    let configuration = format!(
        "<CreateBucketConfiguration><LocationConstraint>{REGION}</LocationConstraint>\
         </CreateBucketConfiguration>"
    );
    let (status, answer) = s3(
        router,
        "PUT",
        &format!("/{bucket}"),
        "application/xml",
        &configuration,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected CreateBucket({bucket:?}) in {REGION} to succeed, got {status} \
         with {answer}"
    );
}

/// Put one source object in the bucket, which is what a data source's crawl scans.
async fn put_object(router: &MultiServiceRouter, case: &str, path: &str, body: &str) {
    let (status, answer) = s3(router, "PUT", path, "text/plain", body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected PutObject({path:?}) to succeed, got {status} with {answer}"
    );
}

/// One OpenSearch REST request to a collection's PUBLISHED endpoint, answered with the status
/// and the RAW BYTES it carried.
///
/// The bytes are what the sibling `rest` helper cannot report: a refusal taken at the
/// endpoint's front door carries NO body, and "no body" and "a body that is not JSON" are the
/// same `null` once parsed. The request body is text rather than a JSON value, so a bulk write
/// (newline-delimited JSON) is sent exactly as a client sends one.
async fn rest_bytes(
    router: &MultiServiceRouter,
    endpoint: &str,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> (u16, Vec<u8>) {
    let (authority, prefix) = addressed(endpoint);
    let request = http::Request::builder()
        .method(method)
        .uri(format!("{prefix}{path}"))
        .header("host", authority)
        .header("content-type", "application/json")
        .header("authorization", authorization(AOSS_SIGNING_NAME))
        .body(Bytes::from(body.unwrap_or_default().to_owned()))
        .expect("a well-formed data-plane request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        response.body().as_ref().to_vec(),
    )
}

/// A `network` security policy document covering one collection's endpoint, admitting public
/// traffic — AWS's "Network access for Amazon OpenSearch Serverless": "you must have at least
/// one network policy that applies to a collection in order to access its endpoint".
fn public_network_policy(collection: &str) -> Value {
    json!({
        "Rules": [{
            "ResourceType": "collection",
            "Resource": [format!("collection/{collection}")],
        }],
        "AllowFromPublic": true,
    })
}

/// The same policy with the OTHER source clause a network policy states: not public, admitted
/// through one VPC endpoint. A client of this emulator dials the one address this process
/// serves the collection at, so there is no network for a request to have arrived from and no
/// stored source to compare it against (`services/service-com-amazonaws-opensearchserverless-\
/// opensearchserverless/src/state/network_access.rs`) — the boundary is whether a policy's
/// rules COVER the collection, which this document and the one above state the same way.
fn vpc_endpoint_network_policy(collection: &str, vpc_endpoint: &str) -> Value {
    json!({
        "Rules": [{
            "ResourceType": "collection",
            "Resource": [format!("collection/{collection}")],
        }],
        "AllowFromPublic": false,
        "SourceVPCEs": [vpc_endpoint],
    })
}

/// Write the `network` security policy `name` carrying `policy`, which is the control plane
/// admitting traffic to the collection's endpoint.
async fn admit_the_collection(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    policy: &Value,
) {
    let (status, answer) = aoss(
        router,
        "CreateSecurityPolicy",
        json!({
            "type": "network",
            "name": name,
            "policy": policy.to_string(),
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the network policy {name:?} accepted, got {status} with \
         {answer}"
    );
}

/// Take the `network` security policy `name` away, which closes the endpoint again.
async fn close_the_collection(router: &MultiServiceRouter, case: &str, name: &str) {
    let (status, answer) = aoss(
        router,
        "DeleteSecurityPolicy",
        json!({ "type": "network", "name": name }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the network policy {name:?} deleted, got {status} with \
         {answer}"
    );
}

/// The index body a vector knowledge base's index is created with: a `knn_vector` field of
/// `dimension` values under the name the storage configuration maps the embeddings to, beside
/// the text and metadata fields it maps — the body `opensearch-py`'s `indices.create` carries
/// for one.
fn knn_index_body(
    vector_field: &str,
    text_field: &str,
    metadata_field: &str,
    dimension: usize,
) -> Value {
    json!({
        "settings": { "index": { "knn": true } },
        "mappings": { "properties": {
            vector_field: { "type": "knn_vector", "dimension": dimension },
            text_field: { "type": "text" },
            metadata_field: { "type": "text", "index": false },
        } },
    })
}

/// The `storageConfiguration` of a knowledge base whose vectors live in `index` of the
/// collection `collection_arn` names, mapped to the three field names given.
fn opensearch_storage(
    collection_arn: &str,
    index: &str,
    vector_field: &str,
    text_field: &str,
    metadata_field: &str,
) -> Value {
    json!({
        "type": "OPENSEARCH_SERVERLESS",
        "opensearchServerlessConfiguration": {
            "collectionArn": collection_arn,
            "vectorIndexName": index,
            "fieldMapping": {
                "vectorField": vector_field,
                "textField": text_field,
                "metadataField": metadata_field,
            },
        },
    })
}

/// Start an ingestion job for one data source and POLL `GetIngestionJob` until the clock has
/// completed it, answering the completed job's statistics.
///
/// Local rather than the sibling suites': those poll through a `bedrock` of their own, signed
/// for THEIR Region, and a knowledge base created in one Region is not a knowledge base of
/// another — so the poll has to go through the same `bedrock` helper this file's knowledge
/// bases were created with.
async fn run_ingestion_job(
    router: &MultiServiceRouter,
    case: &str,
    knowledge_base_id: &str,
    data_source_id: &str,
) -> Value {
    let jobs =
        format!("/knowledgebases/{knowledge_base_id}/datasources/{data_source_id}/ingestionjobs");
    let (status, started) = bedrock(router, "PUT", &format!("{jobs}/"), Some(&json!({}))).await;
    assert_eq!(
        status, 202,
        "case {case:?}: expected StartIngestionJob accepted, got {status} with {started}"
    );
    let job_id = text(&started, "ingestionJob.ingestionJobId");
    for _ in 0..SETTLE_ATTEMPTS {
        let (status, read) = bedrock(router, "GET", &format!("{jobs}/{job_id}"), None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected GetIngestionJob({job_id}) to answer, got {status} with \
             {read}"
        );
        if text(&read, "ingestionJob.status") == "COMPLETE" {
            return at(&read, "ingestionJob.statistics");
        }
        tokio::time::sleep(SETTLE_PAUSE).await;
    }
    panic!("case {case:?}: the ingestion job {job_id} never reached COMPLETE");
}

/// One request the collection's network boundary refuses: the route a client asks for and the
/// body it carries.
struct Refused {
    case: &'static str,
    method: &'static str,
    path: String,
    body: Option<String>,
}

/// RULE: a request no `network` security policy admits is refused at the endpoint's front door
/// before the collection's records are addressed, so it changes NOTHING the collection holds —
/// and a policy covering the collection, with everything else unchanged, makes the identical
/// requests serve and lets a knowledge base over that collection and index reach its terminal
/// status.
#[tokio::test]
async fn a_network_refusal_leaves_the_collection_exactly_as_it_was() {
    const COLLECTION: &str = "gated-collection";
    const INDEX: &str = "bedrock-knowledge-base-gated";
    const POLICY: &str = "gated-network";
    const DOCUMENT: &str = "chunk-1";
    const NEVER_WRITTEN: &str = "chunk-never-written";
    const DIMENSION: usize = 4;
    const MODEL: &str = "amazon.titan-embed-text-v2:0";
    let case = "a collection no network policy covers";

    let router = build_router();
    encrypt_collections(&router).await;
    grant_the_role_data_access(&router, COLLECTION).await;
    // The collection is created and reaches ACTIVE with NO network policy in place: the
    // control plane settles it either way, and the endpoint it publishes is what the boundary
    // decides about.
    let detail = active_collection(&router, COLLECTION).await;
    let endpoint = text(&detail, "collectionEndpoint");
    let collection_arn = text(&detail, "arn");
    assert_eq!(
        text(&detail, "status"),
        "ACTIVE",
        "case {case:?}: expected the collection itself to be ACTIVE, got {detail}"
    );

    let source = json!({
        TEXT_FIELD: "the chunk written once the boundary admits the write",
        METADATA_FIELD: "{\"source\":\"the suite\"}",
    });
    let refused = vec![
        Refused {
            case: "the index create a vector knowledge base needs",
            method: "PUT",
            path: format!("/{INDEX}"),
            body: Some(
                knn_index_body(VECTOR_FIELD, TEXT_FIELD, METADATA_FIELD, DIMENSION).to_string(),
            ),
        },
        Refused {
            case: "a document write into that index",
            method: "PUT",
            path: format!("/{INDEX}/_doc/{DOCUMENT}"),
            body: Some(source.to_string()),
        },
        Refused {
            case: "a bulk write of one document",
            method: "POST",
            path: "/_bulk".to_string(),
            body: Some(format!(
                "{}\n{}\n",
                json!({ "index": { "_index": INDEX, "_id": DOCUMENT } }),
                source
            )),
        },
        Refused {
            case: "a mapping write on the index",
            method: "PUT",
            path: format!("/{INDEX}/_mapping"),
            body: Some(json!({ "properties": { "added": { "type": "text" } } }).to_string()),
        },
        Refused {
            case: "the index delete",
            method: "DELETE",
            path: format!("/{INDEX}"),
            body: None,
        },
        Refused {
            case: "a search of the index",
            method: "POST",
            path: format!("/{INDEX}/_search"),
            body: Some(json!({ "query": { "match_all": {} } }).to_string()),
        },
    ];
    for Refused {
        case: route,
        method,
        path,
        body,
    } in &refused
    {
        let (status, payload) = rest_bytes(&router, &endpoint, method, path, body.as_deref()).await;
        let served = String::from_utf8_lossy(&payload).to_string();
        assert_eq!(
            (status, payload.is_empty()),
            (NETWORK_REFUSAL, true),
            "case {case:?} / {route}: expected {method} {path} to be refused \
             {NETWORK_REFUSAL} with no bytes, got {status} with {served:?}"
        );
    }

    // ...and ONLY the network policy is created: nothing else about the collection, its
    // encryption policy or its data access policy changes.
    admit_the_collection(&router, case, POLICY, &public_network_policy(COLLECTION)).await;

    // What the refused requests asked for is not there: the endpoint now answers, and it
    // answers that the index and the document do not exist.
    for (probe, method, path) in [
        ("the index the refused create addressed", "HEAD", format!("/{INDEX}")),
        ("the index read", "GET", format!("/{INDEX}")),
        (
            "the document the refused writes addressed",
            "GET",
            format!("/{INDEX}/_doc/{DOCUMENT}"),
        ),
    ] {
        let (status, answer) = rest(&router, &endpoint, method, &path, None).await;
        assert_eq!(
            status, 404,
            "case {case:?} / {probe}: expected the admitted endpoint to report it absent, got \
             {status} with {answer}"
        );
    }

    // The IDENTICAL requests the boundary refused are now served, out of the collection's own
    // records.
    let (status, created) = rest(
        &router,
        &endpoint,
        "PUT",
        &format!("/{INDEX}"),
        Some(&knn_index_body(
            VECTOR_FIELD,
            TEXT_FIELD,
            METADATA_FIELD,
            DIMENSION,
        )),
    )
    .await;
    assert_eq!(
        (status, at(&created, "acknowledged")),
        (200, json!(true)),
        "case {case:?}: expected the identical index create to be served once the policy \
         admits it, got {status} with {created}"
    );
    let (status, written) = rest(
        &router,
        &endpoint,
        "PUT",
        &format!("/{INDEX}/_doc/{DOCUMENT}"),
        Some(&source),
    )
    .await;
    assert_eq!(
        (status, text(&written, "result")),
        (201, "created".to_string()),
        "case {case:?}: expected the identical document write to be served, got {status} with \
         {written}"
    );
    let (status, read) = rest(
        &router,
        &endpoint,
        "GET",
        &format!("/{INDEX}/_doc/{DOCUMENT}"),
        None,
    )
    .await;
    assert_eq!(
        (status, at(&read, "_source")),
        (200, source.clone()),
        "case {case:?}: expected the written document read back, got {status} with {read}"
    );

    // ...and a knowledge base over that collection and index reaches its terminal status, so
    // the pipeline the boundary was blocking runs to the end.
    let storage = opensearch_storage(
        &collection_arn,
        INDEX,
        VECTOR_FIELD,
        TEXT_FIELD,
        METADATA_FIELD,
    );
    let (status, knowledge_base) = bedrock(
        &router,
        "PUT",
        "/knowledgebases/",
        Some(&json!({
            "name": COLLECTION,
            "roleArn": KB_ROLE,
            "knowledgeBaseConfiguration": {
                "type": "VECTOR",
                "vectorKnowledgeBaseConfiguration": {
                    "embeddingModelArn":
                        format!("arn:aws:bedrock:{REGION}::foundation-model/{MODEL}"),
                },
            },
            "storageConfiguration": storage,
        })),
    )
    .await;
    assert_eq!(
        status, 202,
        "case {case:?}: expected CreateKnowledgeBase over the admitted collection accepted, \
         got {status} with {knowledge_base}"
    );
    let knowledge_base_id = text(&knowledge_base, "knowledgeBase.knowledgeBaseId");
    let settled = await_active_knowledge_base(&router, case, &knowledge_base_id).await;
    assert_eq!(
        text(&settled, "status"),
        "ACTIVE",
        "case {case:?}: expected the knowledge base to report ACTIVE, got {settled}"
    );

    // The boundary is the policy state the control plane holds at this instant, and a refusal
    // under the closed boundary still changes nothing: the delete takes no index away and the
    // write stores no document.
    close_the_collection(&router, case, POLICY).await;
    for Refused {
        case: route,
        method,
        path,
        body,
    } in [
        Refused {
            case: "the delete of the index that now exists",
            method: "DELETE",
            path: format!("/{INDEX}"),
            body: None,
        },
        Refused {
            case: "a write of a second document",
            method: "PUT",
            path: format!("/{INDEX}/_doc/{NEVER_WRITTEN}"),
            body: Some(source.to_string()),
        },
    ] {
        let (status, payload) = rest_bytes(&router, &endpoint, method, &path, body.as_deref()).await;
        let served = String::from_utf8_lossy(&payload).to_string();
        assert_eq!(
            (status, payload.is_empty()),
            (NETWORK_REFUSAL, true),
            "case {case:?} / {route}: expected {method} {path} to be refused \
             {NETWORK_REFUSAL} with no bytes once the policy is gone, got {status} with \
             {served:?}"
        );
    }
    // The policy is written again in the OTHER source form a network policy states — not
    // public, admitted through one VPC endpoint, which is the form a client that created an
    // endpoint writes. It covers the same collection, so it admits the same requests: the
    // boundary this emulator can observe is coverage, not where a request came from.
    admit_the_collection(
        &router,
        case,
        POLICY,
        &vpc_endpoint_network_policy(COLLECTION, VPC_ENDPOINT),
    )
    .await;
    let (status, present) = rest(&router, &endpoint, "HEAD", &format!("/{INDEX}"), None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the index to have survived the refused delete, got {status} \
         with {present}"
    );
    let (status, counted) = rest(
        &router,
        &endpoint,
        "POST",
        &format!("/{INDEX}/_count"),
        Some(&json!({ "query": { "match_all": {} } })),
    )
    .await;
    assert_eq!(
        (status, at(&counted, "count")),
        (200, json!(1)),
        "case {case:?}: expected the one written document and nothing the refused write \
         stored, got {status} with {counted}"
    );
}

/// One field mapping an ingestion is run under: the three names the knowledge base maps, the
/// dimension the bound index declares, and the index and source prefix of its own.
struct Mapping {
    case: &'static str,
    index: String,
    prefix: &'static str,
    vector_field: String,
    text_field: String,
    metadata_field: String,
    dimension: usize,
    /// The embedding model the knowledge base declares. The dimension written is the BOUND
    /// index's own, so a row whose index declares fewer values than its model publishes is
    /// still written in the index's.
    model: &'static str,
}

/// Every field naming the rows span. The first is the naming AWS's own "Create a vector index
/// in Amazon OpenSearch Service" gives a knowledge base's index; the rest are namings a caller
/// may choose, up to the modelled maximum length of a `FieldName`
/// (`smithy_query(query="shape",shape="OpenSearchServerlessFieldMapping")`: length max 2048,
/// pattern `^.*$`).
fn mappings() -> Vec<Mapping> {
    vec![
        Mapping {
            case: "the field names AWS's own vector-index guide gives",
            index: "kb-mapped-default".to_string(),
            prefix: "default/",
            vector_field: VECTOR_FIELD.to_string(),
            text_field: TEXT_FIELD.to_string(),
            metadata_field: METADATA_FIELD.to_string(),
            dimension: 1536,
            model: "amazon.titan-embed-text-v1",
        },
        Mapping {
            case: "names of the caller's own choosing",
            index: "kb-mapped-chosen".to_string(),
            prefix: "chosen/",
            vector_field: "embedding".to_string(),
            text_field: "chunk".to_string(),
            metadata_field: "meta".to_string(),
            dimension: 1024,
            model: "amazon.titan-embed-text-v2:0",
        },
        Mapping {
            case: "one-character names",
            index: "kb-mapped-terse".to_string(),
            prefix: "terse/",
            vector_field: "v".to_string(),
            text_field: "t".to_string(),
            metadata_field: "m".to_string(),
            dimension: 8,
            model: "amazon.titan-embed-text-v2:0",
        },
        Mapping {
            case: "names carrying digits, hyphens, underscores and upper case",
            index: "kb-mapped-mixed".to_string(),
            prefix: "mixed/",
            vector_field: "Vector_Field-01".to_string(),
            text_field: "Text_Field-01".to_string(),
            metadata_field: "Metadata_Field-01".to_string(),
            dimension: 384,
            model: "amazon.titan-embed-text-v1",
        },
        Mapping {
            case: "a vector field name at the modelled maximum length",
            index: "kb-mapped-longest".to_string(),
            prefix: "longest/",
            vector_field: "v".repeat(2_048),
            text_field: "t".repeat(2_048),
            metadata_field: "m".repeat(2_048),
            dimension: 16,
            model: "amazon.titan-embed-text-v2:0",
        },
    ]
}

/// RULE: every field an ingestion writes a chunk into is one the knowledge base's own
/// `fieldMapping` names — the embedding under the configured vector field with one value per
/// dimension the bound index declares, the raw text under the configured text field, the
/// metadata about the vector store under the configured metadata field — and no field of a
/// name the configuration did not ask for.
#[tokio::test]
async fn an_ingestion_writes_the_fields_the_knowledge_base_maps() {
    const COLLECTION: &str = "mapped-collection";
    const BUCKET: &str = "mapped-collection-source";
    let setup = "a collection admitted by a network policy";

    let router = build_router();
    encrypt_collections(&router).await;
    admit_the_collection(
        &router,
        setup,
        "mapped-network",
        &public_network_policy(COLLECTION),
    )
    .await;
    grant_the_role_data_access(&router, COLLECTION).await;
    let detail = active_collection(&router, COLLECTION).await;
    let endpoint = text(&detail, "collectionEndpoint");
    let collection_arn = text(&detail, "arn");
    create_bucket(&router, setup, BUCKET).await;

    for (position, mapping) in mappings().iter().enumerate() {
        let case = mapping.case;
        let index = mapping.index.as_str();
        let vector_field = mapping.vector_field.as_str();
        let text_field = mapping.text_field.as_str();
        let metadata_field = mapping.metadata_field.as_str();
        let key = format!("{}source.txt", mapping.prefix);
        let body = format!("the source document of {case}");
        let source_uri = format!("s3://{BUCKET}/{key}");
        put_object(&router, case, &format!("/{BUCKET}/{key}"), &body).await;

        // The index the knowledge base will name, created at the address the collection
        // publishes, declaring exactly the fields the configuration below maps.
        let (status, created) = rest(
            &router,
            &endpoint,
            "PUT",
            &format!("/{index}"),
            Some(&knn_index_body(
                vector_field,
                text_field,
                metadata_field,
                mapping.dimension,
            )),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the index create at the published endpoint to be served, \
             got {status} with {created}"
        );

        let (status, knowledge_base) = bedrock(
            &router,
            "PUT",
            "/knowledgebases/",
            Some(&json!({
                "name": format!("kb-mapped-{position}"),
                "roleArn": KB_ROLE,
                "knowledgeBaseConfiguration": {
                    "type": "VECTOR",
                    "vectorKnowledgeBaseConfiguration": {
                        "embeddingModelArn": format!(
                            "arn:aws:bedrock:{REGION}::foundation-model/{}",
                            mapping.model
                        ),
                    },
                },
                "storageConfiguration": opensearch_storage(
                    &collection_arn,
                    index,
                    vector_field,
                    text_field,
                    metadata_field,
                ),
            })),
        )
        .await;
        assert_eq!(
            status, 202,
            "case {case:?}: expected CreateKnowledgeBase accepted, got {status} with \
             {knowledge_base}"
        );
        let knowledge_base_id = text(&knowledge_base, "knowledgeBase.knowledgeBaseId");
        let settled = await_active_knowledge_base(&router, case, &knowledge_base_id).await;
        assert_eq!(
            text(&settled, "status"),
            "ACTIVE",
            "case {case:?}: expected the knowledge base to report ACTIVE, got {settled}"
        );

        let (status, data_source) = bedrock(
            &router,
            "PUT",
            &format!("/knowledgebases/{knowledge_base_id}/datasources/"),
            Some(&json!({
                "name": format!("kb-mapped-source-{position}"),
                "dataSourceConfiguration": {
                    "type": "S3",
                    "s3Configuration": {
                        "bucketArn": format!("arn:aws:s3:::{BUCKET}"),
                        "inclusionPrefixes": [mapping.prefix],
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

        let statistics = run_ingestion_job(&router, case, &knowledge_base_id, &data_source_id).await;
        for (member, expected) in [
            ("numberOfDocumentsScanned", 1),
            ("numberOfNewDocumentsIndexed", 1),
            ("numberOfDocumentsFailed", 0),
        ] {
            // Read as a member that has to BE there: a statistic the job did not report is a
            // job that accounted for nothing, which no row of this table may pass on.
            let reported = at(&statistics, member).as_i64();
            assert_eq!(
                reported,
                Some(expected),
                "case {case:?}: expected statistics.{member} to be {expected}, got \
                 {reported:?} in {statistics}"
            );
        }

        // ...and what the completed job wrote is read back at the published address, in the
        // fields the configuration MAPS and in no others.
        let (status, searched) = rest(
            &router,
            &endpoint,
            "POST",
            &format!("/{index}/_search"),
            Some(&json!({ "query": { "match_all": {} }, "size": 10 })),
        )
        .await;
        assert_eq!(
            (status, at(&searched, "hits.total.value")),
            (200, json!(1)),
            "case {case:?}: expected the collection to answer for the one indexed document, \
             got {status} with {} hit(s)",
            at(&searched, "hits.hits")
                .as_array()
                .map(Vec::len)
                .unwrap_or_default()
        );
        let hit = at(&searched, "hits.hits")
            .as_array()
            .and_then(|hits| hits.first())
            .cloned()
            .unwrap_or(Value::Null);
        assert_eq!(
            text(&hit, "_id"),
            source_uri,
            "case {case:?}: expected the chunk keyed by where its source document lives, got \
             {:?}",
            text(&hit, "_id")
        );
        let held = at(&hit, "_source");
        let members = held.as_object().cloned().unwrap_or_else(|| {
            panic!("case {case:?}: expected a document source, got {held}")
        });
        // The NAMES the chunk was written under, which is what every assertion below is
        // about: the values include an embedding of up to the declared dimension, so a
        // failure names the fields rather than dumping every value of one.
        let names: Vec<&str> = members.keys().map(String::as_str).collect();

        assert_eq!(
            members.get(text_field).and_then(Value::as_str),
            Some(body.as_str()),
            "case {case:?}: expected the source document's own text under the mapped text \
             field {text_field:?}, got the fields {names:?}"
        );
        let values = members
            .get(vector_field)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            values.len(),
            mapping.dimension,
            "case {case:?}: expected the embedding under the mapped vector field \
             {vector_field:?} to carry one value per declared dimension, got {} value(s) \
             among the fields {names:?}",
            values.len()
        );
        assert!(
            values.iter().all(Value::is_number),
            "case {case:?}: expected every value of the mapped vector field {vector_field:?} \
             to be numeric, got {} non-numeric value(s)",
            values.iter().filter(|value| !value.is_number()).count()
        );
        let recorded = members
            .get(metadata_field)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let parsed: Value = serde_json::from_str(&recorded).unwrap_or(Value::Null);
        assert_eq!(
            at(&parsed, SOURCE_URI_METADATA_KEY),
            json!(source_uri),
            "case {case:?}: expected the mapped metadata field {metadata_field:?} to record \
             where the source document lives, got {recorded:?} among the fields {names:?}"
        );

        // The names are the CONFIGURED ones: the embedding and the metadata document are
        // where this row's mapping put them and nowhere else, so a default name it did not
        // ask for is not a field of the written chunk.
        //
        // The default TEXT field name is not checked for absence: it is also the published
        // knowledge-base metadata key a chunk's own text is recorded under
        // (`AMAZON_BEDROCK_TEXT_CHUNK`, which the fields beside the mapped three carry
        // whatever the mapping names), so its presence says nothing about the mapping. What
        // the mapped text field holds is asserted above.
        for unmapped in [VECTOR_FIELD, METADATA_FIELD] {
            if [vector_field, text_field, metadata_field].contains(&unmapped) {
                continue;
            }
            assert!(
                !members.contains_key(unmapped),
                "case {case:?}: expected no field named {unmapped:?}, which this knowledge \
                 base's field mapping does not name, got the fields {names:?}"
            );
        }

        // ...and the embedding really went into the index's own `knn_vector` field: a
        // k-nearest-neighbour query over that field answers the chunk it was written for.
        let (status, neighbours) = rest(
            &router,
            &endpoint,
            "POST",
            &format!("/{index}/_search"),
            Some(&json!({
                "query": { "knn": { vector_field: { "vector": values, "k": 1 } } },
                "size": 1,
            })),
        )
        .await;
        let nearest = text(
            &at(&neighbours, "hits.hits")
                .as_array()
                .and_then(|hits| hits.first())
                .cloned()
                .unwrap_or(Value::Null),
            "_id",
        );
        assert_eq!(
            (status, nearest.as_str()),
            (200, source_uri.as_str()),
            "case {case:?}: expected the k-NN search over the mapped vector field to answer \
             the chunk itself, got {status} with {nearest:?}"
        );
    }
}
