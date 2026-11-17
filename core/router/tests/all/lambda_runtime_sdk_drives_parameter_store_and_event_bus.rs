// SPDX-License-Identifier: CC0-1.0

//! The AWS SDK a managed `python3.x` runtime puts on a handler's import path, used to read and
//! write Parameter Store and to publish onto an event bus.
//!
//! RULE: AWS's managed `python3.x` runtimes carry the AWS SDK for Python in the execution
//! environment, so a handler that imports `boto3` or `botocore` AT MODULE SCOPE loads without
//! the function shipping a dependency of its own — the invocation answers 200 with no
//! `FunctionError` and with whatever the handler returned, never a `Runtime.ImportModuleError`
//! for one of those modules — and every call that SDK makes is served by this emulator's own
//! services as a session of the function's execution role: what the handler writes is what a
//! client reads back afterwards, a name the store does not hold is reported to the handler the
//! way the called operation models it (`ParameterNotFound` for the single read, among
//! `InvalidParameters` for the batch read), and a call no policy in force on that role allows
//! is refused `AccessDeniedException` (HTTP 400) naming the assumed-role session, the action
//! and the resource the grant is missing on.
//!
//! "The execution role grants the function permission to use Amazon Web Services services"
//! (`FunctionConfiguration$Role`), and the identity such a call speaks for is the session
//! `arn:aws:sts::<account>:assumed-role/<role>/<function>`; with neither an identity-based nor
//! a resource-based allow a request is denied (IAM User Guide, *Policy evaluation logic*). The
//! batch read models no not-found refusal at all and reports "a list of parameters that aren't
//! formatted correctly or don't run during an execution"
//! (`smithy_query(query="doc",shape="GetParametersResult",member="InvalidParameters")`), while
//! the single read answers `ParameterNotFound`; publishing is the action `events:PutEvents` on
//! the bus the entry names, and an authorization verdict lands on the REQUEST rather than in
//! the per-entry failure channel.
//!
//! Six crates have to agree for that to hold — Lambda runs the handler and supplies it the SDK,
//! CloudFormation archives the inline handler into a deployment package this emulator can run,
//! IAM holds each role and its inline document, STS mints the session the function's calls are
//! signed with, Systems Manager holds the parameters, and EventBridge owns the buses and the
//! rule — so the suite lives here: a crate-local suite can only ever see one side of it.
//! `lambda_runtime_sdk_reads_a_cmk_encrypted_parameter` pins the same import path for a
//! `SecureString` sealed under a customer-managed key; what only this suite reaches is the
//! writing half (a parameter the handler puts, read back through Parameter Store's own read),
//! the batch read's partial answer, the publication onto a bus, and the full
//! bus -> rule -> function -> parameter chain.
//!
//! The rows of the first test differ along what the rule generalises over:
//!
//! * the `python3.x` runtime the function declares (four of the `Runtime` enum's `python3.x`
//!   values, source: `smithy_query(query="shape",shape="Runtime")`), because the SDK belongs to
//!   every managed Python runtime rather than to one point release;
//! * the FORM the SDK is imported in — `import boto3`, a from-import of the session class
//!   (`from boto3.session import Session`) and botocore's own session
//!   (`import botocore.session`) — each reporting the module names its imports resolved to;
//! * the call made with it: the single read, a write of a new name, a write overwriting it, the
//!   batch read with a name the store does not hold among the ones it does, the single read of
//!   that same absent name, and a publication;
//! * what the role's one inline document allows: every action on the path and the bus, the two
//!   reads alone, the single read alone, and no grant of either service at all — so a refusal
//!   is asked for by an action no statement names, by a resource outside the path the grant
//!   scopes itself to, and by a bus the grant does not name.
//!
//! The rows of the second test differ along the detail the published event carries and whether
//! its `source` matches the rule's pattern at all.

use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{elements, form_encode, opt_element, physical_ids_of};

/// The Region every request here is signed for, and so the Region the parameters, the buses and
/// the functions belong to.
const REGION: &str = "us-east-1";

