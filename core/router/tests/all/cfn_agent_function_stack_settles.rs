// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack whose template spans the services an agent-backed function needs —
//! IAM for the execution role, AWS Lambda for the function, Amazon S3 Vectors for the vector
//! store and the Amazon Bedrock Agent build-time plane for the knowledge base over it — SETTLES,
//! and the function it deployed then answers an invoke.
//!
//! RULE: **every stack operation CloudFormation accepts reaches a TERMINAL status that
//! `DescribeStacks` publishes — `CREATE_COMPLETE` or a rollback for a create, `UPDATE_COMPLETE`
//! or a rollback for an update — however many services own the resource types the template
//! declares and however deep those resources depend on one another; and every resource a settled
//! stack reports created exists in the service that OWNS it, so the function the stack declared is
//! reported `Active` by AWS Lambda and runs when it is invoked.** A `*_IN_PROGRESS` status is one
//! the stack is on its way out of, never one it stays in, so a client polling `DescribeStacks`
//! always terminates — with a status to act on rather than a wait that never ends.
//!
//! Five crates have to agree for that to hold (CloudFormation drives the deployment and decides
//! the stack's status; IAM, Lambda, S3 Vectors and Bedrock Agent each own resources it declares and
//! are the only sides that can say whether one exists), so the suite lives here rather than in any
//! one of them. What the CloudFormation crate's own suites pin is the half that is its: which
//! resource types a body may declare and what identifier each resource then reports.
//!
//! The rows differ along what the rule generalises over: how many owning services one template
//! spans and how deep its resources depend on one another — a function and its role alone, the
//! same plus the two-level S3 Vectors store, and the whole estate with the knowledge base bound to
//! that store's index — so the rule is not keyed to one estate's shape. The second case is the
//! stack UPDATE of the same estates, which adds an agent action to the execution role's inline
//! policy.
//!
//! Every expected value is computed here from what the row declares. The one value that is not is
//! the knowledge base's identifier, which the owning service mints: what is held against it is
//! that the stack publishes it and that the owning service resolves it.
//!
//! The handler each row deploys is a Python module assembled here from `&'static str` constants,
//! with no value of any request reaching it; what bounds and contains the interpreter that runs it
//! is host code of the Lambda crate, which this suite neither adds to nor changes.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery form encoder and the XML readers this test crate writes ONCE
// (`tests/all/query_wire.rs`).
use super::query_wire::{elements, form_encode, opt_element, outputs_of};

/// The Region every request here is signed for, and so the Region every resource below belongs
/// to.
const REGION: &str = "us-east-1";

/// The account this emulator serves, which every ARN below carries.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// Where Lambda binds `GetFunctionConfiguration` and `Invoke`.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The terminal statuses a deployable template's create and update must reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const UPDATE_COMPLETE: &str = "UPDATE_COMPLETE";

/// The suffix every status CloudFormation reports for an operation still in flight ends with, and
/// therefore the one status class a poll looks past.
const IN_PROGRESS: &str = "_IN_PROGRESS";

/// The `State` an invocation needs the function to be in.
const STATE_ACTIVE: &str = "Active";

/// How often to look again for work the clock owes — a stack settling, a function reaching
/// `Active`, an invocation being served — and how long to go on looking. The spans are the
/// services' own, so no fixed sleep names the instant one ends; the deadline is generous because
/// what every case asserts is the CONDITION and never the time it took, so a loaded host cannot
/// decide an outcome.
const LOOK: Duration = Duration::from_millis(50);
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);

/// How long a row waits for Lambda to serve its invocation. Read as a deadline rather than as a
/// number of looks, because one look of this wait is a whole invocation.
const SERVED_DEADLINE: Duration = Duration::from_secs(120);

