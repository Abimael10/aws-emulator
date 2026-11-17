// SPDX-License-Identifier: CC0-1.0

//! The AWS SDK a managed `python3.x` runtime puts on a handler's import path, used to read an
//! SSE-KMS object whose key policy decides whether the function's role may have its plaintext.
//!
//! RULE: AWS's managed `python3.x` runtimes carry the AWS SDK for Python in the execution
//! environment, so a handler that imports `boto3` or `botocore` AT MODULE SCOPE loads without
//! the deployment package shipping either — the invocation answers HTTP 200 with no
//! `X-Amz-Function-Error` header and the handler's own return value as the payload, never a
//! `Runtime.ImportModuleError` for one of those modules — and the call that SDK makes is signed
//! as the session of the function's own execution role, so every authorization decision about it
//! is taken for that session. The decision this suite is about is the one a read of an SSE-KMS
//! object needs beside the bucket's: serving such an object's bytes performs `kms:Decrypt` on the
//! customer-managed key it was stored under, so the key's own policy decides it — a policy that
//! reaches that session serves the stored bytes, and one that does not refuses the read with
//! `AccessDenied` (HTTP 403) carrying KMS's own sentence for WHY (an explicit deny, no
//! resource-based policy, or an account delegation the session's identity policies do not
//! complete), which the handler reports in its own payload. An object stored under encryption
//! that names no customer-managed key asks KMS nothing and is served either way, and a key
//! policy put after a refusal is live: the very next invocation is served.
//!
//! "The execution role grants the function permission to use Amazon Web Services services"
//! (`FunctionConfiguration$Role`), and the identity such a call speaks for is the session
//! `arn:aws:sts::<account>:assumed-role/<role>/<function>`; "if the object is encrypted with
//! SSE-KMS, you must also have the kms:GenerateDataKey and kms:Decrypt permissions in IAM
//! identity-based policies and KMS key policies for the KMS key" (`HeadObject`'s own
//! documentation of what such a read needs), and "an explicit deny in any policy overrides any
//! allows" (IAM User Guide, *Policy evaluation logic*).
//!
//! Five crates have to agree for that to hold — Lambda runs the handler and supplies it the SDK,
//! CloudFormation archives the inline handler into a deployment package this emulator can run,
//! IAM holds the roles and their documents, STS mints the session the function's calls are signed
//! with, Amazon S3 stores the object and asks for the decrypt, and KMS holds each key policy and
//! takes the decision — so the suite lives here: a crate-local suite can only ever see one side of
//! it. What the neighbours pin and this one does not repeat: the S3 crate's
//! `sse_kms_decrypt_authorization` tables the same key-policy decision for a caller presenting a
//! credential of its own, `cognito_vended_session_reads_sse_kms_object` drives it for a vended
//! session, and `lambda_runtime_sdk_reads_a_cmk_encrypted_parameter` drives the runtime's SDK
//! against a sealed Systems Manager parameter. What only this suite reaches is an SSE-KMS OBJECT
//! read by a RUNNING function with the SDK its runtime supplies, which is where a missing SDK
//! shows up as a fault attributed to the caller's own code instead of the key's decision.
//!
//! The rows differ along what the rule generalises over:
//!
//! * the `python3.x` runtime the function declares (three of the `Runtime` enum's `python3.x`
//!   values, source: `smithy_query(query="shape",shape="Runtime")`), because the SDK belongs to
//!   every managed Python runtime rather than to one point release;
//! * the FORM the SDK is imported in — `import boto3`, a from-import of the session class
//!   (`from boto3.session import Session`), and botocore's own session (`import
//!   botocore.session`) — each reporting the module names its imports resolved to;
//! * what the object's key policy says about the session: an explicit deny of the decrypt, an
//!   allow of it, an allow confined to this service's own path to the key (`kms:ViaService`), a
//!   document delegating to the account whose identity policies do not reach the action, and one
//!   whose only decrypt is another role's;
//! * how the object was STORED — SSE-KMS naming the key by the ARN KMS minted, SSE-KMS naming it
//!   by its bare key id, a write naming no encryption at all (which inherits the bucket's own
//!   SSE-KMS default), and SSE-S3, which names no customer-managed key;
//! * the form of the object's key — under a dated prefix, deep under a prefix, and flat.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{elements, form_encode, opt_element, physical_ids_of};