/// The account this emulator serves, which every ARN below carries.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The `StackStatus` a deployed stack must reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The code and status an authorization refusal of either service is answered with: the
/// AWS-common `AccessDeniedException`, carried at the client-error status because an AWS JSON
/// protocol discriminates a failure by the error type in the body.
const ACCESS_DENIED: &str = "AccessDeniedException";
const ACCESS_DENIED_STATUS: u16 = 400;

/// The refusal the single read of a name no parameter is held for answers with, and its status.
/// It carries no sentence: `ParameterNotFound`'s `message` is optional and the service states
/// none, so the SDK reports the empty one.
const NOT_FOUND: &str = "ParameterNotFound";
const NOT_FOUND_STATUS: u16 = 400;

/// The IAM actions the handlers' calls perform.
const READ_ACTION: &str = "ssm:GetParameter";
const BATCH_READ_ACTION: &str = "ssm:GetParameters";
const WRITE_ACTION: &str = "ssm:PutParameter";
const PUBLISH_ACTION: &str = "events:PutEvents";

/// The wildcard a role's document scopes its grants to: every name under the hierarchy the
/// functions' own parameters live in.
const PATH_WILDCARD: &str = "/xsvc-fn-sdk/*";

/// The seeded parameters: two under the granted path, and one outside it.
const HOST: &str = "/xsvc-fn-sdk/db/host";
const HOST_VALUE: &str = "db.internal.example";
const PORT: &str = "/xsvc-fn-sdk/db/port";
const PORT_VALUE: &str = "5432";
const OUTSIDE: &str = "/xsvc-fn-other/private/key";
const OUTSIDE_VALUE: &str = "outside-every-granted-path";

/// The name a row's handler writes, and the name no parameter is ever held for.
const WRITTEN: &str = "/xsvc-fn-sdk/run/written";
const WRITTEN_FIRST: &str = "the value the first write put";
const WRITTEN_SECOND: &str = "the value the overwriting write put";
const ABSENT: &str = "/xsvc-fn-sdk/run/never-written";

/// The two buses: the one the full grant names, and one it does not.
const GRANTED_BUS: &str = "xsvc-fn-sdk-granted";
const OTHER_BUS: &str = "xsvc-fn-sdk-other";

/// What a published event carries. The `source` is also what the second test's rule matches on.
const SOURCE: &str = "xsvc.fn.sdk";
const OTHER_SOURCE: &str = "xsvc.fn.sdk.other";
const DETAIL_TYPE: &str = "Runtime SDK Probe";

/// How long one invocation is given, as the function's own `Timeout`: well past what a run needs
/// on any host, because what is under test is the SDK and the decision rather than how fast an
/// interpreter gets through the handler.
const FUNCTION_TIMEOUT: u64 = 120;

/// How often to look while waiting for work the clock owes — a created function reaching
/// `Active`, an asynchronous delivery landing in Parameter Store — and how many looks to take.
/// The spans are those services' own, so no fixed sleep names the instant one ends, and the
/// deadline is generous so a loaded host cannot decide the outcome.
const LOOK: Duration = Duration::from_millis(100);
const LOOKS: usize = 900;

