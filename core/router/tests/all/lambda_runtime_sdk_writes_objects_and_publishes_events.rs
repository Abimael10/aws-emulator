// SPDX-License-Identifier: CC0-1.0

//! What a RUNNING `python3.x` function DOES with the AWS SDK its runtime supplies: it writes,
//! reads and deletes objects in Amazon S3, and publishes onto an EventBridge bus whose rules
//! carry the publication onwards to their own targets.
//!
//! RULE: a managed `python3.x` runtime carries the AWS SDK for Python on the function's import
//! path, so a handler module that imports the SDK AT MODULE SCOPE is imported successfully and
//! the handler body runs; every call that SDK makes from inside the invocation is signed as a
//! session of the function's execution role and is decided and served by the emulated service
//! it addresses — so a synchronous `Invoke` answers HTTP 200 with NO `X-Amz-Function-Error`
//! header and a `Payload` equal to the handler's OWN return value, whether that value reports
//! a served call or a refusal the handler caught, and the effect of a served call is in the
//! service that served it afterwards. A failure of the runtime to supply a module it is
//! supposed to supply may not stand in for that payload.
//!
//! `InvocationResponse$FunctionError` is bound to the response header `X-Amz-Function-Error`
//! and `StatusCode` to the HTTP status (`smithy_query(query="operation",operation="Invoke")`),
//! so an invocation whose handler ran to completion carries neither signal; "the execution role
//! grants the function permission to use Amazon Web Services services"
//! (`FunctionConfiguration$Role`), and the identity a function's own call speaks for is the
//! session `arn:aws:sts::<account>:assumed-role/<role>/<function>`; with neither an
//! identity-based nor a resource-based allow a request is denied (IAM User Guide, *Policy
//! evaluation logic*).
//!
//! Six crates have to agree for that to hold — CloudFormation archives the inline source into a
//! deployment package and creates the functions, IAM holds each role and its inline document,
//! the token service vends the session the handler's calls are signed with, Lambda runs the
//! handler and supplies it the SDK, and Amazon S3 / EventBridge / Amazon SQS take the decisions
//! and hold what arrived — so the suite lives here: a crate-local suite can only ever see one
//! side of it. What the neighbours pin and this one does not repeat:
//! `lambda_in_function_dynamodb_authorization` and `lambda_in_function_sqs_authorization` drive
//! the same in-function seam against other services' decisions;
//! `s3_object_read_identity_or_bucket_policy` drives S3's decision for a credential a client
//! holds directly; the Lambda crate's own `handler_object_store_sdk` drives ONE role's reads
//! and writes across the binding classes of the object store's protocol; and the EventBridge
//! crate's own `handler_execution_role_publication` drives ONE entry onto the default bus under
//! a granted and a withheld role. What only this suite reaches is the TABLE — the import FORM
//! the handler opens with crossed with the document in force and the operation called, so the
//! permission boundary is observable per action rather than per role, and one `PutEvents` the
//! handler makes carrying SEVERAL entries routed by content to SEVERAL rules' targets.
//!
//! The rows of the first table differ along what the rule generalises over: the FORM the
//! handler imports the SDK in at module scope (the module, the session class, a from-import of
//! the client factory), what the role's own document allows (nothing of the service, this
//! operation's action, another operation's, every action of the service), which resource that
//! statement names (the objects of the bucket called, the objects of another bucket of the
//! account, every resource), and WHICH object operation the handler calls (a write, a read, a
//! delete) — several per role, so one role's document is asked about more than one action. Each
//! probe asserts both halves: what the handler reports of the call, and what the BUCKET holds
//! afterwards, so a refusal that still wrote, and a grant that answered without writing, both
//! fail here.
//!
//! The second test drives the publication plane over the same seam: the entries of one
//! `PutEvents` the handler makes, varying the batch's size and whether an entry's `detail-type`
//! is matched by a rule of the bus — and nothing waits a fixed span for a delivery that must
//! NOT happen, because the queues of the matched rules are polled first and every "received
//! nothing" read is made at the instant those arrivals have already been observed.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::stack_status;
use crate::query_wire::{elements, form_encode, opt_element, physical_ids_of};

/// The Region every request here is signed for, and so the Region the functions, the bus and
/// the queues belong to.
const REGION: &str = "us-east-1";

/// The account this emulator serves, which every session ARN and every bus ARN carries.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The bucket every object probe acts on.
const BUCKET: &str = "xsvc-in-fn-sdk-objects";

/// Another bucket of this account, which one row's statement names instead of the bucket the
/// handler calls.
const OTHER_BUCKET: &str = "xsvc-in-fn-sdk-elsewhere";

