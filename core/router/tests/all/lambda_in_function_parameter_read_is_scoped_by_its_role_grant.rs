// SPDX-License-Identifier: CC0-1.0

//! A running function's own Parameter Store read, made with the SDK its managed runtime
//! bundles, decided against the identity-based documents in force on the function's EXECUTION
//! ROLE — one read per parameter, inside one invocation.
//!
//! RULE: a call an AWS SDK makes from inside a running function is signed as a session of that
//! function's execution role, so Systems Manager decides it against the identity-based
//! documents in force on that role — the action and the ARN of the parameter the request
//! addresses included — before it resolves anything: a read those documents allow answers that
//! parameter's own stored `Value` and `ARN`, and a read no readable document allows is refused
//! the AWS-common `AccessDeniedException` at this service's client-error status, carrying IAM's
//! own sentence (the assumed-role session, `ssm:GetParameter`, and the parameter's ARN) and NO
//! value at all. The decision is per parameter and not per invocation: in one run, the same
//! handler holding the same credential is served the names its role's document covers and
//! refused the names it does not.
//!
//! "The execution role grants the function permission to use Amazon Web Services services"
//! (`FunctionConfiguration$Role`), and the identity such a call speaks for is the session
//! `arn:aws:sts::<account>:assumed-role/<role>/<function>`; with neither an identity-based nor
//! a resource-based allow a request is denied (IAM User Guide, *Policy evaluation logic*).
//! `ssm:GetParameter` is the action that read performs ("Actions defined by AWS Systems
//! Manager", IAM Service Authorization Reference), and the resource it is taken on is the
//! parameter's own ARN, which keeps a hierarchical name's whole path after the `parameter/`
//! prefix (`PutParameter`, `Parameter$ARN`) — so a document scoped to one ARN, or to an ARN
//! prefix, covers exactly the names that ARN pattern spells and no others. The runtime a
//! function declares decides only WHICH SDK is on the handler's import path — Lambda's managed
//! Python runtimes supply boto3 and its `nodejs18.x` and later runtimes the AWS SDK for
//! JavaScript v3 — never who the call speaks for or who decides it.
//!
//! Why it matters beyond the refusal itself: a client validating least privilege (a deploy-time
//! check that a function CANNOT read a parameter its role was never granted, a guardrail over a
//! path-scoped policy) passes against an emulator that serves every in-function read and fails
//! in AWS — and worse, a function the emulator proved "works" is denied at runtime there, with
//! every handler branch written for `AccessDeniedException` never exercised at all.
//!
//! Five crates have to agree for that to hold — IAM holds each role and its inline document,
//! STS mints the session the function's calls are signed with, CloudFormation archives the
//! inline handler into a deployment package this emulator can run, Lambda runs that handler and
//! supplies it the SDK of its declared runtime, and Systems Manager holds the parameters and
//! takes the decision — so the suite lives here: a crate-local suite can only ever see one side
//! of it.
//!
//! What only this suite reaches: `lambda_runtime_sdk_drives_parameter_store_and_event_bus` pins
//! the reads and writes of a boto3 handler under one grant shape per function, and
//! `lambda_in_function_secret_read_follows_each_runtimes_sdk` pins the same credential path for
//! another service's read across both SDK families. The rows here differ along what the rule
//! generalises over:
//!
//! * WHICH runtime the function declares and WHICH form its handler reads with (a boto3 client,
//!   a client built from a boto3 `Session`, the v3 bare client sending a command object, the
//!   aggregated v3 client's own operation method);
//! * HOW its execution role's one inline document is written: nothing of this service at all,
//!   the read on ONE parameter's exact ARN, the read on the ARN PREFIX of one hierarchy, the
//!   read on every resource, and a read whose `Condition` states the hierarchy as a path
//!   prefix — which grants nothing at all, because IAM decides a clause against the request's
//!   own condition context and a clause over a key the request carries no value for does not
//!   hold under an operator without the `IfExists` suffix, so the statement applies to no
//!   request (IAM User Guide, *Condition element*). That is why a parameter hierarchy is scoped
//!   in the `Resource` element, which is where the row above it states it: AWS publishes no
//!   condition key for a parameter's own path, and a conditional grant can never widen a
//!   function's reach past what its resource element spells;
//! * WHICH name is read — a hierarchical name under the granted hierarchy, a second hierarchical
//!   name elsewhere under it, and a FLAT name whose ARN merely shares the hierarchy's leading
//!   characters — each read by every row, so each row reports both halves of its own decision.
//!
//! Every expected value is computed here from what the row declares and from the account every
//! ARN this emulator mints carries; nothing is read back out of the answer being checked. The
//! HTTP status each handler reports is reported as TEXT, because the two SDK families hand a
//! handler the status in their own number type and what this suite is about is the status
//! itself.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{elements, form_encode, opt_element, physical_ids_of};