/// The Region every request here is signed for, and so the Region the keys, the bucket and the
/// functions belong to.
const REGION: &str = "us-east-1";

/// The account this emulator serves, which every ARN below carries.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The bucket every object of this suite is stored in, and the bytes each one holds: a key
/// policy is about who may have an object's plaintext, not about what is in it, so one payload
/// serves every row.
const BUCKET: &str = "xsvc-in-fn-sse-kms-objects";
const BODY: &str = "the bytes the object's own key protects";

/// The execution role every probe function runs as — one role, so a key policy naming it governs
/// the sessions of each function — and a second role that no function runs as, which exists so a
/// key policy can name a real principal of this account that is not the reader.
const ROLE: &str = "xsvc-in-fn-sse-kms-reader";
const OTHER_ROLE: &str = "xsvc-in-fn-sse-kms-other";

/// The stack that declares one function per probe.
const STACK: &str = "xsvc-in-fn-sse-kms-probes";

/// The `StackStatus` that stack must reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The code and status S3 refuses a read no authority in force permits with (Amazon S3 "Error
/// responses": `AccessDenied`, HTTP 403).
const ACCESS_DENIED: &str = "AccessDenied";
const ACCESS_DENIED_STATUS: u64 = 403;

/// The IAM action a read that serves an SSE-KMS object's bytes is authorized under, and the one
/// the function's own role is granted on the bucket.
const DECRYPT_ACTION: &str = "kms:Decrypt";
const READ_ACTION: &str = "s3:GetObject";

/// The `ServerSideEncryption` value that is SSE-KMS, the one a KMS key id belongs to, and the one
/// that is SSE-S3 — "server-side encryption with Amazon S3 managed keys", which names no KMS key
/// of this account (`smithy_query(query="shape",shape="ServerSideEncryption")`).
const SSE_KMS: &str = "aws:kms";
const SSE_S3: &str = "AES256";

/// How long one invocation is given, as the function's own `Timeout`: well past what a run needs
/// on any host, because what is under test is the SDK and the decision rather than how fast an
/// interpreter gets through the handler.
const FUNCTION_TIMEOUT: u64 = 120;

/// How often to look while waiting for work the clock owes — a created function reaching `Active`
/// — and how many looks to take. The span is that service's own, so no fixed sleep names the
/// instant it ends, and the deadline is generous so a loaded host cannot decide the outcome.
const LOOK: Duration = Duration::from_millis(100);
const LOOKS: usize = 1_800;

/// What every handler below does once it holds a client, whichever form built it: read each
/// object the event names and report either the bytes and the encryption the read reported, or
/// the refusal it raised — its code, its sentence and the HTTP status it came back on. `IMPORTED`
/// is what the module-scope imports of the head above resolved to, reported on every answer: an
/// import that did not resolve would have faulted the load instead.
const HANDLER_TAIL: &str = r#"

def handler(event, context):
    objects = _client("s3")
    answered = {
        "imported": IMPORTED,
        "identity": _client("sts").get_caller_identity()["Arn"],
        "reads": {},
    }
    for label, key in event["objects"].items():
        try:
            read = objects.get_object(Bucket=event["bucket"], Key=key)
        except ClientError as refused:
            answered["reads"][label] = {
                "read": False,
                "code": refused.response["Error"]["Code"],
                "message": refused.response["Error"]["Message"],
                "status": refused.response["ResponseMetadata"]["HTTPStatusCode"],
            }
            continue
        answered["reads"][label] = {
            "read": True,
            "body": read["Body"].read().decode("utf-8"),
        }
    return answered
"#;

/// WHICH form a handler imports the SDK in: the module itself, the session class, or botocore's
/// own session — every one of them a module the RUNTIME supplies.
#[derive(Debug, Clone, Copy)]
enum Form {
    /// `import boto3`, the form AWS's own examples use.
    Boto3Module,
    /// A from-import of the session class, which is what an application building its own session
    /// imports.
    Boto3Session,
    /// botocore's own session, the lower half of the SDK the upper half is built on.
    BotocoreSession,
}