/// The bytes planted under a probe's key before the invocation, for a probe whose operation
/// reads or deletes an object: a refusal then differs from an absent object, and a read or a
/// delete that was refused leaves these bytes in place.
const PLANTED: &str = "the body planted before the invocation";

/// The `StackStatus` a deployment must reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The code and status Amazon S3 refuses a call the policies in force do not permit with
/// (Amazon S3 "Error responses").
const ACCESS_DENIED: &str = "AccessDenied";
const ACCESS_DENIED_STATUS: u64 = 403;

/// How long one invocation is given, as the function's own `Timeout`: well past what a run
/// needs on any host, because what is under test is what the handler's SDK call did and not how
/// fast an interpreter gets through the handler.
const FUNCTION_TIMEOUT: u64 = 120;

/// How often to look while waiting for work the clock owes — a stack reaching
/// `CREATE_COMPLETE`, a created function reaching `Active`, a publication reaching a target —
/// and how many looks to take. The spans are those services' own, so no fixed sleep names the
/// instant one ends, and the deadline is generous so a loaded host cannot decide the outcome.
const LOOK: Duration = Duration::from_millis(100);
const LOOKS: usize = 600;

/// The service principal EventBridge delivers to a queue target as, which the queue's own
/// policy has to grant: "for Lambda functions, Amazon SNS topics and Amazon SQS queues,
/// EventBridge relies on resource-based policies" (`PutTargets`).
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";

/// The action a publication onto a bus performs, as a statement names it.
const PUT_EVENTS_ACTION: &str = "events:PutEvents";

/// The `source` every event this suite's handler publishes carries, and the three
/// `detail-type` values the entries carry: two that a rule of the bus matches, and one that no
/// rule matches at all.
const SOURCE: &str = "com.example.in-function-sdk";
const WRITTEN_TYPE: &str = "Object Written";
const REPORT_TYPE: &str = "Report Ready";
const UNWATCHED_TYPE: &str = "Unwatched Change";

/// A `detail-type` nothing here ever publishes, which the third rule's pattern names: a rule
/// that cannot match receives nothing however much is published.
const NEVER_TYPE: &str = "Never Published";

/// The `VisibilityTimeout` every queue here is created with, in seconds: the longest a queue
/// may be configured for ("an integer from 0 to 43,200 (12 hours)",
/// `SetQueueAttributesRequest$Attributes`). An envelope one read took is then still in flight at
/// every later read, however long the suite takes, so no assertion about what a queue holds can
/// turn on how fast the host runs. Nothing here tests the timeout itself.
const VISIBILITY_TIMEOUT: &str = "43200";

/// WHICH object operation one probe's handler calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Put,
    Get,
    Delete,
}

impl Op {
    /// The word the event names it by, which the handler reads.
    fn event(self) -> &'static str {
        match self {
            Self::Put => "put",
            Self::Get => "get",
            Self::Delete => "delete",
        }
    }

    /// The IAM action the operation performs, which is the action a refusal names: Amazon S3's
    /// action list carries one `s3:<OperationName>` per object operation ("you must have the
    /// `s3:PutObject` permission", `PutObject`).
    fn action(self) -> &'static str {
        match self {
            Self::Put => "s3:PutObject",
            Self::Get => "s3:GetObject",
            Self::Delete => "s3:DeleteObject",
        }
    }

    /// Whether a probe's key has to hold an object before the invocation: a read reports one,
    /// a delete removes one, and either that was refused has to leave one in place.
    fn plants_an_object(self) -> bool {
        matches!(self, Self::Get | Self::Delete)
    }
}

/// WHICH resource a row's statement names.
#[derive(Debug, Clone, Copy)]
enum Scope {
    /// Every object of the bucket the handler calls.
    Objects,
    /// Every object of another bucket of the same account.
    OtherObjects,
    /// Every resource, which the bare `*` wildcard stands for.
    Every,
}

impl Scope {
    fn resource(self) -> String {
        match self {
            Self::Objects => format!("arn:aws:s3:::{BUCKET}/*"),
            Self::OtherObjects => format!("arn:aws:s3:::{OTHER_BUCKET}/*"),
            Self::Every => "*".to_string(),
        }
    }
}

/// The FORM a row's handler imports the SDK in, at module scope — the import a real managed
/// runtime resolves out of the SDK it carries, and the thing the rule is about.
#[derive(Debug, Clone, Copy)]
enum Form {
    /// `import boto3`, the form AWS's own examples use.
    Module,
    /// A from-import of the session class, which is what an application builds its own session
    /// with.
    SessionClass,
    /// A from-import of the client factory under a name of the handler's own.
    ClientFactory,
}