/// The status Lambda answers an invocation it has NOT served with, which carries no answer of the
/// handler's: the concurrent-execution limit of this process was reached
/// (`TooManyRequestsException`, `@httpError(429)`). Whether one arrives is decided by what the rest
/// of this process is running at that instant and by nothing a row varies, so a row looks again on
/// it rather than reading an outcome out of an invocation that never ran.
const UNSERVED: u16 = 429;

/// The `errorType` classes of a run THIS PROCESS's own bounds ended rather than the handler: the
/// sandbox stopping a run at the host wall clock an invocation is given, and the memory bound read
/// from the whole process. Neither says anything about what the handler came to, so a row answered
/// with one looks again; every OTHER function error fails the row as it came back.
const SANDBOX_TIMEDOUT: &str = "Sandbox.Timedout";
const RUNTIME_EXIT_ERROR: &str = "Runtime.ExitError";
const RUNTIME_KILLED: &str = "signal: killed";

/// The capability a template declaring IAM resources is deployed with.
const CAPABILITY_IAM: &str = "CAPABILITY_IAM";

/// The `python3.x` runtime every row's function runs, and the handler it entered at.
const RUNTIME: &str = "python3.12";
const HANDLER: &str = "index.handler";

/// How long one invocation is given, as the function's own `Timeout`: well past what a run needs on
/// any host, because what is under test is that the deployed function answers and not how fast an
/// interpreter gets through it.
const FUNCTION_TIMEOUT: i64 = 120;

/// The name of the inline policy every row's execution role carries.
const INLINE_POLICY_NAME: &str = "agent-function-grants";

/// The action a stack UPDATE adds to that policy: invoking a Bedrock agent, which is what a
/// function calling the agent runtime stands on.
const INVOKE_AGENT_ACTION: &str = "bedrock:InvokeAgent";

/// How the rows' index is created: the vectors of an embedding model are 32-bit floats compared by
/// cosine distance, and Titan Text Embeddings G1 produces 1536 of them — the length the index has
/// to hold for the knowledge base over it to be accepted.
const DATA_TYPE: &str = "float32";
const DISTANCE_METRIC: &str = "cosine";
const EMBEDDING_MODEL: &str = "amazon.titan-embed-text-v1";
const EMBEDDING_DIMENSION: i64 = 1536;

/// The `status` Amazon Bedrock Agent reports a usable knowledge base in.
const KNOWLEDGE_BASE_ACTIVE: &str = "ACTIVE";

/// The module every row's function runs: it returns a value COMPUTED from the event it was handed,
/// so a payload that comes back is a handler that ran rather than an echo of the request.
const HANDLER_SOURCE: &str = r#"
def handler(event, context):
    return {
        "ok": True,
        "probe": event["probe"],
        "resources": sorted(event["resources"]),
    }
"#;

/// The emulator as it is ASSEMBLED, on one engine: the services that own the declared resources are
/// the ones CloudFormation's own in-process calls reach, so a resource read back here is the record
/// the deployment really wrote.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate; the credential
    // scope is what routes a request.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service` in [`REGION`], which is
/// what routes the request and where each handler reads its Region from.
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
    let host = format!("{service}.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", host)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(service))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One CloudFormation action over its `awsQuery` wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    query(router, "cloudformation", "2010-05-15", action, members).await
}

