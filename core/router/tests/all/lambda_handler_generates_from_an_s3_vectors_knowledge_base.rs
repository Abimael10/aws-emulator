// SPDX-License-Identifier: CC0-1.0

//! What a RUNNING `python3.x` function gets back when its handler asks a Bedrock knowledge base
//! whose vector store is an index in an S3 VECTORS bucket to generate an answer, end to end over
//! the wire of every service the path spans.
//!
//! RULE: a managed `python3.x` runtime carries the AWS SDK for Python in the execution
//! environment, so a handler that imports it AT MODULE SCOPE is loaded without the function
//! shipping a dependency of its own — the invocation answers HTTP 200 with no
//! `X-Amz-Function-Error` (`Invoke` binds that member to the `X-Amz-Function-Error` header, so a
//! module that could not be imported is reported there, never beside a result) and with the
//! handler's OWN JSON as the payload — and a call that SDK makes is dispatched as a session of
//! the function's execution role, so what the handler observes is the queried plane's own answer:
//! the response generated over the passages an ingestion really recorded, each citation quoting a
//! segment of that response and naming the object it was read from, where the role is granted the
//! action, and that plane's own refusal (403 `AccessDeniedException` where no identity-based
//! policy allows the action, 404 `ResourceNotFoundException` for a well-formed identifier that
//! resolves to nothing) where it is not.
//!
//! "The execution role grants the function permission to use Amazon Web Services services"
//! (`FunctionConfiguration$Role`), and the identity such a call speaks for is the session
//! `arn:aws:sts::<account>:assumed-role/<role>/<function>`; with neither an identity-based nor a
//! resource-based allow a request is denied (IAM User Guide, *Policy evaluation logic*).
//!
//! Seven crates have to agree for that to hold — Amazon S3 holds the source objects, Amazon S3
//! Vectors holds the index the knowledge base writes its vectors into, the Bedrock Agent
//! build-time plane creates the knowledge base and runs the ingestion, CloudFormation archives
//! each inline handler into a deployment package this emulator can run, Lambda runs it and
//! supplies it the SDK, the Bedrock Agent runtime plane generates the response over what was
//! ingested, and IAM and the token service decide and sign the handler's own call — so the suite
//! lives here: a crate-local suite can only ever see one side of it. The generation plane's own
//! suite pins what a response's citations are for a client calling it directly
//! (`retrieve_and_generate.rs` of that crate), the Lambda crate's `python_runtime_bundled_sdk.rs`
//! pins that every `python3.x` release supplies the SDK to every import form, and its
//! `handler_retrieves_an_aoss_backed_knowledge_base.rs` pins the UN-generated retrieval over a
//! collection-backed store; what only this suite reaches is a handler GENERATING from a knowledge
//! base backed by an S3 Vectors index.
//!
//! The rows differ along what the rule generalises over: the `python3.x` point release that runs
//! the module and the form it reaches the SDK through (so the import is not one spelling on one
//! release), the query asked and therefore which ingested passage the response quotes, how the
//! generation model is named (`modelArn` takes a bare model id or that model's own published
//! ARN), how many passages the response may quote (`numberOfResults`), and the outcome class the
//! handler observes (served, 403, 404). No row's outcome may be a fault of the run.
//!
//! WHERE THE CONFINEMENT OF A RUN LIVES, since every row below RUNS a Python module: this suite
//! adds none of it and changes none of it — each handler's source here is test input assembled
//! from `&'static str` constants, with no value of any request reaching it — and what bounds and
//! contains the interpreter that runs it is host code of the Lambda crate, which a handler cannot
//! reach or switch off:
//!
//! * the escapes: the interpreter is built with the engine crate's host-reaching surface compiled
//!   out and registers an ALLOW-LIST of native modules
//!   (`service-com-amazonaws-lambda-awsgirapiservice/src/state/execution/python.rs`,
//!   `ALLOWED_NATIVE_MODULES`), so no socket, subprocess, memory-map, dynamic-loader or
//!   host-library extension exists to be imported however the guest spells the import;
//! * the filesystem and the process: the operating-system modules a Python program expects are
//!   that module's own (`host_posix_module`, `host_thread_module`), whose every call that would
//!   reach a host file, process or descriptor raises, with the environment the invocation's own
//!   variables held in memory;
//! * the network: the only outbound seam a handler has is the one host call into THIS emulator
//!   (`state/execution/aws_api.rs`, which decides which services are reachable, signs the request
//!   itself and bounds it by what the run has left), beside the in-process listeners of
//!   `state/execution/guest_socket.rs` and `guest_http.rs` — which is why the SDK calls these rows
//!   make come back answered by the planes this same suite set up;
//! * the resources: steps, the deadline, memory, recursion, the log and the response are each
//!   bounded in host code (`python.rs`'s `Bounds`, with `sys.settrace` and
//!   `sys.setrecursionlimit` removed), inside the per-run and whole-process envelopes
//!   `state/execution.rs` states (`MAX_RUN_MEMORY_BYTES`, `MAX_CONCURRENT_RUN_MEMORY_MB`,
//!   `HOST_WAIT_CEILING`) and the function's own `Timeout` below.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{elements, form_encode};

/// The Region every request here is signed for, and so the Region the vector store, the
/// knowledge base and the functions belong to.
const REGION: &str = "us-east-1";

/// The account this emulator serves, which every ARN below carries.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// Where Lambda binds `CreateFunction`, `GetFunctionConfiguration` and `Invoke`.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The stack that declares one function per deployment.
const STACK: &str = "xsvc-kb-generate-functions";