impl Form {
    /// The module-scope import the handler opens with, and the function that builds the client
    /// out of what it imported. Each form ends with `ClientError` bound to the class a refusal
    /// is raised as, so the shared body below reads the same name whichever form it follows.
    fn preamble(self) -> &'static str {
        match self {
            Self::Module => {
                "import boto3\nfrom botocore.exceptions import ClientError\n\n\n\
                 def build():\n    return boto3.client(\"s3\")\n"
            }
            Self::SessionClass => {
                "import botocore.exceptions\nfrom boto3.session import Session\n\n\
                 ClientError = botocore.exceptions.ClientError\n\n\n\
                 def build():\n    return Session().client(\"s3\")\n"
            }
            Self::ClientFactory => {
                "import botocore\nfrom boto3 import client as s3_client\n\
                 from botocore.exceptions import ClientError\n\n\n\
                 def build():\n    return s3_client(\"s3\")\n"
            }
        }
    }

    /// The whole handler module for this form: the import, then the one body every row runs.
    fn source(self) -> String {
        format!("{}{PROBE_BODY}", self.preamble())
    }
}

/// The body every object probe runs: it performs the ONE object operation the event names with
/// the client its module-scope import built, and reports either that the call was served — with
/// the bytes it read, for a read — or the refusal Amazon S3 answered it with: its code, its
/// sentence and the HTTP status it came back on.
const PROBE_BODY: &str = r#"

def handler(event, context):
    objects = build()
    bucket = event["bucket"]
    key = event["key"]
    try:
        if event["op"] == "put":
            objects.put_object(
                Bucket=bucket, Key=key, Body=event["body"].encode("utf-8")
            )
            return {"ok": True}
        if event["op"] == "get":
            read = objects.get_object(Bucket=bucket, Key=key)
            return {"ok": True, "body": read["Body"].read().decode("utf-8")}
        objects.delete_object(Bucket=bucket, Key=key)
        return {"ok": True}
    except ClientError as refused:
        return {
            "ok": False,
            "code": refused.response["Error"]["Code"],
            "message": refused.response["Error"]["Message"],
            "status": refused.response["ResponseMetadata"]["HTTPStatusCode"],
        }
"#;

/// The handler the publication test runs: it publishes every entry the event carries in ONE
/// `PutEvents` with the client its module-scope import built, and reports what the call
/// answered — so the payload asserted is what the SDK gave the guest.
const PUBLISHER_SOURCE: &str = r#"
import json

import boto3
import botocore.exceptions


def handler(event, context):
    bus = boto3.client("events")
    entries = [
        {
            "Source": entry["source"],
            "DetailType": entry["detail_type"],
            "Detail": json.dumps(entry["detail"]),
            "EventBusName": event["bus"],
        }
        for entry in event["entries"]
    ]
    try:
        answered = bus.put_events(Entries=entries)
    except botocore.exceptions.ClientError as refused:
        return {
            "ok": False,
            "code": refused.response["Error"]["Code"],
            "message": refused.response["Error"]["Message"],
        }
    return {
        "ok": True,
        "failed": answered["FailedEntryCount"],
        "ids": len([entry["EventId"] for entry in answered["Entries"]]),
    }
"#;

/// One probe of a row's role: the operation the handler calls, and whether the policies in
/// force let it through.
struct Probe {
    case: &'static str,
    op: Op,
    allowed: bool,
}

/// One row: the import form the handler opens with, the document its execution role carries,
/// and the probes taken against it.
struct Row {
    form: Form,
    /// The actions the role's inline document allows besides the log actions every execution
    /// role carries; empty for a role granting none of this service's.
    actions: &'static [&'static str],
    /// The resource that statement names.
    scope: Scope,
    probes: &'static [Probe],
}