/// One IAM action over its `awsQuery` wire, which must be served.
async fn iam_ok(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let (status, body) = query(router, "iam", "2010-05-08", action, members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from IAM {action}, got {status} with {body}"
    );
    body
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

/// One Amazon Bedrock Agent read, over the `restJson1` wire that model declares, on the method and
/// URI the operation's own `@http` binding names, signed for `bedrock` — the signing name every
/// Bedrock-family model declares.
async fn bedrock_agent(router: &MultiServiceRouter, method: &str, uri: &str) -> (u16, Value) {
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("bedrock.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("bedrock"))
        .body(Bytes::new())
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

/// One row: the estate one template declares.
struct Row {
    case: &'static str,
    /// The stack's name, which every resource name below is built from.
    stack: &'static str,
    /// Whether the template declares the S3 Vectors vector bucket and the index inside it — two
    /// resources, the second depending on the first.
    vector_store: bool,
    /// Whether it declares the Bedrock knowledge base bound to that index, whose identifier the
    /// owning service mints (which implies `vector_store`).
    knowledge_base: bool,
}

/// The estates: one owning service beside CloudFormation, then two with a dependency between their
/// resources, then the whole thing with the knowledge base on top of it.
const ROWS: &[Row] = &[
    Row {
        case: "a function and the execution role it runs as",
        stack: "cfn-agent-fn",
        vector_store: false,
        knowledge_base: false,
    },
    Row {
        case: "a function, its role and the vector store an agent queries",
        stack: "cfn-agent-fn-vectors",
        vector_store: true,
        knowledge_base: false,
    },
    Row {
        case: "the whole estate: the function, its role, the vector store and the knowledge base \
               over its index",
        stack: "cfn-agent-fn-knowledge",
        vector_store: true,
        knowledge_base: true,
    },
];

/// The vector bucket, index, function and knowledge base one row declares, each named after its
/// stack so no two rows share a resource.
fn vector_bucket_name(row: &Row) -> String {
    format!("{}-store", row.stack)
}
fn index_name(row: &Row) -> String {
    format!("{}-index", row.stack)
}
fn function_name(row: &Row) -> String {
    format!("{}-handler", row.stack)
}
fn knowledge_base_name(row: &Row) -> String {
    format!("{}-knowledge", row.stack)
}

/// The ARN of the index one row declares, as `IndexArn`'s own pattern spells it.
fn index_arn(row: &Row) -> String {
    format!(
        "arn:aws:s3vectors:{REGION}:{ACCOUNT}:bucket/{}/index/{}",
        vector_bucket_name(row),
        index_name(row)
    )
}

/// The trust policy the rows' one role carries: AWS Lambda assumes it to run the function, and
/// Amazon Bedrock assumes it to read the knowledge base's store.
fn trust_policy() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": ["lambda.amazonaws.com", "bedrock.amazonaws.com"]},
            "Action": "sts:AssumeRole",
        }],
    })
}

/// The inline document that role carries: the log actions every execution role needs, the vector
/// reads where the row declares a vector store, and — once `invoke_agent` is asked for, which is
/// what the UPDATE adds — the agent action.
fn role_document(row: &Row, invoke_agent: bool) -> Value {
    let mut statements = vec![json!({
        "Effect": "Allow",
        "Action": ["logs:CreateLogGroup", "logs:CreateLogStream", "logs:PutLogEvents"],
        "Resource": "*",
    })];
    if row.vector_store {
        statements.push(json!({
            "Effect": "Allow",
            "Action": ["s3vectors:QueryVectors", "s3vectors:GetVectors"],
            "Resource": index_arn(row),
        }));
    }
    if invoke_agent {
        statements.push(json!({
            "Effect": "Allow",
            "Action": [INVOKE_AGENT_ACTION],
            "Resource": format!("arn:aws:bedrock:{REGION}:{ACCOUNT}:agent-alias/*"),
        }));
    }
    json!({"Version": "2012-10-17", "Statement": statements})
}