/// The emulator as it is ASSEMBLED, on one engine: a handler's own SDK call is dispatched back
/// into this same router, so the parameter it writes is the one this suite reads.
fn build_router() -> MultiServiceRouter {
    let engine: std::sync::Arc<engine::InMemoryEngine> = std::sync::Arc::new(
        engine::InMemoryEngine::new(),
    );
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

/// One Systems Manager call, as the account's own credential.
async fn ssm(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    rpc(router, "ssm", "AmazonSSM", operation, body).await
}

/// One EventBridge call, as the account's own credential.
async fn events(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    rpc(router, "events", "AWSEvents", operation, body).await
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

/// The ARN of the event bus `name` names in this Region.
fn bus_arn(name: &str) -> String {
    format!("arn:aws:events:{REGION}:{ACCOUNT}:event-bus/{name}")
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

/// What every handler of the first test does once it holds a client, whichever form built it:
/// make each call the event names and report either the members of the answer the suite asserts
/// on or the refusal it raised — its code, its sentence and the HTTP status it came back on.
/// `IMPORTED` is what the module-scope imports of the head above resolved to, which is reported
/// on every answer: an import that did not resolve would have faulted the load instead.
const HANDLER_TAIL: &str = r#"

def _report(method, answered):
    if method == "get_parameter":
        held = answered["Parameter"]
        return {
            "value": held["Value"],
            "type": held["Type"],
            "arn": held["ARN"],
            "version": held["Version"],
        }
    if method == "get_parameters":
        return {
            "values": {held["Name"]: held["Value"] for held in answered["Parameters"]},
            "invalid": sorted(answered["InvalidParameters"]),
        }
    if method == "put_parameter":
        return {"version": answered["Version"]}
    if method == "put_events":
        return {
            "failed": answered["FailedEntryCount"],
            "minted": [bool(entry.get("EventId")) for entry in answered["Entries"]],
        }
    raise ValueError("no report for " + method)


def handler(event, context):
    results = []
    for step in event["steps"]:
        client = _client(step["service"])
        method = step["method"]
        try:
            answered = getattr(client, method)(**step["arguments"])
        except ClientError as refused:
            results.append({
                "ok": False,
                "code": refused.response["Error"]["Code"],
                "message": refused.response["Error"]["Message"],
                "status": refused.response["ResponseMetadata"]["HTTPStatusCode"],
            })
        else:
            reported = {"ok": True}
            reported.update(_report(method, answered))
            results.append(reported)
    return {"imported": IMPORTED, "results": results}
"#;

/// One SDK call a row's handler makes.
#[derive(Debug, Clone, Copy)]
enum Call {
    /// The single read of one name.
    Read(&'static str),
    /// A write of `value` under `name`, overwriting a name already held when `overwrite`.
    Write {
        name: &'static str,
        value: &'static str,
        overwrite: bool,
    },
    /// The batch read of several names.
    Batch(&'static [&'static str]),
    /// A publication of one entry onto the bus `bus` names.
    Publish(&'static str),
}

impl Call {
    /// The step the handler is handed: which client to build, which method to call on it, and
    /// the keyword arguments that ARE the operation's input document.
    fn step(self) -> Value {
        match self {
            Self::Read(name) => json!({
                "service": "ssm",
                "method": "get_parameter",
                "arguments": {"Name": name},
            }),
            Self::Write {
                name,
                value,
                overwrite,
            } => json!({
                "service": "ssm",
                "method": "put_parameter",
                "arguments": {
                    "Name": name,
                    "Value": value,
                    "Type": "String",
                    "Overwrite": overwrite,
                },
            }),
            Self::Batch(names) => json!({
                "service": "ssm",
                "method": "get_parameters",
                "arguments": {"Names": names},
            }),
            Self::Publish(bus) => json!({
                "service": "events",
                "method": "put_events",
                "arguments": {"Entries": [{
                    "Source": SOURCE,
                    "DetailType": DETAIL_TYPE,
                    "Detail": r#"{"probe":"from the handler"}"#,
                    "EventBusName": bus,
                }]},
            }),
        }
    }
}

/// Which resource an action is refused on.
#[derive(Debug, Clone, Copy)]
enum Resource {
    Parameter(&'static str),
    Bus(&'static str),
}

impl Resource {
    fn arn(self) -> String {
        match self {
            Self::Parameter(name) => parameter_arn(name),
            Self::Bus(name) => bus_arn(name),
        }
    }
}

/// What one call must answer the handler.
#[derive(Debug, Clone, Copy)]
enum Expect {
    /// The single read answered the parameter `name`, holding `value` at version `version`.
    Parameter {
        name: &'static str,
        value: &'static str,
        version: u64,
    },
    /// The write answered the version it created.
    Written { version: u64 },
    /// The batch read answered the names it resolved and reported the others.
    Batch {
        found: &'static [(&'static str, &'static str)],
        invalid: &'static [&'static str],
    },
    /// The publication was accepted, with an `EventId` minted for its one entry.
    Published,
    /// The single read of a name the store holds nothing for.
    NotFound,
    /// No policy in force on the role allows `action` on `resource`.
    Denied {
        action: &'static str,
        resource: Resource,
    },
}

/// One call and what it must come to.
struct Step {
    call: Call,
    expect: Expect,
}

impl Step {
    /// What the handler must report for this step, as the answer document the invocation
    /// carries.
    fn expected(&self, session: &str) -> Value {
        match self.expect {
            Expect::Parameter {
                name,
                value,
                version,
            } => json!({
                "ok": true,
                "value": value,
                "type": "String",
                "arn": parameter_arn(name),
                "version": version,
            }),
            Expect::Written { version } => json!({"ok": true, "version": version}),
            Expect::Batch { found, invalid } => {
                let mut values = serde_json::Map::new();
                for (name, value) in found {
                    values.insert((*name).to_string(), json!(value));
                }
                let mut reported: Vec<&str> = invalid.to_vec();
                reported.sort_unstable();
                json!({"ok": true, "values": Value::Object(values), "invalid": reported})
            }
            Expect::Published => json!({"ok": true, "failed": 0, "minted": [true]}),
            Expect::NotFound => json!({
                "ok": false,
                "code": NOT_FOUND,
                "message": "",
                "status": NOT_FOUND_STATUS,
            }),
            Expect::Denied { action, resource } => json!({
                "ok": false,
                "code": ACCESS_DENIED,
                "message": no_identity_policy_refusal(session, action, &resource.arn()),
                "status": ACCESS_DENIED_STATUS,
            }),
        }
    }
}

/// What one row's execution role is allowed by its own inline document, beside the log actions
/// every execution role carries.
#[derive(Debug, Clone, Copy)]
enum Grant {
    /// Every action the handlers make on the granted path, and the publication onto one bus.
    Everything,
    /// The two reads of the granted path, and nothing else.
    BothReads,
    /// The single read of the granted path, and nothing else.
    SingleRead,
    /// No grant of either service at all.
    Nothing,
}

impl Grant {
    /// The statements this grant adds to the role's one inline document.
    fn statements(self) -> Vec<Value> {
        match self {
            Self::Everything => vec![
                json!({
                    "Effect": "Allow",
                    "Action": [READ_ACTION, BATCH_READ_ACTION, WRITE_ACTION],
                    "Resource": parameter_arn(PATH_WILDCARD),
                }),
                json!({
                    "Effect": "Allow",
                    "Action": PUBLISH_ACTION,
                    "Resource": bus_arn(GRANTED_BUS),
                }),
            ],
            Self::BothReads => vec![json!({
                "Effect": "Allow",
                "Action": [READ_ACTION, BATCH_READ_ACTION],
                "Resource": parameter_arn(PATH_WILDCARD),
            })],
            Self::SingleRead => vec![json!({
                "Effect": "Allow",
                "Action": READ_ACTION,
                "Resource": parameter_arn(PATH_WILDCARD),
            })],
            Self::Nothing => Vec::new(),
        }
    }
}

/// The inline document one role carries: the log actions every execution role needs, and
/// whichever statements the grant adds.
fn role_document(grant: Grant) -> String {
    let mut statements = vec![json!({
        "Effect": "Allow",
        "Action": ["logs:CreateLogGroup", "logs:CreateLogStream", "logs:PutLogEvents"],
        "Resource": "*",
    })];
    statements.extend(grant.statements());
    json!({"Version": "2012-10-17", "Statement": statements}).to_string()
}

/// One row of the first test: a function of its own, with the runtime it declares, the form its
/// handler imports the SDK in, the document its execution role carries, and the calls it makes.
struct Row {
    case: &'static str,
    /// A `python3.x` runtime identifier of the `Runtime` enum's value set.
    runtime: &'static str,
    form: Form,
    grant: Grant,
    steps: Vec<Step>,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a module-scope `import boto3` reads, writes, batches and publishes",
            runtime: "python3.12",
            form: Form::Boto3Module,
            grant: Grant::Everything,
            steps: vec![
                Step {
                    call: Call::Read(HOST),
                    expect: Expect::Parameter {
                        name: HOST,
                        value: HOST_VALUE,
                        version: 1,
                    },
                },
                Step {
                    call: Call::Write {
                        name: WRITTEN,
                        value: WRITTEN_FIRST,
                        overwrite: false,
                    },
                    expect: Expect::Written { version: 1 },
                },
                Step {
                    call: Call::Write {
                        name: WRITTEN,
                        value: WRITTEN_SECOND,
                        overwrite: true,
                    },
                    expect: Expect::Written { version: 2 },
                },
                Step {
                    call: Call::Read(WRITTEN),
                    expect: Expect::Parameter {
                        name: WRITTEN,
                        value: WRITTEN_SECOND,
                        version: 2,
                    },
                },
                Step {
                    call: Call::Batch(&[HOST, PORT, ABSENT]),
                    expect: Expect::Batch {
                        found: &[(HOST, HOST_VALUE), (PORT, PORT_VALUE)],
                        invalid: &[ABSENT],
                    },
                },
                Step {
                    call: Call::Read(ABSENT),
                    expect: Expect::NotFound,
                },
                Step {
                    call: Call::Publish(GRANTED_BUS),
                    expect: Expect::Published,
                },
                Step {
                    call: Call::Publish(OTHER_BUS),
                    expect: Expect::Denied {
                        action: PUBLISH_ACTION,
                        resource: Resource::Bus(OTHER_BUS),
                    },
                },
            ],
        },
        Row {
            case: "a from-imported session class reads but may not publish",
            runtime: "python3.13",
            form: Form::Boto3Session,
            grant: Grant::BothReads,
            steps: vec![
                Step {
                    call: Call::Read(PORT),
                    expect: Expect::Parameter {
                        name: PORT,
                        value: PORT_VALUE,
                        version: 1,
                    },
                },
                Step {
                    call: Call::Batch(&[PORT, HOST]),
                    expect: Expect::Batch {
                        found: &[(HOST, HOST_VALUE), (PORT, PORT_VALUE)],
                        invalid: &[],
                    },
                },
                Step {
                    call: Call::Publish(GRANTED_BUS),
                    expect: Expect::Denied {
                        action: PUBLISH_ACTION,
                        resource: Resource::Bus(GRANTED_BUS),
                    },
                },
            ],
        },
        Row {
            case: "botocore's own session reads inside the granted path and not outside it",
            runtime: "python3.11",
            form: Form::BotocoreSession,
            grant: Grant::SingleRead,
            steps: vec![
                Step {
                    call: Call::Read(HOST),
                    expect: Expect::Parameter {
                        name: HOST,
                        value: HOST_VALUE,
                        version: 1,
                    },
                },
                Step {
                    call: Call::Read(OUTSIDE),
                    expect: Expect::Denied {
                        action: READ_ACTION,
                        resource: Resource::Parameter(OUTSIDE),
                    },
                },
                Step {
                    call: Call::Batch(&[HOST]),
                    expect: Expect::Denied {
                        action: BATCH_READ_ACTION,
                        resource: Resource::Parameter(HOST),
                    },
                },
            ],
        },
        Row {
            case: "a role holding no grant of either service is refused every call",
            runtime: "python3.10",
            form: Form::Boto3Module,
            grant: Grant::Nothing,
            steps: vec![
                Step {
                    call: Call::Read(HOST),
                    expect: Expect::Denied {
                        action: READ_ACTION,
                        resource: Resource::Parameter(HOST),
                    },
                },
                Step {
                    call: Call::Batch(&[PORT]),
                    expect: Expect::Denied {
                        action: BATCH_READ_ACTION,
                        resource: Resource::Parameter(PORT),
                    },
                },
                Step {
                    call: Call::Publish(GRANTED_BUS),
                    expect: Expect::Denied {
                        action: PUBLISH_ACTION,
                        resource: Resource::Bus(GRANTED_BUS),
                    },
                },
            ],
        },
    ]
}

/// The role one function runs as, and the function itself — named here because a refusal's own
/// sentence names both: the identity a function's call is signed as is the session
/// `arn:aws:sts::<account>:assumed-role/<role>/<function>`.
fn role_name(stack: &str, index: usize) -> String {
    format!("{stack}-role-{index}")
}
fn function_name(stack: &str, index: usize) -> String {
    format!("{stack}-probe-{index}")
}
fn session_arn(stack: &str, index: usize) -> String {
    format!(
        "arn:aws:sts::{ACCOUNT}:assumed-role/{}/{}",
        role_name(stack, index),
        function_name(stack, index)
    )
}

/// Create the role at `index` of `stack` with the document `grant` states, and answer the ARN
/// IAM minted for it.
async fn create_role(
    router: &MultiServiceRouter,
    stack: &str,
    index: usize,
    grant: Grant,
) -> String {
    let name = role_name(stack, index);
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

/// Deploy a stack declaring one function per `(role ARN, runtime, handler source)` — the inline
/// source is archived into the deployment package by CloudFormation, which is how a function
/// whose handler is written here comes to exist at all — and answer the physical id of each.
async fn deploy_functions(
    router: &MultiServiceRouter,
    stack: &str,
    declared: &[(String, &str, String)],
) -> Vec<String> {
    let mut resources = serde_json::Map::new();
    for (index, (role_arn, runtime, source)) in declared.iter().enumerate() {
        resources.insert(
            format!("Probe{index}"),
            json!({
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": function_name(stack, index),
                    "Runtime": runtime,
                    "Handler": "index.handler",
                    "Role": role_arn,
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": {"ZipFile": source},
                },
            }),
        );
    }
    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": Value::Object(resources),
    })
    .to_string();
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
    let reported = wait_for_stack_status(router, stack, CREATE_COMPLETE).await;
    let failures = if reported.0 == CREATE_COMPLETE {
        Vec::new()
    } else {
        let (_, raised) = query(
            router,
            "cloudformation",
            "2010-05-15",
            "DescribeStackEvents",
            &[("StackName", stack)],
        )
        .await;
        elements(&raised, "ResourceStatusReason")
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
        &[("StackName", stack)],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: DescribeStackResources({stack:?}) must succeed, got {status} with {resources}"
    );
    let physical = physical_ids_of(&resources);
    (0..declared.len())
        .map(|index| {
            let logical = format!("Probe{index}");
            let name = physical.get(&logical).cloned().unwrap_or_else(|| {
                panic!("setup: the stack must report a physical id for {logical}")
            });
            assert_eq!(
                name,
                function_name(stack, index),
                "setup: the stack must name the function {:?}, got {name:?}",
                function_name(stack, index)
            );
            name
        })
        .collect()
}

/// Wait until Lambda reports `name` `Active`, which is the state an invocation needs, and answer
/// the configuration it reported.
async fn wait_until_active(router: &MultiServiceRouter, name: &str) -> Value {
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
            return configuration;
        }
        last = configuration;
        tokio::time::sleep(LOOK).await;
    }
    panic!("setup: Lambda must report {name:?} Active, last reported {last}");
}