const ROWS: &[Row] = &[
    // The case the rule is stated about: a least-privilege role granting only the log actions a
    // function needs is granted no object operation at all — and the handler still RUNS, which
    // is what makes the refusal its own report rather than a failure of the invocation.
    Row {
        form: Form::Module,
        actions: &[],
        scope: Scope::Objects,
        probes: &[
            Probe {
                case: "a role granting only the log actions writes nothing",
                op: Op::Put,
                allowed: false,
            },
            Probe {
                case: "a role granting only the log actions reads nothing",
                op: Op::Get,
                allowed: false,
            },
        ],
    },
    Row {
        form: Form::Module,
        actions: &["s3:PutObject"],
        scope: Scope::Objects,
        probes: &[
            Probe {
                case: "a role granting the write over the bucket's objects writes one",
                op: Op::Put,
                allowed: true,
            },
            Probe {
                case: "the same role is granted no read by it",
                op: Op::Get,
                allowed: false,
            },
            Probe {
                case: "the same role is granted no delete by it",
                op: Op::Delete,
                allowed: false,
            },
        ],
    },
    Row {
        form: Form::SessionClass,
        actions: &["s3:PutObject"],
        scope: Scope::OtherObjects,
        probes: &[Probe {
            case: "a role granting the write over ANOTHER bucket's objects writes nothing",
            op: Op::Put,
            allowed: false,
        }],
    },
    Row {
        form: Form::SessionClass,
        actions: &["s3:*"],
        scope: Scope::Objects,
        probes: &[
            Probe {
                case: "a role granting every action of the service on the objects writes one",
                op: Op::Put,
                allowed: true,
            },
            Probe {
                case: "and is granted the read by the same statement",
                op: Op::Get,
                allowed: true,
            },
        ],
    },
    Row {
        form: Form::ClientFactory,
        actions: &["s3:GetObject", "s3:DeleteObject"],
        scope: Scope::Every,
        probes: &[
            Probe {
                case: "a role granting the read over every resource reads the object",
                op: Op::Get,
                allowed: true,
            },
            Probe {
                case: "and is granted the delete by the same statement",
                op: Op::Delete,
                allowed: true,
            },
            Probe {
                case: "the same role is granted no write by it",
                op: Op::Put,
                allowed: false,
            },
        ],
    },
];

/// The role one object row runs as, and the function it runs in — named here because the
/// refusal's own sentence names both: the identity a function's call is signed as is the session
/// `arn:aws:sts::<account>:assumed-role/<role>/<function>`.
fn role_name(index: usize) -> String {
    format!("xsvc-in-fn-sdk-role-{index}")
}
fn function_name(index: usize) -> String {
    format!("xsvc-in-fn-sdk-probe-{index}")
}

/// The assumed-role session a call made by an invocation of the function speaks for.
fn session_arn(role: &str, function: &str) -> String {
    format!("arn:aws:sts::{ACCOUNT}:assumed-role/{role}/{function}")
}

/// The ARN a statement and a refusal name one object by — S3's own ARN form, which carries
/// neither a Region nor an account.
fn object_arn(key: &str) -> String {
    format!("arn:aws:s3:::{BUCKET}/{key}")
}

/// The sentence Amazon S3 refuses an object operation nothing in force allows with. Written out
/// here rather than built from the service's own helper, so the wording has something to be
/// wrong about: who was refused, which IAM action on which object — double-quoted, as S3 words
/// it — and why.
fn refusal_message(index: usize, action: &str, key: &str) -> String {
    format!(
        "User: {} is not authorized to perform: {action} on resource: \"{}\" because no \
         identity-based policy allows the {action} action",
        session_arn(&role_name(index), &function_name(index)),
        object_arn(key),
    )
}

/// The ARN of the event bus the publication test's handler publishes onto, which is the
/// resource that publication is authorized on.
fn bus_arn(bus: &str) -> String {
    format!("arn:aws:events:{REGION}:{ACCOUNT}:event-bus/{bus}")
}

/// The emulator as it is ASSEMBLED, on one engine: a handler's own SDK call is dispatched back
/// into this same router, so the bucket it writes and the bus it publishes onto are the ones
/// this suite reads.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate; the
    // credential scope is what routes a request.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request. Its credential is the long-term key every emulator publishes, so these calls
/// are the account's own and not any row's role's.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
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

/// One Amazon S3 call on the path-style address, made as the account's own credential.
async fn s3(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: &[u8],
) -> (u16, Vec<u8>) {
    let host = format!("s3.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("content-type", "application/octet-stream")
        .header("authorization", authorization("s3"))
        .body(Bytes::from(body.to_vec()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, response.body().to_vec())
}

/// One `X-Amz-Target` RPC call of the service whose signing name is `signing_name`, which must
/// be served.
async fn rpc(
    router: &MultiServiceRouter,
    case: &str,
    signing_name: &str,
    target_prefix: &str,
    json_version: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header(
            "content-type",
            format!("application/x-amz-json-{json_version}"),
        )
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .header("authorization", authorization(signing_name))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {signing_name} {operation}, got {status} with \
         {parsed}"
    );
    parsed
}

/// One EventBridge call (`awsJson1_1`, `AWSEvents.<Operation>`) that must be served.
async fn events(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    rpc(router, case, "events", "AWSEvents", "1.1", operation, body).await
}

/// One Amazon SQS call (`awsJson1_0`, `AmazonSQS.<Operation>`) that must be served.
async fn sqs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    rpc(router, case, "sqs", "AmazonSQS", "1.0", operation, body).await
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

/// One inline document: the log actions every execution role needs, and the statement a case
/// grants besides them where it grants one.
fn role_document(actions: &[&str], resource: &str) -> String {
    let mut statements = vec![json!({
        "Effect": "Allow",
        "Action": ["logs:CreateLogGroup", "logs:CreateLogStream", "logs:PutLogEvents"],
        "Resource": "*",
    })];
    if !actions.is_empty() {
        statements.push(json!({
            "Effect": "Allow",
            "Action": actions,
            "Resource": resource,
        }));
    }
    json!({"Version": "2012-10-17", "Statement": statements}).to_string()
}

/// Create the role `name` carrying `document`, and answer the ARN IAM minted for it.
async fn create_role(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    document: &str,
) -> String {
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
        "case {case:?}: CreateRole({name:?}) must succeed, got {status} with {created}"
    );
    let arn = opt_element(&created, "Arn").unwrap_or_else(|| {
        panic!("case {case:?}: CreateRole({name:?}) must answer the role's ARN, got {created}")
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
        "case {case:?}: PutRolePolicy({name:?}) must succeed, got {status} with {embedded}"
    );
    arn
}

/// Create one bucket of this account.
async fn create_bucket(router: &MultiServiceRouter, case: &str, bucket: &str) {
    let (status, created) = s3(router, "PUT", &format!("/{bucket}"), b"").await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateBucket({bucket:?}) must succeed, got {status} with {}",
        String::from_utf8_lossy(&created)
    );
}