/// The template one row deploys, with the execution role granted the agent action or not.
fn template(row: &Row, invoke_agent: bool) -> String {
    let mut resources = serde_json::Map::new();
    resources.insert(
        "ExecutionRole".to_string(),
        json!({
            "Type": "AWS::IAM::Role",
            "Properties": {
                "AssumeRolePolicyDocument": trust_policy(),
                "Policies": [{
                    "PolicyName": INLINE_POLICY_NAME,
                    "PolicyDocument": role_document(row, invoke_agent),
                }],
            },
        }),
    );
    if row.vector_store {
        resources.insert(
            "VectorStore".to_string(),
            json!({
                "Type": "AWS::S3Vectors::VectorBucket",
                "Properties": {"VectorBucketName": vector_bucket_name(row)},
            }),
        );
        resources.insert(
            "Embeddings".to_string(),
            json!({
                "Type": "AWS::S3Vectors::Index",
                "Properties": {
                    "VectorBucketArn": {"Ref": "VectorStore"},
                    "IndexName": index_name(row),
                    "DataType": DATA_TYPE,
                    "Dimension": EMBEDDING_DIMENSION,
                    "DistanceMetric": DISTANCE_METRIC,
                },
            }),
        );
    }
    if row.knowledge_base {
        resources.insert(
            "Knowledge".to_string(),
            json!({
                "Type": "AWS::Bedrock::KnowledgeBase",
                "Properties": {
                    "Name": knowledge_base_name(row),
                    "RoleArn": {"Fn::GetAtt": ["ExecutionRole", "Arn"]},
                    "KnowledgeBaseConfiguration": {
                        "Type": "VECTOR",
                        "VectorKnowledgeBaseConfiguration": {
                            "EmbeddingModelArn":
                                format!("arn:aws:bedrock:{REGION}::foundation-model/{EMBEDDING_MODEL}"),
                        },
                    },
                    "StorageConfiguration": {
                        "Type": "S3_VECTORS",
                        "S3VectorsConfiguration": {"IndexArn": {"Ref": "Embeddings"}},
                    },
                },
            }),
        );
    }
    resources.insert(
        "Handler".to_string(),
        json!({
            "Type": "AWS::Lambda::Function",
            "Properties": {
                "FunctionName": function_name(row),
                "Runtime": RUNTIME,
                "Handler": HANDLER,
                "Role": {"Fn::GetAtt": ["ExecutionRole", "Arn"]},
                "Timeout": FUNCTION_TIMEOUT,
                "Code": {"ZipFile": HANDLER_SOURCE},
            },
        }),
    );

    let mut outputs = serde_json::Map::new();
    outputs.insert(
        "FunctionName".to_string(),
        json!({"Value": {"Ref": "Handler"}}),
    );
    outputs.insert(
        "RoleName".to_string(),
        json!({"Value": {"Ref": "ExecutionRole"}}),
    );
    if row.vector_store {
        outputs.insert(
            "IndexArn".to_string(),
            json!({"Value": {"Ref": "Embeddings"}}),
        );
    }
    if row.knowledge_base {
        outputs.insert(
            "KnowledgeBaseId".to_string(),
            json!({"Value": {"Ref": "Knowledge"}}),
        );
    }

    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": Value::Object(resources),
        "Outputs": Value::Object(outputs),
    })
    .to_string()
}

/// Poll `DescribeStacks` the way a client's waiter does and answer the TERMINAL status the stack
/// settled on, with the answer it was read from: the first status that is not an operation in
/// flight. The wait is on that CONDITION, looking again until it holds or [`SETTLE_DEADLINE`]
/// passes — which is the rule itself, since a stack that never leaves `*_IN_PROGRESS` is what the
/// deadline catches.
async fn settled(router: &MultiServiceRouter, case: &str, stack: &str) -> (String, String) {
    let giving_up_at = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, body) = cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStacks, got {status} with {body}"
        );
        let reported = opt_element(&body, "StackStatus").unwrap_or_default();
        if !reported.is_empty() && !reported.ends_with(IN_PROGRESS) {
            return (reported, body);
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: expected {stack} to reach a terminal status, last saw {reported:?}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// The reasons the stack recorded for the resources whose own operation did not succeed, so a
/// settled status other than the expected one is reported with what caused it.
///
/// The read itself has to be served: a refused `DescribeStackEvents` would otherwise read as "no
/// resource failed" and leave the case's diagnostic silently empty.
async fn failure_reasons(router: &MultiServiceRouter, case: &str, stack: &str) -> Vec<String> {
    let (status, events) =
        cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStackEvents, got {status} with {events}"
    );
    elements(&events, "ResourceStatusReason")
}

