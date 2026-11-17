// SPDX-License-Identifier: CC0-1.0

//! An in-function Amazon S3 read of an SSE-KMS object, decided afresh against the key policy IN
//! FORCE at the moment of the read.
//!
//! RULE: serving the bytes of an object stored under a customer-managed key performs
//! `kms:Decrypt` on THAT key, so every read is decided again against the authorities in force
//! when it is made and against that key alone: while no document refuses the reader the read
//! answers the stored bytes, the moment the key's own policy carries an explicit deny of the
//! decrypt for the reading identity the very next read of an object under that key is refused
//! `AccessDenied` (HTTP 403) carrying nothing of the object, a read of an object under any OTHER
//! key is untouched by that deny, the object itself is unchanged (its metadata still reports the
//! key it was sealed under, and a principal the deny does not name is still served its exact
//! bytes), and replacing the denying document again serves the identical read the identical
//! bytes. A handler is where a client sees all of that: the invocation answers HTTP 200 with no
//! `X-Amz-Function-Error` and the handler's own report as the payload whichever way each read
//! went, so a refused read is a value in the payload and never a failed invocation.
//!
//! "The execution role grants the function permission to use Amazon Web Services services"
//! (`FunctionConfiguration$Role`), and the identity such a call speaks for is the session
//! `arn:aws:sts::<account>:assumed-role/<role>/<function>`; "if the object is encrypted with
//! SSE-KMS, you must also have the kms:GenerateDataKey and kms:Decrypt permissions in IAM
//! identity-based policies and KMS key policies for the KMS key" (`HeadObject`'s own
//! documentation of what such a read needs), a key policy naming the account's root user
//! "delegates the decision to that account's identity-based policies", and "an explicit deny in
//! any policy overrides any allows" (IAM User Guide, *Policy evaluation logic*). `StatusCode` is
//! the invocation's own HTTP status and `FunctionError` the `X-Amz-Function-Error` header
//! (`smithy_query(query="operation",operation="Invoke")`), so a run that completed carries
//! neither a non-200 nor that header.
//!
//! Six crates have to agree for that to hold — KMS holds each key policy and takes the decrypt
//! decision, Amazon S3 stores the object and asks for the decrypt on the reader's behalf, IAM
//! holds the role and its inline document, STS mints the session the handler's own calls are
//! signed as, CloudFormation archives the inline handler into a deployment package this emulator
//! can run, and Lambda runs it and supplies the SDK it reads with — so the suite lives here,
//! where a crate-local one could only ever see one side of it. What the neighbours pin and this
//! one does not repeat: `lambda_runtime_sdk_reads_an_sse_kms_object` tables WHICH documents reach
//! a function's session, each key carrying the document it was created with; the Amazon S3 crate's
//! `sse_kms_decrypt_authorization` tables the same decision for a caller presenting a credential
//! of its own; and `lambda_in_function_kms_grant_and_key_policy` drives a handler's OWN
//! `kms:Decrypt`. What only this suite reaches is the LIFECYCLE a client actually operates — a
//! readable object made unreadable by a later `PutKeyPolicy` and readable again by the next one —
//! and the SCOPE of such a deny, which is one key and one identity.
//!
//! The rows differ along what the rule generalises over: WHICH of the two keys carries the deny
//! (neither, the first, the second, both, neither again — so each key is observed denied while
//! the other serves, and both documents are observed restored), the FORM the deny names the
//! action in (the action itself, a list holding it beside another, and the service's whole
//! wildcard), WHICH key each object was sealed under, and the form of the object's own key (under
//! a dated prefix, and flat). Every phase is observed three ways: what the running handler
//! reports, what the object's metadata still says, and what a principal the deny does not name is
//! still served.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{elements, form_encode, opt_element, physical_ids_of};

/// The Region every request here is signed for, and so the Region the keys, the bucket, the role
/// and the function belong to. `us-east-1`, so a bucket create names no location constraint.
const REGION: &str = "us-east-1";

