// SPDX-License-Identifier: CC0-1.0

//! What authorizes the SEVERAL services ONE running function addresses in a single
//! invocation: the identity-based documents in force on that function's execution role,
//! asked about EACH call on its own.
//!
//! RULE: a call an AWS SDK makes from inside a running function is a real signed call made as
//! a session of that function's execution role, so every service serving one of them decides
//! it against the identity-based documents in force on that role and nothing else — an action
//! those documents do not allow is refused `AccessDeniedException` (HTTP 400, the status these
//! endpoints answer every refusal of a caller under) with IAM's own sentence, naming the
//! assumed-role session, the action the operation performs and the ARN the request addressed,
//! and nothing of the refused call happens; an action they allow is served and its effect is
//! in that service's own state afterwards. The decision is per call, so one role that grants
//! neither of two different services' actions earns a refusal from BOTH in the same
//! invocation, and one that grants exactly one of them is served that one and refused the
//! other. Either way the invocation is HTTP 200 with no `X-Amz-Function-Error`, because what
//! decides that is whether the handler returned: a refusal the handler catches is not a
//! failure of the run.
//!
//! "The execution role grants the function permission to use Amazon Web Services services"
//! (`FunctionConfiguration$Role`), the identity a function's own call speaks for is the
//! session `arn:aws:sts::<account>:assumed-role/<role>/<function>`, and with neither an
//! identity-based nor a resource-based allow a request is denied while an explicit deny wins
//! over any allow (IAM User Guide, *Policy evaluation logic*). Neither EventBridge's
//! `PutEvents` nor Step Functions' `StartExecution` models an authorization error of its own
//! (`smithy_query(query="operation", operation="PutEvents")` names `InternalException` only;
//! `smithy_query(query="digest")` on the Step Functions model lists no access error on any
//! operation), so on both surfaces the refusal is the AWS-common one.
//!
//! Six crates have to agree for that to hold — CloudFormation deploys each row's role and the
//! function running as it, IAM holds the roles and their documents, the token service mints
//! the session the handler's calls are signed as, Lambda runs the handler and supplies it the
//! SDK, EventBridge and Step Functions each take their own decision, and Amazon SQS holds what
//! the matched rule delivered — so the suite lives here: a crate-local suite can only ever see
//! one side of it.
//!
//! What the neighbours pin and this one does not repeat:
//! `lambda_in_function_events_authorization` drives the table of document SHAPES one
//! publication is decided by, `lambda_in_function_step_functions_authorization` the table of
//! Step Functions operations one role is asked about, and `events_lambda_sfn_in_function_chain`
//! one call per invocation along a chain. What only this suite reaches is TWO services deciding
//! TWO calls of ONE invocation against ONE role: that the session both of them blame is the
//! same session, that each grant covers only its own service, and that a role granting nothing
//! is refused by both — which is the whole of what a least-privilege execution role is written
//! for, and the one reading a per-service suite cannot take.
//!
//! The rows differ along what the rule generalises over: WHICH of the two services' actions the
//! role's document grants (neither, the publication's only, the start's only, both), HOW the
//! granting statement names them (the exact action on the exact resource, or every action of
//! the service on every resource) and whether the same document also DENIES one of them
//! explicitly. Each row asserts both halves on each leg — what the SDK gave the handler, and
//! what the addressed service holds afterwards — so a refusal that still published or still
//! started an execution, and a grant that answered without doing either, both fail here.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{element, elements, form_encode, physical_ids_of};

/// The Region every request here is signed for, and so the Region the bus, the queue, the
/// state machine and the functions belong to.
const REGION: &str = "us-east-1";

/// The account this emulator serves, which every ARN and every session below names.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// Where `Invoke` and the configuration read are bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The bus every handler publishes onto — a bus of the account's own rather than the default
/// one, so the ARN a refusal reports is one this suite created.
const BUS: &str = "xsvc-rolewide-bus";

/// The rule watching that bus, and the queue it targets: the place a publication EventBridge
/// accepted is read back from.
const RULE: &str = "xsvc-rolewide-rule";
const SINK: &str = "xsvc-rolewide-sink";

/// The state machine every handler starts, and the role it runs as.
const MACHINE: &str = "xsvc-rolewide-machine";
const MACHINE_ROLE: &str = "xsvc-rolewide-machine-role";

