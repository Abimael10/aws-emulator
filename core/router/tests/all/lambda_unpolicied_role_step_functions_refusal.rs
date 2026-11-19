// SPDX-License-Identifier: CC0-1.0

//! What a RUNNING function's own Step Functions call is authorized against when the function's
//! execution role carries NO identity-based policy at all — no inline document and no
//! attachment.
//!
//! RULE: a call an AWS SDK makes from inside a running function is a signed call made as a
//! session of that function's execution role, and Step Functions decides it at the IAM layer
//! BEFORE anything is read, written or minted: with no identity-based document in force on that
//! role, nothing allows the action and the call falls to IAM's default deny — HTTP 400
//! `AccessDeniedException` carrying IAM's own sentence, which names the assumed-role session the
//! call was signed as, the action the operation performs and the ARN the request addressed — and
//! the refused call leaves the service exactly as it was, so a refused start creates no
//! execution. The same calls by the same function once a document granting them is in force are
//! served, which is what shows the refusal to be the ABSENT document's doing rather than a
//! blanket refusal of role-signed traffic.
//!
//! "The execution role grants the function permission to use Amazon Web Services services"
//! (`FunctionConfiguration$Role`), the identity such a call speaks for is the session
//! `arn:aws:sts::<account>:assumed-role/<role>/<function>`, and with neither an identity-based
//! nor a resource-based allow a request is denied (IAM User Guide, *Policy evaluation logic*).
//! No error shape of the Step Functions model carries that refusal — no operation of that service
//! lists an access error (`smithy_query(query="digest")`) — so it is the AWS-common
//! `AccessDeniedException`, at the status that endpoint refuses a caller under.
//!
//! Five crates have to agree for that to hold — CloudFormation deploys the function, IAM holds
//! the role and (later) its one document, the token service mints the session the handler's calls
//! are signed as, Lambda runs the handler and supplies it the SDK, and Step Functions takes the
//! decision — so the suite lives here: a crate-local suite can only ever see one side of it.
//!
//! What the neighbours pin and this one does not repeat:
//! `lambda_in_function_step_functions_authorization` drives the table of document SHAPES one
//! execution role's calls are decided by, and `lambda_execution_role_decides_every_service_it_calls`
//! drives two services deciding two calls of one invocation. Every row of those gives the role a
//! document to be judged by; what only this suite reaches is the role that carries NO document at
//! all — the least-privilege role a client deploys before it writes any grant — which is the
//! collection answering with an empty set rather than with a document that happens to grant
//! nothing.
//!
//! The rows differ along WHICH operation the handler calls — a start (a write that mints an
//! execution), a listing of the machine's executions and a read of one execution — so the rule is
//! asked about a request addressed by a state machine's ARN and about one addressed by an
//! execution's own, and about a mutating call as well as two reporting ones. Each probe asserts
//! both halves: what the handler reports of the answer its call got, and what Step Functions
//! holds afterwards — so a refusal that still started an execution, and a grant that answered
//! without starting one, both fail here.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{elements, form_encode, opt_element, physical_ids_of};

/// The Region every request here is signed for, and so the Region the state machine and the
/// function belong to.
const REGION: &str = "us-east-1";

/// The account this emulator serves, which every ARN and the session's ARN carries.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The state machine the handler addresses, and the role it runs as.
const MACHINE: &str = "unpolicied-role-machine";
const MACHINE_ROLE: &str = "unpolicied-role-machine-role";

/// The function whose handler makes the calls, the role it runs as — the role that carries no
/// identity-based policy — and the stack that declares the function.
const FUNCTION: &str = "unpolicied-role-probe";
const PROBE_ROLE: &str = "unpolicied-role-probe-role";
const STACK: &str = "unpolicied-role-probe-stack";

/// The logical id the stack declares the function under.
const PROBE_LOGICAL_ID: &str = "Probe";

/// The execution this suite starts itself, as the ACCOUNT's own credential: what the read probe
/// addresses, and the one execution the machine holds while every call of the function is refused.
const CONTROL_EXECUTION: &str = "control-execution";

/// The execution the function starts once a grant is in force. It sorts after
/// [`CONTROL_EXECUTION`], which is the order a listing reports the two in.
const GRANTED_EXECUTION: &str = "granted-run";

/// How long the state machine's one `Wait` state waits, in seconds: a whole day, so every
/// execution started here is still `RUNNING` whenever a later probe reads it, however loaded the
/// host is. Nothing waits this out — it is the span that must NOT elapse — so it is given
/// generous room rather than a budget the probes share.
const HOLD_SECONDS: u64 = 86_400;

/// The status an execution of that machine sits in while its `Wait` is outstanding
/// (`smithy_query(query="shape",shape="ExecutionStatus")`).
const RUNNING: &str = "RUNNING";