/// Deploy `template` with `action` (`CreateStack` or `UpdateStack`), wait for the stack to settle
/// and assert it settled on `wanted`, answering the `DescribeStacks` answer it settled in.
async fn deployed(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    stack: &str,
    template: &str,
    wanted: &str,
) -> String {
    let (status, answered) = cloudformation(
        router,
        action,
        &[
            ("StackName", stack),
            ("TemplateBody", template),
            ("Capabilities.member.1", CAPABILITY_IAM),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with {answered}"
    );
    let (reported, body) = settled(router, case, stack).await;
    let reasons = if reported == wanted {
        Vec::new()
    } else {
        failure_reasons(router, case, stack).await
    };
    assert_eq!(
        reported, wanted,
        "case {case:?}: expected {action} to settle the stack {wanted}, got {body} with resource \
         failures {reasons:?}"
    );
    body
}

/// Wait until Lambda reports `function` `Active`, which is the state an invocation needs, and
/// answer the configuration it reported.
async fn active_function(router: &MultiServiceRouter, case: &str, function: &str) -> Value {
    let giving_up_at = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, _, configuration) = lambda(
            router,
            "GET",
            &format!("{FUNCTIONS_URI}/{function}/configuration"),
            &[],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from GetFunctionConfiguration({function:?}), got \
             {status} with {configuration}"
        );
        if configuration["State"].as_str() == Some(STATE_ACTIVE) {
            return configuration;
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: expected Lambda to report {function:?} {STATE_ACTIVE}, last reported \
             {configuration}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// Whether an answered function error reports one of THIS PROCESS's own bounds rather than what the
/// handler came to.
fn host_bound(payload: &Value) -> bool {
    let class = payload["errorType"].as_str().unwrap_or_default();
    let message = payload["errorMessage"].as_str().unwrap_or_default();
    class == SANDBOX_TIMEDOUT || (class == RUNTIME_EXIT_ERROR && message.contains(RUNTIME_KILLED))
}

/// Invoke `function` with `event` and answer the handler's own payload, asserting on the way that
/// the invocation was served and that the run reported NO function error.
///
/// Looks again, every [`LOOK`] until [`SERVED_DEADLINE`] passes, for as long as Lambda has not
/// served the invocation ([`UNSERVED`]) or ended the run on one of this process's own bounds
/// ([`host_bound`]) — neither of which is an outcome of the deployed function — so what this
/// answers is always the outcome of a handler that ran.
async fn invoked(router: &MultiServiceRouter, case: &str, function: &str, event: &Value) -> Value {
    let body = serde_json::to_vec(event).expect("the event serializes");
    let giving_up_at = Instant::now() + SERVED_DEADLINE;
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
        if Instant::now() < giving_up_at
            && (status == UNSERVED || (function_error.is_some() && host_bound(&payload)))
        {
            tokio::time::sleep(LOOK).await;
            continue;
        }
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from Invoke({function:?}), got {status} with \
             {payload}"
        );
        assert!(
            function_error.is_none(),
            "case {case:?}: expected the handler to run to completion, got the function error \
             {function_error:?} with {payload}"
        );
        return payload;
    }
}

/// An output of a settled stack, which every row publishes for the handles its assertions take.
fn output(outputs: &BTreeMap<String, String>, case: &str, key: &str) -> String {
    outputs
        .get(key)
        .unwrap_or_else(|| panic!("case {case:?}: no {key} output in {outputs:?}"))
        .clone()
}