/// The `source` and `detail-type` every handler publishes under, which is all the rule matches
/// on — so a message on its queue is one of these publications and nothing else.
const SOURCE: &str = "com.demo.xsvc-rolewide";
const DETAIL_TYPE: &str = "In Function Fan Out";

/// The stack the roles and the functions are deployed by.
const STACK: &str = "xsvc-rolewide-probes";

/// The `StackStatus` that stack must reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The actions the handler's two calls perform, as a statement names them and as a refusal
/// reports them.
const PUT_EVENTS_ACTION: &str = "events:PutEvents";
const START_EXECUTION_ACTION: &str = "states:StartExecution";

/// The code and status both surfaces refuse a caller the documents in force do not permit
/// with.
const ACCESS_DENIED: &str = "AccessDeniedException";
const ACCESS_DENIED_STATUS: u64 = 400;

/// The status an execution of [`MACHINE`] sits in while its one `Wait` is outstanding
/// (`smithy_query(query="shape", shape="ExecutionStatus")`).
const RUNNING: &str = "RUNNING";

/// How long that `Wait` waits, in seconds: a whole day, so every execution a served start
/// created is still [`RUNNING`] whenever a later read looks at it, however loaded the host is.
/// Nothing waits this out — it is the span that must NOT elapse — so it is given generous room
/// rather than a budget the rows share.
const HOLD_SECONDS: u64 = 86_400;

/// The service principals the roles below are written for: the one Lambda assumes a function's
/// execution role as, the one Step Functions assumes a state machine's role as, and the one
/// EventBridge delivers to a queue target as.
const LAMBDA_PRINCIPAL: &str = "lambda.amazonaws.com";
const SFN_PRINCIPAL: &str = "states.amazonaws.com";
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";

/// The runtime every function is created with: a managed Python runtime, which carries the SDK
/// the handler imports.
const RUNTIME: &str = "python3.12";

/// The module and attribute the deployment package carries its handler as. CloudFormation
/// archives inline code under the handler's own file stem, so the module is `index`.
const HANDLER: &str = "index.handler";

/// How long one invocation is given, as the function's own `Timeout`: well past what a run
/// needs on any host, because what is under test is the authorization decisions and not how
/// fast an interpreter gets through two SDK calls.
const FUNCTION_TIMEOUT: u64 = 120;

/// How long work the CLOCK owes — a stack completing, a function becoming `Active`, an event
/// reaching the rule's target — is waited for before giving up, and how long between two looks.
/// A GENEROUS ceiling rather than an expectation: every wait here is on the CONDITION, and
/// nothing asserts how long anything took.
const DEADLINE: Duration = Duration::from_secs(180);
const LOOK: Duration = Duration::from_millis(50);

/// The handler every row runs. It makes BOTH calls in the one invocation, with the SDK its
/// managed runtime supplies, and reports per service what that service answered — the result,
/// or the refusal it raised as the SDK's own error class carries it.
const PROBE_HANDLER: &str = r#"
import json

import boto3
from botocore.exceptions import ClientError

EVENTS = boto3.client("events")
STATES = boto3.client("stepfunctions")


def refusal(error):
    return {
        "ok": False,
        "code": error.response["Error"]["Code"],
        "message": error.response["Error"]["Message"],
        "status": error.response["ResponseMetadata"]["HTTPStatusCode"],
    }


def handler(event, context):
    marker = event["marker"]
    report = {"sdk": boto3.__name__}
    try:
        answered = EVENTS.put_events(
            Entries=[
                {
                    "EventBusName": event["bus"],
                    "Source": event["source"],
                    "DetailType": event["detail_type"],
                    "Detail": json.dumps({"marker": marker}),
                }
            ]
        )
        report["events"] = {
            "ok": True,
            "failed_count": answered["FailedEntryCount"],
            "event_ids": [entry.get("EventId") for entry in answered["Entries"]],
        }
    except ClientError as error:
        report["events"] = refusal(error)
    try:
        answered = STATES.start_execution(
            stateMachineArn=event["state_machine"],
            name=marker,
            input=json.dumps({"marker": marker}),
        )
        report["states"] = {"ok": True, "execution_arn": answered["executionArn"]}
    except ClientError as error:
        report["states"] = refusal(error)
    return report