/// The `StackStatus` that stack must reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The `State` an invocation needs the function to be in.
const STATE_ACTIVE: &str = "Active";

/// The vector bucket the knowledge base binds to, and the index inside it.
const VECTOR_BUCKET: &str = "kb-generate-vectors";
const VECTOR_INDEX: &str = "kb-generate-index";

/// How that index is created: the vectors of an embedding model are 32-bit floats compared by
/// cosine distance.
const VECTOR_DATA_TYPE: &str = "float32";
const VECTOR_DISTANCE_METRIC: &str = "cosine";

/// The bucket the knowledge base's one data source is crawled from, and the prefix it admits.
const SOURCE_BUCKET: &str = "kb-generate-sources";
const SOURCE_PREFIX: &str = "manual/";

/// The embedding model the knowledge base is created with, and the number of values its vectors
/// carry — which is the dimension the bound index is created with. `amazon.titan-embed-text-v2:0`
/// publishes 1024-value vectors.
const EMBEDDING_MODEL: &str = "amazon.titan-embed-text-v2:0";
const EMBEDDING_DIMENSION: i64 = 1024;

/// A text model the Bedrock Runtime plane of this emulator publishes, which is what a response is
/// generated with here.
const GENERATION_MODEL: &str = "anthropic.claude-3-5-sonnet-20240620-v1:0";

/// The role the knowledge base itself acts as: a service role of the account this emulator serves,
/// whose ARN matches the Bedrock Agent plane's modelled `KnowledgeBaseRoleArn`. The plane records
/// the role a knowledge base acts as rather than resolving it, so nothing is created for it here.
const KB_ROLE_PATH_AND_NAME: &str = "service-role/AmazonBedrockExecutionRoleForKnowledgeBase";

/// The execution role every function here runs as, whose inline document a row rewrites before it
/// invokes — so a row is about the grant in force when the handler CALLS rather than the one in
/// force when the role was made.
const FUNCTION_ROLE_NAME: &str = "xsvc-kb-generate-role";

/// The IAM action a knowledge-base generation is decided against, as a policy statement spells it.
const GENERATE_ACTION: &str = "bedrock:RetrieveAndGenerate";

/// The error code and HTTP status of each refusal class a caller tells apart.
const ACCESS_DENIED: &str = "AccessDeniedException";
const ACCESS_DENIED_STATUS: i64 = 403;
const NOT_FOUND: &str = "ResourceNotFoundException";
const NOT_FOUND_STATUS: i64 = 404;

/// A well-formed knowledge-base identifier nothing holds (`KnowledgeBaseId`, `@length(max: 10)`,
/// `@pattern ^[0-9a-zA-Z]+$`).
const ABSENT_KNOWLEDGE_BASE: &str = "ZZZZZZZZZZ";

/// How long one invocation is given, as the function's own `Timeout`: well past what a run needs
/// on any host, because what is under test is the answer the handler gets and not how fast an
/// interpreter gets through it.
const FUNCTION_TIMEOUT: i64 = 120;

/// How often to look again for work the clock owes — a knowledge base reaching `ACTIVE`, an
/// ingestion job completing, a stack reaching `CREATE_COMPLETE`, a function reaching `Active` —
/// and how many looks to take. The spans are those services' own, so no fixed sleep names the
/// instant one ends, and the deadline is generous so a loaded host cannot decide the outcome.
const LOOK: Duration = Duration::from_millis(100);
const LOOKS: usize = 1_800;

/// The status Lambda answers an invocation it has NOT served with, which says "not now" and
/// carries no answer from the generation plane: the concurrent-execution limit of this process was
/// reached (`TooManyRequestsException`, `@httpError(429)`). Whether one arrives is decided by what
/// the rest of this process is running at that instant and by nothing a row varies, so a row LOOKS
/// AGAIN on it rather than reading an answer out of an invocation that never ran.
const UNSERVED: u16 = 429;

/// The `errorType` classes of a run THIS PROCESS's own bounds ended rather than the handler: the
/// sandbox stopping a run at the host wall clock an invocation is given, and the memory bound read
/// from the whole process, which stops whichever run is in flight once the test binary as a whole
/// is over it (the same two the Lambda crate's own suites read a host bound from). Neither says
/// anything about what the handler came to, so a row answered with one looks again; every OTHER
/// function error — `Runtime.ImportModuleError` above all — fails the row as it came back.
const SANDBOX_TIMEDOUT: &str = "Sandbox.Timedout";
const RUNTIME_EXIT_ERROR: &str = "Runtime.ExitError";
const RUNTIME_KILLED: &str = "signal: killed";

/// How long a row waits for Lambda to serve its invocation. Generous, and read as a deadline
/// rather than as a number of looks, because one look of this wait is a whole invocation.
const SERVED_DEADLINE: Duration = Duration::from_secs(180);

/// The objects the data source is crawled from: the key under [`SOURCE_PREFIX`] and the text the
/// object carries. Two are on one topic and one is on another, so a query reaches a known passage
/// and a citation can be held against the object it was ingested from.
const DOCUMENTS: &[(&str, &str)] = &[
    (
        "pump.txt",
        "Hydraulic pump P-12 in bay 2 is serviced every 90 days by the maintenance crew.",
    ),
    (
        "coolant.txt",
        "Coolant for the hydraulic system is stored in drum 7 and must stay above 4 degrees.",
    ),
    (
        "menu.txt",
        "Canteen service: barley stew on Monday and flatbread on Friday.",
    ),
];

