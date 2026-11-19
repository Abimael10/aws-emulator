// SPDX-License-Identifier: CC0-1.0

//! The object-store client the runtime's own SDK hands a Python handler, used to read an object
//! sealed under a customer-managed key whose policy decides whether the function's role may have
//! the plaintext: Lambda, Amazon S3, KMS, IAM and the token service on ONE engine.
//!
//! RULE: the SDK a managed `python3.x` runtime supplies builds a client for EVERY service the
//! runtime's call seam binds — so `boto3.client(<token>)` for such a token resolves to that
//! service and the call it makes is really made, and only a token the seam binds NOTHING is
//! refused client-side with botocore's `UnknownServiceError` naming the catalog — and a call so
//! made is decided by the authorities in force on the function's own execution-role session:
//! serving the bytes of an object stored under a customer-managed key performs `kms:Decrypt` on
//! THAT key, so while no document refuses that session the read answers the stored bytes, and the
//! moment the key's own policy carries an explicit deny of the decrypt for it the read is refused
//! `AccessDenied` (HTTP 403) carrying none of the object, with an object under any OTHER key
//! untouched by that deny. A runtime whose SDK cannot build the client for one of its bound
//! services turns every such decision into a fault of the caller's own code, which is neither the
//! served read nor the refusal.
//!
//! "The execution role grants the function permission to use Amazon Web Services services"
//! (`FunctionConfiguration$Role`); "if the object is encrypted with SSE-KMS, you must also have
//! the kms:GenerateDataKey and kms:Decrypt permissions in IAM identity-based policies and KMS key
//! policies for the KMS key" (`HeadObject`'s own documentation of what such a read needs); a key
//! policy naming the account's root user delegates the decision to that account's identity-based
//! policies (`CreateKeyRequest$Policy`); and "an explicit deny in any policy overrides any allows"
//! (IAM User Guide, *Policy evaluation logic*).
//!
//! What this suite pins that its neighbours do not: the two halves TOGETHER, from the crate that
//! owns the SDK shim — that the catalog the shim reports holds the tokens a handler of this kind
//! builds clients for, and that those clients carry a real read all the way to the key's decision.
//! `crate::handler_sdk_catalog_is_complete` probes every bound token but presigns rather than
//! sends; `crate::handler_s3_read_refusal_codes` tables what an unsealed read answers with; and
//! `crate::handler_object_encryption_members` tables which encryption members a read reports.
//!
//! The rows differ along what the rule generalises over: WHICH key each object was sealed under
//! and HOW the write named it (the ARN KMS minted, and the bare key id), the form of the object's
//! own key (under a dated prefix, and flat), WHICH key carries the deny in a phase (neither, the
//! first, the second), and the FORM the deny names the action in (the action itself, and a list
//! holding it beside another of the service).
//!
//! The suite RUNS a handler, so it takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use std::collections::BTreeMap;
use std::sync::Arc;

use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::execution::aws_api::reachable_services;
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mappings::{await_function_active, call};
use crate::handler_s3_object_access::{
    form_encode, role_arn, session_arn, ACCESS_DENIED, ACCESS_DENIED_STATUS,
};
use crate::invoke_request_id::one_invocation_stream_at_a_time;
// The one .zip writer this crate's suites package a handler with, exported by the suite that
// first needed it rather than copied here.
use crate::node_invoke_run::package;

/// The Region every request below is signed for, and so the Region holding the keys, the bucket
/// and the function: an object and the key it is sealed under live at the same endpoint.
const REGION: &str = "us-east-1";

/// Where `CreateFunction` and `Invoke` are bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The module and attribute the deployment package carries its handler as.
const HANDLER: &str = "lambda_function.lambda_handler";

/// The access-key id standing for the account's own published credential: no store holds a record
/// of it, so it resolves to the account's ambient identity, which is what every request this
/// suite makes on its own behalf presents.
const ACCOUNT_KEY: &str = "AKIDEMULATOR";