"#;

/// What a row's execution role grants of the two services the handler calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Grant {
    /// Nothing of either service: only the log writes every execution role carries.
    Neither,
    /// The publication's own action on the bus the entry addresses, and nothing of the other
    /// service.
    PublicationOnTheBus,
    /// The start's own action on the state machine the call names, and nothing of the other
    /// service.
    StartOnTheMachine,
    /// Every action of both services on every resource.
    EveryActionOfBoth,
    /// Every action of both services, with the start explicitly denied on the machine beside
    /// those allows.
    EveryActionWithTheStartDenied,
}

impl Grant {
    /// The statements the role's embedded document carries, the log writes included: what the
    /// decisions of this row are taken against.
    fn statements(self) -> Vec<Value> {
        let mut statements = vec![logs_statement()];
        match self {
            Self::Neither => {}
            Self::PublicationOnTheBus => {
                statements.push(allow(PUT_EVENTS_ACTION, &bus_arn(BUS)));
            }
            Self::StartOnTheMachine => {
                statements.push(allow(START_EXECUTION_ACTION, &machine_arn(MACHINE)));
            }
            Self::EveryActionOfBoth => {
                statements.push(allow("events:*", "*"));
                statements.push(allow("states:*", "*"));
            }
            Self::EveryActionWithTheStartDenied => {
                statements.push(allow("events:*", "*"));
                statements.push(allow("states:*", "*"));
                statements.push(json!({
                    "Effect": "Deny",
                    "Action": START_EXECUTION_ACTION,
                    "Resource": machine_arn(MACHINE),
                }));
            }
        }
        statements
    }
}

/// The statement every execution role carries, so a run can write its own logs — and which
/// grants nothing of either service the handler calls.
fn logs_statement() -> Value {
    json!({
        "Effect": "Allow",
        "Action": ["logs:CreateLogGroup", "logs:CreateLogStream", "logs:PutLogEvents"],
        "Resource": "*",
    })
}

/// One `Allow` statement of an identity-based document.
fn allow(action: &str, resource: &str) -> Value {
    json!({ "Effect": "Allow", "Action": action, "Resource": resource })
}

/// What one of the handler's two calls EARNS: the addressed service serves it, or refuses it
/// with the clause naming which of IAM's rules withheld it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// The documents in force allow the call: it is served and its effect is in that service.
    Served,
    /// They do not, and the refusal closes with this clause.
    Refused(&'static str),
}

/// The clause a refusal closes with when no document in force states anything about the call —
/// IAM's implicit deny — for each of the two actions.
const NO_POLICY_ALLOWS_PUBLICATION: &str =
    "because no identity-based policy allows the events:PutEvents action";
const NO_POLICY_ALLOWS_START: &str =
    "because no identity-based policy allows the states:StartExecution action";

/// The clause it closes with when a document in force DENIES the call explicitly.
const EXPLICIT_DENY: &str = "with an explicit deny in an identity-based policy";

/// One row: what the documents in force on one execution role grant, and what each of the two
/// calls the function running as it makes in ONE invocation earns.
struct Row {
    case: &'static str,
    grant: Grant,
    publication: Outcome,
    start: Outcome,
}

const ROWS: &[Row] = &[
    // The case the issue behind this suite is about: a role that grants neither action is
    // refused by BOTH services in the one invocation.
    Row {
        case: "a role granting neither service's action",
        grant: Grant::Neither,
        publication: Outcome::Refused(NO_POLICY_ALLOWS_PUBLICATION),
        start: Outcome::Refused(NO_POLICY_ALLOWS_START),
    },
    // Each grant covers its own service and says nothing about the other.
    Row {
        case: "a role granting the publication on the bus alone",
        grant: Grant::PublicationOnTheBus,
        publication: Outcome::Served,
        start: Outcome::Refused(NO_POLICY_ALLOWS_START),
    },
    Row {
        case: "a role granting the start on the state machine alone",
        grant: Grant::StartOnTheMachine,
        publication: Outcome::Refused(NO_POLICY_ALLOWS_PUBLICATION),
        start: Outcome::Served,
    },
    Row {
        case: "a role granting every action of both services",
        grant: Grant::EveryActionOfBoth,
        publication: Outcome::Served,
        start: Outcome::Served,
    },
    // An explicit deny wins over the allow beside it, and only over the call it is about.
    Row {
        case: "a role granting both with the start explicitly denied",
        grant: Grant::EveryActionWithTheStartDenied,
        publication: Outcome::Served,
        start: Outcome::Refused(EXPLICIT_DENY),
    },
];