impl Form {
    /// The module-scope head of the handler: the imports, the module names they resolved to, and
    /// the client builder [`HANDLER_TAIL`] calls.
    fn head(self) -> &'static str {
        match self {
            Self::Boto3Module => {
                r#"
import boto3
import botocore
from botocore.exceptions import ClientError

IMPORTED = [boto3.__name__, botocore.__name__, ClientError.__module__]


def _client(service):
    return boto3.client(service)
"#
            }
            Self::Boto3Session => {
                r#"
from boto3.session import Session
from botocore.exceptions import ClientError

IMPORTED = [Session.__module__, ClientError.__module__]


def _client(service):
    return Session().client(service)
"#
            }
            Self::BotocoreSession => {
                r#"
import botocore.session
from botocore.exceptions import ClientError

IMPORTED = [botocore.session.__name__, ClientError.__module__]


def _client(service):
    return botocore.session.get_session().create_client(service)
"#
            }
        }
    }

    /// The module names the head's imports must resolve to, which the answer reports: the runtime
    /// supplies these modules, so each name is the dotted path of a module of the SDK and not of
    /// anything the deployment package shipped.
    fn imported(self) -> Vec<&'static str> {
        match self {
            Self::Boto3Module => vec!["boto3", "botocore", "botocore.exceptions"],
            Self::Boto3Session => vec!["boto3.session", "botocore.exceptions"],
            Self::BotocoreSession => vec!["botocore.session", "botocore.exceptions"],
        }
    }

    /// The whole source of a probe's handler.
    fn source(self) -> String {
        format!("{}{HANDLER_TAIL}", self.head())
    }
}

/// One probe: a function of its own, with the `python3.x` runtime it declares and the form its
/// handler imports the SDK in. Every probe runs as the same execution role, so what differs
/// between them is the runtime and the import, and not the identity its calls are decided for.
struct Probe {
    case: &'static str,
    runtime: &'static str,
    form: Form,
}

const PROBES: &[Probe] = &[
    Probe {
        case: "a module-scope `import boto3`",
        runtime: "python3.12",
        form: Form::Boto3Module,
    },
    Probe {
        case: "a from-imported session class",
        runtime: "python3.13",
        form: Form::Boto3Session,
    },
    Probe {
        case: "botocore's own session",
        runtime: "python3.11",
        form: Form::BotocoreSession,
    },
];

/// What one key's own policy says about the decrypt, which is what decides a read of the objects
/// stored under it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum KeyPolicy {
    /// The account's root statement alone — the document KMS writes for a key whose create names
    /// none, which delegates the decision to the calling principal's own identity-based policies.
    /// The reader role's document reaches no KMS action, so that delegation completes for nobody.
    Delegated,
    /// The account's root statement and an explicit `Deny` of the decrypt to the reader role.
    DenyingTheRole,
    /// The account's root statement and an `Allow` of the decrypt to the reader role.
    GrantingTheRole,
    /// The same allow, confined to THIS service's path to the key (`kms:ViaService`), which is
    /// the path S3 reaches a key over when it serves an object's bytes.
    GrantingViaThisService,
    /// An `Allow` of the decrypt to ANOTHER role, beside the account reaching the data-key
    /// action a WRITE under the key performs and nothing else — so no statement of it reaches the
    /// reader role or its account for the decrypt.
    AnotherRoleOnly,
}

impl KeyPolicy {
    /// Whether a session of the reader role is served a read of an object under this key.
    fn serves_the_role(self) -> bool {
        matches!(self, Self::GrantingTheRole | Self::GrantingViaThisService)
    }