/// Write one `String` parameter as the account's own credential, and assert the account's own
/// read reports it back — so what a handler answers is compared against a value that round-trips
/// outside the function too.
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
}

/// Create one event bus by name.
async fn create_bus(router: &MultiServiceRouter, name: &str) {
    let (status, created) = events(router, "CreateEventBus", &json!({"Name": name})).await;
    assert_eq!(
        status, 200,
        "setup: CreateEventBus({name:?}) must succeed, got {status} with {created}"
    );
    assert_eq!(
        created["EventBusArn"].as_str(),
        Some(bus_arn(name).as_str()),
        "setup: CreateEventBus({name:?}) must answer the bus's own ARN, got {created}"
    );
}

/// Invoke `function` with `event` and answer the payload the handler returned, asserting the
/// invocation was served and the handler ran to completion: the SDK is on the import path, so a
/// module the runtime did not supply would have faulted the load, which the function-error
/// header and an error document are where the caller reads.
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
        "case {case:?}: the handler must run to completion, got {faulted:?} with {payload}"
    );
    assert_eq!(
        payload.get("errorType"),
        None,
        "case {case:?}: the answer must be the handler's own result rather than an error \
         document, got {payload}"
    );
    payload
}

/// Every row: the handler loads with the SDK the runtime supplies, and each call it makes with
/// that SDK is served by this emulator's own services and decided against the policies in force
/// on its execution role.
#[tokio::test]
async fn a_handler_drives_parameter_store_and_an_event_bus_with_the_runtimes_own_sdk() {
    const STACK: &str = "xsvc-fn-sdk-probes";
    let router = build_router();
    for (name, value) in [
        (HOST, HOST_VALUE),
        (PORT, PORT_VALUE),
        (OUTSIDE, OUTSIDE_VALUE),
    ] {
        put_parameter(&router, name, value).await;
    }
    create_bus(&router, GRANTED_BUS).await;
    create_bus(&router, OTHER_BUS).await;

    let rows = rows();
    let mut declared = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let role_arn = create_role(&router, STACK, index, row.grant).await;
        declared.push((role_arn, row.runtime, row.form.source()));
    }
    let functions = deploy_functions(&router, STACK, &declared).await;

    for (index, row) in rows.iter().enumerate() {
        let case = format!("{} on {}", row.case, row.runtime);
        let function = &functions[index];
        wait_until_active(&router, function).await;

        let event = json!({
            "steps": row.steps.iter().map(|step| step.call.step()).collect::<Vec<Value>>(),
        });
        let payload = invoke(&router, &case, function, &event).await;

        assert_eq!(
            payload["imported"],
            json!(row.form.imported()),
            "case {case:?}: the handler's module-scope imports must resolve to the SDK the \
             runtime supplies, got {payload}"
        );
        let answered = payload["results"].as_array().cloned().unwrap_or_default();
        assert_eq!(
            answered.len(),
            row.steps.len(),
            "case {case:?}: expected one result per call, got {payload}"
        );
        let session = session_arn(STACK, index);
        for (position, (step, got)) in row.steps.iter().zip(&answered).enumerate() {
            let expected = step.expected(&session);
            assert_eq!(
                got, &expected,
                "case {case:?}, call {position} ({:?}): expected the handler to report \
                 {expected}, got {got}",
                step.call
            );
        }
    }

    // The write the first row's handler made is the store's own: the account's own read reports
    // the overwriting value at the version that write answered with.
    let (status, read) = ssm(&router, "GetParameter", &json!({"Name": WRITTEN})).await;
    assert_eq!(
        (
            status,
            read["Parameter"]["Value"].as_str(),
            read["Parameter"]["Version"].as_u64(),
        ),
        (200, Some(WRITTEN_SECOND), Some(2)),
        "the parameter the handler wrote must be the one Parameter Store holds, got {status} \
         with {read}"
    );
}