/// The `StackStatus` the function's stack must reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The `State` a function reports once it can be invoked (`FunctionConfiguration$State`: "Active"
/// once "the function is ready to be invoked").
const FUNCTION_ACTIVE: &str = "Active";

/// The code and status Step Functions refuses a caller the documents in force do not permit with:
/// the AWS-common `AccessDeniedException`, at the client-error status that endpoint answers every
/// refusal of a caller under.
const ACCESS_DENIED: &str = "AccessDeniedException";
const ACCESS_DENIED_STATUS: u16 = 400;

/// How long one invocation is given, as the function's own `Timeout`: well past what a run needs
/// on any host, because what is under test is the authorization decision and not how fast an
/// interpreter gets through one SDK call.
const FUNCTION_TIMEOUT: u64 = 900;

/// How often to look while waiting for work the clock owes — a created function reaching
/// [`FUNCTION_ACTIVE`] — and how many looks to take. The span is that service's own, so no fixed
/// sleep names the instant it ends, and the deadline is generous so a loaded host cannot decide
/// the outcome.
const LOOK: Duration = Duration::from_millis(100);
const LOOKS: usize = 1_800;

/// The handler every probe runs: it builds a Step Functions client with the SDK the runtime
/// supplies and performs the one operation the event names, reporting either what the service
/// answered or the refusal it answered with — its code, its sentence and the HTTP status it came
/// back on.
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

/// The emulator as it is ASSEMBLED, on one engine: a handler's own SDK call is dispatched back
/// into this same router, so the state machine it addresses is the one this suite reads.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate; the credential
    // scope is what routes a request.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes the
/// request. Its credential is the long-term key every emulator publishes, so the calls this suite
/// makes itself are the account's own and not the probe role's.
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

/// One Step Functions operation over its `awsJson1_0` wire, made as the account's own credential —
/// a caller this gate does not bound — so what it reads is what the service holds and not the
/// probe role's decision.
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

/// The ARN of a state machine of this account in this Region, which is what a statement names and
/// what a refusal of a machine-addressed call reports.
fn machine_arn(name: &str) -> String {
    format!("arn:aws:states:{REGION}:{ACCOUNT}:stateMachine:{name}")
}

/// The ARN of one execution of [`MACHINE`] — the resource an execution-addressed call is judged
/// against.
fn execution_arn(name: &str) -> String {
    format!("arn:aws:states:{REGION}:{ACCOUNT}:execution:{MACHINE}:{name}")
}

/// The session the function's own calls are signed as: a session of the role it runs as, named
/// after the function.
fn session_arn() -> String {
    format!("arn:aws:sts::{ACCOUNT}:assumed-role/{PROBE_ROLE}/{FUNCTION}")
}

/// WHICH operation of Step Functions one probe's handler calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    /// A write: the start of an execution of the state machine.
    Start,
    /// A list of the machine's executions, addressed by the machine's ARN.
    List,
    /// A read of ONE execution, addressed by that execution's own ARN.
    Describe,
}

impl Op {
    /// The word the event names it by, which the handler reads.
    fn event(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::List => "list",
            Self::Describe => "describe",
        }
    }

    /// The IAM action the operation performs, which is the action a refusal names: the service's
    /// ARN namespace and the operation's own modelled name.
    fn action(self) -> &'static str {
        match self {
            Self::Start => "states:StartExecution",
            Self::List => "states:ListExecutions",
            Self::Describe => "states:DescribeExecution",
        }
    }

    /// The ARN the request ADDRESSES, which is the resource a refusal names: the state machine for
    /// the start and the list, the EXECUTION's own ARN for the read of one.
    fn resource(self) -> String {
        match self {
            Self::Start | Self::List => machine_arn(MACHINE),
            Self::Describe => execution_arn(CONTROL_EXECUTION),
        }
    }
}

/// Every probe, in the order they are taken: the operation the handler calls, and the name it
/// starts its execution under where it starts one.
const PROBES: &[(&str, Op)] = &[
    ("a start by a role with no identity-based policy", Op::Start),
    ("a listing by a role with no identity-based policy", Op::List),
    (
        "a read of one execution by a role with no identity-based policy",
        Op::Describe,
    ),
];

/// The sentence Step Functions refuses one of these calls with: IAM's own wording, naming the
/// session the call was signed as, the action the operation performs, the ARN the request
/// addressed and WHY. Written out here rather than built from the service's own helper, so the
/// wording has something to be wrong about.
fn refusal_message(op: Op) -> String {
    let action = op.action();
    format!(
        "User: {} is not authorized to perform: {action} on resource: {} because no \
         identity-based policy allows the {action} action",
        session_arn(),
        op.resource(),
    )
}

