// SPDX-License-Identifier: CC0-1.0

//! The AWS SDK a managed `python3.x` runtime puts on a handler's import path, used to read a
//! `SecureString` parameter sealed under a customer-managed KMS key.
//!
//! RULE: AWS's managed `python3.x` runtimes carry the AWS SDK for Python in the execution
//! environment, so a handler that imports `boto3` or `botocore` AT MODULE SCOPE is loaded
//! without the function shipping a dependency of its own — the invocation answers 200 with no
//! `FunctionError` and with whatever the handler returned, never a `Runtime.ImportModuleError`
//! for one of those modules — and a call that SDK makes is made as a session of the function's
//! execution role, so a `GetParameter(WithDecryption=true)` of a `SecureString` sealed under a
//! customer-managed key answers the plaintext when the role's documents allow both
//! `ssm:GetParameter` on the parameter and `kms:Decrypt` on that key, and is refused
//! `AccessDeniedException` (HTTP 400) naming the assumed-role session, the action the missing
//! grant covers and the resource it is missing on when either is absent.
//!
//! "The execution role grants the function permission to use Amazon Web Services services"
//! (`FunctionConfiguration$Role`), and the identity such a call speaks for is the session
//! `arn:aws:sts::<account>:assumed-role/<role>/<function>`; with neither an identity-based nor
//! a resource-based allow a request is denied (IAM User Guide, *Policy evaluation logic*).
//! Parameter Store holds a `SecureString`'s value as the ciphertext its key sealed, so the
//! decrypting read performs `kms:Decrypt` on that key for the caller, while "This flag is
//! ignored for String and StringList parameter types"
//! (`smithy_query(query="doc",shape="GetParameterRequest",member="WithDecryption")`) — a
//! `String` parameter reaches no key at all, whatever the flag says.
//!
//! Five crates have to agree for that to hold — Lambda runs the handler and supplies it the
//! SDK, CloudFormation archives the inline handler into a deployment package this emulator can
//! run, IAM holds each role and its inline document, STS mints the session the function's calls
//! are signed with, Systems Manager seals and reports the value, and KMS holds the key and takes
//! the decrypt decision — so the suite lives here: a crate-local suite can only ever see one
//! side of it. `ssm_securestring_ciphertext_decrypts_in_kms` pins the ciphertext and the key's
//! decision for a client calling the two services itself; what only this suite reaches is the
//! same workflow run by a handler with the SDK the runtime supplies.
//!
//! The rows differ along what the rule generalises over:
//!
//! * the `python3.x` runtime the function declares (four of the `Runtime` enum's `python3.x`
//!   values, source: `smithy_query(query="shape",shape="Runtime")`), because the SDK belongs to
//!   every managed Python runtime rather than to one point release;
//! * the FORM the SDK is imported in — `import boto3`, a from-import of the session class
//!   (`from boto3.session import Session`), and botocore's own session
//!   (`import botocore.session`) — each reporting the module names its imports resolved to;
//! * the parameter read: a hierarchical name, a sibling under the same path, a flat one, and a
//!   `String` that is not encrypted at all, with values of different bytes (plain text, a JSON
//!   document, text carrying a newline and non-ASCII characters);
//! * what the role's one inline document allows: the read and the decrypt, the read alone, and
//!   the decrypt alone — so each half of the refusal is asked for on its own.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::stack_status;
use crate::query_wire::{elements, form_encode, opt_element, physical_ids_of};

/// The Region every request here is signed for, and so the Region the key, the parameters and
/// the functions belong to.
const REGION: &str = "us-east-1";

/// The account this emulator serves, which every ARN below carries.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The stack that declares one function per row.
const STACK: &str = "xsvc-in-fn-ssm-cmk-probes";

/// The `StackStatus` that stack must reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The code and status an authorization refusal of either service is answered with: the
/// AWS-common `AccessDeniedException`, carried at the client-error status because an AWS JSON
/// protocol discriminates a failure by the error type in the body.
const ACCESS_DENIED: &str = "AccessDeniedException";
const ACCESS_DENIED_STATUS: u16 = 400;

/// The IAM action the handler's read performs, and the one the decrypt of the stored value
/// performs on the parameter's key.
const READ_ACTION: &str = "ssm:GetParameter";
const DECRYPT_ACTION: &str = "kms:Decrypt";