/// The handler the second test deploys: an EventBridge rule's target, which writes what the
/// delivered envelope carried into the parameter the envelope names.
const TARGET_HANDLER: &str = r#"
import json

import boto3

CLIENT = boto3.client("ssm")


def handler(event, context):
    detail = event["detail"]
    CLIENT.put_parameter(
        Name=detail["record"],
        Value=json.dumps({"source": event["source"], "probe": detail["probe"]}),
        Type="String",
        Overwrite=True,
    )
    return {"written": detail["record"]}
"#;

/// Read the parameter `name` until the store holds it, and answer its value — the delivery is
/// asynchronous, so the write is owed from the instant Lambda attempts the queued invocation,
/// which no fixed sleep can name.
async fn wait_for_parameter(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let mut last = Value::Null;
    for _ in 0..LOOKS {
        let (status, read) = ssm(router, "GetParameter", &json!({"Name": name})).await;
        if status == 200 {
            return read["Parameter"]["Value"]
                .as_str()
                .unwrap_or_default()
                .to_string();
        }
        last = read;
        tokio::time::sleep(LOOK).await;
    }
    panic!("case {case:?}: the handler must write {name:?}, last read answered {last}");
}

/// One published event: the `source` it carries, the detail the handler writes, and whether the
/// rule's pattern matches it at all.
struct Delivery {
    case: &'static str,
    source: &'static str,
    record: &'static str,
    probe: &'static str,
    matches: bool,
}