/// The trust policy a Lambda execution role needs.
fn lambda_trust_policy() -> String {
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

/// The trust policy the state machine's own role carries, so Step Functions may assume it.
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

/// The one document put in force on the probe role for the second phase: every action of Step
/// Functions on every resource, so each of the three probes is granted exactly what its own call
/// asks for.
fn every_action_document() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{"Effect": "Allow", "Action": "states:*", "Resource": "*"}],
    })
    .to_string()
}

/// Create a role named `name` trusting `trust` and carrying NO policy of any kind, answering the
/// ARN IAM minted for it.
async fn create_unpolicied_role(router: &MultiServiceRouter, name: &str, trust: &str) -> String {
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
    opt_element(&created, "Arn").unwrap_or_else(|| {
        panic!("setup: CreateRole({name:?}) must answer the role's ARN, got {created}")
    })
}

/// Put `document` in force as the one inline policy of `role`.
async fn put_role_policy(router: &MultiServiceRouter, case: &str, role: &str, document: &str) {
    let (status, embedded) = query(
        router,
        "iam",
        "2010-05-08",
        "PutRolePolicy",
        &[
            ("RoleName", role),
            ("PolicyName", "probe-grants"),
            ("PolicyDocument", document),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutRolePolicy({role:?}) must succeed, got {status} with {embedded}"
    );
}

/// Create the state machine whose single `Wait` state holds every execution of it `RUNNING` for
/// the whole suite, asserting the ARN it reports is the one the refusals name.
async fn create_machine(router: &MultiServiceRouter, role_arn: &str) {
    let definition = json!({
        "StartAt": "Hold",
        "States": {"Hold": {"Type": "Wait", "Seconds": HOLD_SECONDS, "End": true}},
    })
    .to_string();
    let (status, created) = sfn(
        router,
        "CreateStateMachine",
        &json!({"name": MACHINE, "definition": definition, "roleArn": role_arn}),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateStateMachine({MACHINE:?}) must succeed, got {status} with {created}"
    );
    assert_eq!(
        created["stateMachineArn"].as_str(),
        Some(machine_arn(MACHINE).as_str()),
        "setup: the machine's ARN is what a refusal names, got {created}"
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

/// The names of every execution [`MACHINE`] holds, read through Step Functions' own
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

/// One execution as Step Functions reports it, read as the account's own credential.
async fn described_execution(router: &MultiServiceRouter, case: &str, arn: &str) -> Value {
    let (status, described) = sfn(router, "DescribeExecution", &json!({"executionArn": arn})).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeExecution({arn:?}) must succeed, got {status} with {described}"
    );
    described
}

/// The stack declaring the probe function, running as `role_arn`: the inline source is archived
/// into the deployment package by CloudFormation, which is how a function whose handler is written
/// here comes to exist at all.
fn function_stack(role_arn: &str) -> String {
    let mut resources = serde_json::Map::new();
    resources.insert(
        PROBE_LOGICAL_ID.to_string(),
        json!({
            "Type": "AWS::Lambda::Function",
            "Properties": {
                "FunctionName": FUNCTION,
                "Runtime": "python3.12",
                "Handler": "index.handler",
                "Role": role_arn,
                "Timeout": FUNCTION_TIMEOUT,
                "Code": {"ZipFile": PROBE_HANDLER},
            },
        }),
    );
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": Value::Object(resources),
    })
    .to_string()
}

/// Deploy the function's stack, wait for `CREATE_COMPLETE`, and answer the name Lambda holds the
/// function under.
async fn deploy_function(router: &MultiServiceRouter, role_arn: &str) -> String {
    let template = function_stack(role_arn);
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
    // The stack's completion is owed from the instant Lambda reports its function created, which
    // no fixed sleep can name: this looks until it reports it, with a deadline generous enough
    // that a loaded host does not decide the outcome.
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
    let physical = physical_ids_of(&resources);
    let named = physical
        .get(PROBE_LOGICAL_ID)
        .cloned()
        .unwrap_or_else(|| panic!("setup: the stack must report a physical id for the function"));
    assert_eq!(
        named, FUNCTION,
        "setup: the stack must name the function {FUNCTION:?}, got {named:?}"
    );
    named
}

/// Wait until Lambda reports `name` [`FUNCTION_ACTIVE`], which is the state an invocation needs.
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

/// Invoke the probe function for one operation, answering the payload its run produced — and
/// asserting the invocation itself was served, because what decides that is whether the handler
/// RETURNED: a refusal the handler catches is not a failure of the run.
async fn probe(router: &MultiServiceRouter, case: &str, op: Op, name: &str) -> Value {
    let event = json!({
        "op": op.event(),
        "state_machine": machine_arn(MACHINE),
        "execution": execution_arn(CONTROL_EXECUTION),
        "execution_name": name,
    });
    let (status, headers, payload) = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{FUNCTION}/invocations"),
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
        "case {case:?}: the handler must run to completion and report the service's own answer, \
         got {faulted:?} with {payload}"
    );
    payload
}