/// The role one row runs as, the function it runs in, and the marker that run publishes and
/// names its execution by — the first two spelled here because the refusal's own sentence names
/// both: the identity a function's calls are signed as is the session
/// `assumed-role/<role>/<function>`.
fn role_name(index: usize) -> String {
    format!("xsvc-rolewide-role-{index}")
}
fn function_name(index: usize) -> String {
    format!("xsvc-rolewide-probe-{index}")
}
fn marker(index: usize) -> String {
    format!("xsvc-rolewide-marker-{index}")
}

/// The ARN of an event bus of this account in this Region: the resource a publication onto it
/// is authorized on, and the one a refused publication reports.
fn bus_arn(name: &str) -> String {
    format!("arn:aws:events:{REGION}:{ACCOUNT}:event-bus/{name}")
}

/// The ARN of a state machine of this account in this Region: the resource a start of it is
/// authorized on, and the one a refused start reports.
fn machine_arn(name: &str) -> String {
    format!("arn:aws:states:{REGION}:{ACCOUNT}:stateMachine:{name}")
}

/// The ARN of a queue of this account in this Region.
fn queue_arn(name: &str) -> String {
    format!("arn:aws:sqs:{REGION}:{ACCOUNT}:{name}")
}

/// The session the calls made by an invocation of the row's function speak for: the session is
/// named for the FUNCTION, which is how a refusal says which function was refused.
fn session_arn(index: usize) -> String {
    format!(
        "arn:aws:sts::{ACCOUNT}:assumed-role/{}/{}",
        role_name(index),
        function_name(index)
    )
}

/// The sentence a service refuses one of the handler's calls with. Written out here rather than
/// built from either service's own helper, so the wording has something to be wrong about.
fn refusal_message(index: usize, action: &str, resource: &str, reason: &str) -> String {
    format!(
        "User: {} is not authorized to perform: {action} on resource: {resource} {reason}",
        session_arn(index)
    )
}

/// The emulator as it is ASSEMBLED, on one engine: a handler's own SDK calls are dispatched
/// back into this same router, so the bus and the state machine they address are the ones this
/// suite reads.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The suite signs nothing, as in every other suite of this crate; the credential scope is
    // what routes a request. The HANDLER's own calls are signed by the runtime, with the
    // credential the invocation was given.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request. Its credential is the long-term key every emulator publishes, so every call this
/// suite makes on its own behalf is the account's and not any row's role's.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One `awsQuery` call of `service`, as the HTTP status and the XML it answered.
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
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One `awsQuery` call that must be served, as the XML it carried.
async fn served_query(
    router: &MultiServiceRouter,
    case: &str,
    service: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let (status, body) = query(router, service, version, action, members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {service} {action}, got {status} with {body}"
    );
    body
}

/// One `X-Amz-Target` RPC call that must be served, against the service whose signing name is
/// `signing_name`.
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
        .header("authorization", authorization(signing_name))
        .header(
            "content-type",
            format!("application/x-amz-json-{json_version}"),
        )
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
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

/// One Amazon SQS call that must be served (`awsJson1_0`, `AmazonSQS.<Operation>`).
async fn sqs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    rpc(router, case, "sqs", "AmazonSQS", "1.0", operation, body).await
}

/// One EventBridge call that must be served (`awsJson1_1`, `AWSEvents.<Operation>`).
async fn events(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    rpc(router, case, "events", "AWSEvents", "1.1", operation, body).await
}

/// One Step Functions call that must be served (`awsJson1_0`,
/// `AWSStepFunctions.<Operation>`).
async fn sfn(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    rpc(
        router,
        case,
        "states",
        "AWSStepFunctions",
        "1.0",
        operation,
        body,
    )
    .await
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
    let parsed: Value = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&body).to_string()))
    };
    (parts.status.as_u16(), parts.headers, parsed)
}