/// The inline policy document IAM holds for `role`, as the JSON it encodes.
///
/// IAM reports a policy document "URL-encoded compliant with RFC 3986" and a caller "can use a URL
/// decoding method to convert the policy back to plain JSON text" (`GetRolePolicy`), so the
/// comparison with the declared document reads it through that decode.
async fn inline_policy(router: &MultiServiceRouter, case: &str, role: &str) -> Value {
    let body = iam_ok(
        router,
        case,
        "GetRolePolicy",
        &[("RoleName", role), ("PolicyName", INLINE_POLICY_NAME)],
    )
    .await;
    let encoded = opt_element(&body, "PolicyDocument").unwrap_or_default();
    let decoded = ::smithy_server_runtime::percent_decode(&encoded);
    serde_json::from_str(&decoded).unwrap_or_else(|error| {
        panic!("case {case:?}: IAM reported a policy document that is no JSON ({error}): {decoded}")
    })
}

/// The event every row invokes its function with, and the payload a handler that ran answers it
/// with — computed here from the event, never read out of the answer.
///
/// The list is the logical ids the row's template declares, handed over DELIBERATELY UNSORTED and
/// never one element long: the handler's own answer carries them sorted, so a run that merely
/// echoed its event back would not satisfy any row.
fn probe(row: &Row) -> (Value, Value) {
    let mut resources: Vec<&str> = vec!["Handler", "ExecutionRole"];
    if row.vector_store {
        resources.extend_from_slice(&["Embeddings", "VectorStore"]);
    }
    if row.knowledge_base {
        resources.push("Knowledge");
    }
    let mut sorted = resources.clone();
    sorted.sort_unstable();
    assert_ne!(
        resources, sorted,
        "case {:?}: the probe's list must reach the handler unsorted",
        row.case
    );
    (
        json!({"probe": row.stack, "resources": resources}),
        json!({"ok": true, "probe": row.stack, "resources": sorted}),
    )
}

/// RULE (the create): a stack whose template spans the services an agent-backed function needs
/// reaches `CREATE_COMPLETE` — a terminal status, reported to a client that polls — and every
/// resource it reports created exists in the service that owns it, so the function it deployed is
/// `Active` and answers an invoke with its own computed payload.
#[tokio::test]
async fn a_stack_spanning_the_owning_services_settles_and_its_function_answers() {
    let router = build_router();

    for row in ROWS {
        let case = row.case;
        let body = deployed(
            &router,
            case,
            "CreateStack",
            row.stack,
            &template(row, false),
            CREATE_COMPLETE,
        )
        .await;
        let outputs = outputs_of(&body);

        // The handles the stack published are the ones the owning services answer to.
        let function = output(&outputs, case, "FunctionName");
        assert_eq!(
            function,
            function_name(row),
            "case {case:?}: expected the stack to publish the function name its template declares"
        );
        let role = output(&outputs, case, "RoleName");

        // IAM holds the role, carrying the document the template declared.
        let held = inline_policy(&router, case, &role).await;
        assert_eq!(
            held,
            role_document(row, false),
            "case {case:?}: expected IAM to hold the inline policy the template declares for \
             {role:?}, got {held}"
        );

        // AWS Lambda holds the function, in the state an invocation needs.
        let configuration = active_function(&router, case, &function).await;
        assert_eq!(
            (
                configuration["FunctionName"].as_str(),
                configuration["Runtime"].as_str(),
                configuration["Handler"].as_str(),
            ),
            (Some(function.as_str()), Some(RUNTIME), Some(HANDLER)),
            "case {case:?}: expected Lambda to report the function the template declares, got \
             {configuration}"
        );

        // Amazon S3 Vectors holds the index, under the ARN the stack published for it.
        if row.vector_store {
            let arn = output(&outputs, case, "IndexArn");
            assert_eq!(
                arn,
                index_arn(row),
                "case {case:?}: expected the stack to publish the index's own ARN"
            );
            let (status, answered) = s3vectors(&router, "GetIndex", &json!({"indexArn": arn})).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200 from GetIndex on the ARN the stack published, \
                 got {status} with {answered}"
            );
            assert_eq!(
                (
                    answered["index"]["vectorBucketName"].as_str(),
                    answered["index"]["indexName"].as_str(),
                    answered["index"]["dataType"].as_str(),
                    answered["index"]["dimension"].as_i64(),
                    answered["index"]["distanceMetric"].as_str(),
                ),
                (
                    Some(vector_bucket_name(row).as_str()),
                    Some(index_name(row).as_str()),
                    Some(DATA_TYPE),
                    Some(EMBEDDING_DIMENSION),
                    Some(DISTANCE_METRIC),
                ),
                "case {case:?}: expected S3 Vectors to report the index the template declares, got \
                 {answered}"
            );
        }

        // Amazon Bedrock Agent holds the knowledge base, bound to that index.
        if row.knowledge_base {
            let id = output(&outputs, case, "KnowledgeBaseId");
            let (status, answered) =
                bedrock_agent(&router, "GET", &format!("/knowledgebases/{id}")).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200 from GetKnowledgeBase on the id the stack \
                 published, got {status} with {answered}"
            );
            let held = &answered["knowledgeBase"];
            assert_eq!(
                (
                    held["knowledgeBaseId"].as_str(),
                    held["name"].as_str(),
                    held["status"].as_str(),
                    held["storageConfiguration"]["s3VectorsConfiguration"]["indexArn"].as_str(),
                ),
                (
                    Some(id.as_str()),
                    Some(knowledge_base_name(row).as_str()),
                    Some(KNOWLEDGE_BASE_ACTIVE),
                    Some(index_arn(row).as_str()),
                ),
                "case {case:?}: expected Bedrock Agent to report the knowledge base the template \
                 declares, bound to the index of the same stack, got {answered}"
            );
        }

        // ...and the deployed function RUNS: the payload is the handler's own, computed from the
        // event it was handed.
        let (event, expected) = probe(row);
        let payload = invoked(&router, case, &function, &event).await;
        assert_eq!(
            payload, expected,
            "case {case:?}: expected the deployed function to answer {expected}, got {payload}"
        );
    }
}

