// SPDX-License-Identifier: CC0-1.0

//! What a RUNNING `python3.x` function gets back when its handler queries a Bedrock
//! knowledge base whose vector store is an index in an OpenSearch Serverless COLLECTION,
//! end to end over the wire: AWS Lambda, the Bedrock Agent build-time plane, the Bedrock
//! Agent runtime plane, OpenSearch Serverless, IAM and the token service on ONE engine.
//!
//! Two rules have to hold at once for a caller to get an answer at all, and this suite
//! drives both in one path:
//!
//! * the address a collection publishes as its `collectionEndpoint` — "the
//!   collection-specific endpoint used to submit index, search, and data upload requests"
//!   (`CollectionDetail$collectionEndpoint`) — serves the collection's OpenSearch REST data
//!   plane, which names no RPC target, so the `PUT /{index}` carrying a `knn_vector` mapping
//!   that a vector knowledge base's `storageConfiguration` requires is SERVED there, with no
//!   `x-amz-target` header anywhere in it;
//! * a managed runtime RUNS the function's own deployment package, so the handler's SDK call
//!   is dispatched as the function's execution-role session and what the handler reports is
//!   the queried service's OWN answer — the passages the knowledge base holds when that role
//!   is granted the action, and the service's own refusal (403 `AccessDeniedException` for a
//!   role the account grants the action to nowhere, 404 `ResourceNotFoundException` for a
//!   well-formed identifier that resolves to nothing) when it is not. The two are therefore
//!   distinguishable from inside a handler, which is what makes an IAM boundary observable.
//!
//! The rows span what the second rule generalises over: the bytes the queried document
//! carries (ASCII, four-byte UTF-8, and a combining sequence), how the grant in force is
//! stated (the action allowed, no Bedrock action stated at all, and the action allowed AND
//! explicitly denied), and whether the knowledge base the call names resolves. None of them
//! may change that the package RAN — every row's answer is the handler's own return value,
//! never a runtime fault.
//!
//! The crate-local halves of the first rule are the OpenSearch Serverless suites
//! (`collection_data_plane.rs`, `collection_endpoint_authorization.rs` in that crate), and
//! the refusal classes a handler's Bedrock call can land in are pinned per class in
//! `handler_bedrock_agent_sdk.rs`; what is pinned HERE is the whole chain, whose middle —
//! a knowledge base really backed by an index created at a collection's published endpoint,
//! read from a running function — no suite drives.
//!
//! The suite RUNS handlers, so it takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]): this emulator serves at
//! most `max_concurrent_runs` invocations at once for the whole process.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mapping_role_access::form_encode;
use crate::event_source_mappings::{await_function_active, call, router_on};
use crate::node_invoke_run::package;

/// Where `CreateFunction` and `Invoke` are bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The Region every request of this suite is signed for, which is the Region the Lambda
/// harness addresses and therefore the one the function's own invocation runs in.
///
/// `pub(crate)` with the setup helpers below, which the sibling suite that drives the
/// GENERATION half of the same chain
/// (`crate::handler_generates_from_an_aoss_backed_knowledge_base`) stands its knowledge base
/// up with rather than copying them.
pub(crate) const REGION: &str = "us-east-1";

/// The module and attribute the deployment package below carries its handler as.
const HANDLER: &str = "lambda_function.lambda_handler";

/// The name of the inline identity policy a row writes on the execution role.
const INLINE_POLICY: &str = "inline";

/// The access-key id this suite's own calls present. It names no IAM record, which is the
/// position an account's own published credential is in.
const ACCOUNT_KEY: &str = "AKIDEMULATOR";

/// This service's OpenSearch Serverless neighbour: the signing name a control-plane call and
/// a collection-endpoint call both carry, and the RPC target prefix only the CONTROL plane
/// has (`OPERATIONS.md`, wire protocol). A REST request to a collection endpoint carries no
/// target at all, which is the first rule under test.
const AOSS_SIGNING_NAME: &str = "aoss";
const AOSS_TARGET_PREFIX: &str = "OpenSearchServerless";

/// The signing name every Bedrock-family endpoint shares, which routes both Bedrock Agent
/// planes (each claims its own path space).
const BEDROCK_SIGNING_NAME: &str = "bedrock";

/// The collection the knowledge base's vectors live in (`CollectionName`, len 3..64, pattern
/// `^[a-z][a-z0-9-]+$`), and the index inside it.
const COLLECTION: &str = "kb-fn-vectors";
const INDEX: &str = "bedrock-knowledge-base-fn-index";