/// The account this emulator serves, which every ARN below carries.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The bucket every object of this suite is stored in.
const BUCKET: &str = "xsvc-live-key-policy-sse-kms";

/// The execution role the function runs as — the identity every key policy below decides about —
/// and the function itself, whose name completes the session ARN a refusal names.
const ROLE: &str = "xsvc-live-key-policy-reader";
const FUNCTION: &str = "xsvc-live-key-policy-reader-fn";

/// The stack that declares the function: `Code.ZipFile` in a template is the inline source
/// CloudFormation archives into a deployment package this emulator can run.
const STACK: &str = "xsvc-live-key-policy-probe";

/// The `StackStatus` that stack must reach, and the `State` a function serves invocations in.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const FUNCTION_ACTIVE: &str = "Active";

/// The runtime the function declares: a managed `python3.x` one, which carries the AWS SDK for
/// Python on the handler's import path (source:
/// `smithy_query(query="shape",shape="Runtime")`).
const RUNTIME: &str = "python3.12";

/// The module and attribute CloudFormation's inline source is archived under.
const HANDLER: &str = "index.handler";

/// The environment variable the handler reads the bucket's name from, which is how a function
/// shipped once is pointed at the bucket it serves.
const BUCKET_VARIABLE: &str = "BUCKET";

/// The code and status Amazon S3 refuses a read no authority in force permits with (Amazon S3
/// "Error responses": `AccessDenied`, HTTP 403).
const ACCESS_DENIED: &str = "AccessDenied";
const ACCESS_DENIED_STATUS: u64 = 403;

/// The action serving an SSE-KMS object's bytes performs on the key it was sealed under, the one
/// a write of such an object performs, and the read the function's role is granted on the bucket.
const DECRYPT_ACTION: &str = "kms:Decrypt";
const DATA_KEY_ACTION: &str = "kms:GenerateDataKey";
const READ_ACTION: &str = "s3:GetObject";

/// The `ServerSideEncryption` value that is SSE-KMS — the one a KMS key id belongs to
/// (source: `smithy_query(query="shape",shape="ServerSideEncryption")`) — and the two headers an
/// object's own encryption is reported in.
const SSE_KMS: &str = "aws:kms";
const SSE_HEADER: &str = "x-amz-server-side-encryption";
const SSE_KEY_HEADER: &str = "x-amz-server-side-encryption-aws-kms-key-id";

/// The one name a key holds a policy under.
const KEY_POLICY_NAME: &str = "default";

/// The API versions an awsQuery call to IAM and to CloudFormation names.
const IAM_API_VERSION: &str = "2010-05-08";
const CLOUDFORMATION_API_VERSION: &str = "2010-05-15";

/// How long one invocation is given, as the function's own `Timeout`: well past what a run needs
/// on any host, because what is under test is the decision and not how fast an interpreter gets
/// through the handler.
const FUNCTION_TIMEOUT: u64 = 120;

/// How often to look while waiting for work the clock owes — a created function reaching
/// `Active` — and how many looks to take. The span is Lambda's own, so no fixed sleep names the
/// instant it ends, and the deadline is generous enough that a loaded host cannot decide the
/// outcome.
const LOOK: Duration = Duration::from_millis(100);
const LOOKS: usize = 1_800;

/// The handler the function runs.
///
/// The SDK its managed runtime supplies is imported and its clients built at MODULE scope — where
/// a runtime that made no SDK importable would fault the LOAD rather than one call — the bucket
/// comes from the function's own environment, and each object the event names is read, reporting
/// either the bytes or Amazon S3's own refusal: its code, its sentence and the status it came back
/// on. A refused read reports NO bytes at all, which is what says nothing of the object reached a
/// caller the key refused.
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

/// WHICH of this suite's two customer-managed keys an object was sealed under: a deny is written
/// on ONE key's document, so two keys are what makes the scope of one observable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Key {
    First,
    Second,
}

impl Key {
    /// Every key this suite creates.
    const ALL: &'static [Self] = &[Self::First, Self::Second];
}