    /// The sentence KMS words a refusal under this document in, written out here rather than
    /// built from either service's own helper so the wording has something to be wrong about:
    /// who was refused, which action, on which key, and the clause naming the document that
    /// decided it.
    fn refusal(self, principal: &str, key_arn: &str) -> String {
        let head = format!(
            "User: {principal} is not authorized to perform: {DECRYPT_ACTION} on resource: \
             {key_arn}"
        );
        match self {
            Self::DenyingTheRole => {
                format!("{head} with an explicit deny in a resource-based policy")
            }
            Self::Delegated => {
                format!("{head} because no identity-based policy allows the {DECRYPT_ACTION} action")
            }
            Self::AnotherRoleOnly => {
                format!("{head} because no resource-based policy allows the {DECRYPT_ACTION} action")
            }
            Self::GrantingTheRole | Self::GrantingViaThisService => {
                panic!("{self:?} serves the read, so it words no refusal")
            }
        }
    }
}

/// Every key this suite creates, one per document under test.
const KEYS: &[KeyPolicy] = &[
    KeyPolicy::Delegated,
    KeyPolicy::DenyingTheRole,
    KeyPolicy::GrantingTheRole,
    KeyPolicy::GrantingViaThisService,
    KeyPolicy::AnotherRoleOnly,
];

/// Which key the bucket's own default encryption names, so an object written with no encryption
/// member at all is stored under a customer-managed key all the same.
const BUCKET_DEFAULT_KEY: KeyPolicy = KeyPolicy::GrantingTheRole;

/// How one object was STORED, which decides what a read of it has to ask KMS.
#[derive(Debug, Clone, Copy)]
enum Stored {
    /// SSE-KMS naming the key by the ARN KMS minted for it.
    KmsByArn(KeyPolicy),
    /// SSE-KMS naming the key by its bare key id.
    KmsById(KeyPolicy),
    /// The write names no encryption at all, so the object inherits the bucket's own default —
    /// SSE-KMS under [`BUCKET_DEFAULT_KEY`].
    BucketDefault,
    /// SSE-S3: encryption that names no customer-managed key, so serving the bytes asks KMS
    /// nothing.
    SseS3,
}

impl Stored {
    /// The key whose policy decides a read of this object, or `None` where no customer-managed
    /// key is involved at all.
    fn key(self) -> Option<KeyPolicy> {
        match self {
            Self::KmsByArn(key) | Self::KmsById(key) => Some(key),
            Self::BucketDefault => Some(BUCKET_DEFAULT_KEY),
            Self::SseS3 => None,
        }
    }

    /// The `ServerSideEncryption` the write must report for the stored object.
    fn algorithm(self) -> &'static str {
        match self {
            Self::KmsByArn(_) | Self::KmsById(_) | Self::BucketDefault => SSE_KMS,
            Self::SseS3 => SSE_S3,
        }
    }
}

/// One row: an object stored one way under one key policy, read by one probe.
struct Row {
    case: &'static str,
    /// The object's own key, so one row's policy cannot decide another's read.
    object_key: &'static str,
    stored: Stored,
    /// Which of [`PROBES`] reads it, by index.
    probe: usize,
}

const ROWS: &[Row] = &[
    Row {
        case: "a key policy that explicitly denies the function's role",
        object_key: "reports/2026/denied.txt",
        stored: Stored::KmsByArn(KeyPolicy::DenyingTheRole),
        probe: 0,
    },
    Row {
        case: "a write naming no encryption, inheriting the bucket's SSE-KMS default",
        object_key: "reports/2026/inherited.txt",
        stored: Stored::BucketDefault,
        probe: 0,
    },
    Row {
        case: "SSE-S3, which names no customer-managed key",
        object_key: "flat-sse-s3",
        stored: Stored::SseS3,
        probe: 0,
    },
    Row {
        case: "a key policy delegating to the account, whose identity policies omit the decrypt",
        object_key: "flat-delegated",
        stored: Stored::KmsById(KeyPolicy::Delegated),
        probe: 1,
    },
    Row {
        case: "a key policy naming the function's own role",
        object_key: "granted/by-arn.txt",
        stored: Stored::KmsByArn(KeyPolicy::GrantingTheRole),
        probe: 1,
    },
    Row {
        case: "a key policy naming only another role",
        object_key: "deep/under/a/prefix/another-role.txt",
        stored: Stored::KmsByArn(KeyPolicy::AnotherRoleOnly),
        probe: 2,
    },
    Row {
        case: "a grant confined to this service's path to the key",
        object_key: "granted/via-service",
        stored: Stored::KmsById(KeyPolicy::GrantingViaThisService),
        probe: 2,
    },
];