/// How long one invocation is given, as the function's own `Timeout`: well past what a run
/// needs on any host, because what is under test is the SDK and the decision rather than how
/// fast an interpreter gets through the handler.
const FUNCTION_TIMEOUT: u64 = 120;

/// How often to look while waiting for work the clock owes — a stack reaching
/// `CREATE_COMPLETE`, a created function reaching `Active` — and how many looks to take. The
/// spans are those services' own, so no fixed sleep names the instant one ends, and the
/// deadline is generous so a loaded host cannot decide the outcome.
const LOOK: Duration = Duration::from_millis(100);
const LOOKS: usize = 1_800;

/// What every handler below does once it holds a client, whichever form built it: read the one
/// parameter the event names and report either the parameter the SDK answered with or the
/// refusal it raised — its code, its sentence and the HTTP status it came back on. `IMPORTED`
/// is what the module-scope imports of the head above resolved to, which is reported on every
/// answer: an import that did not resolve would have faulted the load instead.
const HANDLER_TAIL: &str = r#"

def handler(event, context):
    client = _client()
    try:
        answered = client.get_parameter(
            Name=event["name"], WithDecryption=event["decrypt"]
        )
    except ClientError as refused:
        return {
            "ok": False,
            "imported": IMPORTED,
            "code": refused.response["Error"]["Code"],
            "message": refused.response["Error"]["Message"],
            "status": refused.response["ResponseMetadata"]["HTTPStatusCode"],
        }
    parameter = answered["Parameter"]
    return {
        "ok": True,
        "imported": IMPORTED,
        "value": parameter["Value"],
        "type": parameter["Type"],
        "arn": parameter["ARN"],
    }
"#;

/// WHICH form a row's handler imports the SDK in.
#[derive(Debug, Clone, Copy)]
enum Form {
    /// `import boto3`, the form AWS's own examples use.
    Boto3Module,
    /// A from-import of the session class, which is what an application that builds its own
    /// session imports.
    Boto3Session,
    /// botocore's own session, the lower half of the SDK the upper half is built on.
    BotocoreSession,
}

impl Form {
    /// The module-scope head of the handler: the imports, the module names they resolved to,
    /// and the client builder [`HANDLER_TAIL`] calls.
    fn head(self) -> &'static str {
        match self {
            Self::Boto3Module => {
                r#"
import boto3
import botocore
from botocore.exceptions import ClientError

IMPORTED = [boto3.__name__, botocore.__name__, ClientError.__module__]


def _client():
    return boto3.client("ssm")
"#
            }
            Self::Boto3Session => {
                r#"
from boto3.session import Session
from botocore.exceptions import ClientError

IMPORTED = [Session.__module__, ClientError.__module__]


def _client():
    return Session().client("ssm")
"#
            }
            Self::BotocoreSession => {
                r#"
import botocore.session
from botocore.exceptions import ClientError

IMPORTED = [botocore.session.__name__, ClientError.__module__]


def _client():
    return botocore.session.get_session().create_client("ssm")
"#
            }
        }
    }

    /// The module names the head's imports must resolve to, which the answer reports: the
    /// runtime supplies these modules, so each name is the dotted path of a module of the SDK
    /// and not of anything the deployment package shipped.
    fn imported(self) -> Vec<&'static str> {
        match self {
            Self::Boto3Module => vec!["boto3", "botocore", "botocore.exceptions"],
            Self::Boto3Session => vec!["boto3.session", "botocore.exceptions"],
            Self::BotocoreSession => vec!["botocore.session", "botocore.exceptions"],
        }
    }

    /// The whole source of a row's handler.
    fn source(self) -> String {
        format!("{}{HANDLER_TAIL}", self.head())
    }
}

/// One parameter the handlers read: what was written, and whether it is sealed under the
/// customer-managed key.
struct Parameter {
    case: &'static str,
    name: &'static str,
    value: &'static str,
    /// The `Type` the put wrote and a read reports.
    type_: &'static str,
}

impl Parameter {
    /// Whether reading this parameter decrypted performs `kms:Decrypt` on the key: only a
    /// `SecureString` holds a ciphertext, and the flag "is ignored for String and StringList
    /// parameter types".
    fn is_sealed(&self) -> bool {
        self.type_ == "SecureString"
    }
}

