// SPDX-License-Identifier: CC0-1.0

//! What a RUNNING function's own Step Functions call is authorized against: the policies in
//! force on the function's execution role.
//!
//! RULE: a call an AWS SDK makes from inside a running function is made as a session of that
//! function's execution role, so the service serving it decides it against the policies in
//! force on that role — an operation of Step Functions that no identity-based policy in force
//! on the role allows is refused `AccessDeniedException` (HTTP 400, the status this endpoint
//! answers every refusal of a caller under) naming the assumed-role session, the IAM action
//! the operation performs and the ARN the request addressed, and the refused call starts,
//! reads and changes nothing; the same call by a role whose documents allow that action on
//! that ARN is served and its effect is in the service afterwards. The invocation itself
//! still answers HTTP 200 with no `X-Amz-Function-Error`, because the handler RAN and caught
//! the refusal — the distinction a least-privilege pair exists to observe.
//!
//! "The execution role grants the function permission to use Amazon Web Services services"
//! (`FunctionConfiguration$Role`), and the identity a function's own call speaks for is the
//! session `arn:aws:sts::<account>:assumed-role/<role>/<function>`; with neither an
//! identity-based nor a resource-based allow a request is denied, and an explicit deny wins
//! over any allow (IAM User Guide, *Policy evaluation logic*).
//!
//! Four crates have to agree for that to hold — IAM holds each role and its inline document,
//! STS mints the session the function's calls are signed with, Lambda runs the handler and
//! supplies it the SDK, and Step Functions takes the decision — so the suite lives here: a
//! crate-local suite can only ever see one side of it. The Step Functions crate's own
//! `identity_policy_authorization` suite pins the decision for a credential a client assumed
//! directly, the Lambda crate's `handler_step_functions_sdk` pins that the SDK a managed
//! runtime supplies reaches this service at all, and `events_lambda_sfn_in_function_chain`
//! pins the GRANTED and ungranted start of one pair; what only this suite reaches is the
//! decision taken on SEVERAL operations of this service called by a FUNCTION, against the
//! role it runs as, which is the identity a least-privilege execution role is written for.
//!
//! The rows differ along what the rule generalises over: what the role's own document allows
//! (nothing of this service, this operation's action, another operation's action, every
//! action of the service, two actions at once), which resource that statement names (the
//! state machine, another state machine of the account, the machine's executions, every
//! resource), whether the same document also DENIES the action, and WHICH operation the
//! handler calls (a start, a read of one execution, a list of them) — several per role, so
//! one role's document is asked about more than one action and more than one kind of ARN.
//! Each probe asserts both halves — the handler's own report of what Step Functions answered,
//! and what the SERVICE holds afterwards — so a refusal that still started an execution, and
//! a grant that answered without starting one, both fail here.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::stack_status;
use crate::query_wire::{elements, form_encode, opt_element, physical_ids_of};

/// The Region every request here is signed for, and so the Region the state machines and the
/// functions belong to.
const REGION: &str = "us-east-1";

/// The account this emulator serves, which every ARN and every session's ARN carries.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The state machine every row's handler calls.
const MACHINE: &str = "xsvc-in-fn-sfn-machine";

/// Another state machine of this account, which one row's statement names instead of the one
/// the handler calls — created too, so a refusal cannot be about a machine that is not there.
const OTHER_MACHINE: &str = "xsvc-in-fn-sfn-elsewhere";

/// The execution this suite starts itself, as the ACCOUNT's own credential, so the read and
/// the list probes have something to report that no probe can have created.
const CONTROL_EXECUTION: &str = "control-execution";

/// How long the state machines' one `Wait` state waits, in seconds: a whole day, so every
/// execution started here — the control execution this suite starts first included — is still
/// `RUNNING` whenever a later probe reads it, however loaded the host running the suite is.
/// Nothing waits this out; it is the span that must NOT elapse, so it is given generous room
/// rather than a budget the probes share.
const HOLD_SECONDS: u64 = 86_400;

/// The status an execution of these machines sits in while its `Wait` is outstanding
/// (`smithy_query(query="shape",shape="ExecutionStatus")`).
const RUNNING: &str = "RUNNING";