/// A trust policy admitting one service principal to `sts:AssumeRole`.
fn trust_policy(principal: &str) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": principal},
            "Action": "sts:AssumeRole",
        }],
    })
}

/// The bus every handler publishes onto, created by the account itself.
async fn given_bus(router: &MultiServiceRouter) {
    let created = events(router, "setup", "CreateEventBus", &json!({ "Name": BUS })).await;
    assert_eq!(
        created["EventBusArn"].as_str(),
        Some(bus_arn(BUS).as_str()),
        "setup: expected the created bus to report the ARN a refusal names, got {created}"
    );
}

/// The state machine every handler starts: one `Wait` state, so a start that was served leaves
/// an execution a later read finds still [`RUNNING`].
async fn given_machine(router: &MultiServiceRouter) {
    let created = served_query(
        router,
        "setup",
        "iam",
        "2010-05-08",
        "CreateRole",
        &[
            ("RoleName", MACHINE_ROLE),
            (
                "AssumeRolePolicyDocument",
                &trust_policy(SFN_PRINCIPAL).to_string(),
            ),
        ],
    )
    .await;
    let role_arn = element(&created, "Arn");
    let definition = json!({
        "StartAt": "Hold",
        "States": { "Hold": { "Type": "Wait", "Seconds": HOLD_SECONDS, "End": true } },
    })
    .to_string();
    let machine = sfn(
        router,
        "setup",
        "CreateStateMachine",
        &json!({
            "name": MACHINE,
            "definition": definition,
            "roleArn": role_arn,
        }),
    )
    .await;
    assert_eq!(
        machine["stateMachineArn"].as_str(),
        Some(machine_arn(MACHINE).as_str()),
        "setup: expected the created machine to report the ARN a refusal names, got {machine}"
    );
}

/// The queue the rule targets, its policy admitting the delivering service scoped to that rule,
/// and the rule itself. Answers the queue's url.
async fn given_rule_and_sink(router: &MultiServiceRouter) -> String {
    let case = "setup";
    let created = sqs(router, case, "CreateQueue", &json!({ "QueueName": SINK })).await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: expected a QueueUrl in {created}"))
        .to_string();

    let pattern = json!({ "source": [SOURCE], "detail-type": [DETAIL_TYPE] }).to_string();
    let rule = events(
        router,
        case,
        "PutRule",
        &json!({
            "Name": RULE,
            "EventBusName": BUS,
            "EventPattern": pattern,
            "State": "ENABLED",
        }),
    )
    .await;
    let rule_arn = rule["RuleArn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: expected a RuleArn in {rule}"))
        .to_string();

    // "EventBridge relies on resource-based policies" for a queue target (`PutTargets`), so the
    // queue admits the delivering service scoped to this rule and nothing else.
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "AllowRuleDelivery",
            "Effect": "Allow",
            "Principal": {"Service": EVENTS_PRINCIPAL},
            "Action": "sqs:SendMessage",
            "Resource": queue_arn(SINK),
            "Condition": {"ArnEquals": {"aws:SourceArn": rule_arn}},
        }],
    })
    .to_string();
    sqs(
        router,
        case,
        "SetQueueAttributes",
        &json!({ "QueueUrl": &url, "Attributes": { "Policy": policy } }),
    )
    .await;

    let targets = events(
        router,
        case,
        "PutTargets",
        &json!({
            "Rule": RULE,
            "EventBusName": BUS,
            "Targets": [{ "Id": "sink", "Arn": queue_arn(SINK) }],
        }),
    )
    .await;
    assert_eq!(
        targets["FailedEntryCount"],
        json!(0),
        "setup: expected PutTargets to accept the queue target, got {targets}"
    );
    url
}