/// The Region every request here is signed for, and so the Region the parameters and the
/// functions belong to.
const REGION: &str = "us-east-1";

/// The account this emulator serves, which every ARN below carries.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The stack that declares one function per row.
const STACK: &str = "xsvc-in-fn-ssm-grants";

/// The `StackStatus` that stack must reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The IAM action the handler's read performs, which a grant names and a refusal blames.
const READ_ACTION: &str = "ssm:GetParameter";

/// The code Systems Manager refuses an unauthorized call with, and the status it comes back on:
/// the AWS-common `AccessDeniedException`, at this service's client-error status because
/// awsJson1_1 discriminates a failure by the error type in the body rather than by the
/// transport status.
const ACCESS_DENIED: &str = "AccessDeniedException";
const ACCESS_DENIED_STATUS_TEXT: &str = "400";

/// The name of the one inline policy each row's role carries.
const POLICY_NAME: &str = "inline";

/// The hierarchy the two hierarchical parameters live in, which one grant form scopes itself to
/// by the ARN prefix of every name under it.
const HIERARCHY: &str = "/xsvc-in-fn-ssm";

/// The three names every row's handler reads, with the value each holds.
///
/// They span the forms the rule generalises over: a name under the granted hierarchy, a second
/// one elsewhere under it, and a FLAT name — one carrying no path at all, whose ARN shares the
/// hierarchy's leading characters without being under it, so an ARN-prefix grant that covered
/// it would be matching text rather than the hierarchy.
const SCOPED: &str = "/xsvc-in-fn-ssm/scoped/db-host";
const SCOPED_VALUE: &str = "db.internal.example:5432";
const ELSEWHERE: &str = "/xsvc-in-fn-ssm/elsewhere/client";
const ELSEWHERE_VALUE: &str = r#"{"region":"us-east-1","retries":3}"#;
const FLAT: &str = "xsvc-in-fn-ssm-flat";
const FLAT_VALUE: &str = "a flat name's value, with = and + in it";

/// Every name a row reads, in the order its handler is handed them.
const NAMES: &[(&str, &str)] = &[
    (SCOPED, SCOPED_VALUE),
    (ELSEWHERE, ELSEWHERE_VALUE),
    (FLAT, FLAT_VALUE),
];

/// How long one invocation is given, as the function's own `Timeout`: well past what three
/// reads take, so nothing here is decided by how fast the host runs.
const FUNCTION_TIMEOUT: u64 = 60;

/// How often to look while waiting for work the clock owes — a created function reaching
/// `Active` — and how many looks to take. The span is that service's own, so no fixed sleep
/// names the instant it ends, and the deadline is generous so a loaded host cannot decide the
/// outcome.
const LOOK: Duration = Duration::from_millis(100);
const LOOKS: usize = 1_200;

/// The Python handler: the SDK a managed Python runtime bundles is boto3, and the plain way to
/// build a client of it is `boto3.client`.
const PYTHON_MODULE_CLIENT: &str = r##"
import boto3
from botocore.exceptions import ClientError


def _client():
    return boto3.client("ssm")


def handler(event, context):
    client = _client()
    answered = {}
    for name in event["names"]:
        try:
            held = client.get_parameter(Name=name)["Parameter"]
        except ClientError as refused:
            answered[name] = {
                "ok": False,
                "code": refused.response["Error"]["Code"],
                "message": refused.response["Error"]["Message"],
                "status": str(refused.response["ResponseMetadata"]["HTTPStatusCode"]),
            }
        else:
            answered[name] = {"ok": True, "value": held["Value"], "arn": held["ARN"]}
    return answered
"##;

/// The same reads through a client built from a boto3 `Session` — the other form boto3
/// publishes for building one, which a handler that wants a session of its own uses.
const PYTHON_SESSION_CLIENT: &str = r##"
import boto3
from botocore.exceptions import ClientError


def _client():
    return boto3.Session().client("ssm")