/// The `StackStatus` the functions' stack must reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The code and status Step Functions refuses an unauthorized call with.
const ACCESS_DENIED: &str = "AccessDeniedException";
const ACCESS_DENIED_STATUS: u16 = 400;

/// How long one invocation is given, as the function's own `Timeout`: the longest Lambda
/// allows, which is well past what a run needs on any host, because what is under test is the
/// authorization decision and not how fast an interpreter gets through the handler.
const FUNCTION_TIMEOUT: u64 = 900;

/// How often to look while waiting for work the clock owes — a stack reaching
/// `CREATE_COMPLETE`, a created function reaching `Active` — and how many looks to take. The
/// spans are those services' own, so no fixed sleep names the instant one ends, and the
/// deadline is generous so a loaded host cannot decide the outcome.
const LOOK: Duration = Duration::from_millis(100);
const LOOKS: usize = 1_800;

/// The handler every row runs: it builds a Step Functions client with the SDK the runtime
/// supplies and performs the one operation the event names, reporting either what the service
/// answered or the refusal it answered with — its code, its sentence and the HTTP status it
/// came back on.
const PROBE_HANDLER: &str = r##"
import json

import boto3
from botocore.exceptions import ClientError


def handler(event, context):
    client = boto3.client("stepfunctions")
    try:
        if event["op"] == "start":
            answered = client.start_execution(
                stateMachineArn=event["state_machine"],
                name=event["execution_name"],
                input=json.dumps({"marker": event["execution_name"]}),
            )
            return {"ok": True, "started": answered["executionArn"]}
        if event["op"] == "describe":
            answered = client.describe_execution(executionArn=event["execution"])
            return {
                "ok": True,
                "described": answered["name"],
                "state": answered["status"],
            }
        answered = client.list_executions(stateMachineArn=event["state_machine"])
        return {
            "ok": True,
            "listed": sorted(entry["name"] for entry in answered["executions"]),
        }
    except ClientError as refused:
        return {
            "ok": False,
            "code": refused.response["Error"]["Code"],
            "message": refused.response["Error"]["Message"],
            "status": refused.response["ResponseMetadata"]["HTTPStatusCode"],
        }
"##;

/// The emulator as it is ASSEMBLED, on one engine: a handler's own SDK call is dispatched
/// back into this same router, so the state machine it addresses is the one this suite reads.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate; the
    // credential scope is what routes a request.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what
/// routes the request. Its credential is the long-term key every emulator publishes, so these
/// calls are the account's own and not any row's role's.
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

/// One Step Functions operation over its `awsJson1_0` wire, made as the account's own
/// credential, which is a caller this gate does not bound — so what it reads is what the
/// service holds and not another row's decision.
async fn sfn(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("states.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("authorization", authorization("states"))
        .header("x-amz-target", format!("AWSStepFunctions.{operation}"))
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

/// The ARN of a state machine of this account in this Region.
fn machine_arn(name: &str) -> String {
    format!("arn:aws:states:{REGION}:{ACCOUNT}:stateMachine:{name}")
}

/// The ARN of one execution of [`MACHINE`].
fn execution_arn(name: &str) -> String {
    format!("arn:aws:states:{REGION}:{ACCOUNT}:execution:{MACHINE}:{name}")
}

/// WHICH operation of this service one probe's handler calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    /// A write: the start of an execution of the state machine.
    Start,
    /// A read of ONE execution, addressed by that execution's own ARN.
    Describe,
    /// A list of the machine's executions, addressed by the machine's ARN.
    List,
}

impl Op {
    /// The word the event names it by, which the handler reads.
    fn event(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Describe => "describe",
            Self::List => "list",
        }
    }

    /// The IAM action the operation performs, which is the action a refusal names: this
    /// service's ARN namespace and the operation's own modelled name.
    fn action(self) -> &'static str {
        match self {
            Self::Start => "states:StartExecution",
            Self::Describe => "states:DescribeExecution",
            Self::List => "states:ListExecutions",
        }
    }

    /// The ARN the request ADDRESSES, which is the resource a refusal names and the one a
    /// statement has to cover: the state machine for the start and the list, the EXECUTION's
    /// own ARN for the read of one.
    fn resource(self) -> String {
        match self {
            Self::Start | Self::List => machine_arn(MACHINE),
            Self::Describe => execution_arn(CONTROL_EXECUTION),
        }
    }
}