/// How one key's document names the action it DENIES the reading role, or that it denies nothing.
///
/// The forms are the ones a policy author writes the same deny in, so a deny that only bites in
/// one of them fails here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Deny {
    /// No deny at all: the document is the account delegation alone, which the role's own inline
    /// policy completes.
    Nothing,
    /// `"Action": "kms:Decrypt"` — the action the read performs, named on its own.
    TheAction,
    /// A list holding that action beside another of the service, which is how a document denies
    /// several at once.
    AListHoldingTheAction,
    /// `"Action": "kms:*"` — the service's whole wildcard, which covers the action.
    TheServiceWildcard,
}

impl Deny {
    /// Whether a read of an object under a key carrying this document is served.
    fn serves(self) -> bool {
        self == Self::Nothing
    }

    /// The `Action` element of the deny statement, or `None` where there is no such statement.
    fn actions(self) -> Option<Value> {
        match self {
            Self::Nothing => None,
            Self::TheAction => Some(json!(DECRYPT_ACTION)),
            Self::AListHoldingTheAction => Some(json!([DATA_KEY_ACTION, DECRYPT_ACTION])),
            Self::TheServiceWildcard => Some(json!("kms:*")),
        }
    }
}

/// One object of this suite: the key it was sealed under, the object key it is stored at and the
/// bytes it holds — distinct per object, so "the exact bytes this object holds" is something a
/// read can be wrong about.
struct Sealed {
    label: &'static str,
    object_key: &'static str,
    body: &'static str,
    sealed_under: Key,
}

const OBJECTS: &[Sealed] = &[
    Sealed {
        label: "the object under the first key",
        object_key: "reports/2026/first.txt",
        body: "region,amount\nus-east-1,17\n",
        sealed_under: Key::First,
    },
    Sealed {
        label: "the object under the second key",
        object_key: "second-flat",
        body: "the bytes the other key protects",
        sealed_under: Key::Second,
    },
];

/// One phase of the lifecycle: the document each key carries while the handler reads both objects.
///
/// The phases run in order on ONE bucket, ONE role and ONE function, because what a phase changes
/// is the documents in force and nothing else — which is exactly what the rule is about.
struct Phase {
    case: &'static str,
    first: Deny,
    second: Deny,
}

const PHASES: &[Phase] = &[
    Phase {
        case: "the documents both keys were created with",
        first: Deny::Nothing,
        second: Deny::Nothing,
    },
    Phase {
        case: "an explicit deny of the decrypt put on the first key",
        first: Deny::TheAction,
        second: Deny::Nothing,
    },
    Phase {
        case: "the deny lifted from the first key and put on the second, as a list of actions",
        first: Deny::Nothing,
        second: Deny::AListHoldingTheAction,
    },
    Phase {
        case: "a deny on both keys, one of them the service's whole wildcard",
        first: Deny::TheServiceWildcard,
        second: Deny::TheAction,
    },
    Phase {
        case: "both documents restored",
        first: Deny::Nothing,
        second: Deny::Nothing,
    },
];

impl Phase {
    /// The document in force on `key` during this phase.
    fn deny_on(&self, key: Key) -> Deny {
        match key {
            Key::First => self.first,
            Key::Second => self.second,
        }
    }
}

/// The emulator as it is ASSEMBLED, on one engine: the handler's own SDK call is dispatched back
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