def handler(event, context):
    client = _client()
    answered = {}
    for name in event["names"]:
        try:
            held = client.get_parameter(Name=name)["Parameter"]
        except ClientError as refused:
            answered[name] = {
                "ok": False,
                "code": refused.response["Error"]["Code"],
                "message": refused.response["Error"]["Message"],
                "status": str(refused.response["ResponseMetadata"]["HTTPStatusCode"]),
            }
        else:
            answered[name] = {"ok": True, "value": held["Value"], "arn": held["ARN"]}
    return answered
"##;

/// The Node handler: the SDK a managed `nodejs18.x`-and-later runtime bundles is the AWS SDK
/// for JavaScript v3, whose bare client sends one COMMAND object per call and throws a refusal
/// as an error carrying the service's own error name and the answer's `$metadata`.
const NODE_COMMAND_CLIENT: &str = r#"
const { SSMClient, GetParameterCommand } = require('@aws-sdk/client-ssm');

exports.handler = async (event) => {
    const client = new SSMClient({});
    const answered = {};
    for (const name of event.names) {
        try {
            const held = await client.send(new GetParameterCommand({ Name: name }));
            answered[name] = {
                ok: true,
                value: held.Parameter.Value,
                arn: held.Parameter.ARN,
            };
        } catch (refused) {
            answered[name] = {
                ok: false,
                code: refused.name,
                message: refused.message,
                status: String(refused.$metadata.httpStatusCode),
            };
        }
    }
    return answered;
};
"#;

/// The same reads through the AGGREGATED client the same v3 package publishes beside the bare
/// one, which carries one method per operation instead of taking a command object.
const NODE_AGGREGATED_CLIENT: &str = r#"
const { SSM } = require('@aws-sdk/client-ssm');

exports.handler = async (event) => {
    const client = new SSM({});
    const answered = {};
    for (const name of event.names) {
        try {
            const held = await client.getParameter({ Name: name });
            answered[name] = {
                ok: true,
                value: held.Parameter.Value,
                arn: held.Parameter.ARN,
            };
        } catch (refused) {
            answered[name] = {
                ok: false,
                code: refused.name,
                message: refused.message,
                status: String(refused.$metadata.httpStatusCode),
            };
        }
    }
    return answered;
};
"#;

/// What one row's execution role is allowed of THIS SERVICE by its own inline document, beside
/// the log actions every execution role carries.
#[derive(Debug, Clone, Copy)]
enum Grant {
    /// Nothing of this service at all.
    Nothing,
    /// The read on ONE parameter's exact ARN.
    OneParameter,
    /// The read on the ARN prefix of every name under one hierarchy.
    OneHierarchy,
    /// The read on every resource, which is what an administrator's grant spells.
    Everything,
    /// The read on the hierarchy, stated as a `Condition` over the parameter's path instead
    /// of in the `Resource` element — a clause no request to this service carries a value
    /// for, so it grants nothing (see the module docs).
    ConditionedPath,
}

impl Grant {
    /// The statements this grant adds to the role's one inline document.
    fn statements(self) -> Vec<Value> {
        match self {
            Self::Nothing => Vec::new(),
            Self::OneParameter => vec![json!({
                "Effect": "Allow",
                "Action": READ_ACTION,
                "Resource": parameter_arn(SCOPED),
            })],
            Self::OneHierarchy => vec![json!({
                "Effect": "Allow",
                "Action": READ_ACTION,
                "Resource": format!("{}/*", parameter_arn(HIERARCHY)),
            })],
            Self::Everything => vec![json!({
                "Effect": "Allow",
                "Action": READ_ACTION,
                "Resource": "*",
            })],
            Self::ConditionedPath => vec![json!({
                "Effect": "Allow",
                "Action": READ_ACTION,
                "Resource": format!("{}/*", parameter_arn(HIERARCHY)),
                "Condition": {
                    "StringLike": {"ssm:ParameterPath": format!("{HIERARCHY}/scoped/*")},
                },
            })],
        }
    }

    /// Whether this grant covers the read of `name`: the exact-ARN grant covers the one name it
    /// spells, the hierarchy's prefix grant covers the names UNDER that hierarchy (its
    /// separator included, so a flat name merely sharing its leading characters is not one of
    /// them), and the wildcard grant covers every name.
    fn covers(self, name: &str) -> bool {
        match self {
            Self::Nothing => false,
            Self::OneParameter => name == SCOPED,
            Self::OneHierarchy => name.starts_with(&format!("{HIERARCHY}/")),
            Self::Everything => true,
            // The one grant this document carries applies to no request at all, so it covers
            // no name — not even the one its own clause spells.
            Self::ConditionedPath => false,
        }
    }
}