/// The policies that collection is created under and reached through (`PolicyName`, len
/// 3..32, pattern `^[a-z][a-z0-9-]+$`).
const ENCRYPTION_POLICY: &str = "kb-fn-encryption";
const NETWORK_POLICY: &str = "kb-fn-network";
const DATA_POLICY: &str = "kb-fn-data";

/// The field names AWS's "Create a vector index in Amazon OpenSearch Service" gives a
/// knowledge base's index, which are what the `fieldMapping` of the storage configuration
/// names and what the index's own `knn_vector` mapping declares.
const VECTOR_FIELD: &str = "bedrock-knowledge-base-default-vector";
const TEXT_FIELD: &str = "AMAZON_BEDROCK_TEXT_CHUNK";
const METADATA_FIELD: &str = "AMAZON_BEDROCK_METADATA";

/// The embedding model the knowledge base is created with, and the number of values its
/// vectors carry — which is the dimension the index is created with.
/// `amazon.titan-embed-text-v2:0` publishes 1024-value vectors.
const EMBEDDING_MODEL: &str = "amazon.titan-embed-text-v2:0";
const DIMENSION: usize = 1024;

/// The role the knowledge base acts as, which is the principal the collection's `data`
/// access policy grants the index permissions to. Matches the modelled
/// `KnowledgeBaseRoleArn` pattern.
const KB_ROLE: &str =
    "arn:aws:iam::000000000000:role/service-role/AmazonBedrockExecutionRoleForKnowledgeBase";

/// The execution role the function runs as, and the function itself.
const FUNCTION_ROLE: &str = "kb-fn-retriever-role";
pub(crate) const FUNCTION: &str = "kb-fn-retriever";

/// The IAM action a knowledge-base retrieval is decided against, as a policy statement spells
/// it (`bedrock:<operation>`), and the operation it names.
const RETRIEVE_ACTION: &str = "bedrock:Retrieve";

/// The error code and HTTP status of each refusal class a caller tells apart.
pub(crate) const ACCESS_DENIED: &str = "AccessDeniedException";
pub(crate) const ACCESS_DENIED_STATUS: i64 = 403;
pub(crate) const NOT_FOUND: &str = "ResourceNotFoundException";
pub(crate) const NOT_FOUND_STATUS: i64 = 404;

/// A well-formed knowledge-base identifier nothing holds (`KnowledgeBaseIdentifier`, pattern
/// `^[0-9a-zA-Z]{10}$`).
pub(crate) const ABSENT_KNOWLEDGE_BASE: &str = "ZZZZZZZZZZ";

/// How long one invocation is given, as the function's own `Timeout`: well past what a run
/// needs on any host, because what is under test is the answer the handler gets and not how
/// fast an interpreter gets through it.
const FUNCTION_TIMEOUT: i64 = 120;

/// How many times a read looks again for a transition the CLOCK owes, and how long one look
/// waits. Generous: the suite waits for the CONDITION and asserts nothing about how long it
/// took.
const LOOKS: usize = 1_200;
const LOOK: Duration = Duration::from_millis(50);

/// The documents the knowledge base ingests: an id and the bytes it carries. The bytes differ
/// in their encoding width, which is what the retrieval must carry through unchanged.
pub(crate) const DOCUMENTS: &[(&str, &str)] = &[
    ("doc-ascii", "escalation runbook: restart the ingest worker"),
    ("doc-emoji", "launch checklist \u{1f680} stage two \u{1f6a8}"),
    (
        "doc-combining",
        "cafe\u{301} inventory for the de\u{301}panneur",
    ),
];

/// The handler the function runs: it builds the Bedrock Agent runtime client, makes the one
/// retrieval the event names, and reports what the SDK gave it — the passages, or the error
/// code, message and status of a refusal — so a refusal is the handler's own answer rather
/// than a fault of the run.
const HANDLER_SOURCE: &str = r#"
import boto3
import botocore.exceptions