/// What one call answered.
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
    for (name, value) in members {
        body.push('&');
        body.push_str(&form_encode(name));
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
fn role_arn(name: &str) -> String {
    format!("arn:aws:iam::{ACCOUNT}:role/{name}")
}

/// The session a running function's own calls are signed as, which is the identity every decision
/// about them is taken for and the principal a refusal names.
fn session_arn() -> String {
    format!("arn:aws:sts::{ACCOUNT}:assumed-role/{ROLE}/{FUNCTION}")
}

/// The document one key carries while `deny` is in force on it: the account delegation every key
/// policy starts from, and the deny statement the phase names, if any.
fn key_document(deny: Deny) -> String {
    let mut statements = vec![json!({
        "Effect": "Allow",
        "Principal": {"AWS": format!("arn:aws:iam::{ACCOUNT}:root")},
        "Action": "kms:*",
        "Resource": "*",
    })];
    if let Some(actions) = deny.actions() {
        statements.push(json!({
            "Effect": "Deny",
            "Principal": {"AWS": role_arn(ROLE)},
            "Action": actions,
            "Resource": "*",
        }));
    }
    json!({"Version": "2012-10-17", "Statement": statements}).to_string()
}

/// The sentence KMS words a refusal by an explicit deny in a key policy in, written out here
/// rather than built from either service's own helper so the wording has something to be wrong
/// about: who was refused, which action, on which key, and the clause naming the document that
/// decided it.
fn refusal(principal: &str, key_arn: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {DECRYPT_ACTION} on resource: \
         {key_arn} with an explicit deny in a resource-based policy"
    )
}

/// Create one symmetric encryption key per [`Key`], each carrying the document it starts the
/// lifecycle with, as `(key id, key ARN)`.
async fn create_keys(router: &MultiServiceRouter) -> BTreeMap<Key, (String, String)> {
    let mut created = BTreeMap::new();
    for which in Key::ALL {
        let (status, answered) = kms(
            router,
            "CreateKey",
            &json!({
                "Description": format!("{which:?}"),
                "Policy": key_document(Deny::Nothing),
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "setup {which:?}: CreateKey must succeed, got {status} with {answered}"
        );
        let metadata = &answered["KeyMetadata"];
        let id = metadata["KeyId"].as_str().unwrap_or_else(|| {
            panic!("setup {which:?}: CreateKey must answer the key's KeyId, got {answered}")
        });
        let arn = metadata["Arn"].as_str().unwrap_or_else(|| {
            panic!("setup {which:?}: CreateKey must answer the key's Arn, got {answered}")
        });
        created.insert(*which, (id.to_string(), arn.to_string()));
    }
    created
}

/// Replace one key's own document — the call a client makes a readable object unreadable, and
/// readable again, with.
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

/// The trust policy the execution role carries: Lambda has to be able to assume it for the
/// function to be handed it at all.
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

/// The inline document the reader role carries: the log actions every execution role needs, the
/// read of this bucket's objects, and the decrypt on BOTH keys by ARN — so the role's own half of
/// the decision is settled for every phase and what is left to decide a read is each key's own
/// document.
fn reader_document(key_arns: &[&str]) -> String {
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
            {"Effect": "Allow", "Action": DECRYPT_ACTION, "Resource": key_arns},
        ],
    })
    .to_string()
}