/// One row: the runtime the function declares, the handler it runs — written against the SDK
/// that runtime bundles — and the document its execution role carries.
struct Row {
    case: &'static str,
    /// A `Runtime` value this emulator runs a package for (source:
    /// `smithy_query(query="shape", shape="Runtime")`).
    runtime: &'static str,
    source: &'static str,
    grant: Grant,
}

const ROWS: &[Row] = &[
    Row {
        case: "python3.12, a boto3 client, a role granted nothing of this service",
        runtime: "python3.12",
        source: PYTHON_MODULE_CLIENT,
        grant: Grant::Nothing,
    },
    Row {
        case: "python3.11, a client built from a boto3 Session, a role granted one parameter's \
               own ARN",
        runtime: "python3.11",
        source: PYTHON_SESSION_CLIENT,
        grant: Grant::OneParameter,
    },
    Row {
        case: "nodejs20.x, the v3 bare client sending a command, a role granted one hierarchy's \
               ARN prefix",
        runtime: "nodejs20.x",
        source: NODE_COMMAND_CLIENT,
        grant: Grant::OneHierarchy,
    },
    Row {
        case: "nodejs22.x, the aggregated v3 client's own method, a role granted the read on \
               every resource",
        runtime: "nodejs22.x",
        source: NODE_AGGREGATED_CLIENT,
        grant: Grant::Everything,
    },
    Row {
        case: "python3.13, a boto3 client, a role whose read is scoped by a path-prefix \
               Condition instead of by its Resource",
        runtime: "python3.13",
        source: PYTHON_MODULE_CLIENT,
        grant: Grant::ConditionedPath,
    },
];

/// The emulator as it is ASSEMBLED, on one engine: a handler's own SDK call is dispatched back
/// into this same router, so the parameter it reads is the parameter this suite wrote.
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