def lambda_handler(event, context):
    client = boto3.client("bedrock-agent-runtime")
    try:
        answered = client.retrieve(
            knowledgeBaseId=event["knowledgeBaseId"],
            retrievalQuery={"text": event["query"]},
        )
    except botocore.exceptions.ClientError as error:
        return {
            "ok": False,
            "raised": "ClientError",
            "code": error.response["Error"]["Code"],
            "message": error.response["Error"]["Message"],
            "status": error.response["ResponseMetadata"]["HTTPStatusCode"],
        }
    except botocore.exceptions.BotoCoreError as error:
        return {"ok": False, "raised": type(error).__name__, "message": str(error)}
    passages = []
    for result in answered.get("retrievalResults") or []:
        location = result.get("location") or {}
        custom = location.get("customDocumentLocation") or {}
        passages.append(
            {
                "text": result["content"]["text"],
                "id": custom.get("id"),
                "locationType": location.get("type"),
            }
        )
    return {"ok": True, "passages": passages}
"#;

/// Lambda, OpenSearch Serverless, both Bedrock Agent planes, IAM and the token service over
/// ONE shared engine, as `core/router/src/lib.rs` mounts them: the handler's SDK call is
/// dispatched back into this same router and signed as the session of its own execution role,
/// so the service that answers it is the service this suite set up.
pub(crate) fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = router_on(&engine);
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
        service_com_amazonaws_bedrockagentruntime_amazonbedrockagentruntimeservice::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what routes
/// the request and where each handler reads its Region and its caller from. The signature is
/// never checked here.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={ACCOUNT_KEY}/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=not-verified"
    )
}

/// Dispatch one request and parse its JSON answer.
async fn dispatch(router: &MultiServiceRouter, request: http::Request<bytes::Bytes>) -> (u16, Value) {
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or_else(|_| {
            Value::String(String::from_utf8_lossy(response.body()).to_string())
        })
    };
    (status, parsed)
}

/// One OpenSearch Serverless CONTROL-plane call, over the `awsJson1_0` wire its model
/// declares.
async fn aoss(router: &MultiServiceRouter, operation: &str, body: Value) -> (u16, Value) {
    let host = format!("{AOSS_SIGNING_NAME}.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", &host)
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("{AOSS_TARGET_PREFIX}.{operation}"))
        .header("authorization", authorization(AOSS_SIGNING_NAME))
        .body(bytes::Bytes::from(body.to_string()))
        .expect("a well-formed control-plane request");
    dispatch(router, request).await
}

/// One OpenSearch Serverless control-plane call that must be served.
async fn aoss_ok(router: &MultiServiceRouter, operation: &str, body: Value) -> Value {
    let (status, answered) = aoss(router, operation, body).await;
    assert_eq!(
        status, 200,
        "setup: expected {operation} to be served, got {status} with {answered}"
    );
    answered
}

/// The authority a published endpoint is dialled at and the path prefix it carries, so a REST
/// request is addressed exactly where the collection said to address it — whether the
/// published address names the collection in its HOST or in its PATH.
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

/// One OpenSearch REST request to a collection's published endpoint, as `opensearch-py`'s
/// `indices.create` / `indices.get` sends it: addressed to the authority the endpoint names,
/// signed for the `aoss` signing name, and carrying NO `x-amz-target`, because the OpenSearch
/// REST API has no RPC target.
async fn collection_rest(
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
        .header("authorization", authorization(AOSS_SIGNING_NAME))
        .body(bytes::Bytes::from(
            body.map(Value::to_string).unwrap_or_default(),
        ))
        .expect("a well-formed data-plane request");
    dispatch(router, request).await
}