/// Create `ROLE` with the Lambda trust policy and `document` as its one inline policy, and answer
/// the ARN IAM minted for it.
async fn create_role(router: &MultiServiceRouter, document: &str) -> String {
    let (status, created) = query(
        router,
        "iam",
        IAM_API_VERSION,
        "CreateRole",
        &[
            ("RoleName", ROLE),
            ("AssumeRolePolicyDocument", &trust_policy()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateRole({ROLE:?}) must succeed, got {status} with {created}"
    );
    let arn = opt_element(&created, "Arn").unwrap_or_else(|| {
        panic!("setup: CreateRole({ROLE:?}) must answer the role's ARN, got {created}")
    });
    let (status, embedded) = query(
        router,
        "iam",
        IAM_API_VERSION,
        "PutRolePolicy",
        &[
            ("RoleName", ROLE),
            ("PolicyName", "probe-grants"),
            ("PolicyDocument", document),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: PutRolePolicy({ROLE:?}) must succeed, got {status} with {embedded}"
    );
    arn
}

/// Create the bucket every object is stored in.
async fn create_bucket(router: &MultiServiceRouter) {
    let created = s3(router, "PUT", &format!("/{BUCKET}"), &[], "").await;
    assert_eq!(
        created.status, 200,
        "setup: CreateBucket({BUCKET:?}) must succeed, got {} with {}",
        created.status, created.body
    );
}

/// Store one object under its own key with SSE-KMS naming that key by the ARN KMS minted, and
/// assert the write reported it sealed under exactly that key: the read under test is of an object
/// this write has already proved is sealed under the key whose document the phases change.
async fn put_object(
    router: &MultiServiceRouter,
    object: &Sealed,
    keys: &BTreeMap<Key, (String, String)>,
) {
    let key_arn = keys[&object.sealed_under].1.clone();
    let written = s3(
        router,
        "PUT",
        &format!("/{BUCKET}/{}", object.object_key),
        &[
            ("content-type", "text/plain"),
            (SSE_HEADER, SSE_KMS),
            (SSE_KEY_HEADER, &key_arn),
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
        (Some(SSE_KMS), Some(key_arn.as_str())),
        "setup {:?}: the write must report the object sealed under {key_arn:?} with {SSE_KMS}, \
         got {:?} and {:?}",
        object.label,
        written.header(SSE_HEADER),
        written.header(SSE_KEY_HEADER)
    );
}

/// The stack declaring the one function every phase invokes: the inline source is archived into
/// the deployment package by CloudFormation, which is how a function whose handler is written here
/// comes to exist at all.
fn function_stack(role: &str) -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Probe": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": FUNCTION,
                    "Runtime": RUNTIME,
                    "Handler": HANDLER,
                    "Role": role,
                    "Timeout": FUNCTION_TIMEOUT,
                    "Environment": {"Variables": {BUCKET_VARIABLE: BUCKET}},
                    "Code": {"ZipFile": PROBE_HANDLER},
                },
            },
        },
    })
    .to_string()
}

/// Deploy the probe's stack, wait for `CREATE_COMPLETE` and answer the physical id of the function
/// it declared.
async fn deploy_function(router: &MultiServiceRouter, role: &str) -> String {
    let template = function_stack(role);
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
    // The stack's completion is owed from the instant Lambda reports its function created, which
    // no fixed sleep can name: this looks until it reports it, on the shared wait whose deadline is
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
    let (status, resources) = query(
        router,
        "cloudformation",
        CLOUDFORMATION_API_VERSION,
        "DescribeStackResources",
        &[("StackName", STACK)],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: DescribeStackResources({STACK:?}) must succeed, got {status} with {resources}"
    );
    physical_ids_of(&resources)
        .get("Probe")
        .cloned()
        .unwrap_or_default()
}

/// Wait until Lambda reports the function `Active`, which is the state an invocation needs.
async fn wait_until_active(router: &MultiServiceRouter, name: &str) {
    let mut last = Value::Null;
    for _ in 0..LOOKS {
        let (_, _, configuration) = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{name}/configuration"),
            &[],
        )
        .await;
        if configuration["State"].as_str() == Some(FUNCTION_ACTIVE) {
            return;
        }
        last = configuration;
        tokio::time::sleep(LOOK).await;
    }
    panic!("setup: Lambda must report {name:?} {FUNCTION_ACTIVE}, last reported {last}");
}

/// Invoke the function over every object, and answer the per-object report its handler returned —
/// asserting on the way that the invocation itself was served and the run completed: a refused
/// read is a value in the payload, never a failed invocation.
async fn invoke(router: &MultiServiceRouter, case: &str) -> Value {
    let reads: BTreeMap<&str, &str> = OBJECTS
        .iter()
        .map(|object| (object.label, object.object_key))
        .collect();
    let (status, headers, payload) = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{FUNCTION}/invocations"),
        json!({"objects": reads}).to_string().as_bytes(),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the invocation must be served, got {status} with {payload}"
    );
    let faulted = headers
        .get("x-amz-function-error")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    assert_eq!(
        faulted, None,
        "case {case:?}: the handler must run to completion, got {faulted:?} with {payload}"
    );
    assert_eq!(
        payload.get("errorType"),
        None,
        "case {case:?}: the answer must be the handler's own report rather than an error \
         document, got {payload}"
    );
    assert_eq!(
        payload["identity"].as_str(),
        Some(session_arn().as_str()),
        "case {case:?}: the handler's calls must be signed as the execution role's own session, \
         got {payload}"
    );
    payload["reads"].clone()
}