const DELIVERIES: &[Delivery] = &[
    Delivery {
        case: "a matching event carrying one word of detail",
        source: SOURCE,
        record: "/xsvc-fn-sdk/delivered/first",
        probe: "first",
        matches: true,
    },
    Delivery {
        case: "a second matching event carrying other detail",
        source: SOURCE,
        record: "/xsvc-fn-sdk/delivered/second",
        probe: "a longer detail, with punctuation: ✓",
        matches: true,
    },
    Delivery {
        case: "an event whose source the rule's pattern does not name",
        source: OTHER_SOURCE,
        record: "/xsvc-fn-sdk/delivered/unmatched",
        probe: "never delivered",
        matches: false,
    },
];

/// A bus's rule invokes its Lambda target, and the handler it runs writes through the SDK the
/// runtime supplies: what the published event carried is what Parameter Store then holds, and an
/// event the rule's pattern does not match leads to no write at all.
#[tokio::test]
async fn an_event_bus_rule_runs_a_handler_that_writes_a_parameter_with_the_runtimes_own_sdk() {
    const STACK: &str = "xsvc-fn-sdk-target";
    const RULE: &str = "xsvc-fn-sdk-target-rule";
    let router = build_router();
    create_bus(&router, GRANTED_BUS).await;

    let role_arn = create_role(&router, STACK, 0, Grant::Everything).await;
    let functions = deploy_functions(
        &router,
        STACK,
        &[(role_arn, "python3.12", TARGET_HANDLER.to_string())],
    )
    .await;
    let function = &functions[0];
    let configuration = wait_until_active(&router, function).await;
    let function_arn = configuration["FunctionArn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: the configuration must carry a FunctionArn"))
        .to_string();

    // "For Lambda functions … EventBridge relies on resource-based policies", so the grant that
    // admits the invocation is added before the rule is wired.
    let (status, _headers, body) = lambda(
        &router,
        "POST",
        &format!("/2015-03-31/functions/{function}/policy"),
        json!({
            "StatementId": "events-invoke",
            "Action": "lambda:InvokeFunction",
            "Principal": "events.amazonaws.com",
        })
        .to_string()
        .as_bytes(),
    )
    .await;
    assert_eq!(
        status, 201,
        "setup: AddPermission must succeed, got {status} with {body}"
    );

    let (status, rule) = events(
        &router,
        "PutRule",
        &json!({
            "Name": RULE,
            "EventBusName": GRANTED_BUS,
            "EventPattern": json!({"source": [SOURCE]}).to_string(),
            "State": "ENABLED",
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: PutRule must succeed, got {status} with {rule}"
    );
    let (status, wired) = events(
        &router,
        "PutTargets",
        &json!({
            "Rule": RULE,
            "EventBusName": GRANTED_BUS,
            "Targets": [{"Id": "fn", "Arn": function_arn}],
        }),
    )
    .await;
    assert_eq!(
        (status, wired["FailedEntryCount"].as_u64()),
        (200, Some(0)),
        "setup: PutTargets must accept the function target, got {status} with {wired}"
    );

    // The unmatched event is published FIRST, so that by the time the matched ones have been
    // observed there has been at least as long for a write it must not have made to appear.
    for delivery in DELIVERIES {
        let case = delivery.case;
        let (status, published) = events(
            &router,
            "PutEvents",
            &json!({
                "Entries": [{
                    "Source": delivery.source,
                    "DetailType": DETAIL_TYPE,
                    "EventBusName": GRANTED_BUS,
                    "Detail": json!({
                        "record": delivery.record,
                        "probe": delivery.probe,
                    })
                    .to_string(),
                }],
            }),
        )
        .await;
        assert_eq!(
            (status, published["FailedEntryCount"].as_u64()),
            (200, Some(0)),
            "case {case:?}: the publication must be accepted, got {status} with {published}"
        );
    }

    for delivery in DELIVERIES.iter().filter(|delivery| delivery.matches) {
        let case = delivery.case;
        let value = wait_for_parameter(&router, case, delivery.record).await;
        let held: Value = serde_json::from_str(&value).unwrap_or(Value::Null);
        assert_eq!(
            held,
            json!({"source": delivery.source, "probe": delivery.probe}),
            "case {case:?}: the parameter the handler wrote must carry what the delivered \
             envelope held, got {value:?}"
        );
    }

    for delivery in DELIVERIES.iter().filter(|delivery| !delivery.matches) {
        let case = delivery.case;
        let (status, read) = ssm(&router, "GetParameter", &json!({"Name": delivery.record})).await;
        let rendered = read.to_string();
        assert!(
            status == NOT_FOUND_STATUS && rendered.contains(NOT_FOUND),
            "case {case:?}: no handler must have written {:?}, so the read of it must answer \
             {NOT_FOUND_STATUS} {NOT_FOUND}, got {status} with {rendered}",
            delivery.record
        );
    }
}