/// The bucket every object below is stored in.
const BUCKET: &str = "handler-key-policy-objects";

/// The execution role the function runs as — the identity every key policy below decides about —
/// and the function itself, whose name completes the session ARN a refusal names.
const ROLE: &str = "handler-key-policy-reader";
const FUNCTION: &str = "handler-key-policy-reader-fn";

/// The name of the one inline identity policy the role carries.
const INLINE_POLICY: &str = "probe-grants";

/// The one name a KMS key holds a policy under.
const KEY_POLICY_NAME: &str = "default";

/// The action serving a sealed object's bytes performs on the key it was sealed under, the one a
/// write of such an object performs, and the read the role is granted on the bucket.
const DECRYPT_ACTION: &str = "kms:Decrypt";
const DATA_KEY_ACTION: &str = "kms:GenerateDataKey";
const READ_ACTION: &str = "s3:GetObject";

/// The `ServerSideEncryption` value a KMS key id belongs to (source:
/// `smithy_query(query="shape",shape="ServerSideEncryption")`), and the two headers an object's
/// own encryption is reported in (`PutObjectOutput$ServerSideEncryption` ->
/// `x-amz-server-side-encryption`, `$SSEKMSKeyId` ->
/// `x-amz-server-side-encryption-aws-kms-key-id`).
const SSE_KMS: &str = "aws:kms";
const SSE_HEADER: &str = "x-amz-server-side-encryption";
const SSE_KEY_HEADER: &str = "x-amz-server-side-encryption-aws-kms-key-id";

/// The service tokens the handler builds a client for: the object store the read is made through,
/// the key service holding the documents, and the token service it reads its own identity from.
/// Each is a token the call seam binds, which the test asserts before it invokes anything.
const CLIENT_TOKENS: &[&str] = &["s3", "kms", "sts"];

/// The token the read is made through, and the one the handler reads its own identity from.
const OBJECT_STORE_TOKEN: &str = "s3";
const IDENTITY_TOKEN: &str = "sts";

/// A token the call seam binds NOTHING, probed in the same invocation so the negative half of the
/// rule is observed beside the positive one. It names no AWS service at all, so no service the
/// emulator gains can bind it; that it is unbound is asserted from the catalog even so.
const UNBOUND_TOKEN: &str = "no-aws-service-answers-to-this";

/// The class botocore raises for a token no service answers to.
const UNKNOWN_SERVICE: &str = "UnknownServiceError";

/// The handler every phase invokes.
///
/// It builds one client per token the event names and reports what each client was built FOR, then
/// reads each object the event names through the object-store client and reports either the bytes
/// or Amazon S3's own refusal — its code, its message and the status it came back on — and finally
/// probes the unbound token so what the SDK does with one is read off the same run. A refused read
/// reports NO bytes at all, which is what says nothing of the object reached a caller the key
/// refused; a failure of the SDK ITSELF is reported as `sdk_error`, so a client that could not be
/// built is told apart from a read that was refused.
const HANDLER_SOURCE: &str = r#"
import boto3
import botocore.exceptions


def lambda_handler(event, context):
    report = {"clients": {}, "reads": {}}
    for token in event["tokens"]:
        built = boto3.client(token)
        report["clients"][token] = {
            "service": built.meta.service_model.service_name,
            "region": built.meta.region_name,
        }
    identities = boto3.client(event["identity_token"])
    report["identity"] = identities.get_caller_identity()["Arn"]
    objects = boto3.client(event["object_store_token"])
    for label, key in event["objects"].items():
        try:
            read = objects.get_object(Bucket=event["bucket"], Key=key)
        except botocore.exceptions.ClientError as refused:
            report["reads"][label] = {
                "code": refused.response["Error"]["Code"],
                "message": refused.response["Error"]["Message"],
                "status": refused.response["ResponseMetadata"]["HTTPStatusCode"],
            }
            continue
        except botocore.exceptions.BotoCoreError as broken:
            report["reads"][label] = {
                "sdk_error": type(broken).__name__,
                "sentence": str(broken),
            }
            continue
        report["reads"][label] = {"body": read["Body"].read().decode("utf-8")}
    try:
        boto3.client(event["unbound_token"]).list_things()
    except botocore.exceptions.BotoCoreError as refused:
        report["unbound"] = {"raised": type(refused).__name__, "sentence": str(refused)}
    except botocore.exceptions.ClientError as answered:
        report["unbound"] = {"raised": "ClientError", "sentence": str(answered)}
    else:
        report["unbound"] = {"raised": None}
    return report
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

