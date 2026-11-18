// SPDX-License-Identifier: CC0-1.0

//! WHOSE in-function reads an explicit key-policy deny reaches: every session of the role it
//! names, and no session of any other.
//!
//! RULE: serving the bytes of an object stored under a customer-managed key performs
//! `kms:Decrypt` on that key as the caller, and the caller of a read an AWS SDK makes inside a
//! running function is a SESSION of that function's execution role
//! (`arn:aws:sts::<account>:assumed-role/<role>/<function>`). A key policy statement naming a ROLE
//! is about that role, so an explicit deny of the decrypt naming it refuses the read for EVERY
//! session of it — each function running that role, each refusal naming that function's own session
//! — while a session of any other role, which the statement does not name, is served the stored
//! bytes under the same document and from the same key; and the object a read is refused is the
//! same object it is served, whichever reference the write that stored it named the key by. Each
//! read is decided afresh, so lifting the deny serves the very next invocation.
//!
//! "The execution role grants the function permission to use Amazon Web Services services"
//! (`FunctionConfiguration$Role`); "if the object is encrypted with SSE-KMS, you must also have the
//! kms:GenerateDataKey and kms:Decrypt permissions in IAM identity-based policies and KMS key
//! policies for the KMS key" (`HeadObject`'s own documentation of what such a read needs); a key
//! policy naming the account's root user delegates the decision to that account's identity-based
//! policies, a `Deny` applies to the principals its own `Principal` element names, and "an explicit
//! deny in any policy overrides any allows" (IAM User Guide, *Policy evaluation logic*).
//! `StatusCode` is the invocation's own HTTP status and `FunctionError` the `X-Amz-Function-Error`
//! header (`smithy_query(query="operation",operation="Invoke")`), so a refused read is a value in
//! the handler's payload and never a failed invocation.
//!
//! Six crates have to agree for that to hold — KMS holds the key policy and takes the decrypt
//! decision, Amazon S3 stores the object and asks for the decrypt on the reader's behalf, IAM holds
//! the two roles and their inline documents, STS mints the session each function's calls are signed
//! as, CloudFormation archives the inline handler into a deployment package this emulator can run,
//! and Lambda runs each function as its own role — so the suite lives here, where a crate-local one
//! could only ever see one side of it. What the neighbours pin and this one does not repeat:
//! `lambda_in_function_sse_kms_read_follows_the_live_key_policy` drives the lifecycle and the
//! per-KEY scope of a deny with ONE function on ONE role, so nothing there could tell a deny about
//! that role from a deny about any principal; `lambda_runtime_sdk_reads_an_sse_kms_object` tables
//! which documents reach a function's session; the Amazon S3 crate's
//! `sse_kms_decrypt_authorization` tables the decision for a caller presenting a credential of its
//! own. What only this suite reaches is the PRINCIPAL scope of one document, observed across THREE
//! sessions at once.
//!
//! The rows differ along what the rule generalises over: WHICH role the document's deny names (no
//! deny at all, the role two of the functions run as, the role the third runs as, and no deny
//! again), and within every row WHICH session reads (two functions of the denied role, so a deny
//! that reached only the session it was first observed on would fail here, and one function of the
//! other role) and HOW the write named the key the object was sealed under (the ARN KMS minted, and
//! the bare key id).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{elements, form_encode, opt_element};

/// The Region every request here is signed for, and so the Region the key, the bucket, the roles
/// and the functions belong to. `us-east-1`, so a bucket create names no location constraint.
const REGION: &str = "us-east-1";

/// The account this emulator serves, which every ARN below carries.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The bucket both objects of this suite are stored in.
const BUCKET: &str = "xsvc-key-policy-principal-scope";

/// The stack that declares every function: `Code.ZipFile` in a template is the inline source
/// CloudFormation archives into a deployment package this emulator can run.
const STACK: &str = "xsvc-key-policy-principal-scope-probes";

/// The `StackStatus` that stack must reach, and the `State` a function serves invocations in.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const FUNCTION_ACTIVE: &str = "Active";

/// The runtime every function declares: a managed `python3.x` one, which carries the AWS SDK for
/// Python on the handler's import path (source: `smithy_query(query="shape",shape="Runtime")`).
const RUNTIME: &str = "python3.12";