/// The `s3://` URI of one source object, as `S3Location$uri` spells it and as a citation of its
/// passage names it.
fn uri(key: &str) -> String {
    format!("s3://{SOURCE_BUCKET}/{SOURCE_PREFIX}{key}")
}

/// The ARN of one role of this account, as every ARN here carries the account the emulator serves.
fn role_arn(name: &str) -> String {
    format!("arn:aws:iam::{ACCOUNT}:role/{name}")
}

/// The emulator as it is ASSEMBLED, on one engine: a handler's own SDK call is dispatched back
/// into this same router, so the knowledge base it queries is the one this suite built and the
/// role its credentials are minted of is the record IAM holds.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate; the credential
    // scope is what routes a request.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes the
/// request and where each handler reads its Region and its caller from. Its credential is the
/// long-term key every emulator publishes, so these calls are the account's own and never a
/// function's role's.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One `awsQuery` call of `service`, answered with the HTTP status and the XML body.
async fn query(
    router: &MultiServiceRouter,
    service: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{service}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(service))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One Lambda `restJson1` call, as its status, its headers and its parsed body.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: &[u8],
) -> (u16, http::HeaderMap, Value) {
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"))
        .body(Bytes::from(body.to_vec()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    let parsed = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&body).to_string()))
    };
    (parts.status.as_u16(), parts.headers, parsed)
}