/// One Systems Manager operation over its `awsJson1_1` wire, made as the account's own
/// credential — the caller this service's authorization gate does not bound, so the seeding
/// says nothing about any row's decision.
async fn ssm(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("ssm.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("authorization", authorization("ssm"))
        .header("x-amz-target", format!("AmazonSSM.{operation}"))
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

/// The ARN of the parameter (or hierarchy) `name`: a hierarchical name keeps its whole path
/// after the `parameter/` prefix, and the leading separator is not part of it
/// (`PutParameter`, `Parameter$ARN`).
fn parameter_arn(name: &str) -> String {
    format!(
        "arn:aws:ssm:{REGION}:{ACCOUNT}:parameter/{}",
        name.strip_prefix('/').unwrap_or(name)
    )
}

/// The role one row runs as and the function it runs in — named here because a refusal's own
/// sentence names both: the identity a function's call is signed as is the session
/// `arn:aws:sts::<account>:assumed-role/<role>/<function>`.
fn role_name(index: usize) -> String {
    format!("xsvc-in-fn-ssm-role-{index}")
}
fn function_name(index: usize) -> String {
    format!("xsvc-in-fn-ssm-probe-{index}")
}
fn session_arn(index: usize) -> String {
    format!(
        "arn:aws:sts::{ACCOUNT}:assumed-role/{}/{}",
        role_name(index),
        function_name(index)
    )
}

/// The sentence Systems Manager refuses a read nothing in force allows with. Written out here
/// rather than built from the service's own helper, so the wording has something to be wrong
/// about: the principal is the session the function's call is signed as, the action is the one
/// the read performs, and the resource is the ARN of the parameter the request addressed.
fn refusal_message(index: usize, name: &str) -> String {
    format!(
        "User: {} is not authorized to perform: {READ_ACTION} on resource: {} because no \
         identity-based policy allows the {READ_ACTION} action",
        session_arn(index),
        parameter_arn(name)
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

/// The document one row's role carries: the log actions a function writes its own log stream
/// with — which grant nothing of this service — and whichever statements its grant adds.
fn role_document(grant: Grant) -> String {
    let mut statements = vec![json!({
        "Effect": "Allow",
        "Action": ["logs:CreateLogGroup", "logs:CreateLogStream", "logs:PutLogEvents"],
        "Resource": "*",
    })];
    statements.extend(grant.statements());
    json!({"Version": "2012-10-17", "Statement": statements}).to_string()
}

/// Create the role one row runs as with the document its grant states, and answer the ARN IAM
/// minted for it.
async fn create_role(router: &MultiServiceRouter, index: usize, grant: Grant) -> String {
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
            ("PolicyName", POLICY_NAME),
            ("PolicyDocument", &role_document(grant)),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: PutRolePolicy({name:?}) must succeed, got {status} with {embedded}"
    );
    arn
}

/// Write one `String` parameter as the account's own credential, and assert the account's own
/// read reports it back — so what a handler answers is compared against a value that round
/// trips outside the function too.
async fn put_parameter(router: &MultiServiceRouter, name: &str, value: &str) {
    let (status, put) = ssm(
        router,
        "PutParameter",
        &json!({"Name": name, "Type": "String", "Value": value}),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup {name:?}: PutParameter must succeed, got {status} with {put}"
    );
    let (status, read) = ssm(router, "GetParameter", &json!({"Name": name})).await;
    assert_eq!(
        (status, read["Parameter"]["Value"].as_str()),
        (200, Some(value)),
        "setup {name:?}: the account's own read must report the value the put wrote, got \
         {status} with {read}"
    );
    assert_eq!(
        read["Parameter"]["ARN"].as_str(),
        Some(parameter_arn(name).as_str()),
        "setup {name:?}: the account's own read must report the parameter's own ARN, got {read}"
    );
}

/// The template the suite deploys: one function per row, in that row's own runtime, running as
/// that row's role. The inline source is archived into the deployment package by
/// CloudFormation, under the file name the declared runtime's handler resolves — which is how a
/// function whose handler is written here comes to exist at all.
fn stack_template(roles: &[String]) -> String {
    let mut resources = serde_json::Map::new();
    for (index, row) in ROWS.iter().enumerate() {
        resources.insert(
            format!("Probe{index}"),
            json!({
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": function_name(index),
                    "Runtime": row.runtime,
                    "Handler": "index.handler",
                    "Role": roles[index],
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": {"ZipFile": row.source},
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

/// Deploy the stack, wait for `CREATE_COMPLETE` and answer the physical id of each declared
/// function.
async fn deploy(router: &MultiServiceRouter, roles: &[String]) -> BTreeMap<String, String> {
    let template = stack_template(roles);
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
    // which no fixed sleep can name: this looks until it reports it, under a deadline generous
    // enough that a loaded host does not decide the outcome.
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

/// Wait until Lambda reports `name` `Active` — "the function is ready to be invoked"
/// (`FunctionConfiguration$State`) — which is the state an invocation needs.
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

/// Invoke one row's function over every name and answer the document its handler returned,
/// asserting the invocation was served and the handler ran to completion: the SDK is on the
/// import path, so a module the runtime did not supply would have faulted the load, which the
/// function-error header and an error document are where the caller reads.
async fn probe(router: &MultiServiceRouter, case: &str, function: &str) -> Value {
    let names: Vec<&str> = NAMES.iter().map(|(name, _)| *name).collect();
    let event = json!({"names": names});
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
    payload
}

/// What the handler must report for one name under one row's grant: the stored value and the
/// parameter's own ARN where the document covers the read, and the refusal — its code, IAM's
/// sentence and the HTTP status, with NO value anywhere in it — where it does not.
fn expected(index: usize, grant: Grant, name: &str, value: &str) -> Value {
    if grant.covers(name) {
        return json!({"ok": true, "value": value, "arn": parameter_arn(name)});
    }
    json!({
        "ok": false,
        "code": ACCESS_DENIED,
        "message": refusal_message(index, name),
        "status": ACCESS_DENIED_STATUS_TEXT,
    })
}

/// Every row: the function's own read of each name, made with the SDK its declared runtime
/// bundles, is decided against the document in force on its execution role and on the ARN of
/// the parameter the request addresses — so one invocation is served the names that document
/// covers and refused the names it does not, with IAM's own sentence and no value at all.
#[tokio::test]
async fn an_in_function_parameter_read_is_decided_per_parameter_against_the_roles_grant() {
    let router = build_router();

    for (name, value) in NAMES {
        put_parameter(&router, name, value).await;
    }
    let mut roles = Vec::new();
    for (index, row) in ROWS.iter().enumerate() {
        roles.push(create_role(&router, index, row.grant).await);
    }
    let physical = deploy(&router, &roles).await;

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
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

        let answered = probe(&router, case, &function).await;
        for (name, value) in NAMES {
            let wanted = expected(index, row.grant, name, value);
            let reported = &answered[*name];
            assert_eq!(
                reported, &wanted,
                "case {case:?}, {name:?}: expected the handler to report {wanted}, got \
                 {reported} (whole answer {answered})"
            );
        }
    }
}