/// The module and attribute CloudFormation's inline source is archived under.
const HANDLER: &str = "index.handler";

/// The environment variable each handler reads the bucket's name from.
const BUCKET_VARIABLE: &str = "BUCKET";

/// The code and status Amazon S3 refuses a read no authority in force permits with (Amazon S3
/// "Error responses": `AccessDenied`, HTTP 403).
const ACCESS_DENIED: &str = "AccessDenied";
const ACCESS_DENIED_STATUS: u64 = 403;

/// The action serving an SSE-KMS object's bytes performs on the key it was sealed under, the
/// service's whole wildcard a key's delegation is written with, and the read each role is granted
/// on the bucket.
const DECRYPT_ACTION: &str = "kms:Decrypt";
const EVERY_KMS_ACTION: &str = "kms:*";
const READ_ACTION: &str = "s3:GetObject";

/// The `ServerSideEncryption` value that is SSE-KMS — the one a KMS key id belongs to (source:
/// `smithy_query(query="shape",shape="ServerSideEncryption")`) — and the two headers an object's own
/// encryption is reported in.
const SSE_KMS: &str = "aws:kms";
const SSE_HEADER: &str = "x-amz-server-side-encryption";
const SSE_KEY_HEADER: &str = "x-amz-server-side-encryption-aws-kms-key-id";

/// The one name a key holds a policy under.
const KEY_POLICY_NAME: &str = "default";

/// The API versions an awsQuery call to IAM and to CloudFormation names.
const IAM_API_VERSION: &str = "2010-05-08";
const CLOUDFORMATION_API_VERSION: &str = "2010-05-15";

/// How long one invocation is given, as each function's own `Timeout`: well past what a run needs
/// on any host, because what is under test is the decision and not how fast an interpreter gets
/// through the handler.
const FUNCTION_TIMEOUT: u64 = 120;

/// How often to look while waiting for work the clock owes — a created function reaching `Active` —
/// and how many looks to take. The span is Lambda's own, so no fixed sleep names the instant it
/// ends, and the deadline is generous enough that a loaded host cannot decide the outcome.
const LOOK: Duration = Duration::from_millis(100);
const LOOKS: usize = 300;

/// The handler every function runs: the SDK its managed runtime supplies, imported and its clients
/// built at MODULE scope, the bucket from the function's own environment, and each object the event
/// names read for its bytes — reporting either them or Amazon S3's own refusal: its code, its
/// sentence and the status it came back on.
const PROBE_HANDLER: &str = r#"
import os

import boto3
from botocore.exceptions import ClientError

OBJECTS = boto3.client("s3")
IDENTITIES = boto3.client("sts")
BUCKET = os.environ["BUCKET"]


def handler(event, context):
    answered = {"identity": IDENTITIES.get_caller_identity()["Arn"], "reads": {}}
    for label, key in event["objects"].items():
        try:
            read = OBJECTS.get_object(Bucket=BUCKET, Key=key)
        except ClientError as refused:
            answered["reads"][label] = {
                "code": refused.response["Error"]["Code"],
                "message": refused.response["Error"]["Message"],
                "status": refused.response["ResponseMetadata"]["HTTPStatusCode"],
            }
            continue
        answered["reads"][label] = {"body": read["Body"].read().decode("utf-8")}
    return answered
"#;

/// WHICH of the two roles a function runs as: a key policy statement names a role, so two roles are
/// what makes the principal scope of one statement observable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Role {
    /// The role TWO of the functions run as.
    Shared,
    /// The role the third function runs as.
    Other,
}

impl Role {
    /// Every role this suite creates.
    const ALL: &'static [Self] = &[Self::Shared, Self::Other];

    /// The name IAM holds this role under.
    fn name(self) -> &'static str {
        match self {
            Self::Shared => "xsvc-key-policy-scope-shared",
            Self::Other => "xsvc-key-policy-scope-other",
        }
    }
}

/// One probe: a function of its own, running one of the two roles. Two probes run the SAME role, so
/// a document naming that role is observed on two different sessions of it.
struct Probe {
    case: &'static str,
    function: &'static str,
    role: Role,
}