/// WHICH resource a row's statement names.
#[derive(Debug, Clone, Copy)]
enum Scope {
    /// The state machine the handler calls.
    Machine,
    /// Another state machine of the same account.
    OtherMachine,
    /// Every execution of the state machine the handler calls.
    Executions,
    /// Every resource, which the bare `*` wildcard stands for.
    Every,
}

impl Scope {
    fn resource(self) -> String {
        match self {
            Self::Machine => machine_arn(MACHINE),
            Self::OtherMachine => machine_arn(OTHER_MACHINE),
            Self::Executions => execution_arn("*"),
            Self::Every => "*".to_string(),
        }
    }
}

/// What the policies in force on a row's role make of one probe's call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expected {
    /// The documents allow it, so the service serves it.
    Served,
    /// No document in force allows it, so it falls to IAM's default deny.
    NoPolicy,
    /// A document in force denies it outright, which wins over the allow beside it.
    ExplicitDeny,
}

/// One probe of a row's role: the operation the handler calls, and what the documents in force
/// make of it.
struct Probe {
    case: &'static str,
    op: Op,
    expected: Expected,
}

/// One row: the document the execution role carries, and the probes taken against it.
struct Row {
    /// The actions the role's inline document allows besides the log actions every execution
    /// role carries; empty for a role granting none of this service's.
    allowed: &'static [&'static str],
    /// The actions that same document DENIES outright, over the same resource; empty for a
    /// document that denies nothing.
    denied: &'static [&'static str],
    /// The resource both statements name.
    scope: Scope,
    probes: &'static [Probe],
}

const ROWS: &[Row] = &[
    // The case the rule is stated about: a least-privilege role granting only the log actions
    // a function needs is granted no operation of this service at all.
    Row {
        allowed: &[],
        denied: &[],
        scope: Scope::Machine,
        probes: &[
            Probe {
                case: "a role granting only the log actions starts nothing",
                op: Op::Start,
                expected: Expected::NoPolicy,
            },
            Probe {
                case: "a role granting only the log actions reads no execution",
                op: Op::Describe,
                expected: Expected::NoPolicy,
            },
            Probe {
                case: "a role granting only the log actions lists nothing",
                op: Op::List,
                expected: Expected::NoPolicy,
            },
        ],
    },
    Row {
        allowed: &["states:StartExecution"],
        denied: &[],
        scope: Scope::Machine,
        probes: &[
            Probe {
                case: "a role granting the start on the state machine starts an execution",
                op: Op::Start,
                expected: Expected::Served,
            },
            Probe {
                case: "the same role is granted no read of an execution by it",
                op: Op::Describe,
                expected: Expected::NoPolicy,
            },
            Probe {
                case: "the same role is granted no list by it",
                op: Op::List,
                expected: Expected::NoPolicy,
            },
        ],
    },
    Row {
        allowed: &["states:StartExecution"],
        denied: &[],
        scope: Scope::OtherMachine,
        probes: &[Probe {
            case: "a role granting the start on ANOTHER state machine starts nothing",
            op: Op::Start,
            expected: Expected::NoPolicy,
        }],
    },
    Row {
        allowed: &["states:*"],
        denied: &[],
        scope: Scope::Machine,
        probes: &[
            Probe {
                case: "a role granting every action of the service on the machine starts one",
                op: Op::Start,
                expected: Expected::Served,
            },
            Probe {
                case: "and is granted the list by the same statement",
                op: Op::List,
                expected: Expected::Served,
            },
            Probe {
                case: "but not the read of an execution, whose own ARN it does not name",
                op: Op::Describe,
                expected: Expected::NoPolicy,
            },
        ],
    },
    Row {
        allowed: &["states:DescribeExecution"],
        denied: &[],
        scope: Scope::Executions,
        probes: &[
            Probe {
                case: "a role granting the read on the machine's executions reads one",
                op: Op::Describe,
                expected: Expected::Served,
            },
            Probe {
                case: "the same role is granted no start by it",
                op: Op::Start,
                expected: Expected::NoPolicy,
            },
        ],
    },
    Row {
        allowed: &["states:ListExecutions", "states:StartExecution"],
        denied: &[],
        scope: Scope::Every,
        probes: &[
            Probe {
                case: "a role granting both on every resource starts an execution",
                op: Op::Start,
                expected: Expected::Served,
            },
            Probe {
                case: "and lists the machine's executions",
                op: Op::List,
                expected: Expected::Served,
            },
        ],
    },
    Row {
        allowed: &["states:StartExecution"],
        denied: &["states:StartExecution"],
        scope: Scope::Machine,
        probes: &[Probe {
            case: "a start allowed and denied by the same role starts nothing",
            op: Op::Start,
            expected: Expected::ExplicitDeny,
        }],
    },
];