/// Every probe: a function whose execution role carries NO identity-based policy is refused each
/// Step Functions call it makes, with IAM's own sentence and nothing created, and is served the
/// identical calls once one document granting them is in force.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_function_whose_role_carries_no_identity_policy_is_refused_its_step_functions_calls() {
    let router = build_router();

    let machine_trust = machine_trust_policy();
    let machine_role = create_unpolicied_role(&router, MACHINE_ROLE, &machine_trust).await;
    create_machine(&router, &machine_role).await;
    let control = start_execution(&router, CONTROL_EXECUTION).await;
    assert_eq!(
        control,
        execution_arn(CONTROL_EXECUTION),
        "setup: the control execution's ARN is what a refusal of the read names"
    );

    // The function's execution role carries NO identity-based policy at all: no inline document
    // and no attachment, which is the role a client deploys before it writes any grant.
    let probe_trust = lambda_trust_policy();
    let probe_role = create_unpolicied_role(&router, PROBE_ROLE, &probe_trust).await;
    let function = deploy_function(&router, &probe_role).await;
    wait_until_active(&router, &function).await;

    // The only execution the machine holds while every call of the function is refused.
    let only_the_control = vec![CONTROL_EXECUTION.to_string()];

    for (case, op) in PROBES {
        let case = *case;
        let op = *op;
        let refused_name = format!("refused-{}", op.event());
        let payload = probe(&router, case, op, &refused_name).await;
        let expected = json!({
            "ok": false,
            "code": ACCESS_DENIED,
            "message": refusal_message(op),
            "status": ACCESS_DENIED_STATUS,
        });
        assert_eq!(
            payload, expected,
            "case {case:?}: expected the handler to report {expected}, got {payload}"
        );
        // And nothing of the refused call happened: the machine holds what it held before it.
        let held = held_executions(&router, case).await;
        assert_eq!(
            held, only_the_control,
            "case {case:?}: expected the machine to hold {only_the_control:?}, got {held:?}"
        );
    }

    // The same function, the same calls, once ONE document granting them is in force on the same
    // role: each is served, which is what shows the refusals above to be the ABSENT document's
    // doing and not a blanket refusal of the session the handler's calls are signed as.
    let case = "every action of the service granted on every resource";
    put_role_policy(&router, case, PROBE_ROLE, &every_action_document()).await;

    let started = probe(&router, case, Op::Start, GRANTED_EXECUTION).await;
    assert_eq!(
        started,
        json!({"ok": true, "started": execution_arn(GRANTED_EXECUTION)}),
        "case {case:?}: expected the start to be served, got {started}"
    );
    let both = vec![CONTROL_EXECUTION.to_string(), GRANTED_EXECUTION.to_string()];
    let listed = probe(&router, case, Op::List, GRANTED_EXECUTION).await;
    assert_eq!(
        listed,
        json!({"ok": true, "listed": both}),
        "case {case:?}: expected the handler to list {both:?}, got {listed}"
    );
    let described = probe(&router, case, Op::Describe, GRANTED_EXECUTION).await;
    assert_eq!(
        described,
        json!({"ok": true, "described": CONTROL_EXECUTION, "state": RUNNING}),
        "case {case:?}: expected the read of the control execution to be served, got {described}"
    );

    // And the served start is an execution of the machine, read back through Step Functions' own
    // reader: the name and the input the handler asked for, under the ARN it was answered with.
    let held = held_executions(&router, case).await;
    assert_eq!(
        held, both,
        "case {case:?}: expected the machine to hold {both:?}, got {held:?}"
    );
    let granted_arn = execution_arn(GRANTED_EXECUTION);
    let read_back = described_execution(&router, case, &granted_arn).await;
    assert_eq!(
        read_back["stateMachineArn"].as_str(),
        Some(machine_arn(MACHINE).as_str()),
        "case {case:?}: expected the execution to be of the called machine, got {read_back}"
    );
    assert_eq!(
        read_back["name"].as_str(),
        Some(GRANTED_EXECUTION),
        "case {case:?}: expected the name the handler asked for, got {read_back}"
    );
    // The input is carried as the TEXT the handler serialized, so it is compared as the document
    // that text is: the SDK's own separators are not part of it.
    let carried: Value = read_back["input"]
        .as_str()
        .map(|text| serde_json::from_str(text).expect("the input is a JSON document"))
        .unwrap_or_else(|| panic!("case {case:?}: expected an input in {read_back}"));
    assert_eq!(
        carried,
        json!({"marker": GRANTED_EXECUTION}),
        "case {case:?}: expected the input the handler sent, got {read_back}"
    );
    assert_eq!(
        read_back["status"].as_str(),
        Some(RUNNING),
        "case {case:?}: expected the started execution to be {RUNNING}, got {read_back}"
    );
}