const PROBES: &[Probe] = &[
    Probe {
        case: "the first function of the shared role",
        function: "xsvc-key-policy-scope-shared-one",
        role: Role::Shared,
    },
    Probe {
        case: "the second function of the shared role",
        function: "xsvc-key-policy-scope-shared-two",
        role: Role::Shared,
    },
    Probe {
        case: "the function of the other role",
        function: "xsvc-key-policy-scope-other-one",
        role: Role::Other,
    },
];

/// HOW the write named the key it sealed the object under — the two references `SSEKMSKeyId` takes.
/// Whichever one the write named, the object's record reports the key's ARN and the decision about
/// its bytes is that key's.
#[derive(Debug, Clone, Copy)]
enum Named {
    /// The ARN KMS minted for the key.
    ByArn,
    /// The bare key id.
    ById,
}

/// One object of this suite: where it sits, the bytes it holds — distinct per object, so "the exact
/// bytes this object holds" is something a read can be wrong about — and the reference its write
/// named the key by.
struct Sealed {
    label: &'static str,
    object_key: &'static str,
    body: &'static str,
    named: Named,
}

const OBJECTS: &[Sealed] = &[
    Sealed {
        label: "the object whose write named the key by its ARN, flat",
        object_key: "named-by-arn",
        body: "the bytes the key protects",
        named: Named::ByArn,
    },
    Sealed {
        label: "the object whose write named the key by its bare id, under a dated prefix",
        object_key: "reports/2026/11/06/named-by-key-id.txt",
        body: "region,amount\nus-east-1,17\n",
        named: Named::ById,
    },
];

/// One row: WHICH role the key document's explicit deny of the decrypt names while every probe
/// reads, and whether that document is PUT — the first row reads the key as KMS made it, so nothing
/// has to be written for the reads to start out served, and the last puts that document back.
struct Row {
    case: &'static str,
    denied: Option<Role>,
    put: bool,
}

const ROWS: &[Row] = &[
    Row {
        case: "the document the key was created with",
        denied: None,
        put: false,
    },
    Row {
        case: "an explicit deny of the decrypt naming the shared role",
        denied: Some(Role::Shared),
        put: true,
    },
    Row {
        case: "that deny naming the other role instead",
        denied: Some(Role::Other),
        put: true,
    },
    Row {
        case: "the original document put back",
        denied: None,
        put: true,
    },
];

/// The emulator as it is ASSEMBLED, on one engine: each handler's own SDK call is dispatched back
/// into this same router, so the object it reads and the key policy that decides the read are the
/// ones this suite planted.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate; the credential
    // scope is what routes a request.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes the
/// request. Its credential is the long-term key every emulator publishes, so these calls are the
/// account's own and not any role's.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// What one Amazon S3 call answered.
struct Answer {
    status: u16,
    headers: http::HeaderMap,
    body: String,
}

impl Answer {
    /// A bound response header's value.
    fn header(&self, name: &str) -> Option<String> {
        self.headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    }
}