/// One Bedrock call, over the `restJson1` wire both Bedrock Agent planes declare, on the
/// method and URI the operation's own `@http` binding names.
async fn bedrock(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("bedrock.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization(BEDROCK_SIGNING_NAME))
        .body(bytes::Bytes::from(
            body.map(Value::to_string).unwrap_or_default(),
        ))
        .expect("a well-formed Bedrock request");
    dispatch(router, request).await
}

/// One awsQuery call to IAM that must be served.
async fn iam(router: &MultiServiceRouter, what: &str, action: &str, params: &[(&str, String)]) {
    let mut body = format!("Action={action}&Version=2010-05-08");
    for (name, value) in params {
        body.push('&');
        body.push_str(name);
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("https://iam.amazonaws.com/")
        .header("host", "iam.amazonaws.com")
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("iam"))
        .body(bytes::Bytes::from(body))
        .expect("a well-formed IAM request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    assert_eq!(
        status,
        200,
        "setup {what}: expected IAM {action} to be served, got {status} {}",
        String::from_utf8_lossy(response.body())
    );
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

/// One identity policy document with the statements `statements` names.
pub(crate) fn policy(statements: Value) -> String {
    json!({ "Version": "2012-10-17", "Statement": statements }).to_string()
}

/// The grant a row runs under: the action allowed.
fn allow_retrieval() -> String {
    policy(json!([{ "Effect": "Allow", "Action": [RETRIEVE_ACTION], "Resource": "*" }]))
}

/// The grant of a role whose policy names no Bedrock action at all — the position a role
/// nobody extended is in.
pub(crate) fn logs_only() -> String {
    policy(json!([{
        "Effect": "Allow",
        "Action": ["logs:CreateLogGroup", "logs:CreateLogStream", "logs:PutLogEvents"],
        "Resource": "*",
    }]))
}

/// The action allowed AND explicitly denied, which IAM resolves to a denial whatever else
/// allows it.
fn allow_and_deny_retrieval() -> String {
    policy(json!([
        { "Effect": "Allow", "Action": [RETRIEVE_ACTION], "Resource": "*" },
        { "Effect": "Deny", "Action": [RETRIEVE_ACTION], "Resource": "*" },
    ]))
}

/// The identity policy in force on the execution role from now on, so a row is about the
/// grant in force when the handler calls rather than the one in force when the role was made.
pub(crate) async fn role_policy(router: &MultiServiceRouter, what: &str, document: &str) {
    iam(
        router,
        what,
        "PutRolePolicy",
        &[
            ("RoleName", FUNCTION_ROLE.to_string()),
            ("PolicyName", INLINE_POLICY.to_string()),
            ("PolicyDocument", document.to_string()),
        ],
    )
    .await;
}

/// The assumed-role session the function's own calls are made as:
/// `arn:aws:sts::<account>:assumed-role/<role name>/<function name>`, the identity AWS reports
/// for a function's calls — built here from the role and the function rather than read off the
/// crate under test.
pub(crate) fn session_principal() -> String {
    format!(
        "arn:aws:sts::{}:assumed-role/{FUNCTION_ROLE}/{FUNCTION}",
        engine::emulator_account_id()
    )
}

/// The ARN a retrieval is decided on, which an identity-based policy for `bedrock:Retrieve`
/// names.
pub(crate) fn knowledge_base_arn(knowledge_base_id: &str) -> String {
    format!(
        "arn:aws:bedrock:{REGION}:{}:knowledge-base/{knowledge_base_id}",
        engine::emulator_account_id()
    )
}

/// The sentence IAM refuses a request with, built here from the principal, the action and the
/// resource rather than read off the crates under test.
fn denial(resource: &str, reason: &str) -> String {
    format!(
        "User: {} is not authorized to perform: {RETRIEVE_ACTION} on resource: {resource} \
         {reason}",
        session_principal()
    )
}

/// The index body a Bedrock knowledge base's vector index is created with: the body
/// `opensearch-py`'s `indices.create` carries for one.
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

/// The `storageConfiguration` of a knowledge base whose vectors live in [`INDEX`] of the
/// collection `collection_arn` names.
fn opensearch_storage(collection_arn: &str) -> Value {
    json!({
        "type": "OPENSEARCH_SERVERLESS",
        "opensearchServerlessConfiguration": {
            "collectionArn": collection_arn,
            "vectorIndexName": INDEX,
            "fieldMapping": {
                "vectorField": VECTOR_FIELD,
                "textField": TEXT_FIELD,
                "metadataField": METADATA_FIELD,
            },
        },
    })
}

/// A custom document whose content is defined inline IN BYTES, which is how a client pushes
/// bytes it holds straight into a knowledge base: `data` travels base64-encoded, as the
/// modelled `ByteContentDoc$data` blob does.
fn inline_byte_document(id: &str, bytes: &str) -> Value {
    json!({
        "content": {
            "dataSourceType": "CUSTOM",
            "custom": {
                "customDocumentIdentifier": { "id": id },
                "sourceType": "IN_LINE",
                "inlineContent": {
                    "type": "BYTE",
                    "byteContent": {
                        "mimeType": "text/plain",
                        "data": ::aws_smithy_types::base64::encode(bytes),
                    },
                },
            },
        },
    })
}

/// Create the collection's policies and the collection, and answer its published endpoint
/// beside its ARN once it reports `ACTIVE`.
///
/// The `data` access policy names the knowledge base's role, which is the principal the
/// knowledge-base create is resolved for, AND the function's execution role, as AWS's
/// "Create a vector index in Amazon OpenSearch Service" has a caller grant them.
pub(crate) async fn published_collection(router: &MultiServiceRouter) -> (String, String) {
    // A collection cannot be created without an `encryption` policy covering its name...
    aoss_ok(
        router,
        "CreateSecurityPolicy",
        json!({
            "type": "encryption",
            "name": ENCRYPTION_POLICY,
            "policy": json!({
                "Rules": [{ "ResourceType": "collection", "Resource": ["collection/*"] }],
                "AWSOwnedKey": true,
            })
            .to_string(),
        }),
    )
    .await;
    // ...and its endpoint is reachable only where a `network` policy admits traffic to it:
    // "you must have at least one network policy that applies to a collection in order to
    // access its endpoint".
    aoss_ok(
        router,
        "CreateSecurityPolicy",
        json!({
            "type": "network",
            "name": NETWORK_POLICY,
            "policy": json!({
                "Rules": [{ "ResourceType": "collection", "Resource": ["collection/*"] }],
                "AllowFromPublic": true,
            })
            .to_string(),
        }),
    )
    .await;
    aoss_ok(
        router,
        "CreateAccessPolicy",
        json!({
            "type": "data",
            "name": DATA_POLICY,
            "policy": json!([{
                "Rules": [
                    {
                        "ResourceType": "collection",
                        "Resource": [format!("collection/{COLLECTION}")],
                        "Permission": ["aoss:*"],
                    },
                    {
                        "ResourceType": "index",
                        "Resource": [format!("index/{COLLECTION}/*")],
                        "Permission": ["aoss:*"],
                    },
                ],
                "Principal": [
                    KB_ROLE,
                    format!("arn:aws:iam::{}:role/{FUNCTION_ROLE}", engine::emulator_account_id()),
                ],
            }])
            .to_string(),
        }),
    )
    .await;
    let created = aoss_ok(
        router,
        "CreateCollection",
        json!({ "name": COLLECTION, "type": "VECTORSEARCH" }),
    )
    .await;
    let id = text(&created, "createCollectionDetail.id");
    assert!(
        !id.is_empty(),
        "setup: expected CreateCollection to mint an id, got {created}"
    );
    // The `CREATING -> ACTIVE` move is owed by the clock, so this looks again until the
    // collection reports ACTIVE or a generous deadline passes.
    for _ in 0..LOOKS {
        let read = aoss_ok(router, "BatchGetCollection", json!({ "ids": [&id] })).await;
        let detail = at(&read, "collectionDetails")
            .as_array()
            .and_then(|details| details.first())
            .cloned()
            .unwrap_or(Value::Null);
        if text(&detail, "status") == "ACTIVE" {
            let endpoint = text(&detail, "collectionEndpoint");
            let arn = text(&detail, "arn");
            assert!(
                !endpoint.is_empty() && !arn.is_empty(),
                "setup: expected an ACTIVE collection to publish an endpoint and an ARN, got \
                 {detail}"
            );
            return (endpoint, arn);
        }
        tokio::time::sleep(LOOK).await;
    }
    panic!("setup: the collection {COLLECTION} never reached ACTIVE");
}

/// Create the vector index at the collection's PUBLISHED endpoint and read it back there.
///
/// This is the first rule under test: the request is the one a stock OpenSearch client sends
/// — no RPC target, at the root of the address the collection published — so it must be
/// SERVED by the collection's REST data plane rather than answered by an RPC front door that
/// routes on a header this request does not carry.
pub(crate) async fn vector_index_at_the_published_endpoint(
    router: &MultiServiceRouter,
    endpoint: &str,
) {
    let body = knn_index_body();
    let (status, created) =
        collection_rest(router, endpoint, "PUT", &format!("/{INDEX}"), Some(&body)).await;
    assert_eq!(
        status, 200,
        "setup: expected the index create at the published endpoint to be served, got {status} \
         with {created}"
    );
    assert_eq!(
        at(&created, "__type"),
        Value::Null,
        "setup: expected an OpenSearch answer rather than an RPC protocol fault, got {created}"
    );
    assert_eq!(
        at(&created, "index"),
        json!(INDEX),
        "setup: expected the create to acknowledge the index it created, got {created}"
    );
    let (status, read) =
        collection_rest(router, endpoint, "GET", &format!("/{INDEX}"), None).await;
    assert_eq!(
        status, 200,
        "setup: expected the created index to be readable back, got {status} with {read}"
    );
    assert_eq!(
        at(
            &read,
            &format!("{INDEX}.mappings.properties.{VECTOR_FIELD}.dimension")
        ),
        json!(DIMENSION),
        "setup: expected the index to hold the knn_vector mapping the create carried, got {read}"
    );
}

/// Create the knowledge base over that index, push [`DOCUMENTS`] into a custom data source of
/// it, and answer the knowledge base's id once every document reports `INDEXED`.
pub(crate) async fn ingested_knowledge_base(
    router: &MultiServiceRouter,
    collection_arn: &str,
) -> String {
    let (status, created) = bedrock(
        router,
        "PUT",
        "/knowledgebases/",
        Some(&json!({
            "name": "kb-fn-retrieval",
            "roleArn": KB_ROLE,
            "knowledgeBaseConfiguration": {
                "type": "VECTOR",
                "vectorKnowledgeBaseConfiguration": {
                    "embeddingModelArn":
                        format!("arn:aws:bedrock:{REGION}::foundation-model/{EMBEDDING_MODEL}"),
                },
            },
            "storageConfiguration": opensearch_storage(collection_arn),
        })),
    )
    .await;
    assert_eq!(
        status, 202,
        "setup: expected CreateKnowledgeBase over the created index to be accepted, got {status} \
         with {created}"
    );
    let knowledge_base_id = text(&created, "knowledgeBase.knowledgeBaseId");
    assert!(
        !knowledge_base_id.is_empty(),
        "setup: expected a minted knowledgeBaseId, got {created}"
    );
    // Its ACTIVE status is owed by the clock, so it is polled for.
    let mut last = Value::Null;
    let mut active = false;
    for _ in 0..LOOKS {
        let (status, read) = bedrock(
            router,
            "GET",
            &format!("/knowledgebases/{knowledge_base_id}"),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "setup: expected GetKnowledgeBase to answer, got {status} with {read}"
        );
        last = read;
        if text(&last, "knowledgeBase.status") == "ACTIVE" {
            active = true;
            break;
        }
        tokio::time::sleep(LOOK).await;
    }
    assert!(
        active,
        "setup: the knowledge base never reached ACTIVE, last read {last}"
    );

    let (status, created) = bedrock(
        router,
        "PUT",
        &format!("/knowledgebases/{knowledge_base_id}/datasources/"),
        Some(&json!({
            "name": "kb-fn-pushed",
            "dataSourceConfiguration": { "type": "CUSTOM" },
        })),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: expected CreateDataSource(CUSTOM) to be served, got {status} with {created}"
    );
    let data_source_id = text(&created, "dataSource.dataSourceId");
    assert!(
        !data_source_id.is_empty(),
        "setup: expected a minted dataSourceId, got {created}"
    );

    let (status, ingested) = bedrock(
        router,
        "PUT",
        &format!("/knowledgebases/{knowledge_base_id}/datasources/{data_source_id}/documents"),
        Some(&json!({
            "documents": DOCUMENTS
                .iter()
                .map(|(id, bytes)| inline_byte_document(id, bytes))
                .collect::<Vec<Value>>(),
        })),
    )
    .await;
    assert_eq!(
        status, 202,
        "setup: expected the ingestion to be accepted, got {status} with {ingested}"
    );

    // Each document reaches INDEXED on its own, with nobody calling: the clock owes it.
    let uri = format!(
        "/knowledgebases/{knowledge_base_id}/datasources/{data_source_id}/documents/getDocuments"
    );
    for (id, _) in DOCUMENTS {
        let body = json!({
            "documentIdentifiers": [{ "dataSourceType": "CUSTOM", "custom": { "id": id } }],
        });
        let mut last = Value::Null;
        let mut indexed = false;
        for _ in 0..LOOKS {
            let (status, read) = bedrock(router, "POST", &uri, Some(&body)).await;
            assert_eq!(
                status, 200,
                "setup: expected GetKnowledgeBaseDocuments to answer for {id:?}, got {status} \
                 with {read}"
            );
            last = read;
            let detail = at(&last, "documentDetails")
                .as_array()
                .and_then(|details| details.first())
                .cloned()
                .unwrap_or(Value::Null);
            if text(&detail, "status") == "INDEXED" {
                indexed = true;
                break;
            }
            tokio::time::sleep(LOOK).await;
        }
        assert!(
            indexed,
            "setup: the document {id:?} never reached INDEXED, last read {last}"
        );
    }
    knowledge_base_id
}

/// Create the execution role, trusted by Lambda, and the `python3.12` function that runs
/// `handler_source` as it — [`HANDLER_SOURCE`] for this suite, and its own handler for the
/// sibling suite that drives the generation half of the chain.
pub(crate) async fn deployed_function(router: &MultiServiceRouter, handler_source: &str) {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "lambda.amazonaws.com" },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    iam(
        router,
        "the execution role",
        "CreateRole",
        &[
            ("RoleName", FUNCTION_ROLE.to_string()),
            ("AssumeRolePolicyDocument", trust),
        ],
    )
    .await;
    role_policy(router, "the execution role", &logs_only()).await;
    let created = call(
        router,
        "POST",
        FUNCTIONS_URI,
        &json!({
            "FunctionName": FUNCTION,
            "Role": format!("arn:aws:iam::{}:role/{FUNCTION_ROLE}", engine::emulator_account_id()),
            "Runtime": "python3.12",
            "Handler": HANDLER,
            "Timeout": FUNCTION_TIMEOUT,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(&[("lambda_function.py", handler_source)])),
            },
        }),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "setup: expected CreateFunction to answer 201, got {} {}",
        created.status,
        created.text()
    );
    await_function_active(router, "setup", FUNCTION).await;
}