/// The parameters the rows read: a hierarchical name, a sibling under the same path, a flat
/// one, and a `String` under the same path that no key is involved in — with values of
/// different bytes.
const PARAMETERS: &[Parameter] = &[
    Parameter {
        case: "a hierarchical SecureString of plain text",
        name: "/xsvc-in-fn-cmk/db/password",
        value: "pa55w0rd-sealed-under-the-cmk",
        type_: "SecureString",
    },
    Parameter {
        case: "a sibling SecureString holding a JSON document",
        name: "/xsvc-in-fn-cmk/db/connection",
        value: r#"{"user":"app","password":"s3cret","port":5432}"#,
        type_: "SecureString",
    },
    Parameter {
        case: "a flat SecureString holding a newline and non-ASCII characters",
        name: "xsvc-in-fn-cmk-flat",
        value: "first line\nsecond lïne ✓ ключ",
        type_: "SecureString",
    },
    Parameter {
        case: "a String, which the decryption flag is ignored for",
        name: "/xsvc-in-fn-cmk/db/endpoint",
        value: "db.internal.example:5432",
        type_: "String",
    },
];

/// What one row's execution role is allowed by its own inline document.
#[derive(Debug, Clone, Copy)]
enum Grant {
    /// The read and the decrypt: the pair a function reading a CMK-encrypted parameter needs.
    ReadAndDecrypt,
    /// The read alone, with no permission on the parameter's key.
    ReadOnly,
    /// The decrypt alone, with no permission to read a parameter.
    DecryptOnly,
}

impl Grant {
    fn reads(self) -> bool {
        matches!(self, Self::ReadAndDecrypt | Self::ReadOnly)
    }

    fn decrypts(self) -> bool {
        matches!(self, Self::ReadAndDecrypt | Self::DecryptOnly)
    }
}

/// One row: a function of its own, with the runtime it declares, the form its handler imports
/// the SDK in, the document its execution role carries, and the parameters it reads.
struct Row {
    case: &'static str,
    /// A `python3.x` runtime identifier of the `Runtime` enum's value set.
    runtime: &'static str,
    form: Form,
    grant: Grant,
    /// Which of [`PARAMETERS`] this row's handler is asked to read, by index.
    reads: &'static [usize],
}

const ROWS: &[Row] = &[
    Row {
        case: "a module-scope `import boto3` reads every parameter",
        runtime: "python3.12",
        form: Form::Boto3Module,
        grant: Grant::ReadAndDecrypt,
        reads: &[0, 1, 2, 3],
    },
    Row {
        case: "a from-imported session class builds the same client",
        runtime: "python3.13",
        form: Form::Boto3Session,
        grant: Grant::ReadAndDecrypt,
        reads: &[0, 2],
    },
    Row {
        case: "botocore's own session creates the client",
        runtime: "python3.11",
        form: Form::BotocoreSession,
        grant: Grant::ReadAndDecrypt,
        reads: &[1, 3],
    },
    Row {
        case: "a role holding the read but not the decrypt",
        runtime: "python3.9",
        form: Form::Boto3Module,
        grant: Grant::ReadOnly,
        reads: &[0, 3],
    },
    Row {
        case: "a role holding the decrypt but not the read",
        runtime: "python3.12",
        form: Form::Boto3Module,
        grant: Grant::DecryptOnly,
        reads: &[0],
    },
];

/// The emulator as it is ASSEMBLED, on one engine: a handler's own SDK call is dispatched back
/// into this same router, so the parameter it reads is the one this suite wrote.
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