/// One awsQuery call of `service`, answered with the HTTP status and the XML body.
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
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// One KMS `awsJson1_1` call, made as the account's own credential.
async fn kms(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("kms.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("TrentService.{operation}"))
        .header("authorization", authorization("kms"))
        .body(Bytes::from(body.to_string()))
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

/// One Amazon S3 call, addressed path-style as the account's own credential.
async fn s3(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> Answer {
    let host = format!("s3.{REGION}.amazonaws.com");
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("authorization", authorization("s3"));
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder
        .body(Bytes::from(body.to_owned()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    Answer {
        status: parts.status.as_u16(),
        headers: parts.headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    }
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

/// The ARN a role of this account carries, which a key policy statement names.
fn role_arn(role: Role) -> String {
    format!("arn:aws:iam::{ACCOUNT}:role/{}", role.name())
}

/// The session one probe's own calls are signed as: the identity every decision about them is taken
/// for, and the principal its refusals name. Two probes of the same role carry DIFFERENT sessions of
/// it, which is what a statement about that role has to reach.
fn session_arn(probe: &Probe) -> String {
    format!(
        "arn:aws:sts::{ACCOUNT}:assumed-role/{}/{}",
        probe.role.name(),
        probe.function
    )
}

/// The document the key carries under one row: the account delegation every key policy starts from,
/// and the deny statement naming the row's role, if any.
fn key_document(row: &Row) -> String {
    let mut statements = vec![json!({
        "Effect": "Allow",
        "Principal": {"AWS": format!("arn:aws:iam::{ACCOUNT}:root")},
        "Action": EVERY_KMS_ACTION,
        "Resource": "*",
    })];
    if let Some(role) = row.denied {
        statements.push(json!({
            "Effect": "Deny",
            "Principal": {"AWS": role_arn(role)},
            "Action": DECRYPT_ACTION,
            "Resource": "*",
        }));
    }
    json!({"Version": "2012-10-17", "Statement": statements}).to_string()
}

/// The sentence KMS words a refusal by an explicit deny in a key policy in, written out here rather
/// than built from either service's own helper so the wording has something to be wrong about: who
/// was refused, which action, on which key, and the clause naming the document that decided it.
fn refusal(session_arn: &str, key_arn: &str) -> String {
    format!(
        "User: {session_arn} is not authorized to perform: {DECRYPT_ACTION} on resource: \
         {key_arn} with an explicit deny in a resource-based policy"
    )
}

/// Create the one symmetric encryption key both objects are sealed under, carrying the document KMS
/// writes for a new key, as `(key id, key ARN)`.
async fn create_key(router: &MultiServiceRouter) -> (String, String) {
    let (status, answered) = kms(
        router,
        "CreateKey",
        &json!({"Description": "the key both objects of this suite are sealed under"}),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateKey must succeed, got {status} with {answered}"
    );
    let metadata = &answered["KeyMetadata"];
    let id = metadata["KeyId"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: CreateKey must answer the key's KeyId, got {answered}"));
    let arn = metadata["Arn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: CreateKey must answer the key's Arn, got {answered}"));
    (id.to_string(), arn.to_string())
}

/// Replace the key's own document — the call one row's decision is put in force with.
async fn put_key_policy(router: &MultiServiceRouter, case: &str, key_arn: &str, document: &str) {
    let (status, answered) = kms(
        router,
        "PutKeyPolicy",
        &json!({
            "KeyId": key_arn,
            "PolicyName": KEY_POLICY_NAME,
            "Policy": document,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutKeyPolicy({key_arn:?}) must succeed, got {status} with {answered}"
    );
}

/// The trust policy an execution role carries: Lambda has to be able to assume it for a function to
/// be handed it at all.
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

/// The inline document BOTH roles carry: the log actions every execution role needs, the read of
/// this bucket's objects, and the decrypt on the key by ARN — identical for the two, so the roles
/// differ in nothing but their identity and what decides a read is the key's own document.
fn reader_document(key_arn: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [
            {
                "Effect": "Allow",
                "Action": ["logs:CreateLogGroup", "logs:CreateLogStream", "logs:PutLogEvents"],
                "Resource": "*",
            },
            {
                "Effect": "Allow",
                "Action": READ_ACTION,
                "Resource": format!("arn:aws:s3:::{BUCKET}/*"),
            },
            {"Effect": "Allow", "Action": DECRYPT_ACTION, "Resource": key_arn},
        ],
    })
    .to_string()
}

/// Create one role with the Lambda trust policy and `document` as its one inline policy, and answer
/// the ARN IAM minted for it.
async fn create_role(router: &MultiServiceRouter, role: Role, document: &str) -> String {
    let name = role.name();
    let (status, created) = query(
        router,
        "iam",
        IAM_API_VERSION,
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
    let arn = opt_element(&created, "Arn").unwrap_or_else(|| {
        panic!("setup: CreateRole({name:?}) must answer the role's ARN, got {created}")
    });
    let (status, embedded) = query(
        router,
        "iam",
        IAM_API_VERSION,
        "PutRolePolicy",
        &[
            ("RoleName", name),
            ("PolicyName", "probe-grants"),
            ("PolicyDocument", document),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: PutRolePolicy({name:?}) must succeed, got {status} with {embedded}"
    );
    arn
}

/// Create the bucket both objects are stored in.
async fn create_bucket(router: &MultiServiceRouter) {
    let created = s3(router, "PUT", &format!("/{BUCKET}"), &[], "").await;
    assert_eq!(
        created.status, 200,
        "setup: CreateBucket({BUCKET:?}) must succeed, got {} with {}",
        created.status, created.body
    );
}

/// Store one object with SSE-KMS naming the key by the reference its row states, and assert the
/// write reported it sealed under that key's ARN whichever reference was named: the reads under test
/// are of objects this write has already proved are sealed under the key the rows' documents govern.
async fn put_object(router: &MultiServiceRouter, object: &Sealed, key: &(String, String)) {
    let (id, arn) = key;
    let named = match object.named {
        Named::ByArn => arn.as_str(),
        Named::ById => id.as_str(),
    };
    let written = s3(
        router,
        "PUT",
        &format!("/{BUCKET}/{}", object.object_key),
        &[
            ("content-type", "text/plain"),
            (SSE_HEADER, SSE_KMS),
            (SSE_KEY_HEADER, named),
        ],
        object.body,
    )
    .await;
    assert_eq!(
        written.status, 200,
        "setup {:?}: PutObject({:?}) must succeed, got {} with {}",
        object.label, object.object_key, written.status, written.body
    );
    assert_eq!(
        (
            written.header(SSE_HEADER).as_deref(),
            written.header(SSE_KEY_HEADER).as_deref(),
        ),
        (Some(SSE_KMS), Some(arn.as_str())),
        "setup {:?}: a write naming the key {named:?} must report the object sealed under \
         {arn:?} with {SSE_KMS}, got {:?} and {:?}",
        object.label,
        written.header(SSE_HEADER),
        written.header(SSE_KEY_HEADER)
    );
}

/// The stack declaring one function per probe, each on its own probe's role: the inline source is
/// archived into the deployment package by CloudFormation, which is how a function whose handler is
/// written here comes to exist at all.
fn function_stack(roles: &BTreeMap<Role, String>) -> String {
    let mut resources = serde_json::Map::new();
    for (index, probe) in PROBES.iter().enumerate() {
        resources.insert(
            format!("Probe{index}"),
            json!({
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": probe.function,
                    "Runtime": RUNTIME,
                    "Handler": HANDLER,
                    "Role": roles[&probe.role],
                    "Timeout": FUNCTION_TIMEOUT,
                    "Environment": {"Variables": {BUCKET_VARIABLE: BUCKET}},
                    "Code": {"ZipFile": PROBE_HANDLER},
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

/// Deploy the probes' stack, wait for `CREATE_COMPLETE`, and wait until Lambda reports every
/// function `Active`.
async fn deploy_functions(router: &MultiServiceRouter, roles: &BTreeMap<Role, String>) {
    let template = function_stack(roles);
    let (status, answered) = query(
        router,
        "cloudformation",
        CLOUDFORMATION_API_VERSION,
        "CreateStack",
        &[("StackName", STACK), ("TemplateBody", &template)],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateStack({STACK:?}) must be accepted, got {status} with {answered}"
    );
    // The stack's completion is owed from the instant Lambda reports its functions created, which no
    // fixed sleep can name: this looks until it reports it, on the shared wait whose deadline is
    // generous enough that a loaded host does not decide the outcome.
    let reported = wait_for_stack_status(router, STACK, CREATE_COMPLETE).await;
    let failures = if reported.0 == CREATE_COMPLETE {
        Vec::new()
    } else {
        let (_, events) = query(
            router,
            "cloudformation",
            CLOUDFORMATION_API_VERSION,
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
    for probe in PROBES {
        wait_until_active(router, probe.function).await;
    }
}

/// Wait until Lambda reports one function `Active`, which is the state an invocation needs.
async fn wait_until_active(router: &MultiServiceRouter, function: &str) {
    let mut last = Value::Null;
    for _ in 0..LOOKS {
        let (_, _, configuration) = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{function}/configuration"),
            &[],
        )
        .await;
        if configuration["State"].as_str() == Some(FUNCTION_ACTIVE) {
            return;
        }
        last = configuration;
        tokio::time::sleep(LOOK).await;
    }
    panic!("setup: Lambda must report {function:?} {FUNCTION_ACTIVE}, last reported {last}");
}

/// Invoke one probe over both objects, and answer the per-object report its handler returned —
/// asserting on the way that the invocation itself was served, that the run completed and that its
/// calls spoke for that function's OWN session of its role: a refused read is a value in the
/// payload, never a failed invocation.
async fn invoke(router: &MultiServiceRouter, case: &str, probe: &Probe) -> Value {
    let reads: BTreeMap<&str, &str> = OBJECTS
        .iter()
        .map(|object| (object.label, object.object_key))
        .collect();
    let (status, headers, payload) = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{}/invocations", probe.function),
        json!({"objects": reads}).to_string().as_bytes(),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}, {:?}: the invocation must be served, got {status} with {payload}",
        probe.case
    );
    let faulted = headers
        .get("x-amz-function-error")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    assert_eq!(
        faulted, None,
        "case {case:?}, {:?}: the handler must run to completion, got {faulted:?} with {payload}",
        probe.case
    );
    assert_eq!(
        payload.get("errorType"),
        None,
        "case {case:?}, {:?}: the answer must be the handler's own report rather than an error \
         document, got {payload}",
        probe.case
    );
    assert_eq!(
        payload["identity"].as_str(),
        Some(session_arn(probe).as_str()),
        "case {case:?}, {:?}: the handler's calls must be signed as this function's own session of \
         its role, got {payload}",
        probe.case
    );
    payload["reads"].clone()
}

/// What one probe's read of one object must report under one row: the bytes it holds, or the refusal
/// the key document's explicit deny of THIS session's role was worded in — with no bytes at all.
fn expected(row: &Row, probe: &Probe, object: &Sealed, key_arn: &str) -> Value {
    if row.denied != Some(probe.role) {
        return json!({"body": object.body});
    }
    json!({
        "code": ACCESS_DENIED,
        "message": refusal(&session_arn(probe), key_arn),
        "status": ACCESS_DENIED_STATUS,
    })
}

/// RULE: an explicit deny of the decrypt in a key's own document is about the ROLE it names — every
/// session of that role is refused the bytes of every object under that key, each refusal naming
/// that session, and a session of the other role is served the same objects from the same key under
/// the same document — and lifting the deny serves the very next invocation.
#[tokio::test]
async fn a_key_policy_deny_reaches_every_session_of_the_role_it_names() {
    let router = build_router();
    let key = create_key(&router).await;
    let document = reader_document(&key.1);
    let mut roles = BTreeMap::new();
    for role in Role::ALL {
        roles.insert(*role, create_role(&router, *role, &document).await);
    }
    create_bucket(&router).await;
    for object in OBJECTS {
        put_object(&router, object, &key).await;
    }
    deploy_functions(&router, &roles).await;

    for row in ROWS {
        let case = row.case;
        if row.put {
            put_key_policy(&router, case, &key.1, &key_document(row)).await;
        }
        for probe in PROBES {
            let reported = invoke(&router, case, probe).await;
            for object in OBJECTS {
                let label = object.label;
                let wanted = expected(row, probe, object, &key.1);
                assert_eq!(
                    reported[label], wanted,
                    "case {case:?}, {:?}, {label:?}: expected the read to report {wanted}, got {}",
                    probe.case, reported[label]
                );
                if wanted.get("body").is_none() {
                    assert_eq!(
                        reported[label].get("body"),
                        None,
                        "case {case:?}, {:?}, {label:?}: a refused read must carry no bytes at \
                         all, got {}",
                        probe.case,
                        reported[label]
                    );
                }
            }
        }
    }

    // Nothing of the rows changed what is stored: the account — a principal no document here names —
    // is served each object's exact bytes at the end.
    for object in OBJECTS {
        let held = s3(
            &router,
            "GET",
            &format!("/{BUCKET}/{}", object.object_key),
            &[],
            "",
        )
        .await;
        assert_eq!(
            (held.status, held.body.as_str()),
            (200, object.body),
            "case {:?}: the object must be exactly as it was planted, got {} with {:?}",
            object.label,
            held.status,
            held.body
        );
    }
}