/// Which knowledge base a row's call names.
#[derive(Debug)]
enum Target {
    /// The one this suite built over the collection's index.
    Held,
    /// A well-formed identifier nothing holds.
    Absent,
}

/// What a row's call must come back as.
#[derive(Debug)]
enum Expected {
    /// The retrieval was served, and the document whose ingested bytes it must carry.
    Retrieved { document: &'static str },
    /// The plane refused it with this error code and HTTP status, and — for an authorization
    /// refusal — IAM's own sentence about this action and resource, with `reason` as its tail.
    Refused {
        code: &'static str,
        status: i64,
        reason: Option<&'static str>,
    },
}

/// One row: the grant in force, the knowledge base named, the query sent, and what the
/// handler must observe.
struct Row {
    case: &'static str,
    policy: fn() -> String,
    target: Target,
    /// The document whose own bytes the row queries with.
    query: &'static str,
    expected: Expected,
}

/// The bytes the document `id` was ingested with.
pub(crate) fn ingested(id: &str) -> &'static str {
    DOCUMENTS
        .iter()
        .find(|(document, _)| *document == id)
        .map(|(_, bytes)| *bytes)
        .unwrap_or_else(|| panic!("{id:?} is no document this suite ingests"))
}

/// The rows, spanning the bytes the queried document carries, how the grant in force is
/// stated, and whether the knowledge base the call names resolves.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "ascii bytes retrieved by a role the account grants the action to",
            policy: allow_retrieval,
            target: Target::Held,
            query: "doc-ascii",
            expected: Expected::Retrieved {
                document: "doc-ascii",
            },
        },
        Row {
            case: "four-byte UTF-8 bytes retrieved by that same role",
            policy: allow_retrieval,
            target: Target::Held,
            query: "doc-emoji",
            expected: Expected::Retrieved {
                document: "doc-emoji",
            },
        },
        Row {
            case: "a combining sequence retrieved by that same role",
            policy: allow_retrieval,
            target: Target::Held,
            query: "doc-combining",
            expected: Expected::Retrieved {
                document: "doc-combining",
            },
        },
        Row {
            case: "the same retrieval by a role whose policy names no Bedrock action",
            policy: logs_only,
            target: Target::Held,
            query: "doc-ascii",
            expected: Expected::Refused {
                code: ACCESS_DENIED,
                status: ACCESS_DENIED_STATUS,
                reason: Some("because no identity-based policy allows the bedrock:Retrieve action"),
            },
        },
        Row {
            case: "the same retrieval by a role granted the action and explicitly denied it",
            policy: allow_and_deny_retrieval,
            target: Target::Held,
            query: "doc-ascii",
            expected: Expected::Refused {
                code: ACCESS_DENIED,
                status: ACCESS_DENIED_STATUS,
                reason: Some(
                    "with an explicit deny in an identity-based policy for the bedrock:Retrieve \
                     action",
                ),
            },
        },
        Row {
            case: "a granted role's retrieval of a knowledge base nobody created",
            policy: allow_retrieval,
            target: Target::Absent,
            query: "doc-ascii",
            expected: Expected::Refused {
                code: NOT_FOUND,
                status: NOT_FOUND_STATUS,
                reason: None,
            },
        },
    ]
}