/// RULE (the update): an `UpdateStack` that adds the agent action to the execution role's inline
/// policy settles too — the stack reaches `UPDATE_COMPLETE`, another terminal status — and the
/// grant it declared is the grant IAM then holds for that same role.
#[tokio::test]
async fn an_update_that_grants_the_role_the_agent_action_settles() {
    let router = build_router();

    for row in ROWS {
        let case = row.case;
        let body = deployed(
            &router,
            case,
            "CreateStack",
            row.stack,
            &template(row, false),
            CREATE_COMPLETE,
        )
        .await;
        let role = output(&outputs_of(&body), case, "RoleName");
        let before = inline_policy(&router, case, &role).await;
        assert_eq!(
            before,
            role_document(row, false),
            "case {case:?}: expected the created role to carry the declared document, got {before}"
        );

        let body = deployed(
            &router,
            case,
            "UpdateStack",
            row.stack,
            &template(row, true),
            UPDATE_COMPLETE,
        )
        .await;
        // The same role, not a replacement: the stack still publishes the handle it created.
        let after_role = output(&outputs_of(&body), case, "RoleName");
        assert_eq!(
            after_role, role,
            "case {case:?}: expected the update to keep the role the create made"
        );

        let after = inline_policy(&router, case, &role).await;
        assert_eq!(
            after,
            role_document(row, true),
            "case {case:?}: expected IAM to hold the document the update declares, got {after}"
        );
        let granted = after["Statement"]
            .as_array()
            .map(|statements| {
                statements.iter().any(|statement| {
                    statement["Action"].as_array().is_some_and(|actions| {
                        actions.iter().any(|action| action == INVOKE_AGENT_ACTION)
                    })
                })
            })
            .unwrap_or_default();
        assert!(
            granted,
            "case {case:?}: expected the updated policy to grant {INVOKE_AGENT_ACTION}, got {after}"
        );
    }
}