/// One awsJson1_1 call of `service`, under the `X-Amz-Target` prefix `target_prefix`, made as
/// the account's own credential.
async fn rpc(
    router: &MultiServiceRouter,
    service: &str,
    target_prefix: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{service}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .header("authorization", authorization(service))
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

/// The ARN of the parameter `name` names in this Region: a hierarchical name keeps its whole
/// path after the `parameter/` prefix, and the leading separator is not part of it.
fn parameter_arn(name: &str) -> String {
    format!(
        "arn:aws:ssm:{REGION}:{ACCOUNT}:parameter/{}",
        name.strip_prefix('/').unwrap_or(name)
    )
}

/// The role one row runs as, and the function it runs in — named here because a refusal's own
/// sentence names both: the identity a function's call is signed as is the session
/// `arn:aws:sts::<account>:assumed-role/<role>/<function>`.
fn role_name(index: usize) -> String {
    format!("xsvc-in-fn-cmk-role-{index}")
}
fn function_name(index: usize) -> String {
    format!("xsvc-in-fn-cmk-probe-{index}")
}
fn session_arn(index: usize) -> String {
    format!(
        "arn:aws:sts::{ACCOUNT}:assumed-role/{}/{}",
        role_name(index),
        function_name(index)
    )
}

/// The sentence AWS words for an action no identity-based policy in force allows. Written out
/// here rather than built from either service's own helper, so the wording has something to be
/// wrong about.
fn no_identity_policy_refusal(principal: &str, action: &str, resource: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {action} on resource: {resource} \
         because no identity-based policy allows the {action} action"
    )
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

/// The inline document one row's role carries: the log actions every execution role needs, and
/// whichever of the read and the decrypt the row's grant holds.
fn role_document(grant: Grant, key_arn: &str) -> String {
    let mut statements = vec![json!({
        "Effect": "Allow",
        "Action": ["logs:CreateLogGroup", "logs:CreateLogStream", "logs:PutLogEvents"],
        "Resource": "*",
    })];
    if grant.reads() {
        statements.push(json!({
            "Effect": "Allow",
            "Action": READ_ACTION,
            "Resource": parameter_arn("*"),
        }));
    }
    if grant.decrypts() {
        statements.push(json!({
            "Effect": "Allow",
            "Action": DECRYPT_ACTION,
            "Resource": key_arn,
        }));
    }
    json!({"Version": "2012-10-17", "Statement": statements}).to_string()
}

/// Create a symmetric encryption key through KMS's own `CreateKey`, with the account-root key
/// policy KMS writes for a key whose create names none — the policy that delegates the decision
/// to the calling principal's own identity-based policies — and answer its ARN.
async fn create_key(router: &MultiServiceRouter) -> String {
    let (status, created) = rpc(
        router,
        "kms",
        "TrentService",
        "CreateKey",
        &json!({ "Description": "the key this suite's parameters are sealed under" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateKey must succeed, got {status} with {created}"
    );
    created["KeyMetadata"]["Arn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: CreateKey must answer the key's Arn, got {created}"))
        .to_string()
}

/// Write every parameter of [`PARAMETERS`], sealing each `SecureString` under `key_arn`, and
/// assert the account's own decrypting read reports the value back — so what a handler's read
/// answers is compared against a value that round-trips outside the function too.
async fn put_parameters(router: &MultiServiceRouter, key_arn: &str) {
    for parameter in PARAMETERS {
        let case = parameter.case;
        let mut request = json!({
            "Name": parameter.name,
            "Type": parameter.type_,
            "Value": parameter.value,
        });
        if parameter.is_sealed() {
            request["KeyId"] = json!(key_arn);
        }
        let (status, put) = rpc(router, "ssm", "AmazonSSM", "PutParameter", &request).await;
        assert_eq!(
            status, 200,
            "setup {case:?}: PutParameter must succeed, got {status} with {put}"
        );
        let (status, read) = rpc(
            router,
            "ssm",
            "AmazonSSM",
            "GetParameter",
            &json!({ "Name": parameter.name, "WithDecryption": true }),
        )
        .await;
        assert_eq!(
            (status, read["Parameter"]["Value"].as_str()),
            (200, Some(parameter.value)),
            "setup {case:?}: the account's own decrypting read must report the value the put \
             wrote, got {status} with {read}"
        );
        assert_eq!(
            read["Parameter"]["ARN"].as_str(),
            Some(parameter_arn(parameter.name).as_str()),
            "setup {case:?}: the read must report the parameter's own ARN, got {read}"
        );
    }
}

/// Create the role one row runs as, with the document [`role_document`] states, and answer the
/// ARN IAM minted for it.
async fn create_role(
    router: &MultiServiceRouter,
    index: usize,
    grant: Grant,
    key_arn: &str,
) -> String {
    let name = role_name(index);
    let (status, created) = query(
        router,
        "iam",
        "2010-05-08",
        "CreateRole",
        &[
            ("RoleName", &name),
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
            ("RoleName", &name),
            ("PolicyName", "probe-grants"),
            ("PolicyDocument", &role_document(grant, key_arn)),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: PutRolePolicy({name:?}) must succeed, got {status} with {embedded}"
    );
    arn
}

/// The stack declaring one function per row, each with that row's runtime, handler source and
/// execution role: the inline source is archived into the deployment package by CloudFormation,
/// which is how a function whose handler is written here comes to exist at all.
fn function_stack(role_arns: &[String]) -> String {
    let mut resources = serde_json::Map::new();
    for (index, (row, role_arn)) in ROWS.iter().zip(role_arns).enumerate() {
        resources.insert(
            format!("Probe{index}"),
            json!({
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": function_name(index),
                    "Runtime": row.runtime,
                    "Handler": "index.handler",
                    "Role": role_arn,
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": {"ZipFile": row.form.source()},
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
    role_arns: &[String],
) -> BTreeMap<String, String> {
    let template = function_stack(role_arns);
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
    // The stack's completion is owed from the instant Lambda reports its functions created,
    // which no fixed sleep can name: this looks until it reports it, with a deadline generous
    // enough that a loaded host does not decide the outcome.
    let mut reported = stack_status(router, STACK).await;
    for _ in 0..LOOKS {
        if reported.0 == CREATE_COMPLETE {
            break;
        }
        tokio::time::sleep(LOOK).await;
        reported = stack_status(router, STACK).await;
    }
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
        "setup: the stack must reach {CREATE_COMPLETE}, got {} with resource failures \
         {failures:?}",
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

/// What one probe's handler must answer: the parameter the SDK read, or the refusal of
/// whichever service withheld the grant — Parameter Store's own when the role may not read the
/// parameter at all, and the one it relays from the key when the role may read it but not open
/// its value.
fn expected_answer(
    index: usize,
    row: &Row,
    parameter: &Parameter,
    key_arn: &str,
    imported: &[&str],
) -> Value {
    let refused = |action: &str, resource: &str| {
        json!({
            "ok": false,
            "imported": imported,
            "code": ACCESS_DENIED,
            "message": no_identity_policy_refusal(&session_arn(index), action, resource),
            "status": ACCESS_DENIED_STATUS,
        })
    };
    if !row.grant.reads() {
        return refused(READ_ACTION, &parameter_arn(parameter.name));
    }
    // Only a `SecureString` holds a ciphertext, so only its decrypting read needs the key.
    if parameter.is_sealed() && !row.grant.decrypts() {
        return refused(DECRYPT_ACTION, key_arn);
    }
    json!({
        "ok": true,
        "imported": imported,
        "value": parameter.value,
        "type": parameter.type_,
        "arn": parameter_arn(parameter.name),
    })
}

/// Every row: the handler loads with the SDK the runtime supplies, and the read it makes with
/// that SDK is decided against the policies in force on its execution role.
#[tokio::test]
async fn a_handler_reads_a_cmk_encrypted_parameter_with_the_runtimes_own_sdk() {
    let router = build_router();
    let key_arn = create_key(&router).await;
    put_parameters(&router, &key_arn).await;

    let mut role_arns = Vec::new();
    for (index, row) in ROWS.iter().enumerate() {
        role_arns.push(create_role(&router, index, row.grant, &key_arn).await);
    }
    let physical = deploy_functions(&router, &role_arns).await;

    for (index, row) in ROWS.iter().enumerate() {
        let logical = format!("Probe{index}");
        let function = physical
            .get(&logical)
            .cloned()
            .unwrap_or_else(|| panic!("setup: the stack must report a physical id for {logical}"));
        assert_eq!(
            function,
            function_name(index),
            "setup: the stack must name the function {:?}, got {function:?}",
            function_name(index)
        );
        wait_until_active(&router, &function).await;

        let imported = row.form.imported();
        for read in row.reads {
            let parameter = &PARAMETERS[*read];
            let case = format!("{} on {}: {}", row.case, row.runtime, parameter.case);
            let event = json!({ "name": parameter.name, "decrypt": true });
            let (status, headers, payload) = lambda(
                &router,
                "POST",
                &format!("/2015-03-31/functions/{function}/invocations"),
                event.to_string().as_bytes(),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: the invocation must be served, got {status} with {payload}"
            );
            // The SDK is on the import path, so the handler LOADED: a module the runtime did
            // not supply would have faulted the load, which this header and this payload are
            // where the caller reads.
            let faulted = headers
                .get("x-amz-function-error")
                .and_then(|value| value.to_str().ok())
                .map(str::to_string);
            assert_eq!(
                faulted, None,
                "case {case:?}: the handler must run to completion, got {faulted:?} with \
                 {payload}"
            );
            assert_eq!(
                payload.get("errorType"),
                None,
                "case {case:?}: the answer must be the handler's own result rather than an \
                 error document, got {payload}"
            );

            let expected = expected_answer(index, row, parameter, &key_arn, &imported);
            assert_eq!(
                payload, expected,
                "case {case:?}: expected the handler to report {expected}, got {payload}"
            );
        }
    }
}