/// The role one row runs as, and the function it runs in — named here because the refusal's
/// own sentence names both: the identity a function's call is signed as is the session
/// `arn:aws:sts::<account>:assumed-role/<role>/<function>`.
fn role_name(index: usize) -> String {
    format!("xsvc-in-fn-sfn-role-{index}")
}
fn function_name(index: usize) -> String {
    format!("xsvc-in-fn-sfn-probe-{index}")
}

/// The sentence Step Functions refuses a call with: IAM's own wording, naming the session, the
/// action and the ARN the request addressed, and WHY. Written out here rather than built from
/// the service's own helper, so the wording has something to be wrong about.
fn refusal_message(index: usize, probe: &Probe) -> String {
    let action = probe.op.action();
    let reason = match probe.expected {
        Expected::ExplicitDeny => "with an explicit deny in an identity-based policy".to_string(),
        _ => format!("because no identity-based policy allows the {action} action"),
    };
    format!(
        "User: arn:aws:sts::{ACCOUNT}:assumed-role/{}/{} is not authorized to perform: {action} \
         on resource: {} {reason}",
        role_name(index),
        function_name(index),
        probe.op.resource(),
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

/// The trust policy the state machines' own role carries, so this service may assume it.
fn machine_trust_policy() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": "states.amazonaws.com"},
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// The inline document one row's role carries: the log actions every execution role needs,
/// the row's own allow where it has one, and its deny where it has one.
fn role_document(row: &Row) -> String {
    let mut statements = vec![json!({
        "Effect": "Allow",
        "Action": ["logs:CreateLogGroup", "logs:CreateLogStream", "logs:PutLogEvents"],
        "Resource": "*",
    })];
    if !row.allowed.is_empty() {
        statements.push(json!({
            "Effect": "Allow",
            "Action": row.allowed,
            "Resource": row.scope.resource(),
        }));
    }
    if !row.denied.is_empty() {
        statements.push(json!({
            "Effect": "Deny",
            "Action": row.denied,
            "Resource": row.scope.resource(),
        }));
    }
    json!({"Version": "2012-10-17", "Statement": statements}).to_string()
}

/// Create a role named `name` trusting `trust`, with `document` inline where there is one, and
/// answer the ARN IAM minted for it.
async fn create_role(
    router: &MultiServiceRouter,
    name: &str,
    trust: &str,
    document: Option<&str>,
) -> String {
    let (status, created) = query(
        router,
        "iam",
        "2010-05-08",
        "CreateRole",
        &[("RoleName", name), ("AssumeRolePolicyDocument", trust)],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateRole({name:?}) must succeed, got {status} with {created}"
    );
    let arn = opt_element(&created, "Arn").unwrap_or_else(|| {
        panic!("setup: CreateRole({name:?}) must answer the role's ARN, got {created}")
    });
    if let Some(document) = document {
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
    }
    arn
}

/// Create one state machine whose single `Wait` state holds every execution of it `RUNNING`
/// for the whole suite, asserting the ARN it reports is the one the statements and the
/// refusals name.
async fn create_machine(router: &MultiServiceRouter, name: &str, role_arn: &str) {
    let definition = json!({
        "StartAt": "Hold",
        "States": {"Hold": {"Type": "Wait", "Seconds": HOLD_SECONDS, "End": true}},
    })
    .to_string();
    let (status, created) = sfn(
        router,
        "CreateStateMachine",
        &json!({"name": name, "definition": definition, "roleArn": role_arn}),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateStateMachine({name:?}) must succeed, got {status} with {created}"
    );
    assert_eq!(
        created["stateMachineArn"].as_str(),
        Some(machine_arn(name).as_str()),
        "setup: the machine's ARN is what a statement and a refusal name, got {created}"
    );
}

/// Start one execution of [`MACHINE`] as the ACCOUNT's own credential, answering its ARN.
async fn start_execution(router: &MultiServiceRouter, name: &str) -> String {
    let (status, started) = sfn(
        router,
        "StartExecution",
        &json!({
            "stateMachineArn": machine_arn(MACHINE),
            "name": name,
            "input": json!({"marker": name}).to_string(),
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: StartExecution({name:?}) must succeed, got {status} with {started}"
    );
    started["executionArn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: StartExecution({name:?}) must answer an ARN, {started}"))
        .to_string()
}

/// The names of every execution [`MACHINE`] holds, read through this service's own
/// `ListExecutions` as the account's own credential.
async fn held_executions(router: &MultiServiceRouter, case: &str) -> Vec<String> {
    let (status, listed) = sfn(
        router,
        "ListExecutions",
        &json!({"stateMachineArn": machine_arn(MACHINE)}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: ListExecutions must succeed, got {status} with {listed}"
    );
    let mut names: Vec<String> = listed["executions"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|entry| entry["name"].as_str().map(ToString::to_string))
        .collect();
    names.sort();
    names
}

/// One execution as this service reports it, read as the account's own credential.
async fn described_execution(router: &MultiServiceRouter, case: &str, arn: &str) -> Value {
    let (status, described) = sfn(router, "DescribeExecution", &json!({"executionArn": arn})).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeExecution({arn:?}) must succeed, got {status} with {described}"
    );
    described
}

/// The stack declaring one function per row, each running as that row's role: the inline
/// source is archived into the deployment package by CloudFormation, which is how a function
/// whose handler is written here comes to exist at all.
fn function_stack(role_arns: &[String]) -> String {
    let mut resources = serde_json::Map::new();
    for (index, role_arn) in role_arns.iter().enumerate() {
        resources.insert(
            format!("Probe{index}"),
            json!({
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": function_name(index),
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": role_arn,
                    "Timeout": FUNCTION_TIMEOUT,
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

/// Deploy the functions' stack, wait for `CREATE_COMPLETE` and answer the physical id of each
/// declared function.
async fn deploy_functions(
    router: &MultiServiceRouter,
    stack: &str,
    role_arns: &[String],
) -> BTreeMap<String, String> {
    let template = function_stack(role_arns);
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
        let (_, events) = query(
            router,
            "cloudformation",
            "2010-05-15",
            "DescribeStackEvents",
            &[("StackName", stack)],
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
        &[("StackName", stack)],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: DescribeStackResources({stack:?}) must succeed, got {status} with {resources}"
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

/// Every probe: the function's own Step Functions call is decided against the policies in
/// force on its execution role, and what the service holds afterwards follows that decision.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_in_function_step_functions_call_is_decided_against_its_execution_role() {
    let router = build_router();

    let machine_role = create_role(
        &router,
        "xsvc-in-fn-sfn-machine-role",
        &machine_trust_policy(),
        None,
    )
    .await;
    create_machine(&router, MACHINE, &machine_role).await;
    create_machine(&router, OTHER_MACHINE, &machine_role).await;
    let control = start_execution(&router, CONTROL_EXECUTION).await;
    assert_eq!(
        control,
        execution_arn(CONTROL_EXECUTION),
        "setup: the control execution's ARN is what a statement and a refusal name"
    );

    let mut role_arns = Vec::new();
    for (index, row) in ROWS.iter().enumerate() {
        role_arns.push(
            create_role(
                &router,
                &role_name(index),
                &trust_policy(),
                Some(&role_document(row)),
            )
            .await,
        );
    }
    let physical = deploy_functions(&router, "xsvc-in-fn-sfn-probes", &role_arns).await;

    // Every execution the machine holds, in the order a list reports them: the control
    // execution, plus each one a SERVED start added — which is what a granted list must report
    // and what a refused start must leave untouched.
    let mut expected_executions = vec![CONTROL_EXECUTION.to_string()];

    for (index, row) in ROWS.iter().enumerate() {
        let logical = format!("Probe{index}");
        let function = physical.get(&logical).cloned().unwrap_or_else(|| {
            panic!("setup: the stack must report a physical id for {logical}")
        });
        assert_eq!(
            function,
            function_name(index),
            "setup: the stack must name the function {:?}, got {function:?}",
            function_name(index)
        );
        wait_until_active(&router, &function).await;

        for (probe_index, probe) in row.probes.iter().enumerate() {
            let case = probe.case;
            let execution_name = format!("probe-{index}-{probe_index}");

            let event = json!({
                "op": probe.op.event(),
                "state_machine": machine_arn(MACHINE),
                "execution": control,
                "execution_name": execution_name,
            });
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
            let faulted = headers
                .get("x-amz-function-error")
                .and_then(|value| value.to_str().ok())
                .map(str::to_string);
            assert_eq!(
                faulted, None,
                "case {case:?}: the handler must run to completion and report the service's own \
                 answer, got {faulted:?} with {payload}"
            );

            // What the handler reports is what Step Functions answered its call with.
            let served = probe.expected == Expected::Served;
            if served && probe.op == Op::Start {
                expected_executions.push(execution_name.clone());
                expected_executions.sort();
            }
            let expected = match (probe.expected, probe.op) {
                (Expected::Served, Op::Start) => json!({
                    "ok": true,
                    "started": execution_arn(&execution_name),
                }),
                (Expected::Served, Op::Describe) => json!({
                    "ok": true,
                    "described": CONTROL_EXECUTION,
                    "state": RUNNING,
                }),
                (Expected::Served, Op::List) => json!({
                    "ok": true,
                    "listed": expected_executions,
                }),
                _ => json!({
                    "ok": false,
                    "code": ACCESS_DENIED,
                    "message": refusal_message(index, probe),
                    "status": ACCESS_DENIED_STATUS,
                }),
            };
            assert_eq!(
                payload, expected,
                "case {case:?}: expected the handler to report {expected}, got {payload}"
            );

            // And what the SERVICE holds: a served start is an execution of the machine, with
            // the name and the input the handler asked for, and a refused one added nothing.
            let held = held_executions(&router, case).await;
            assert_eq!(
                held, expected_executions,
                "case {case:?}: expected the machine to hold {expected_executions:?}, got {held:?}"
            );
            if served && probe.op == Op::Start {
                let described =
                    described_execution(&router, case, &execution_arn(&execution_name)).await;
                assert_eq!(
                    described["stateMachineArn"].as_str(),
                    Some(machine_arn(MACHINE).as_str()),
                    "case {case:?}: expected the execution to be of the called machine, got \
                     {described}"
                );
                assert_eq!(
                    described["name"].as_str(),
                    Some(execution_name.as_str()),
                    "case {case:?}: expected the name the handler asked for, got {described}"
                );
                // The input is carried as the TEXT the handler serialized, so it is compared
                // as the document that text is: the SDK's own separators are not part of it.
                let carried: Value = described["input"]
                    .as_str()
                    .map(|text| serde_json::from_str(text).expect("the input is a JSON document"))
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: expected an input in {described}")
                    });
                assert_eq!(
                    carried,
                    json!({"marker": execution_name}),
                    "case {case:?}: expected the input the handler sent, got {described}"
                );
                assert_eq!(
                    described["status"].as_str(),
                    Some(RUNNING),
                    "case {case:?}: expected the started execution to be {RUNNING}, got \
                     {described}"
                );
            }
        }
    }
}