/// The emulator as it is ASSEMBLED, on one engine: a handler's own SDK call is dispatched back
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

/// One S3 call, addressed path-style as the account's own credential.
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

/// The ARN a role of this account carries, which a key policy statement and a refusal name.
fn role_arn(name: &str) -> String {
    format!("arn:aws:iam::{ACCOUNT}:role/{name}")
}

/// The function one probe runs in, and the session its own calls are signed as: the identity a
/// function's SDK call speaks for is `arn:aws:sts::<account>:assumed-role/<role>/<function>`.
fn function_name(index: usize) -> String {
    format!("xsvc-in-fn-sse-kms-probe-{index}")
}
fn session_arn(index: usize) -> String {
    format!(
        "arn:aws:sts::{ACCOUNT}:assumed-role/{ROLE}/{}",
        function_name(index)
    )
}

/// A key policy statement over every resource with the effect, principal, actions and condition
/// given (no `Condition` when it is [`Value::Null`]).
fn statement(effect: &str, principal: &str, actions: Value, condition: Value) -> Value {
    let mut statement = json!({
        "Effect": effect,
        "Principal": {"AWS": principal},
        "Action": actions,
        "Resource": "*",
    });
    if !condition.is_null() {
        statement["Condition"] = condition;
    }
    statement
}

/// The statement the DEFAULT key policy carries: the account's root user, which delegates the
/// decision to that account's own identity-based policies.
fn allow_account() -> Value {
    statement(
        "Allow",
        &format!("arn:aws:iam::{ACCOUNT}:root"),
        json!("kms:*"),
        Value::Null,
    )
}

/// The document one key carries, as [`KeyPolicy`] states it.
fn key_document(which: KeyPolicy) -> String {
    let statements = match which {
        KeyPolicy::Delegated => vec![allow_account()],
        KeyPolicy::DenyingTheRole => vec![
            allow_account(),
            statement("Deny", &role_arn(ROLE), json!(DECRYPT_ACTION), Value::Null),
        ],
        KeyPolicy::GrantingTheRole => vec![
            allow_account(),
            statement("Allow", &role_arn(ROLE), json!(DECRYPT_ACTION), Value::Null),
        ],
        KeyPolicy::GrantingViaThisService => vec![
            allow_account(),
            statement(
                "Allow",
                &role_arn(ROLE),
                json!(DECRYPT_ACTION),
                json!({"StringEquals": {"kms:ViaService": format!("s3.{REGION}.amazonaws.com")}}),
            ),
        ],
        KeyPolicy::AnotherRoleOnly => vec![
            // Storing an object under a key performs `kms:GenerateDataKey` on it, so the account
            // that writes the object reaches that action — and only it, which leaves the decrypt
            // to the one role the next statement names.
            statement(
                "Allow",
                &format!("arn:aws:iam::{ACCOUNT}:root"),
                json!("kms:GenerateDataKey"),
                Value::Null,
            ),
            statement(
                "Allow",
                &role_arn(OTHER_ROLE),
                json!(DECRYPT_ACTION),
                Value::Null,
            ),
        ],
    };
    json!({"Version": "2012-10-17", "Statement": statements}).to_string()
}