/// The template the suite deploys: one execution role per row, carrying that row's embedded
/// document, and one function per row running as it. The inline source is archived into the
/// deployment package by CloudFormation, which is what makes the package hold exactly ONE
/// module — the handler's — and no SDK of its own.
fn stack_template() -> String {
    let mut resources = serde_json::Map::new();
    for (index, row) in ROWS.iter().enumerate() {
        let role_logical = format!("Role{index}");
        resources.insert(
            role_logical.clone(),
            json!({
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "RoleName": role_name(index),
                    "AssumeRolePolicyDocument": trust_policy(LAMBDA_PRINCIPAL),
                    "Policies": [{
                        "PolicyName": "call-grants",
                        "PolicyDocument": {
                            "Version": "2012-10-17",
                            "Statement": row.grant.statements(),
                        },
                    }],
                },
            }),
        );
        resources.insert(
            format!("Probe{index}"),
            json!({
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": function_name(index),
                    "Runtime": RUNTIME,
                    "Handler": HANDLER,
                    "Role": {"Fn::GetAtt": [role_logical, "Arn"]},
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

/// Deploy the stack, wait for `CREATE_COMPLETE` and answer the physical id of each declared
/// resource.
async fn deploy(router: &MultiServiceRouter) -> BTreeMap<String, String> {
    let template = stack_template();
    let (status, answered) = query(
        router,
        "cloudformation",
        "2010-05-15",
        "CreateStack",
        &[
            ("StackName", STACK),
            ("TemplateBody", &template),
            // The template declares named IAM roles, which a create acknowledges before
            // CloudFormation provisions them.
            ("Capabilities.member.1", "CAPABILITY_NAMED_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateStack({STACK:?}) must be accepted, got {status} with {answered}"
    );
    // The stack's completion is owed from the instant the owning services report its resources
    // ready, which no fixed sleep can name: the shared wait looks until it reports it, under a
    // deadline generous enough that a loaded host does not decide the outcome.
    let (reported, body) = wait_for_stack_status(router, STACK, CREATE_COMPLETE).await;
    let failures = if reported == CREATE_COMPLETE {
        Vec::new()
    } else {
        let (_, stack_events) = query(
            router,
            "cloudformation",
            "2010-05-15",
            "DescribeStackEvents",
            &[("StackName", STACK)],
        )
        .await;
        elements(&stack_events, "ResourceStatusReason")
    };
    assert_eq!(
        reported, CREATE_COMPLETE,
        "setup: the stack must reach {CREATE_COMPLETE}, got {body} with resource failures \
         {failures:?}"
    );
    let resources = served_query(
        router,
        "setup",
        "cloudformation",
        "2010-05-15",
        "DescribeStackResources",
        &[("StackName", STACK)],
    )
    .await;
    physical_ids_of(&resources)
}

/// Wait until Lambda reports `name` `Active`, which is the state an invocation needs.
async fn wait_until_active(router: &MultiServiceRouter, name: &str) {
    let giving_up_at = Instant::now() + DEADLINE;
    loop {
        let configuration = lambda(
            router,
            "GET",
            &format!("{FUNCTIONS_URI}/{name}/configuration"),
            &[],
        )
        .await
        .2;
        if configuration["State"].as_str() == Some("Active") {
            return;
        }
        assert!(
            Instant::now() < giving_up_at,
            "setup: Lambda must report {name:?} Active, last reported {configuration}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// `Invoke` the row's function synchronously, asking it to make both calls under `marker`, and
/// answer what it reported.
///
/// The invocation itself must be HTTP 200 with no `FunctionError` whichever way the two
/// decisions went: the handler catches a refusal and reports it, so a function error here is a
/// crash the runtime must not produce — and the report naming the SDK's own module is what says
/// the package's `import` resolved at all.
async fn invoked(router: &MultiServiceRouter, case: &str, function: &str, marker: &str) -> Value {
    let event = json!({
        "bus": BUS,
        "source": SOURCE,
        "detail_type": DETAIL_TYPE,
        "state_machine": machine_arn(MACHINE),
        "marker": marker,
    });
    let (status, headers, payload) = lambda(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{function}/invocations"),
        event.to_string().as_bytes(),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Invoke, got {status} with {payload}"
    );
    let signal = headers
        .get("x-amz-function-error")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    assert_eq!(
        signal, None,
        "case {case:?}: expected no FunctionError, got {signal:?} with {payload}"
    );
    assert_eq!(
        payload["sdk"],
        json!("boto3"),
        "case {case:?}: expected the run to report the SDK module it imported, got {payload}"
    );
    payload
}

/// Assert the refusal one leg reported: the addressed service's own code and status, and IAM's
/// sentence naming the session, the action and the ARN the request addressed.
fn assert_refused(
    reported: &Value,
    case: &str,
    index: usize,
    action: &str,
    resource: &str,
    reason: &str,
) {
    assert_eq!(
        reported["ok"],
        json!(false),
        "case {case:?}: expected {action} to be refused, got {reported}"
    );
    assert_eq!(
        reported["code"],
        json!(ACCESS_DENIED),
        "case {case:?}: expected the refusal code for {action}, got {reported}"
    );
    assert_eq!(
        reported["status"],
        json!(ACCESS_DENIED_STATUS),
        "case {case:?}: expected {action} refused on HTTP {ACCESS_DENIED_STATUS}, got {reported}"
    );
    assert_eq!(
        reported["message"],
        json!(refusal_message(index, action, resource, reason)),
        "case {case:?}: expected the refusal IAM words for {action}, got {reported}"
    );
}

/// The names of every execution [`MACHINE`] holds right now.
async fn execution_names(router: &MultiServiceRouter, case: &str) -> BTreeSet<String> {
    let listed = sfn(
        router,
        case,
        "ListExecutions",
        &json!({ "stateMachineArn": machine_arn(MACHINE) }),
    )
    .await;
    listed["executions"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|execution| execution["name"].as_str().map(ToString::to_string))
        .collect()
}

/// Every marker that has arrived on the rule's target queue, taken off it as it arrives and
/// accumulated across the whole suite — so a "nothing arrived" read is made over everything the
/// queue ever held rather than over one receive.
struct Sink {
    url: String,
    seen: Vec<String>,
}

impl Sink {
    /// Look again at the queue until `wanted` has arrived on it, or the generous deadline
    /// passes; everything received on the way is kept.
    async fn until(&mut self, router: &MultiServiceRouter, case: &str, wanted: &str) {
        let giving_up_at = Instant::now() + DEADLINE;
        loop {
            self.drain(router, case).await;
            if self.holds(wanted) {
                return;
            }
            assert!(
                Instant::now() < giving_up_at,
                "case {case:?}: expected {wanted:?} to be delivered to the rule's queue, got {:?}",
                self.seen
            );
            tokio::time::sleep(LOOK).await;
        }
    }

    /// Take what the queue hands over now, keeping each delivered event's marker and deleting
    /// the message so one delivery is counted once.
    async fn drain(&mut self, router: &MultiServiceRouter, case: &str) {
        let received = sqs(
            router,
            case,
            "ReceiveMessage",
            &json!({ "QueueUrl": &self.url, "MaxNumberOfMessages": 10 }),
        )
        .await;
        for message in received["Messages"].as_array().cloned().unwrap_or_default() {
            let body = message["Body"].as_str().unwrap_or_default();
            let envelope: Value = serde_json::from_str(body).unwrap_or(Value::Null);
            assert_eq!(
                envelope["source"],
                json!(SOURCE),
                "case {case:?}: expected the published source on the rule's queue, got {body}"
            );
            assert_eq!(
                envelope["detail-type"],
                json!(DETAIL_TYPE),
                "case {case:?}: expected the published detail-type on the rule's queue, got {body}"
            );
            if let Some(found) = envelope["detail"]["marker"].as_str() {
                self.seen.push(found.to_string());
            }
            sqs(
                router,
                case,
                "DeleteMessage",
                &json!({
                    "QueueUrl": &self.url,
                    "ReceiptHandle": message["ReceiptHandle"].as_str().unwrap_or_default(),
                }),
            )
            .await;
        }
    }

    /// Whether `marker` has ever arrived on that queue.
    fn holds(&self, marker: &str) -> bool {
        self.seen.iter().any(|seen| seen == marker)
    }
}

/// Every row: the two calls one invocation makes with the SDK its runtime supplies are each
/// decided by the service serving them against the documents in force on the one execution role
/// the invocation runs as — so a role granting neither is refused by both, a role granting one
/// is served that one and refused the other, and what the bus's rule delivered and what the
/// state machine holds afterwards follow those decisions.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn each_service_decides_an_in_function_call_against_the_execution_role() {
    let router = build_router();
    given_bus(&router).await;
    given_machine(&router).await;
    let url = given_rule_and_sink(&router).await;
    let physical = deploy(&router).await;
    let mut sink = Sink {
        url,
        seen: Vec::new(),
    };
    let mut started: BTreeSet<String> = BTreeSet::new();

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
            "setup: expected the stack to report the function it declared for {case:?}"
        );
        wait_until_active(&router, &function).await;

        let under = marker(index);
        let report = invoked(&router, case, &function, &under).await;
        let publication = &report["events"];
        let start = &report["states"];

        match row.publication {
            Outcome::Served => {
                assert_eq!(
                    publication["ok"],
                    json!(true),
                    "case {case:?}: expected the publication to be served, got {report}"
                );
                assert_eq!(
                    publication["failed_count"],
                    json!(0),
                    "case {case:?}: expected no failed entry, got {report}"
                );
                let ids = publication["event_ids"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                assert_eq!(
                    ids.len(),
                    1,
                    "case {case:?}: expected one result entry per published entry, got {report}"
                );
                assert!(
                    ids[0].as_str().is_some_and(|id| !id.is_empty()),
                    "case {case:?}: expected EventBridge's own event id, got {report}"
                );
                // The accepted event is matched by the rule and delivered to its target, which
                // is where the publication's effect is read.
                sink.until(&router, case, &under).await;
            }
            Outcome::Refused(reason) => assert_refused(
                publication,
                case,
                index,
                PUT_EVENTS_ACTION,
                &bus_arn(BUS),
                reason,
            ),
        }

        match row.start {
            Outcome::Served => {
                let arn = start["execution_arn"].as_str().unwrap_or_else(|| {
                    panic!("case {case:?}: expected the started execution's ARN, got {report}")
                });
                let described = sfn(
                    &router,
                    case,
                    "DescribeExecution",
                    &json!({ "executionArn": arn }),
                )
                .await;
                assert_eq!(
                    described["name"].as_str(),
                    Some(under.as_str()),
                    "case {case:?}: expected the execution the handler named, got {described}"
                );
                assert_eq!(
                    described["stateMachineArn"].as_str(),
                    Some(machine_arn(MACHINE).as_str()),
                    "case {case:?}: expected the machine the handler named, got {described}"
                );
                assert_eq!(
                    described["status"].as_str(),
                    Some(RUNNING),
                    "case {case:?}: expected the started execution {RUNNING}, got {described}"
                );
                let input: Value =
                    serde_json::from_str(described["input"].as_str().unwrap_or_default())
                        .unwrap_or(Value::Null);
                assert_eq!(
                    input,
                    json!({ "marker": under }),
                    "case {case:?}: expected the input the handler sent, got {described}"
                );
                started.insert(under.clone());
            }
            Outcome::Refused(reason) => assert_refused(
                start,
                case,
                index,
                START_EXECUTION_ACTION,
                &machine_arn(MACHINE),
                reason,
            ),
        }
    }

    // Nothing of a refused start is in Step Functions: the machine holds exactly the executions
    // the SERVED starts created, read through the service's own listing.
    let held = execution_names(&router, "what the state machine holds afterwards").await;
    assert_eq!(
        held, started,
        "expected the machine to hold exactly the executions the served starts created, got \
         {held:?} against {started:?}"
    );

    // Nothing of a refused publication ever reached the bus. The read is taken at an instant by
    // which an event planted through the SAME path by the account has already been delivered, so
    // the absence is the refusal's and not the clock's.
    let control = "an event planted by the account after the refused publications";
    let sentinel = "xsvc-rolewide-sentinel";
    let planted = events(
        &router,
        control,
        "PutEvents",
        &json!({
            "Entries": [{
                "EventBusName": BUS,
                "Source": SOURCE,
                "DetailType": DETAIL_TYPE,
                "Detail": json!({ "marker": sentinel }).to_string(),
            }],
        }),
    )
    .await;
    assert_eq!(
        planted["FailedEntryCount"],
        json!(0),
        "case {control:?}: expected the account's own publication to be accepted, got {planted}"
    );
    sink.until(&router, control, sentinel).await;
    for (index, row) in ROWS.iter().enumerate() {
        if row.publication == Outcome::Served {
            continue;
        }
        let refused = marker(index);
        assert!(
            !sink.holds(&refused),
            "case {:?}: expected nothing of the refused publication on the rule's queue, got {:?}",
            row.case,
            sink.seen
        );
    }
}