/// HOW a write names the key it seals an object under: the two references `SSEKMSKeyId` takes for
/// a key of this account.
#[derive(Debug, Clone, Copy)]
enum Reference {
    /// The ARN KMS minted for the key.
    Arn,
    /// The bare key id.
    Id,
}

/// How one key's document names the action it DENIES the reading role, or that it denies nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Deny {
    /// No deny at all: the account delegation alone, which the role's own inline policy completes.
    Nothing,
    /// `"Action": "kms:Decrypt"` — the action the read performs, named on its own.
    TheAction,
    /// A list holding that action beside another of the service, which is how one document denies
    /// several at once.
    AListHoldingTheAction,
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
        }
    }
}

/// One object of this suite: the key it was sealed under, how the write named that key, the object
/// key it is stored at, and the bytes it holds — distinct per object, so "the exact bytes this
/// object holds" is something a read can be wrong about.
struct Sealed {
    label: &'static str,
    object_key: &'static str,
    body: &'static str,
    sealed_under: Key,
    reference: Reference,
}

const OBJECTS: &[Sealed] = &[
    Sealed {
        label: "the object under the first key, named by ARN",
        object_key: "reports/2026/first.txt",
        body: "region,amount\nus-east-1,17\n",
        sealed_under: Key::First,
        reference: Reference::Arn,
    },
    Sealed {
        label: "the object under the second key, named by its bare key id",
        object_key: "second-flat",
        body: "the bytes the other key protects",
        sealed_under: Key::Second,
        reference: Reference::Id,
    },
];

/// One phase: the document each key carries while the handler reads both objects. The phases run
/// in order on ONE bucket, ONE role and ONE function, because what a phase changes is the
/// documents in force and nothing else.
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

/// Lambda, Amazon S3, KMS, IAM and the token service over ONE shared engine, as
/// `core/router/src/lib.rs` mounts them: the handler's own calls are dispatched back into this
/// same router, so the object it reads is the object this suite wrote and the key that object
/// names is the key KMS holds.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The suite signs nothing: the `Authorization` header carries only the credential scope, which
    // is where a service reads the Region and the caller's access key id. The handler's OWN calls
    // are signed by the runtime, with the credential the invocation was given.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(service_com_amazonaws_kms_trentservice::EmulatedService::new(
        Arc::clone(&engine),
    ));
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

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request. The signature is never checked here.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={ACCOUNT_KEY}/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// What one Amazon S3 call answered.
struct Answered {
    status: u16,
    headers: http::HeaderMap,
    body: String,
}

impl Answered {
    /// A bound response header's value.
    fn header(&self, name: &str) -> Option<String> {
        self.headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    }
}