/// Create one symmetric encryption key per document under test, as `(key id, key ARN)`.
///
/// A document here is the one UNDER TEST: it names whichever principal the row is about, so it
/// need not leave the creating caller a path to a later `kms:PutKeyPolicy` and KMS's
/// policy-lockout safety check would refuse it. That check has its own tests in the KMS crate, so
/// the key is created the way the KMS model provides for.
async fn create_keys(router: &MultiServiceRouter) -> BTreeMap<KeyPolicy, (String, String)> {
    let mut created = BTreeMap::new();
    for which in KEYS {
        let (status, answered) = kms(
            router,
            "CreateKey",
            &json!({
                "Description": format!("{which:?}"),
                "Policy": key_document(*which),
                "BypassPolicyLockoutSafetyCheck": true,
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

/// Replace one key's own document — the call that makes a key readable by a principal its policy
/// refused before.
async fn put_key_policy(router: &MultiServiceRouter, key_arn: &str, document: &str) {
    let (status, answered) = kms(
        router,
        "PutKeyPolicy",
        &json!({
            "KeyId": key_arn,
            "PolicyName": "default",
            "Policy": document,
            "BypassPolicyLockoutSafetyCheck": true,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "PutKeyPolicy({key_arn:?}) must succeed, got {status} with {answered}"
    );
}

/// The trust policy a Lambda execution role needs: Lambda has to be able to assume it for the
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

/// The inline document the reader role carries: the log actions every execution role needs and
/// the read of this bucket's objects — and NOTHING of KMS, so what decides a read is the object's
/// own key policy.
fn reader_document() -> String {
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
        ],
    })
    .to_string()
}

/// Create `name` with the Lambda trust policy and `document` as its one inline policy, and answer
/// the ARN IAM minted for it.
async fn create_role(router: &MultiServiceRouter, name: &str, document: &str) -> String {
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
    let arn = opt_element(&created, "Arn").unwrap_or_else(|| {
        panic!("setup: CreateRole({name:?}) must answer the role's ARN, got {created}")
    });
    let (status, embedded) = query(
        router,
        "iam",
        "2010-05-08",
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

/// Create the bucket every row's object is stored in, and make SSE-KMS under the key
/// [`BUCKET_DEFAULT_KEY`] names its default encryption — the configuration that decides how an
/// object written with no encryption member of its own is stored.
async fn create_bucket(router: &MultiServiceRouter, default_key_arn: &str) {
    let created = s3(router, "PUT", &format!("/{BUCKET}"), &[], "").await;
    assert_eq!(
        created.status, 200,
        "setup: CreateBucket({BUCKET:?}) must succeed, got {} with {}",
        created.status, created.body
    );
    let configuration = format!(
        "<ServerSideEncryptionConfiguration><Rule>\
         <ApplyServerSideEncryptionByDefault>\
         <SSEAlgorithm>{SSE_KMS}</SSEAlgorithm><KMSMasterKeyID>{default_key_arn}</KMSMasterKeyID>\
         </ApplyServerSideEncryptionByDefault></Rule>\
         </ServerSideEncryptionConfiguration>"
    );
    let stored = s3(
        router,
        "PUT",
        &format!("/{BUCKET}?encryption"),
        &[("content-type", "application/xml")],
        &configuration,
    )
    .await;
    assert!(
        (200..300).contains(&stored.status),
        "setup: PutBucketEncryption({BUCKET:?}) must succeed, got {} with {}",
        stored.status,
        stored.body
    );
}

/// Store one row's object the way its [`Stored`] says, and assert the write reported the
/// encryption it was stored under: an SSE-KMS write names a customer-managed key, and the row's
/// read is of an object this write has already proved is sealed under it.
async fn put_object(
    router: &MultiServiceRouter,
    row: &Row,
    keys: &BTreeMap<KeyPolicy, (String, String)>,
) {
    let case = row.case;
    let mut headers: Vec<(&str, &str)> = vec![("content-type", "application/octet-stream")];
    let named_key;
    match row.stored {
        Stored::KmsByArn(which) => {
            named_key = keys[&which].1.clone();
            headers.push(("x-amz-server-side-encryption", SSE_KMS));
            headers.push(("x-amz-server-side-encryption-aws-kms-key-id", &named_key));
        }
        Stored::KmsById(which) => {
            named_key = keys[&which].0.clone();
            headers.push(("x-amz-server-side-encryption", SSE_KMS));
            headers.push(("x-amz-server-side-encryption-aws-kms-key-id", &named_key));
        }
        Stored::SseS3 => headers.push(("x-amz-server-side-encryption", SSE_S3)),
        // A write naming no encryption member at all: what it is stored under is the bucket's
        // own default.
        Stored::BucketDefault => {}
    }
    let written = s3(
        router,
        "PUT",
        &format!("/{BUCKET}/{}", row.object_key),
        &headers,
        BODY,
    )
    .await;
    assert_eq!(
        written.status, 200,
        "setup {case:?}: PutObject({:?}) must succeed, got {} with {}",
        row.object_key, written.status, written.body
    );
    assert_eq!(
        written
            .header("x-amz-server-side-encryption")
            .unwrap_or_default(),
        row.stored.algorithm(),
        "setup {case:?}: the write must report the object stored under {:?}, got {:?}",
        row.stored.algorithm(),
        written.header("x-amz-server-side-encryption")
    );
    let reported_key = written.header("x-amz-server-side-encryption-aws-kms-key-id");
    match row.stored.key() {
        Some(which) => assert_eq!(
            reported_key.as_deref(),
            Some(keys[&which].1.as_str()),
            "setup {case:?}: an SSE-KMS write must report the key the object is sealed under, \
             got {reported_key:?}"
        ),
        None => assert_eq!(
            reported_key, None,
            "setup {case:?}: a write naming no customer-managed key must report none, got \
             {reported_key:?}"
        ),
    }
}

/// The stack declaring one function per probe, each with that probe's runtime and handler source
/// and all of them on the one reader role: the inline source is archived into the deployment
/// package by CloudFormation, which is how a function whose handler is written here comes to
/// exist at all.
fn function_stack(role: &str) -> String {
    let mut resources = serde_json::Map::new();
    for (index, probe) in PROBES.iter().enumerate() {
        resources.insert(
            format!("Probe{index}"),
            json!({
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": function_name(index),
                    "Runtime": probe.runtime,
                    "Handler": "index.handler",
                    "Role": role,
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": {"ZipFile": probe.form.source()},
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

/// Deploy the probes' stack, wait for `CREATE_COMPLETE` and answer the physical id of each
/// declared function.
async fn deploy_functions(
    router: &MultiServiceRouter,
    role: &str,
) -> BTreeMap<String, String> {
    let template = function_stack(role);
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
    // The stack's completion is owed from the instant Lambda reports its functions created, which
    // no fixed sleep can name: this looks until it reports it, on the shared wait whose deadline
    // is generous enough that a loaded host does not decide the outcome.
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
    let (status, resources) = query(
        router,
        "cloudformation",
        "2010-05-15",
        "DescribeStackResources",
        &[("StackName", STACK)],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: DescribeStackResources({STACK:?}) must succeed, got {status} with {resources}"
    );
    physical_ids_of(&resources)
}

/// Wait until Lambda reports `name` `Active`, which is the state an invocation needs.
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
        if configuration["State"].as_str() == Some("Active") {
            return;
        }
        last = configuration;
        tokio::time::sleep(LOOK).await;
    }
    panic!("setup: Lambda must report {name:?} Active, last reported {last}");
}

/// Invoke one probe over the object keys `reads` names, and answer the document its handler
/// returned — asserting on the way that the run itself completed: the SDK is on the import path,
/// so a module the runtime did not supply would have faulted the LOAD, which the status, the
/// `X-Amz-Function-Error` header and an error payload are where a caller reads.
async fn probe(
    router: &MultiServiceRouter,
    index: usize,
    reads: &BTreeMap<String, String>,
) -> Value {
    let case = PROBES[index].case;
    let function = function_name(index);
    let event = json!({"bucket": BUCKET, "objects": reads});
    let (status, headers, payload) = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{function}/invocations"),
        event.to_string().as_bytes(),
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
        "case {case:?}: the answer must be the handler's own result rather than an error \
         document, got {payload}"
    );
    assert_eq!(
        payload["imported"],
        json!(PROBES[index].form.imported()),
        "case {case:?}: the imports must resolve to the SDK modules the runtime supplies, got \
         {payload}"
    );
    assert_eq!(
        payload["identity"].as_str(),
        Some(session_arn(index).as_str()),
        "case {case:?}: the handler's calls must be signed as the execution role's own session, \
         got {payload}"
    );
    payload["reads"].clone()
}

/// What one row's read must report: the stored bytes, or the refusal KMS's decision was worded
/// in.
fn expected(row: &Row, keys: &BTreeMap<KeyPolicy, (String, String)>) -> Value {
    match row.stored.key() {
        Some(which) if !which.serves_the_role() => json!({
            "read": false,
            "code": ACCESS_DENIED,
            "message": which.refusal(&session_arn(row.probe), &keys[&which].1),
            "status": ACCESS_DENIED_STATUS,
        }),
        _ => json!({"read": true, "body": BODY}),
    }
}

/// Every row: the handler loads with the SDK the runtime supplies, and the read it makes with that
/// SDK is served or refused by the policy of the key the object was stored under.
#[tokio::test]
async fn a_handler_reads_an_sse_kms_object_only_when_its_key_policy_reaches_the_role() {
    let router = build_router();
    let role = create_role(&router, ROLE, &reader_document()).await;
    // A role no function runs as, so a key policy below can name a real principal of this account
    // that is not the reader.
    create_role(&router, OTHER_ROLE, &reader_document()).await;
    let keys = create_keys(&router).await;
    create_bucket(&router, &keys[&BUCKET_DEFAULT_KEY].1).await;
    for row in ROWS {
        put_object(&router, row, &keys).await;
    }

    let physical = deploy_functions(&router, &role).await;
    for (index, _) in PROBES.iter().enumerate() {
        let logical = format!("Probe{index}");
        let reported = physical.get(&logical).cloned().unwrap_or_default();
        assert_eq!(
            reported,
            function_name(index),
            "setup: the stack must report {logical} as {:?}, got {reported:?}",
            function_name(index)
        );
        wait_until_active(&router, &reported).await;
    }

    // One invocation per probe, reading every object that probe's rows name: whichever of them the
    // key policies refuse, the RUN completes and the handler's own payload is the answer.
    for (index, probe_row) in PROBES.iter().enumerate() {
        let mine: Vec<&Row> = ROWS.iter().filter(|row| row.probe == index).collect();
        let reads: BTreeMap<String, String> = mine
            .iter()
            .map(|row| (row.case.to_string(), row.object_key.to_string()))
            .collect();
        let answered = probe(&router, index, &reads).await;
        for row in mine {
            let case = format!("{} on {}: {}", probe_row.case, probe_row.runtime, row.case);
            let wanted = expected(row, &keys);
            assert_eq!(
                answered[row.case], wanted,
                "case {case:?}: expected the handler to report {wanted}, got {}",
                answered[row.case]
            );
        }
    }

    // The refusal is the KEY POLICY's and nothing else's: replacing the denying document with one
    // that grants the same role serves the very next invocation the bytes, with no other call in
    // between and nothing about the object changed.
    let denied = ROWS
        .iter()
        .find(|row| matches!(row.stored.key(), Some(KeyPolicy::DenyingTheRole)))
        .expect("one row reads an object under the denying key");
    put_key_policy(
        &router,
        &keys[&KeyPolicy::DenyingTheRole].1,
        &key_document(KeyPolicy::GrantingTheRole),
    )
    .await;
    let reads = BTreeMap::from([(denied.case.to_string(), denied.object_key.to_string())]);
    let answered = probe(&router, denied.probe, &reads).await;
    assert_eq!(
        answered[denied.case],
        json!({"read": true, "body": BODY}),
        "case the denying document replaced by one granting the role: the read must be served the \
         stored bytes, got {}",
        answered[denied.case]
    );

    // Every object is exactly as it was planted: every row only ever read, and a refused read read
    // nothing at all.
    for row in ROWS {
        let held = s3(
            &router,
            "GET",
            &format!("/{BUCKET}/{}", row.object_key),
            &[],
            "",
        )
        .await;
        if row.stored.key() == Some(KeyPolicy::AnotherRoleOnly) {
            // The account is no principal of that document either, so its own read is refused the
            // same way — which is the decision under test, taken for another caller.
            assert_eq!(
                held.status, 403,
                "case {:?}: the account's own read must be refused too, got {} with {}",
                row.case, held.status, held.body
            );
            continue;
        }
        assert_eq!(
            (held.status, held.body.as_str()),
            (200, BODY),
            "case {:?}: the object must be exactly as it was planted, got {} with {:?}",
            row.case,
            held.status,
            held.body
        );
    }
}