/// Write one object under `key` as the ACCOUNT's own credential, which a probe's own write
/// cannot be confused with.
async fn plant_object(router: &MultiServiceRouter, case: &str, key: &str) {
    let (status, written) = s3(
        router,
        "PUT",
        &format!("/{BUCKET}/{key}"),
        PLANTED.as_bytes(),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: planting the object under {key:?} must succeed, got {status} with {}",
        String::from_utf8_lossy(&written)
    );
}

/// The bytes the bucket holds under `key`, read back through S3's own `GetObject` as the
/// account's own credential, or `None` where it holds no object there.
async fn held_object(router: &MultiServiceRouter, case: &str, key: &str) -> Option<String> {
    let (status, body) = s3(router, "GET", &format!("/{BUCKET}/{key}"), b"").await;
    let text = String::from_utf8_lossy(&body).to_string();
    match status {
        200 => Some(text),
        404 => None,
        other => panic!(
            "case {case:?}: reading {key:?} back must answer 200 or 404, got {other} with {text}"
        ),
    }
}

/// One function a deployment declares: its logical id in the template, the name it carries, the
/// role it runs as and the handler module it runs.
struct Declared {
    logical: String,
    name: String,
    role_arn: String,
    source: String,
}

/// The stack declaring one function per entry of `functions`: the inline source is archived
/// into the deployment package by CloudFormation, which is how a function whose handler is
/// written here comes to exist at all.
fn function_stack(functions: &[Declared]) -> String {
    let mut resources = serde_json::Map::new();
    for declared in functions {
        resources.insert(
            declared.logical.clone(),
            json!({
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": declared.name,
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": declared.role_arn,
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": {"ZipFile": declared.source},
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

/// Deploy the functions' stack, wait for `CREATE_COMPLETE` and answer the physical id of each
/// declared function.
async fn deploy_functions(
    router: &MultiServiceRouter,
    stack: &str,
    functions: &[Declared],
) -> BTreeMap<String, String> {
    let template = function_stack(functions);
    let (status, answered) = query(
        router,
        "cloudformation",
        "2010-05-15",
        "CreateStack",
        &[("StackName", stack), ("TemplateBody", &template)],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateStack({stack:?}) must be accepted, got {status} with {answered}"
    );
    // The stack's completion is owed from the instant Lambda reports its functions created,
    // which no fixed sleep can name: this looks until it reports it, with a deadline generous
    // enough that a loaded host does not decide the outcome.
    let mut reported = stack_status(router, stack).await;
    for _ in 0..LOOKS {
        if reported.0 == CREATE_COMPLETE {
            break;
        }
        tokio::time::sleep(LOOK).await;
        reported = stack_status(router, stack).await;
    }
    let failures = if reported.0 == CREATE_COMPLETE {
        Vec::new()
    } else {
        let (_, stack_events) = query(
            router,
            "cloudformation",
            "2010-05-15",
            "DescribeStackEvents",
            &[("StackName", stack)],
        )
        .await;
        elements(&stack_events, "ResourceStatusReason")
    };
    assert_eq!(
        reported.0, CREATE_COMPLETE,
        "setup: the stack {stack:?} must reach {CREATE_COMPLETE}, got {} with resource failures \
         {failures:?}",
        reported.1
    );
    let (status, resources) = query(
        router,
        "cloudformation",
        "2010-05-15",
        "DescribeStackResources",
        &[("StackName", stack)],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: DescribeStackResources({stack:?}) must succeed, got {status} with {resources}"
    );
    physical_ids_of(&resources)
}

/// The physical id the stack reports for `logical`, asserted to be the name the template asked
/// for, waited until Lambda reports it `Active` — the state an invocation needs.
async fn ready_function(
    router: &MultiServiceRouter,
    physical: &BTreeMap<String, String>,
    declared: &Declared,
) -> String {
    let function = physical
        .get(&declared.logical)
        .cloned()
        .unwrap_or_else(|| {
            panic!(
                "setup: the stack must report a physical id for {}",
                declared.logical
            )
        });
    assert_eq!(
        function, declared.name,
        "setup: the stack must name the function {:?}, got {function:?}",
        declared.name
    );
    let mut last = Value::Null;
    for _ in 0..LOOKS {
        let (_, _, configuration) = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{function}/configuration"),
            &[],
        )
        .await;
        if configuration["State"].as_str() == Some("Active") {
            return function;
        }
        last = configuration;
        tokio::time::sleep(LOOK).await;
    }
    panic!("setup: Lambda must report {function:?} Active, last reported {last}");
}

/// `Invoke` `function` synchronously with `event`, asserting the invocation itself was served
/// and the handler ran to completion — HTTP 200 with no `X-Amz-Function-Error` — and answering
/// the payload the handler returned.
async fn invoke(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    event: &Value,
) -> Value {
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
        "case {case:?}: the handler must run to completion, got the function-error signal \
         {faulted:?} with {payload}"
    );
    payload
}

/// A queue carrying the grant a rule's target needs, answering its url and the ARN a rule names
/// it by.
async fn queue(router: &MultiServiceRouter, case: &str, name: &str) -> (String, String) {
    let created = sqs(
        router,
        case,
        "CreateQueue",
        &json!({
            "QueueName": name,
            "Attributes": {"VisibilityTimeout": VISIBILITY_TIMEOUT},
        }),
    )
    .await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no QueueUrl in {created}"))
        .to_string();
    let attributes = sqs(
        router,
        case,
        "GetQueueAttributes",
        &json!({"QueueUrl": &url, "AttributeNames": ["QueueArn"]}),
    )
    .await;
    let arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no QueueArn in {attributes}"))
        .to_string();
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": EVENTS_PRINCIPAL},
            "Action": "sqs:SendMessage",
            "Resource": &arn,
        }],
    })
    .to_string();
    sqs(
        router,
        case,
        "SetQueueAttributes",
        &json!({"QueueUrl": &url, "Attributes": {"Policy": policy}}),
    )
    .await;
    (url, arn)
}