/// What one object's read must report in one phase: the bytes it holds, or the refusal the key's
/// explicit deny was worded in — with no bytes at all.
fn expected(phase: &Phase, object: &Sealed, keys: &BTreeMap<Key, (String, String)>) -> Value {
    if phase.deny_on(object.sealed_under).serves() {
        return json!({"body": object.body});
    }
    json!({
        "code": ACCESS_DENIED,
        "message": refusal(&session_arn(), &keys[&object.sealed_under].1),
        "status": ACCESS_DENIED_STATUS,
    })
}

/// Assert the object is exactly as it was planted, whichever way the handler's read went: its
/// metadata still reports the key it was sealed under, and the account — a principal no deny here
/// names — is still served its exact bytes.
async fn assert_object_intact(
    router: &MultiServiceRouter,
    case: &str,
    object: &Sealed,
    keys: &BTreeMap<Key, (String, String)>,
) {
    let key_arn = keys[&object.sealed_under].1.clone();
    let described = s3(
        router,
        "HEAD",
        &format!("/{BUCKET}/{}", object.object_key),
        &[],
        "",
    )
    .await;
    assert_eq!(
        (
            described.status,
            described.header(SSE_HEADER).as_deref(),
            described.header(SSE_KEY_HEADER).as_deref(),
        ),
        (200, Some(SSE_KMS), Some(key_arn.as_str())),
        "case {case:?}, {:?}: HeadObject must still report the object sealed under {key_arn:?}, \
         got {} with {:?} and {:?}",
        object.label,
        described.status,
        described.header(SSE_HEADER),
        described.header(SSE_KEY_HEADER)
    );
    let held = s3(
        router,
        "GET",
        &format!("/{BUCKET}/{}", object.object_key),
        &[],
        "",
    )
    .await;
    assert_eq!(
        (held.status, held.body.as_str()),
        (200, object.body),
        "case {case:?}, {:?}: a principal the deny does not name must still be served the stored \
         bytes, got {} with {:?}",
        object.label,
        held.status,
        held.body
    );
}

/// Every phase: what the running handler is served depends on the document each key carries AT THE
/// TIME OF THE READ and on nothing else — not on the document the key was created with, not on an
/// earlier read of the same object, and not on the other key's document.
#[tokio::test]
async fn an_in_function_sse_kms_read_follows_each_keys_document_as_it_stands() {
    let router = build_router();
    let keys = create_keys(&router).await;
    let key_arns: Vec<&str> = Key::ALL.iter().map(|which| keys[which].1.as_str()).collect();
    let role = create_role(&router, &reader_document(&key_arns)).await;
    create_bucket(&router).await;
    for object in OBJECTS {
        put_object(&router, object, &keys).await;
    }

    let deployed = deploy_function(&router, &role).await;
    assert_eq!(
        deployed, FUNCTION,
        "setup: the stack must report its function as {FUNCTION:?}, got {deployed:?}"
    );
    wait_until_active(&router, &deployed).await;

    for phase in PHASES {
        for which in Key::ALL {
            put_key_policy(
                &router,
                phase.case,
                &keys[which].1,
                &key_document(phase.deny_on(*which)),
            )
            .await;
        }
        let reported = invoke(&router, phase.case).await;
        for object in OBJECTS {
            let wanted = expected(phase, object, &keys);
            assert_eq!(
                reported[object.label], wanted,
                "case {:?}, {:?}: expected the handler to report {wanted}, got {}",
                phase.case, object.label, reported[object.label]
            );
            assert_object_intact(&router, phase.case, object, &keys).await;
        }
    }
}