/// One Amazon S3 request, path style, as the account's own credential: its protocol is `restXml`,
/// so the answer's own HEADERS are where an object's encryption is reported.
async fn s3(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> Answered {
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
        .body(bytes::Bytes::from(body.to_owned()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    Answered {
        status: parts.status.as_u16(),
        headers: parts.headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    }
}

/// One KMS `awsJson1_1` call, reached by `X-Amz-Target: TrentService.<Operation>`, that must be
/// served.
async fn kms(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let host = format!("kms.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host)
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("TrentService.{operation}"))
        .header("authorization", authorization("kms"))
        .body(bytes::Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    assert_eq!(
        status, 200,
        "case {case:?}: KMS {operation} must succeed, got {status} with {parsed}"
    );
    parsed
}

/// One awsQuery call to IAM that must be served.
async fn iam(router: &MultiServiceRouter, case: &str, action: &str, params: &[(&str, &str)]) {
    let mut body = format!("Action={action}&Version=2010-05-08");
    for (name, value) in params {
        body.push('&');
        body.push_str(name);
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://iam.{REGION}.amazonaws.com/"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("iam"))
        .body(bytes::Bytes::from(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let answered = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: IAM {action} must succeed, got {status} with {answered}"
    );
}

/// The document one key carries while `deny` is in force on it: the account delegation every key
/// policy starts from, and the deny statement the phase names, if any.
fn key_document(deny: Deny) -> String {
    let mut statements = vec![json!({
        "Effect": "Allow",
        "Principal": {"AWS": format!("arn:aws:iam::{}:root", engine::emulator_account_id())},
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
fn refusal(key_arn: &str) -> String {
    format!(
        "User: {} is not authorized to perform: {DECRYPT_ACTION} on resource: {key_arn} with an \
         explicit deny in a resource-based policy",
        session_arn(ROLE, FUNCTION)
    )
}

/// Create one symmetric encryption key per [`Key`], each under the document it starts with, as
/// `(key id, key ARN)`.
async fn create_keys(router: &MultiServiceRouter) -> BTreeMap<Key, (String, String)> {
    let mut created = BTreeMap::new();
    for which in Key::ALL {
        let case = "setup: the keys the objects are sealed under";
        let answered = kms(
            router,
            case,
            "CreateKey",
            &json!({
                "Description": format!("{which:?}"),
                "Policy": key_document(Deny::Nothing),
            }),
        )
        .await;
        let member = |name: &str| {
            answered["KeyMetadata"][name]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: CreateKey answered no {name} for {which:?}: {answered}")
                })
                .to_string()
        };
        created.insert(*which, (member("KeyId"), member("Arn")));
    }
    created
}

/// Replace one key's own document — the call a client makes a readable object unreadable, and
/// readable again, with.
async fn put_key_policy(router: &MultiServiceRouter, case: &str, key_arn: &str, document: &str) {
    kms(
        router,
        case,
        "PutKeyPolicy",
        &json!({
            "KeyId": key_arn,
            "PolicyName": KEY_POLICY_NAME,
            "Policy": document,
        }),
    )
    .await;
}

/// Create `ROLE`, trusted by Lambda, carrying the inline document that settles the role's own half
/// of every decision: the log actions an execution role needs, the read of this bucket's objects,
/// and the decrypt on BOTH keys — so what is left to decide a read is each key's own document.
async fn create_role(router: &MultiServiceRouter, key_arns: &[&str]) -> String {
    let case = "setup: the execution role the function runs as";
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": "lambda.amazonaws.com"},
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    iam(
        router,
        case,
        "CreateRole",
        &[("RoleName", ROLE), ("AssumeRolePolicyDocument", &trust)],
    )
    .await;
    let document = json!({
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
    .to_string();
    iam(
        router,
        case,
        "PutRolePolicy",
        &[
            ("RoleName", ROLE),
            ("PolicyName", INLINE_POLICY),
            ("PolicyDocument", &document),
        ],
    )
    .await;
    role_arn(ROLE)
}

/// Create the bucket every object is stored in.
async fn create_bucket(router: &MultiServiceRouter) {
    let created = s3(router, "PUT", &format!("/{BUCKET}"), &[], "").await;
    assert!(
        (200..300).contains(&created.status),
        "setup: CreateBucket({BUCKET:?}) must succeed, got {} with {}",
        created.status,
        created.body
    );
}

/// Store one object under its own key, the write naming that key by the reference the row states,
/// and assert the write reported it sealed under the key KMS minted: the read under test is of an
/// object this write has already proved is sealed under the key whose document the phases change.
async fn put_object(
    router: &MultiServiceRouter,
    object: &Sealed,
    keys: &BTreeMap<Key, (String, String)>,
) {
    let (key_id, key_arn) = keys[&object.sealed_under].clone();
    let reference = match object.reference {
        Reference::Arn => key_arn.clone(),
        Reference::Id => key_id,
    };
    let written = s3(
        router,
        "PUT",
        &format!("/{BUCKET}/{}", object.object_key),
        &[
            ("content-type", "text/plain"),
            (SSE_HEADER, SSE_KMS),
            (SSE_KEY_HEADER, &reference),
        ],
        object.body,
    )
    .await;
    assert_eq!(
        (
            written.status,
            written.header(SSE_HEADER).as_deref(),
            written.header(SSE_KEY_HEADER).as_deref(),
        ),
        (200, Some(SSE_KMS), Some(key_arn.as_str())),
        "setup {:?}: the write naming the key {reference:?} must report the object sealed under \
         {key_arn:?} with {SSE_KMS}, got {} with {:?} and {:?} ({})",
        object.label,
        written.status,
        written.header(SSE_HEADER),
        written.header(SSE_KEY_HEADER),
        written.body
    );
}

/// Create the one function every phase invokes and wait until Lambda reports it `Active`, which is
/// the state an invocation needs.
async fn create_function(router: &MultiServiceRouter, role: &str) {
    let case = "setup: the function the phases invoke";
    let created = call(
        router,
        "POST",
        FUNCTIONS_URI,
        &json!({
            "FunctionName": FUNCTION,
            "Role": role,
            "Runtime": "python3.12",
            "Handler": HANDLER,
            "Timeout": 120,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(&[("lambda_function.py", HANDLER_SOURCE)])),
            },
        }),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "case {case:?}: expected CreateFunction to answer 201, got {} {}",
        created.status,
        created.text()
    );
    await_function_active(router, case, FUNCTION).await;
}

/// Invoke the function over every object, answering the report its handler returned — asserting on
/// the way that the invocation was served and the run completed: a refused read is a value in the
/// payload, never a failed invocation.
async fn invoke(router: &MultiServiceRouter, case: &str) -> Value {
    let reads: BTreeMap<&str, &str> = OBJECTS
        .iter()
        .map(|object| (object.label, object.object_key))
        .collect();
    let answered = call(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{FUNCTION}/invocations"),
        &json!({
            "tokens": CLIENT_TOKENS,
            "object_store_token": OBJECT_STORE_TOKEN,
            "identity_token": IDENTITY_TOKEN,
            "unbound_token": UNBOUND_TOKEN,
            "bucket": BUCKET,
            "objects": reads,
        }),
    )
    .await;
    assert_eq!(
        answered.status,
        200,
        "case {case:?}: the invocation must be served, got {} {}",
        answered.status,
        answered.text()
    );
    let faulted = answered.header("x-amz-function-error");
    assert_eq!(
        faulted, None,
        "case {case:?}: the handler must run to completion, got {faulted:?} with {}",
        answered.text()
    );
    answered.json()
}

/// What one object's read must report in one phase: the bytes it holds, or the refusal the key's
/// explicit deny was worded in — with no bytes at all.
fn expected(phase: &Phase, object: &Sealed, keys: &BTreeMap<Key, (String, String)>) -> Value {
    if phase.deny_on(object.sealed_under).serves() {
        return json!({"body": object.body});
    }
    json!({
        "code": ACCESS_DENIED,
        "message": refusal(&keys[&object.sealed_under].1),
        "status": ACCESS_DENIED_STATUS,
    })
}

/// Assert the object is exactly as it was planted, whichever way the handler's read went: its
/// metadata still reports the key it was sealed under, and a principal the deny does not name is
/// still served its exact bytes.
async fn assert_object_intact(
    router: &MultiServiceRouter,
    case: &str,
    object: &Sealed,
    keys: &BTreeMap<Key, (String, String)>,
) {
    let key_arn = keys[&object.sealed_under].1.clone();
    let path = format!("/{BUCKET}/{}", object.object_key);
    let described = s3(router, "HEAD", &path, &[], "").await;
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
    let held = s3(router, "GET", &path, &[], "").await;
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

/// The clients the SDK built and the unbound token it refused, asserted on every phase's report:
/// the positive half of the rule is that a token the seam binds builds a client FOR that service
/// and carries a real call, and the negative half is that a token it binds nothing is refused
/// client-side with botocore's own sentence, naming the catalog the runtime holds.
fn assert_sdk_catalog(case: &str, report: &Value) {
    for token in CLIENT_TOKENS {
        let built = report["clients"][*token].clone();
        assert_eq!(
            (built["service"].as_str(), built["region"].as_str()),
            (Some(*token), Some(REGION)),
            "case {case:?}: expected a client built for {token:?} in {REGION}, got {built}"
        );
    }
    let unbound = report["unbound"].clone();
    assert_eq!(
        unbound["raised"].as_str(),
        Some(UNKNOWN_SERVICE),
        "case {case:?}: expected {UNKNOWN_SERVICE} for the unbound token {UNBOUND_TOKEN:?}, got \
         {unbound}"
    );
    let sentence = unbound["sentence"].as_str().unwrap_or_default();
    let listed = format!(
        "Unknown service: '{UNBOUND_TOKEN}'. Valid service names are: {}",
        reachable_services().join(", ")
    );
    assert_eq!(
        sentence, listed,
        "case {case:?}: expected botocore's unknown-service sentence listing the catalog, got \
         {sentence:?}"
    );
    assert_eq!(
        report["identity"].as_str(),
        Some(session_arn(ROLE, FUNCTION).as_str()),
        "case {case:?}: the handler's calls must be signed as the execution role's own session, \
         got {}",
        report["identity"]
    );
}

/// Every phase: the client the runtime's SDK builds for the object store carries a real read, and
/// what that read is served depends on the document each key carries AT THE TIME OF THE READ and
/// on nothing else — not on the document the key was created with, not on an earlier read of the
/// same object, and not on the other key's document.
#[tokio::test]
async fn a_handlers_sse_kms_read_follows_the_key_policy_in_force() {
    let _stream = one_invocation_stream_at_a_time().await;
    let router = emulator();

    // The catalog the runtime holds is what decides which clients can be built at all, so the
    // tokens this suite's rule is stated over are read off it rather than assumed.
    let catalog = reachable_services();
    for token in CLIENT_TOKENS {
        assert!(
            catalog.contains(token),
            "setup: the call seam must bind {token:?} for a handler to reach it, got {catalog:?}"
        );
    }
    assert!(
        !catalog.contains(&UNBOUND_TOKEN),
        "setup: {UNBOUND_TOKEN:?} must be a token the seam binds nothing, got {catalog:?}"
    );

    let keys = create_keys(&router).await;
    let key_arns: Vec<&str> = Key::ALL.iter().map(|which| keys[which].1.as_str()).collect();
    let role = create_role(&router, &key_arns).await;
    create_bucket(&router).await;
    for object in OBJECTS {
        put_object(&router, object, &keys).await;
    }
    create_function(&router, &role).await;

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
        let report = invoke(&router, phase.case).await;
        assert_sdk_catalog(phase.case, &report);
        for object in OBJECTS {
            let wanted = expected(phase, object, &keys);
            assert_eq!(
                report["reads"][object.label], wanted,
                "case {:?}, {:?}: expected the handler to report {wanted}, got {}",
                phase.case, object.label, report["reads"][object.label]
            );
            assert_object_intact(&router, phase.case, object, &keys).await;
        }
    }
}