/// An ENABLED rule on `bus` matching `detail_type` of this suite's own source, carrying the
/// queue at `queue_arn` as its one target.
async fn rule_with_target(
    router: &MultiServiceRouter,
    case: &str,
    bus: &str,
    name: &str,
    detail_type: &str,
    queue_arn: &str,
) {
    events(
        router,
        case,
        "PutRule",
        &json!({
            "Name": name,
            "EventBusName": bus,
            "EventPattern": json!({"source": [SOURCE], "detail-type": [detail_type]})
                .to_string(),
            "State": "ENABLED",
        }),
    )
    .await;
    let targeted = events(
        router,
        case,
        "PutTargets",
        &json!({
            "Rule": name,
            "EventBusName": bus,
            "Targets": [{"Id": "t1", "Arn": queue_arn}],
        }),
    )
    .await;
    assert_eq!(
        targeted["FailedEntryCount"],
        json!(0),
        "case {case:?}: every target of {name:?} must be accepted, got {targeted}"
    );
}

/// Every envelope now waiting on the queue at `url`, taken off it.
async fn drain(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<Value> {
    let received = sqs(
        router,
        case,
        "ReceiveMessage",
        &json!({"QueueUrl": url, "MaxNumberOfMessages": 10}),
    )
    .await;
    received["Messages"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|message| {
            let raw = message["Body"].as_str().unwrap_or_default();
            serde_json::from_str::<Value>(raw).unwrap_or(Value::Null)
        })
        .collect()
}

/// Collect envelopes off the queue at `url` until one whose `detail.marker` is `marker` has
/// arrived or the deadline passes, and answer everything that arrived.
async fn collect_until(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    marker: &str,
) -> Vec<Value> {
    let mut arrived: Vec<Value> = Vec::new();
    for _ in 0..LOOKS {
        arrived.extend(drain(router, case, url).await);
        if arrived
            .iter()
            .any(|envelope| envelope.pointer("/detail/marker") == Some(&json!(marker)))
        {
            return arrived;
        }
        tokio::time::sleep(LOOK).await;
    }
    arrived
}

/// The `detail.marker` of every envelope of `arrived`, sorted.
fn markers(arrived: &[Value]) -> Vec<String> {
    let mut found: Vec<String> = arrived
        .iter()
        .map(|envelope| {
            envelope
                .pointer("/detail/marker")
                .and_then(Value::as_str)
                .unwrap_or("<no marker>")
                .to_string()
        })
        .collect();
    found.sort();
    found
}

/// RULE: a handler whose module scope imports the SDK the runtime supplies RUNS, and the object
/// operation it makes with that SDK is decided against the policies in force on its execution
/// role — the invocation is HTTP 200 with no function-error signal either way, its payload is
/// the handler's own report, and what the bucket holds afterwards follows the decision.
#[tokio::test]
async fn an_imported_sdks_object_call_is_decided_against_its_execution_role() {
    let router = build_router();
    create_bucket(&router, "setup", BUCKET).await;
    create_bucket(&router, "setup", OTHER_BUCKET).await;

    let mut declared = Vec::new();
    for (index, row) in ROWS.iter().enumerate() {
        let role_arn = create_role(
            &router,
            "setup",
            &role_name(index),
            &role_document(row.actions, &row.scope.resource()),
        )
        .await;
        declared.push(Declared {
            logical: format!("Probe{index}"),
            name: function_name(index),
            role_arn,
            source: row.form.source(),
        });
    }
    let physical = deploy_functions(&router, "xsvc-in-fn-sdk-objects", &declared).await;

    for (index, row) in ROWS.iter().enumerate() {
        let function = ready_function(&router, &physical, &declared[index]).await;

        for (probe_index, probe) in row.probes.iter().enumerate() {
            let case = probe.case;
            let key = format!("row-{index}/probe-{probe_index}.txt");
            let body = format!("written-by-{index}-{probe_index}");
            if probe.op.plants_an_object() {
                plant_object(&router, case, &key).await;
            }

            let payload = invoke(
                &router,
                case,
                &function,
                &json!({
                    "op": probe.op.event(),
                    "bucket": BUCKET,
                    "key": key,
                    "body": body,
                }),
            )
            .await;

            // What the handler reports is what Amazon S3 answered its call with.
            let expected = if probe.allowed {
                match probe.op {
                    Op::Put | Op::Delete => json!({"ok": true}),
                    Op::Get => json!({"ok": true, "body": PLANTED}),
                }
            } else {
                json!({
                    "ok": false,
                    "code": ACCESS_DENIED,
                    "message": refusal_message(index, probe.op.action(), &key),
                    "status": ACCESS_DENIED_STATUS,
                })
            };
            assert_eq!(
                payload, expected,
                "case {case:?}: expected the handler to report {expected}, got {payload}"
            );

            // And what the BUCKET holds: the effect of a served call is there, and a refused
            // one left the bucket exactly as it was.
            let expected_object = match (probe.allowed, probe.op) {
                (true, Op::Put) => Some(body),
                (true, Op::Get) => Some(PLANTED.to_string()),
                (true, Op::Delete) => None,
                (false, op) if op.plants_an_object() => Some(PLANTED.to_string()),
                (false, _) => None,
            };
            let held = held_object(&router, case, &key).await;
            assert_eq!(
                held, expected_object,
                "case {case:?}: expected the bucket to hold {expected_object:?} under {key:?}, \
                 got {held:?}"
            );
        }
    }
}

/// RULE: the publication a handler makes with the SDK the runtime supplies is served against
/// the policies in force on its execution role, and EventBridge then routes each published
/// entry by its CONTENT — so every entry a rule of the bus matches reaches that rule's target,
/// an entry no rule matches reaches none, and a rule whose pattern matches nothing published
/// receives nothing.
#[tokio::test]
async fn an_imported_sdks_publication_reaches_every_matching_rules_queue_target() {
    let case = "a handler that publishes with the SDK its runtime supplies";
    let router = build_router();
    let bus = "xsvc-in-fn-sdk-bus";
    events(&router, case, "CreateEventBus", &json!({"Name": bus})).await;

    // One queue per rule: two rules matching a `detail-type` the handler publishes, and one
    // whose pattern names a type nothing here ever publishes.
    let (written_url, written_arn) = queue(&router, case, "xsvc-in-fn-sdk-written").await;
    let (report_url, report_arn) = queue(&router, case, "xsvc-in-fn-sdk-reports").await;
    let (quiet_url, quiet_arn) = queue(&router, case, "xsvc-in-fn-sdk-quiet").await;
    rule_with_target(
        &router,
        case,
        bus,
        "xsvc-in-fn-sdk-written",
        WRITTEN_TYPE,
        &written_arn,
    )
    .await;
    rule_with_target(
        &router,
        case,
        bus,
        "xsvc-in-fn-sdk-reports",
        REPORT_TYPE,
        &report_arn,
    )
    .await;
    rule_with_target(
        &router,
        case,
        bus,
        "xsvc-in-fn-sdk-quiet",
        NEVER_TYPE,
        &quiet_arn,
    )
    .await;

    let role_arn = create_role(
        &router,
        case,
        "xsvc-in-fn-sdk-publisher-role",
        &role_document(&[PUT_EVENTS_ACTION], &bus_arn(bus)),
    )
    .await;
    let declared = [Declared {
        logical: "Publisher".to_string(),
        name: "xsvc-in-fn-sdk-publisher".to_string(),
        role_arn,
        source: PUBLISHER_SOURCE.to_string(),
    }];
    let physical = deploy_functions(&router, "xsvc-in-fn-sdk-publications", &declared).await;
    let function = ready_function(&router, &physical, &declared[0]).await;

    // (the case, the entries of one PutEvents as (detail-type, marker))
    let batches: [(&str, &[(&str, &str)]); 2] = [
        (
            "one entry, matched by one rule",
            &[(WRITTEN_TYPE, "single-written")],
        ),
        (
            "three entries: one per matching rule, and one no rule matches",
            &[
                (WRITTEN_TYPE, "batch-written"),
                (REPORT_TYPE, "batch-report"),
                (UNWATCHED_TYPE, "batch-unwatched"),
            ],
        ),
    ];

    let mut on_written: Vec<Value> = Vec::new();
    let mut on_report: Vec<Value> = Vec::new();
    for (batch_case, entries) in batches {
        let event = json!({
            "bus": bus,
            "entries": entries
                .iter()
                .map(|(detail_type, marker)| json!({
                    "source": SOURCE,
                    "detail_type": detail_type,
                    "detail": {"marker": marker, "bucket": BUCKET},
                }))
                .collect::<Vec<Value>>(),
        });
        let payload = invoke(&router, batch_case, &function, &event).await;
        let expected = json!({"ok": true, "failed": 0, "ids": entries.len()});
        assert_eq!(
            payload, expected,
            "case {batch_case:?}: expected the handler to report {expected}, got {payload}"
        );

        // Each matched entry reaches its own rule's target. The arrival is polled for, not
        // waited out: it is owed from the instant the publication was accepted.
        for (detail_type, marker) in entries {
            let (url, arrived) = match *detail_type {
                WRITTEN_TYPE => (&written_url, &mut on_written),
                REPORT_TYPE => (&report_url, &mut on_report),
                _ => continue,
            };
            let collected = collect_until(&router, batch_case, url, marker).await;
            arrived.extend(collected);
            let seen = markers(arrived);
            assert!(
                seen.iter().any(|found| found.as_str() == *marker),
                "case {batch_case:?}: expected the entry {marker:?} on the queue its rule \
                 targets, got {seen:?}"
            );
            // The envelope carries the publication's own members, and this account and Region.
            let envelope = arrived
                .iter()
                .find(|envelope| envelope.pointer("/detail/marker") == Some(&json!(marker)))
                .expect("the envelope just asserted to be there");
            for (pointer, expected) in [
                ("/source", json!(SOURCE)),
                ("/detail-type", json!(detail_type)),
                ("/account", json!(ACCOUNT)),
                ("/region", json!(REGION)),
                ("/detail", json!({"marker": marker, "bucket": BUCKET})),
            ] {
                let actual = envelope.pointer(pointer).cloned().unwrap_or(Value::Null);
                assert_eq!(
                    actual, expected,
                    "case {batch_case:?}: expected {pointer} of the delivered envelope to be \
                     {expected}, got {actual} in {envelope}"
                );
            }
        }
    }

    // Both matched entries of the batch have arrived, so the publication has been routed: at
    // this instant the entry no rule matched has reached neither queue, and the rule whose
    // pattern names a type nothing published has received nothing at all.
    on_written.extend(drain(&router, case, &written_url).await);
    on_report.extend(drain(&router, case, &report_url).await);
    assert_eq!(
        markers(&on_written),
        vec!["batch-written".to_string(), "single-written".to_string()],
        "case {case:?}: expected exactly the entries of the matching detail-type on the rule's \
         queue, got {:?}",
        markers(&on_written)
    );
    assert_eq!(
        markers(&on_report),
        vec!["batch-report".to_string()],
        "case {case:?}: expected exactly the one entry of the other matching detail-type, got \
         {:?}",
        markers(&on_report)
    );
    let quiet = drain(&router, case, &quiet_url).await;
    assert!(
        quiet.is_empty(),
        "case {case:?}: a rule whose pattern matches nothing published must receive nothing, \
         got {:?}",
        markers(&quiet)
    );
}