/// RULE: an index created at a collection's own published endpoint backs a knowledge base, and
/// a RUNNING function's handler queries it through the runtime plane as its own execution-role
/// session — so what the handler reports is that plane's own answer: the ingested passages
/// byte for byte where the role is granted the action, and the plane's own refusal (403 for a
/// missing or denied grant, 404 for an identifier that resolves to nothing) where it is not.
#[tokio::test]
async fn a_handler_retrieves_the_knowledge_base_backed_by_its_collections_index() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let router = emulator();
    let (endpoint, collection_arn) = published_collection(&router).await;
    vector_index_at_the_published_endpoint(&router, &endpoint).await;
    let knowledge_base_id = ingested_knowledge_base(&router, &collection_arn).await;
    deployed_function(&router, HANDLER_SOURCE).await;

    for row in rows() {
        let case = row.case;
        role_policy(&router, case, &(row.policy)()).await;
        let named = match row.target {
            Target::Held => knowledge_base_id.clone(),
            Target::Absent => ABSENT_KNOWLEDGE_BASE.to_string(),
        };
        let answered = call(
            &router,
            "POST",
            &format!("{FUNCTIONS_URI}/{FUNCTION}/invocations"),
            &json!({ "knowledgeBaseId": named, "query": ingested(row.query) }),
        )
        .await;
        assert_eq!(
            answered.status,
            200,
            "case {case:?}: expected HTTP 200 from Invoke, got {} {}",
            answered.status,
            answered.text()
        );
        // The DEPLOYMENT PACKAGE RAN: every row's answer below is the handler's own return
        // value, so a run that faulted is not a row's outcome.
        assert_eq!(
            answered.header("x-amz-function-error"),
            None,
            "case {case:?}: expected the handler to run to completion, got {}",
            answered.text()
        );
        let payload = answered.json();
        assert_ne!(
            payload["raised"].as_str(),
            Some("UnknownServiceError"),
            "case {case:?}: expected the client to be built and the call dispatched, got {payload}"
        );

        match row.expected {
            Expected::Retrieved { document } => {
                assert_eq!(
                    payload["ok"],
                    json!(true),
                    "case {case:?}: expected the retrieval to be served, got {payload}"
                );
                let passages = payload["passages"].as_array().cloned().unwrap_or_default();
                let found = passages
                    .iter()
                    .find(|passage| passage["id"] == json!(document))
                    .unwrap_or_else(|| {
                        panic!(
                            "case {case:?}: expected the retrieval to cite the document \
                             {document:?}, got {payload}"
                        )
                    });
                assert_eq!(
                    found["text"],
                    json!(ingested(document)),
                    "case {case:?}: expected the retrieved content to be the bytes ingested, got \
                     {found}"
                );
                assert_eq!(
                    found["locationType"],
                    json!("CUSTOM"),
                    "case {case:?}: expected a document of a custom data source to be cited as \
                     CUSTOM, got {found}"
                );
                // Nothing is invented: every passage the handler got back is one of the
                // documents this suite really ingested.
                for passage in &passages {
                    let carried = passage["text"].as_str().unwrap_or_default();
                    assert!(
                        DOCUMENTS.iter().any(|(_, bytes)| *bytes == carried),
                        "case {case:?}: expected every retrieved passage to be an ingested \
                         document, got {passage}"
                    );
                }
            }
            Expected::Refused {
                code,
                status,
                reason,
            } => {
                assert_eq!(
                    payload["ok"],
                    json!(false),
                    "case {case:?}: expected the plane's own refusal, got {payload}"
                );
                assert_eq!(
                    payload["raised"],
                    json!("ClientError"),
                    "case {case:?}: expected the refusal to reach the handler as the SDK's \
                     service error, got {payload}"
                );
                assert_eq!(
                    payload["code"],
                    json!(code),
                    "case {case:?}: expected the refusal {code:?}, got {payload}"
                );
                assert_eq!(
                    payload["status"],
                    json!(status),
                    "case {case:?}: expected HTTP {status} for that refusal, got {payload}"
                );
                match reason {
                    Some(reason) => assert_eq!(
                        payload["message"],
                        json!(denial(&knowledge_base_arn(&named), reason)),
                        "case {case:?}: expected IAM's own denial sentence, got {payload}"
                    ),
                    None => assert!(
                        payload["message"]
                            .as_str()
                            .is_some_and(|message| !message.is_empty()),
                        "case {case:?}: expected the plane's own message, got {payload}"
                    ),
                }
            }
        }
    }
}