/// One path-style Amazon S3 request, answered with its status and body text.
async fn s3(router: &MultiServiceRouter, method: &str, path: &str, body: &[u8]) -> (u16, String) {
    let host = format!("s3.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("authorization", authorization("s3"))
        .header("content-type", "application/octet-stream")
        .body(Bytes::copy_from_slice(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (
        status,
        String::from_utf8_lossy(response.body()).into_owned(),
    )
}

/// One Amazon S3 Vectors operation over its `restJson1` wire: a POST to the operation's own URI,
/// which is how that model binds every one of them.
async fn s3vectors(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("/{operation}"))
        .header("host", format!("s3vectors.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("s3vectors"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// One Bedrock call, over the `restJson1` wire both Bedrock Agent planes declare, on the method
/// and URI the operation's own `@http` binding names, signed for `bedrock` — the signing name
/// every Bedrock-family model declares.
async fn bedrock(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", format!("bedrock.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("bedrock"))
        .body(Bytes::from(
            body.map(Value::to_string).unwrap_or_default(),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// A string member of an answer, by dotted path.
fn text(document: &Value, path: &str) -> String {
    let mut at = document;
    for key in path.split('.') {
        at = &at[key];
    }
    at.as_str()
        .unwrap_or_else(|| panic!("{path} is no string in {document}"))
        .to_string()
}

/// Create the vector bucket and the index the knowledge base binds to, answering the bucket's ARN
/// as the owning service minted it.
async fn vector_store(router: &MultiServiceRouter) -> String {
    let (status, created) = s3vectors(
        router,
        "CreateVectorBucket",
        &json!({ "vectorBucketName": VECTOR_BUCKET }),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateVectorBucket must be served, got {status} with {created}"
    );
    let vector_bucket_arn = text(&created, "vectorBucketArn");
    let (status, index) = s3vectors(
        router,
        "CreateIndex",
        &json!({
            "vectorBucketName": VECTOR_BUCKET,
            "indexName": VECTOR_INDEX,
            "dataType": VECTOR_DATA_TYPE,
            "dimension": EMBEDDING_DIMENSION,
            "distanceMetric": VECTOR_DISTANCE_METRIC,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateIndex must be served, got {status} with {index}"
    );
    vector_bucket_arn
}

/// Write [`DOCUMENTS`] where the data source's crawl finds them, through Amazon S3's own
/// `CreateBucket` and `PutObject`.
async fn source_objects(router: &MultiServiceRouter) {
    let (status, created) = s3(router, "PUT", &format!("/{SOURCE_BUCKET}"), &[]).await;
    assert_eq!(
        status, 200,
        "setup: CreateBucket({SOURCE_BUCKET:?}) must be served, got {status} with {created}"
    );
    for (key, body) in DOCUMENTS {
        let (status, written) = s3(
            router,
            "PUT",
            &format!("/{SOURCE_BUCKET}/{SOURCE_PREFIX}{key}"),
            body.as_bytes(),
        )
        .await;
        assert_eq!(
            status, 200,
            "setup: PutObject({key:?}) must be served, got {status} with {written}"
        );
    }
}

/// Create the knowledge base over the bound index, attach the S3 data source and run its ingestion
/// to completion, answering the knowledge base's minted id.
async fn ingested_knowledge_base(router: &MultiServiceRouter, vector_bucket_arn: &str) -> String {
    let (status, created) = bedrock(
        router,
        "PUT",
        "/knowledgebases/",
        Some(&json!({
            "name": "kb-generate",
            "roleArn": role_arn(KB_ROLE_PATH_AND_NAME),
            "knowledgeBaseConfiguration": {
                "type": "VECTOR",
                "vectorKnowledgeBaseConfiguration": {
                    "embeddingModelArn":
                        format!("arn:aws:bedrock:{REGION}::foundation-model/{EMBEDDING_MODEL}"),
                },
            },
            "storageConfiguration": {
                "type": "S3_VECTORS",
                "s3VectorsConfiguration": {
                    "vectorBucketArn": vector_bucket_arn,
                    "indexName": VECTOR_INDEX,
                },
            },
        })),
    )
    .await;
    assert_eq!(
        status, 202,
        "setup: CreateKnowledgeBase over the created index must be accepted, got {status} with \
         {created}"
    );
    let knowledge_base_id = text(&created, "knowledgeBase.knowledgeBaseId");

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
            "setup: GetKnowledgeBase must answer, got {status} with {read}"
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
            "name": "kb-generate-manuals",
            "dataSourceConfiguration": {
                "type": "S3",
                "s3Configuration": {
                    "bucketArn": format!("arn:aws:s3:::{SOURCE_BUCKET}"),
                    "inclusionPrefixes": [SOURCE_PREFIX],
                },
            },
        })),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateDataSource(S3) must be served, got {status} with {created}"
    );
    let data_source_id = text(&created, "dataSource.dataSourceId");

    let jobs =
        format!("/knowledgebases/{knowledge_base_id}/datasources/{data_source_id}/ingestionjobs");
    let (status, started) = bedrock(router, "PUT", &format!("{jobs}/"), Some(&json!({}))).await;
    assert_eq!(
        status, 202,
        "setup: StartIngestionJob must be accepted, got {status} with {started}"
    );
    let ingestion_job_id = text(&started, "ingestionJob.ingestionJobId");

    // The job's run is owed by the clock too, so its COMPLETE status is polled for.
    let mut last = Value::Null;
    let mut complete = false;
    for _ in 0..LOOKS {
        let (status, read) = bedrock(router, "GET", &format!("{jobs}/{ingestion_job_id}"), None)
            .await;
        assert_eq!(
            status, 200,
            "setup: GetIngestionJob must answer, got {status} with {read}"
        );
        last = read;
        if text(&last, "ingestionJob.status") == "COMPLETE" {
            complete = true;
            break;
        }
        tokio::time::sleep(LOOK).await;
    }
    assert!(
        complete,
        "setup: the ingestion job never completed, last read {last}"
    );
    assert_eq!(
        last["ingestionJob"]["statistics"]["numberOfNewDocumentsIndexed"].as_i64(),
        Some(i64::try_from(DOCUMENTS.len()).expect("a small count")),
        "setup: every source object must be indexed as new, got {last}"
    );
    knowledge_base_id
}

/// The handler a deployment runs: it imports the SDK the runtime supplies AT MODULE SCOPE through
/// the form `imports` names, builds the Bedrock Agent runtime client with `client`, makes each
/// generation call the event lists, and reports what the SDK gave it for each one — the generated
/// text, the segments its citations quote and the objects they were quoted from, or the error code,
/// sentence and HTTP status of a refusal — so a refusal is the handler's own answer rather than a
/// fault of the run. `sdk` is the expression reporting the module name the import really resolved
/// to.
///
/// One invocation serves every call of one grant, because what the rows vary is the REQUEST the SDK
/// makes and not how many runs make them: a run per row would buy nothing and spend a whole
/// execution environment each time.
fn handler_source(imports: &str, client: &str, sdk: &str) -> String {
    format!(
        r#"
import os

{imports}


def generated(client, call):
    configuration = {{
        "knowledgeBaseId": call["kb_id"],
        "modelArn": call["gen_model_arn"],
    }}
    if call.get("number_of_results"):
        configuration["retrievalConfiguration"] = {{
            "vectorSearchConfiguration": {{"numberOfResults": call["number_of_results"]}},
        }}
    try:
        answered = client.retrieve_and_generate(
            input={{"text": call["query"]}},
            retrieveAndGenerateConfiguration={{
                "type": "KNOWLEDGE_BASE",
                "knowledgeBaseConfiguration": configuration,
            }},
        )
    except ClientError as error:
        return {{
            "ok": False,
            "code": error.response["Error"]["Code"],
            "message": error.response["Error"]["Message"],
            "status": error.response["ResponseMetadata"]["HTTPStatusCode"],
        }}
    citations = answered.get("citations") or []
    refs = []
    segments = []
    for citation in citations:
        part = (citation.get("generatedResponsePart") or {{}}).get("textResponsePart") or {{}}
        segments.append(part.get("text") or "")
        cited = []
        for reference in citation.get("retrievedReferences") or []:
            location = reference.get("location") or {{}}
            cited.append((location.get("s3Location") or {{}}).get("uri"))
        refs.append(cited)
    return {{
        "ok": True,
        "answer": answered["output"]["text"],
        "num_citations": len(citations),
        "refs": refs,
        "segments": segments,
        "session_id": answered.get("sessionId"),
    }}


def handler(event, context):
    client = {client}
    return {{
        "sdk": {sdk},
        "ran_under": os.environ["AWS_EXECUTION_ENV"],
        "results": [generated(client, call) for call in event["calls"]],
    }}
"#
    )
}

/// One function this suite deploys: the runtime that runs it, the import form its module reaches
/// the SDK through, and whether its execution role is granted the generation action.
struct Deployment {
    /// The logical id the stack declares it under, which is also its `FunctionName` and therefore
    /// the name of the assumed-role session its own calls speak for.
    function: &'static str,
    /// A `python3.x` identifier of the model's `Runtime` value set
    /// (`smithy_query(query="shape",shape="Runtime")`) that the Lambda crate's catalog of the
    /// identifiers Lambda no longer supports (`state::DEPRECATED_RUNTIMES`) does not name.
    runtime: &'static str,
    /// What the module imports, at module scope.
    imports: &'static str,
    /// The expression the handler builds its client with, which may only use what `imports` bound.
    client: &'static str,
    /// The expression reporting the module name the import resolved to, and the name it must
    /// answer.
    sdk: &'static str,
    sdk_name: &'static str,
}

/// The functions: the SDK reached through two of its import forms, on two point releases of the
/// runtime, both running as the one execution role whose document a row puts in force.
const DEPLOYMENTS: &[Deployment] = &[
    Deployment {
        function: "KbGeneratePackageImport",
        runtime: "python3.12",
        imports: "import boto3\nfrom botocore.exceptions import ClientError",
        client: r#"boto3.client("bedrock-agent-runtime")"#,
        sdk: "boto3.__name__",
        sdk_name: "boto3",
    },
    Deployment {
        function: "KbGenerateSessionImport",
        runtime: "python3.13",
        imports: "from boto3.session import Session\nfrom botocore.exceptions import ClientError",
        client: r#"Session().client("bedrock-agent-runtime")"#,
        sdk: "Session.__module__",
        sdk_name: "boto3.session",
    },
];

/// The deployment `function` names.
fn deployment(function: &str) -> &'static Deployment {
    DEPLOYMENTS
        .iter()
        .find(|deployment| deployment.function == function)
        .unwrap_or_else(|| panic!("{function:?} is no function this suite deploys"))
}

/// The trust policy a Lambda execution role needs.
fn trust_policy() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": "lambda.amazonaws.com"},
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// The inline document one role carries: the log actions every execution role needs, and — for a
/// granted role — the generation action on the account's knowledge bases in this Region, which is
/// the resource an identity-based policy for it names. Scoped that way rather than to one id, so
/// the row naming a knowledge base nobody created is decided on its existence rather than on the
/// reach of the grant.
fn role_document(granted: bool) -> String {
    let mut statements = vec![json!({
        "Effect": "Allow",
        "Action": ["logs:CreateLogGroup", "logs:CreateLogStream", "logs:PutLogEvents"],
        "Resource": "*",
    })];
    if granted {
        statements.push(json!({
            "Effect": "Allow",
            "Action": [GENERATE_ACTION],
            "Resource": format!("arn:aws:bedrock:{REGION}:{ACCOUNT}:knowledge-base/*"),
        }));
    }
    json!({ "Version": "2012-10-17", "Statement": statements }).to_string()
}

/// Create the role `name`, trusted by Lambda, answering the ARN IAM minted.
async fn create_role(router: &MultiServiceRouter, name: &str) -> String {
    let (status, created) = query(
        router,
        "iam",
        "2010-05-08",
        "CreateRole",
        &[
            ("RoleName", name),
            ("AssumeRolePolicyDocument", &trust_policy()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateRole({name:?}) must succeed, got {status} with {created}"
    );
    crate::query_wire::element(&created, "Arn")
}

/// Put [`role_document`]'s document in force on the execution role from now on, so what decides a
/// row's call is the grant in force when the handler makes it.
async fn role_policy(router: &MultiServiceRouter, what: &str, granted: bool) {
    let (status, embedded) = query(
        router,
        "iam",
        "2010-05-08",
        "PutRolePolicy",
        &[
            ("RoleName", FUNCTION_ROLE_NAME),
            ("PolicyName", "generation-grants"),
            ("PolicyDocument", &role_document(granted)),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "{what}: PutRolePolicy({FUNCTION_ROLE_NAME:?}) must succeed, got {status} with {embedded}"
    );
}

/// The stack declaring one function per deployment, each running as its own role: the inline source
/// is archived into the deployment package by CloudFormation, which is how a function whose handler
/// is written here comes to exist at all.
fn function_stack(execution_role_arn: &str) -> String {
    let mut resources = serde_json::Map::new();
    for deployment in DEPLOYMENTS {
        resources.insert(
            deployment.function.to_string(),
            json!({
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": deployment.function,
                    "Runtime": deployment.runtime,
                    "Handler": "index.handler",
                    "Role": execution_role_arn,
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": {
                        "ZipFile": handler_source(
                            deployment.imports,
                            deployment.client,
                            deployment.sdk,
                        ),
                    },
                },
            }),
        );
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": Value::Object(resources),
    })
    .to_string()
}

/// Deploy the functions' stack and wait for `CREATE_COMPLETE`.
async fn deploy_functions(router: &MultiServiceRouter, execution_role_arn: &str) {
    let template = function_stack(execution_role_arn);
    let (status, answered) = query(
        router,
        "cloudformation",
        "2010-05-15",
        "CreateStack",
        &[("StackName", STACK), ("TemplateBody", &template)],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateStack({STACK:?}) must be accepted, got {status} with {answered}"
    );
    let reported = wait_for_stack_status(router, STACK, CREATE_COMPLETE).await;
    let failures = if reported.0 == CREATE_COMPLETE {
        Vec::new()
    } else {
        let (_, events) = query(
            router,
            "cloudformation",
            "2010-05-15",
            "DescribeStackEvents",
            &[("StackName", STACK)],
        )
        .await;
        elements(&events, "ResourceStatusReason")
    };
    assert_eq!(
        reported.0, CREATE_COMPLETE,
        "setup: the stack must reach {CREATE_COMPLETE}, got {} with resource failures {failures:?}",
        reported.1
    );
}

/// Wait until Lambda reports `name` `Active`, which is the state an invocation needs.
async fn wait_until_active(router: &MultiServiceRouter, name: &str) {
    let mut last = Value::Null;
    for _ in 0..LOOKS {
        let (status, _, configuration) = lambda(
            router,
            "GET",
            &format!("{FUNCTIONS_URI}/{name}/configuration"),
            &[],
        )
        .await;
        assert_eq!(
            status, 200,
            "setup: GetFunctionConfiguration({name:?}) must be served, got {status} with \
             {configuration}"
        );
        if configuration["State"].as_str() == Some(STATE_ACTIVE) {
            return;
        }
        last = configuration;
        tokio::time::sleep(LOOK).await;
    }
    panic!("setup: Lambda must report {name:?} {STATE_ACTIVE}, last reported {last}");
}

/// Whether an answered function error reports one of THIS PROCESS's own bounds rather than what
/// the handler came to ([`SANDBOX_TIMEDOUT`], [`RUNTIME_EXIT_ERROR`] with [`RUNTIME_KILLED`]).
fn host_bound(payload: &Value) -> bool {
    let class = payload["errorType"].as_str().unwrap_or_default();
    let message = payload["errorMessage"].as_str().unwrap_or_default();
    class == SANDBOX_TIMEDOUT || (class == RUNTIME_EXIT_ERROR && message.contains(RUNTIME_KILLED))
}

/// Invoke `function` with `event` and answer the handler's own payload, asserting on the way that
/// the invocation was served and that the run reported NO function error — which is what a runtime
/// with no SDK on the import path answers instead of a result.
///
/// Looks again, every [`LOOK`] until [`SERVED_DEADLINE`] passes, for as long as Lambda has not
/// served the invocation ([`UNSERVED`]) or ended the run on one of this process's own bounds
/// ([`host_bound`]) — neither of which is an answer of the generation plane — so what this answers
/// is always the outcome of a handler that ran. Every other status, and every other function
/// error, fails the row as it came back.
async fn invoked(router: &MultiServiceRouter, case: &str, function: &str, event: &Value) -> Value {
    let body = serde_json::to_vec(event).expect("the event serializes");
    let deadline = Instant::now() + SERVED_DEADLINE;
    loop {
        let (status, headers, payload) = lambda(
            router,
            "POST",
            &format!("{FUNCTIONS_URI}/{function}/invocations"),
            &body,
        )
        .await;
        let function_error = headers
            .get("x-amz-function-error")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        if Instant::now() < deadline
            && (status == UNSERVED || (function_error.is_some() && host_bound(&payload)))
        {
            tokio::time::sleep(LOOK).await;
            continue;
        }
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from Invoke, got {status} with {payload}"
        );
        // The DEPLOYMENT PACKAGE RAN: the payload below is the handler's own return value, so a
        // module the runtime could not import — which `Invoke` reports in this header — is never
        // a row's outcome.
        assert!(
            function_error.is_none(),
            "case {case:?}: expected the handler to run to completion, got the function error \
             {function_error:?} with {payload}"
        );
        return payload;
    }
}

/// What a row's call must come back to the handler as.
enum Expected {
    /// The generation was served, and the object whose ingested passage its citations must quote.
    Generated {
        quotes: &'static str,
        /// How many citations the response must carry, where the row bounded them.
        citations: Option<i64>,
    },
    /// The plane refused it with this error code, HTTP status and sentence.
    Refused {
        code: &'static str,
        status: i64,
        sentence: Sentence,
    },
}

/// Which refusal sentence a refused row must carry: the fixed text of that error class, with its
/// variable parts taken from the request and from this emulator's own state.
enum Sentence {
    /// The runtime plane's wording for a knowledge base it holds none of, whose one variable part
    /// is the identifier the request carried.
    UnknownKnowledgeBase,
    /// The wording of a call no identity-based policy in force allows, whose variable parts are
    /// the assumed-role session the invocation speaks for, the action it performs and the
    /// knowledge base it names.
    NotAuthorized,
}

/// One row: the function invoked, the query asked, how the model is named, how many passages the
/// response may quote, which knowledge base is named, and what the handler must observe.
struct Row {
    case: &'static str,
    function: &'static str,
    query: &'static str,
    /// Whether `modelArn` names the model by its own published ARN rather than by its bare id.
    model_by_arn: bool,
    number_of_results: Option<i64>,
    /// Whether the call names the knowledge base this suite ingested, or one nothing holds.
    held: bool,
    /// Whether the execution role's document allows the generation action when the handler calls.
    granted: bool,
    expected: Expected,
}

/// The rows, spanning the runtime and import form that ran the module, the query asked, the form
/// `modelArn` takes, the bound on how many passages may be quoted, the grant in force, and the
/// outcome class.
///
/// Ordered so that the rows of one function under one grant sit together: each such run of rows is
/// ONE invocation carrying every call it makes ([`handler_source`]), and the ungranted row is last
/// because it leaves the role's document without the action.
const ROWS: &[Row] = &[
    Row {
        case: "a module-scope package import generates over the pump manual",
        function: "KbGeneratePackageImport",
        query: "hydraulic pump serviced every 90 days",
        model_by_arn: false,
        number_of_results: None,
        held: true,
        granted: true,
        expected: Expected::Generated {
            quotes: "pump.txt",
            citations: None,
        },
    },
    Row {
        case: "numberOfResults 1, so one passage is quoted and one citation reported",
        function: "KbGeneratePackageImport",
        query: "coolant drum 7",
        model_by_arn: false,
        number_of_results: Some(1),
        held: true,
        granted: true,
        expected: Expected::Generated {
            quotes: "coolant.txt",
            citations: Some(1),
        },
    },
    Row {
        case: "a granted role's generation over a knowledge base nobody created",
        function: "KbGeneratePackageImport",
        query: "hydraulic pump serviced every 90 days",
        model_by_arn: false,
        number_of_results: None,
        held: false,
        granted: true,
        expected: Expected::Refused {
            code: NOT_FOUND,
            status: NOT_FOUND_STATUS,
            sentence: Sentence::UnknownKnowledgeBase,
        },
    },
    Row {
        case: "a from-import of the session class generates over the canteen notice, with the \
               model named by its published ARN",
        function: "KbGenerateSessionImport",
        query: "barley stew on Monday",
        model_by_arn: true,
        number_of_results: None,
        held: true,
        granted: true,
        expected: Expected::Generated {
            quotes: "menu.txt",
            citations: None,
        },
    },
    // Last, because it leaves the role's document without the action: what decides a call is the
    // grant in force when the handler makes it.
    Row {
        case: "the same generation once the role's document names no Bedrock action",
        function: "KbGenerateSessionImport",
        query: "hydraulic pump serviced every 90 days",
        model_by_arn: false,
        number_of_results: None,
        held: true,
        granted: false,
        expected: Expected::Refused {
            code: ACCESS_DENIED,
            status: ACCESS_DENIED_STATUS,
            sentence: Sentence::NotAuthorized,
        },
    },
];

/// The strings a reported list of strings holds, so a failure message names the values.
fn strings(payload: &Value, member: &str) -> Vec<String> {
    payload[member]
        .as_array()
        .unwrap_or_else(|| panic!("{member} is no list in {payload}"))
        .iter()
        .map(|value| {
            value
                .as_str()
                .unwrap_or_else(|| panic!("{member} holds a non-string in {payload}"))
                .to_string()
        })
        .collect()
}

/// The objects each citation of a served response names, one list per citation, as the handler
/// read them off its `retrievedReferences`.
fn cited_objects(payload: &Value) -> Vec<Vec<String>> {
    payload["refs"]
        .as_array()
        .unwrap_or_else(|| panic!("refs is no list in {payload}"))
        .iter()
        .map(|cited| {
            cited
                .as_array()
                .unwrap_or_else(|| panic!("refs holds a non-list in {payload}"))
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .unwrap_or_else(|| panic!("a reference is no string in {payload}"))
                        .to_string()
                })
                .collect()
        })
        .collect()
}

/// The knowledge base a row's call names: the one this suite ingested, or the well-formed
/// identifier nothing holds.
fn named_knowledge_base(row: &Row, ingested: &str) -> String {
    if row.held {
        ingested.to_string()
    } else {
        ABSENT_KNOWLEDGE_BASE.to_string()
    }
}

/// RULE: a managed `python3.x` runtime supplies the AWS SDK on the handler's import path, so a
/// handler whose module scope imports it runs — the invocation answers 200 with no function error
/// and the handler's own JSON — and the call it makes through that SDK gets the queried plane's own
/// answer: a response generated over the passages an ingestion recorded into the knowledge base's
/// S3 Vectors index where the execution role is granted the action, and that plane's own refusal
/// where it is not.
#[tokio::test]
async fn a_handler_generates_from_its_s3_vectors_backed_knowledge_base() {
    let router = build_router();

    let vector_bucket_arn = vector_store(&router).await;
    source_objects(&router).await;
    let knowledge_base_id = ingested_knowledge_base(&router, &vector_bucket_arn).await;

    // Every function runs as the one execution role whose document each row puts in force.
    let execution_role_arn = create_role(&router, FUNCTION_ROLE_NAME).await;

    // Every function is declared in ONE stack, so the provisioning spans the clock owes them run
    // down together rather than one after another.
    deploy_functions(&router, &execution_role_arn).await;
    for deployment in DEPLOYMENTS {
        wait_until_active(&router, deployment.function).await;
    }

    let ingested: BTreeSet<String> = DOCUMENTS.iter().map(|(key, _)| uri(key)).collect();

    // One invocation per run of rows sharing a function and a grant: the call each row makes
    // travels in that invocation's event, so the table costs one execution environment per grant
    // rather than one per row.
    let mut at = 0;
    while at < ROWS.len() {
        let mut upto = at + 1;
        while upto < ROWS.len()
            && ROWS[upto].function == ROWS[at].function
            && ROWS[upto].granted == ROWS[at].granted
        {
            upto += 1;
        }
        let group = &ROWS[at..upto];
        at = upto;

        let deployed = deployment(group[0].function);
        let calls: Vec<Value> = group
            .iter()
            .map(|row| {
                let model = if row.model_by_arn {
                    format!("arn:aws:bedrock:{REGION}::foundation-model/{GENERATION_MODEL}")
                } else {
                    GENERATION_MODEL.to_string()
                };
                let named = named_knowledge_base(row, &knowledge_base_id);
                let mut call = json!({
                    "kb_id": named,
                    "gen_model_arn": model,
                    "query": row.query,
                });
                if let Some(bound) = row.number_of_results {
                    call["number_of_results"] = json!(bound);
                }
                call
            })
            .collect();
        // The invocation this group's calls travel in is named by the function it runs in and the
        // grant in force, so a failure of one of the group's own assertions names the invocation
        // rather than the first row that happens to be in it.
        let case = format!(
            "{} as a role {} the generation action",
            group[0].function,
            if group[0].granted {
                "granted"
            } else {
                "not granted"
            }
        );
        let case = case.as_str();
        role_policy(&router, case, group[0].granted).await;
        let payload = invoked(
            &router,
            case,
            group[0].function,
            &json!({ "calls": calls }),
        )
        .await;

        // The SDK the module imported is a module of the runtime's own, and the runtime that ran it
        // is the one the function declares.
        assert_eq!(
            payload["sdk"].as_str(),
            Some(deployed.sdk_name),
            "case {case:?}: expected the import to resolve to {:?}, got {payload}",
            deployed.sdk_name
        );
        assert_eq!(
            payload["ran_under"].as_str(),
            Some(format!("AWS_Lambda_{}", deployed.runtime).as_str()),
            "case {case:?}: expected the invocation to run under {:?}, got {payload}",
            deployed.runtime
        );
        let answered = payload["results"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: no results in {payload}"));
        assert_eq!(
            answered.len(),
            group.len(),
            "case {case:?}: expected one result per call the event carried, got {payload}"
        );

        for (row, result) in group.iter().zip(answered) {
            let case = row.case;
            let payload = result;
            match row.expected {
                Expected::Generated { quotes, citations } => {
                    assert_eq!(
                        payload["ok"].as_bool(),
                        Some(true),
                        "case {case:?}: expected the generation to be served, got {payload}"
                    );
                    let answer = payload["answer"]
                        .as_str()
                        .unwrap_or_else(|| panic!("case {case:?}: no answer in {payload}"));
                    assert!(
                        !answer.is_empty(),
                        "case {case:?}: expected a generated response, got {payload}"
                    );
                    assert!(
                        !payload["session_id"]
                            .as_str()
                            .unwrap_or_default()
                            .is_empty(),
                        "case {case:?}: expected the session the turn was recorded in, got \
                         {payload}"
                    );
                    let reported = payload["num_citations"].as_i64().unwrap_or_default();
                    assert!(
                        reported >= 1,
                        "case {case:?}: expected the response to cite the passages it quoted, got \
                         {payload}"
                    );
                    if let Some(bound) = citations {
                        assert_eq!(
                            reported, bound,
                            "case {case:?}: expected {bound} citation(s) under the bound the \
                             request carried, got {payload}"
                        );
                    }
                    // Every citation quotes a segment of the response it indexes, and names the
                    // object its passage was ingested from — so a citation of something nobody
                    // ingested, or of text the response does not hold, fails here.
                    let segments = strings(payload, "segments");
                    let refs = cited_objects(payload);
                    assert_eq!(
                        refs.len(),
                        segments.len(),
                        "case {case:?}: expected the references of each citation beside its own \
                         segment, got {payload}"
                    );
                    for (segment, cited) in segments.iter().zip(&refs) {
                        assert!(
                            !segment.is_empty() && answer.contains(segment),
                            "case {case:?}: expected the cited segment {segment:?} to be a \
                             segment of the response {answer:?}"
                        );
                        assert!(
                            !cited.is_empty(),
                            "case {case:?}: expected the citation of {segment:?} to name the \
                             object it was quoted from, got {payload}"
                        );
                        for object in cited {
                            assert!(
                                ingested.contains(object),
                                "case {case:?}: expected every citation to name an object this \
                                 suite ingested ({ingested:?}), got {object:?}"
                            );
                        }
                    }
                    let quoted: BTreeSet<&String> = refs.iter().flatten().collect();
                    assert!(
                        quoted.contains(&uri(quotes)),
                        "case {case:?}: expected the response to quote {:?}, got {quoted:?}",
                        uri(quotes)
                    );
                }
                Expected::Refused {
                    code,
                    status,
                    ref sentence,
                } => {
                    assert_eq!(
                        payload["ok"].as_bool(),
                        Some(false),
                        "case {case:?}: expected the plane's own refusal, got {payload}"
                    );
                    assert_eq!(
                        payload["code"].as_str(),
                        Some(code),
                        "case {case:?}: expected the refusal {code}, got {payload}"
                    );
                    assert_eq!(
                        payload["status"].as_i64(),
                        Some(status),
                        "case {case:?}: expected the refusal at HTTP {status}, got {payload}"
                    );
                    // The sentence is a requirement of the error class: its fixed text, with the
                    // identifier the request carried, the session the invocation speaks for, the
                    // action it performs and the knowledge base it names filling the variable
                    // parts.
                    let named = named_knowledge_base(row, &knowledge_base_id);
                    let expected = match sentence {
                        Sentence::UnknownKnowledgeBase => {
                            format!("Knowledge Base with id {named} does not exist")
                        }
                        Sentence::NotAuthorized => format!(
                            "User: arn:aws:sts::{ACCOUNT}:assumed-role/{FUNCTION_ROLE_NAME}/{} is \
                             not authorized to perform: {GENERATE_ACTION} on resource: \
                             arn:aws:bedrock:{REGION}:{ACCOUNT}:knowledge-base/{named} because no \
                             identity-based policy allows the {GENERATE_ACTION} action",
                            row.function
                        ),
                    };
                    assert_eq!(
                        payload["message"].as_str(),
                        Some(expected.as_str()),
                        "case {case:?}: expected the sentence of this error class, got {payload}"
                    );
                }
            }
        }
    }
}
