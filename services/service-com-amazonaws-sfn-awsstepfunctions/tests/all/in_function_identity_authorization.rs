// SPDX-License-Identifier: CC0-1.0

//! What a call this service is made by a RUNNING LAMBDA FUNCTION is authorized against: the
//! identity-based policies in force on that function's EXECUTION ROLE at the instant of the
//! call.
//!
//! Driven over each service's own wire protocol through the production `MultiServiceRouter`
//! with Step Functions, Lambda, IAM and STS mounted on ONE engine — the shape
//! `core/router/src/lib.rs` mounts them in — so the role a decision reads the documents of is
//! the role IAM's own handlers wrote, the session the handler's call is signed as is the one
//! Lambda ran it under, and the executions it starts, lists, reads and stops are the ones this
//! suite reads back through this service's own API.
//!
//! RULE: a call an AWS SDK makes from inside a running function is made as a session of that
//! function's execution role, and this service decides it at the IAM layer BEFORE it is
//! dispatched — served only where the documents then in force on that role allow
//! `states:<OperationName>` on the ARN the request names, and otherwise refused HTTP 400
//! `AccessDeniedException` naming the assumed-role session, that action and that ARN, with
//! nothing of the account read or changed on the way to the refusal. The decision is taken per
//! request, so a statement just withdrawn refuses the next call and one put back serves it
//! again.
//!
//! "The execution role grants the function permission to use Amazon Web Services services"
//! (`FunctionConfiguration$Role`), and the identity such a call speaks for is the session
//! `arn:aws:sts::<account>:assumed-role/<role>/<function>`; with neither an identity-based nor
//! a resource-based allow a request is denied (IAM User Guide, *Policy evaluation logic*). No
//! error shape of this model carries that refusal — no operation of this service lists an
//! access error (`smithy_query(query="digest")`) — so it is the AWS-common
//! `AccessDeniedException` the authorization layer in front of this service answers, at the
//! status this endpoint refuses a caller under.
//!
//! What the neighbours pin and this one does not repeat: `identity_policy_authorization`
//! drives the same decision for a session a CLIENT assumed directly through STS, and
//! `lambda_callback_task_token` drives a handler answering a callback token under a role
//! nothing decides. What only this suite reaches is the decision taken on a call a FUNCTION
//! made as the role it RUNS AS, for the operations a least-privilege orchestration role is
//! written about — including the settlement of a callback task whose token the handler holds,
//! which names no ARN of its own.
//!
//! The rows differ along what the rule generalises over: WHICH operation the handler calls (a
//! start, a list of a machine's executions, a read of one execution, a stop, and — in the last
//! test — a `SendTaskSuccess` addressed only by its task token), WHAT the role's document
//! allows (nothing of this service, the operation's own action, another operation's action,
//! every action of the service), WHICH resource that statement names (the state machine the
//! call addresses, another machine of the account, that machine's executions, every resource),
//! and WHEN the document says it (granted, withdrawn, put back). Every probe asserts both
//! halves: what the handler reports of the call, and what THIS SERVICE holds afterwards — so a
//! refusal that still started or stopped an execution, and a grant that answered without doing
//! it, both fail here.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_sfn_awsstepfunctions::state::EVENT_TASK_SUBMITTED;
use service_com_amazonaws_sfn_awsstepfunctions::EmulatedService;

use crate::execution_data_encryption::{form_encode, xml_element};
use crate::lambda_task_control_flow::package;
use crate::state_machine_lifecycle::{error_code, string, ACCOUNT, REGION};

/// How one service's requests are addressed on the wire: Step Functions' signing name and
/// `X-Amz-Target` prefix (`smithy_query(query="service_info")`).
const SIGNING_NAME: &str = "states";
/// See [`SIGNING_NAME`].
const TARGET_PREFIX: &str = "AWSStepFunctions";

/// Where Lambda binds `CreateFunction` (`POST /2015-03-31/functions`).
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The `State` a function reports once it can be invoked (`FunctionConfiguration$State`:
/// "Active" once "the function is ready to be invoked").
const FUNCTION_ACTIVE: &str = "Active";

/// The runtime the probe handler is written for.
const RUNTIME: &str = "python3.12";

/// The handler the package's one module exports, as `Handler` names it.
const HANDLER: &str = "index.handler";

/// The code and status this service refuses a call the documents in force do not allow with:
/// the AWS-common `AccessDeniedException`, at the client-error status this `awsJson1_0`
/// endpoint answers every refusal of a caller under.
const ACCESS_DENIED: &str = "AccessDeniedException";
const ACCESS_DENIED_STATUS: u64 = 400;

/// The modelled refusal a read of an execution that was never created answers
/// (`DescribeExecution`'s `ExecutionDoesNotExist`), which is how a refused start is shown to
/// have created nothing.
const EXECUTION_DOES_NOT_EXIST: &str = "ExecutionDoesNotExist";

/// The name of the one inline policy every role here carries, which each rewrite replaces.
const POLICY_NAME: &str = "inline";

/// The service principal a role THIS service acts as has to trust, so the integration call a
/// callback `Task` makes can be made at all.
const SFN_SERVICE_PRINCIPAL: &str = "states.amazonaws.com";

/// The service principal a LAMBDA execution role has to trust, so the function can be handed
/// the role at all.
const LAMBDA_SERVICE_PRINCIPAL: &str = "lambda.amazonaws.com";

/// A definition whose one state holds, so every execution started below stays `RUNNING` for
/// the whole suite: a stop has something to abort and a read has a status to report, and no
/// assertion turns on how fast the host runs.
const HOLDING_DEFINITION: &str =
    r#"{"StartAt":"Hold","States":{"Hold":{"Type":"Wait","Seconds":900,"End":true}}}"#;

/// The statuses an execution reports here (`ExecutionStatus`).
const RUNNING: &str = "RUNNING";
const ABORTED: &str = "ABORTED";
const SUCCEEDED: &str = "SUCCEEDED";

/// How long a poll below waits for work the clock owes — a created function reaching `Active`,
/// a callback task handed over, a settled execution reaching its end — and how long it waits
/// between two looks. Generous: nothing here measures how fast the host is, so the deadline is
/// only a backstop against a condition that never holds at all.
const PATIENCE: Duration = Duration::from_secs(120);
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The handler every probe runs. It performs the ONE operation of this service the event names
/// with the SDK its runtime supplies, and reports either what that call answered or the refusal
/// the service answered it with — its code, its sentence and the HTTP status it came back on.
const PROBE_HANDLER: &str = r#"
import boto3
from botocore.exceptions import ClientError


def call(client, event):
    operation = event["op"]
    if operation == "start":
        answered = client.start_execution(
            stateMachineArn=event["machine"], name=event["run"]
        )
        return {"ok": True, "execution": answered["executionArn"]}
    if operation == "list":
        answered = client.list_executions(stateMachineArn=event["machine"])
        return {
            "ok": True,
            "executions": sorted(
                entry["executionArn"] for entry in answered["executions"]
            ),
        }
    if operation == "describe":
        answered = client.describe_execution(executionArn=event["execution"])
        return {"ok": True, "status": answered["status"]}
    if operation == "stop":
        answered = client.stop_execution(executionArn=event["execution"])
        return {"ok": True, "stopped": bool(answered.get("stopDate"))}
    client.send_task_success(taskToken=event["token"], output=event["output"])
    return {"ok": True, "settled": True}


def handler(event, context):
    client = boto3.client("stepfunctions")
    try:
        return call(client, event)
    except ClientError as refused:
        return {
            "ok": False,
            "code": refused.response["Error"]["Code"],
            "message": refused.response["Error"]["Message"],
            "status": refused.response["ResponseMetadata"]["HTTPStatusCode"],
        }
"#;

/// Step Functions, Lambda, IAM and STS over ONE engine: a handler's own SDK call is dispatched
/// back into this same router, so the decision taken on it reads the documents IAM holds and
/// the executions it changes are the ones this suite reads.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // The suite signs nothing: the `Authorization` header carries only the credential
        // scope, which is where a service reads the caller's access key id from.
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        router.register_service(
            service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        router.register_service(
            service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        router.register_service(
            service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(engine),
        );
        Self { router }
    }

    /// A SigV4 `Authorization` header whose credential scope names `service`, which is what
    /// routes the request. Its credential is the account's own published key, so every call
    /// this suite makes itself is the account's and not any row's role's.
    fn authorization(service: &str) -> String {
        format!(
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
             SignedHeaders=host, Signature=0123456789abcdef"
        )
    }

    /// One Step Functions call (`awsJson1_0`), as its status and parsed body.
    async fn sfn_raw(&self, operation: &str, body: &Value) -> (u16, Value) {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", format!("{SIGNING_NAME}.{REGION}.amazonaws.com"))
            .header("content-type", "application/x-amz-json-1.0")
            .header("x-amz-target", format!("{TARGET_PREFIX}.{operation}"))
            .header("authorization", Self::authorization(SIGNING_NAME))
            .body(Bytes::from(body.to_string()))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let body = response.into_body();
        let parsed = if body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&body).unwrap_or(Value::Null)
        };
        (status, parsed)
    }

    /// The same, where the call must be served.
    async fn sfn(&self, case: &str, operation: &str, body: &Value) -> Value {
        let (status, parsed) = self.sfn_raw(operation, body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: {operation} as the account must answer 200, got {status} with \
             {parsed}"
        );
        parsed
    }

    /// One `awsQuery` call to IAM, requiring a 200.
    async fn iam(&self, case: &str, action: &str, params: &[(&str, String)]) -> String {
        let mut body = format!("Action={action}&Version=2010-05-08");
        for (name, value) in params {
            body.push('&');
            body.push_str(name);
            body.push('=');
            body.push_str(&form_encode(value));
        }
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", "iam.amazonaws.com")
            .header("content-type", "application/x-www-form-urlencoded")
            .header("authorization", Self::authorization("iam"))
            .body(Bytes::from(body))
            .expect("a well-formed awsQuery request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let text = String::from_utf8_lossy(&response.into_body()).to_string();
        assert_eq!(
            status, 200,
            "case {case:?}: {action} must answer 200 from IAM, got {status} with {text}"
        );
        text
    }

    /// One `restJson1` call to Lambda, as its status, its headers and its parsed body.
    async fn lambda(&self, method: &str, uri: &str, body: &Value) -> (u16, http::HeaderMap, Value) {
        let payload = if body.is_null() {
            Vec::new()
        } else {
            body.to_string().into_bytes()
        };
        let request = http::Request::builder()
            .method(method)
            .uri(uri)
            .header("host", format!("lambda.{REGION}.amazonaws.com"))
            .header("content-type", "application/json")
            .header("authorization", Self::authorization("lambda"))
            .body(Bytes::from(payload))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let (parts, body) = response.into_parts();
        let parsed = if body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&body)
                .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&body).to_string()))
        };
        (parts.status.as_u16(), parts.headers, parsed)
    }

    /// Create one role trusted by `principal`, and answer the ARN IAM minted for it — read out
    /// of IAM's own answer rather than built here, because the role is that service's resource.
    async fn role(&self, case: &str, name: &str, principal: &str) -> String {
        let trust = json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Principal": { "Service": principal },
                "Action": "sts:AssumeRole",
            }],
        });
        let created = self
            .iam(
                case,
                "CreateRole",
                &[
                    ("RoleName", name.to_string()),
                    ("AssumeRolePolicyDocument", trust.to_string()),
                ],
            )
            .await;
        xml_element(&created, "Arn")
            .unwrap_or_else(|| panic!("case {case:?}: CreateRole answers the Arn, got {created}"))
    }

    /// Put `document` in force as the one inline policy of `role`.
    async fn put_role_policy(&self, case: &str, role: &str, document: &str) {
        self.iam(
            case,
            "PutRolePolicy",
            &[
                ("RoleName", role.to_string()),
                ("PolicyName", POLICY_NAME.to_string()),
                ("PolicyDocument", document.to_string()),
            ],
        )
        .await;
    }

    /// Create the probe function `name` running as `role_arn`, and wait until Lambda reports it
    /// `Active`, which is the state an invocation needs.
    async fn given_function(&self, case: &str, name: &str, role_arn: &str) {
        let (status, _, created) = self
            .lambda(
                "POST",
                FUNCTIONS_URI,
                &json!({
                    "FunctionName": name,
                    "Role": role_arn,
                    "Runtime": RUNTIME,
                    "Handler": HANDLER,
                    "Code": { "ZipFile": package(PROBE_HANDLER) },
                }),
            )
            .await;
        assert_eq!(
            status, 201,
            "case {case:?}: CreateFunction({name:?}) must answer 201, got {status} with {created}"
        );
        let began = Instant::now();
        loop {
            let (status, _, configuration) = self
                .lambda(
                    "GET",
                    &format!("{FUNCTIONS_URI}/{name}/configuration"),
                    &Value::Null,
                )
                .await;
            assert_eq!(
                status, 200,
                "case {case:?}: GetFunctionConfiguration({name:?}) must resolve it, got {status} \
                 with {configuration}"
            );
            if configuration.get("State").and_then(Value::as_str) == Some(FUNCTION_ACTIVE) {
                return;
            }
            assert!(
                began.elapsed() < PATIENCE,
                "case {case:?}: {name:?} must reach {FUNCTION_ACTIVE} within {PATIENCE:?}, last \
                 saw {configuration}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// Invoke `function` on `event` and answer the document its handler returned, requiring the
    /// run to have completed: every refusal here is one the handler catches, so a function
    /// error means it got no answer it could report.
    async fn invoke(&self, case: &str, function: &str, event: &Value) -> Value {
        let (status, headers, payload) = self
            .lambda(
                "POST",
                &format!("{FUNCTIONS_URI}/{function}/invocations"),
                event,
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

    /// Create a state machine of `name` whose one state holds, and answer its ARN.
    async fn holding_machine(&self, case: &str, name: &str, role_arn: &str) -> String {
        let created = self
            .sfn(
                case,
                "CreateStateMachine",
                &json!({
                    "name": name,
                    "definition": HOLDING_DEFINITION,
                    "roleArn": role_arn,
                }),
            )
            .await;
        let arn = string(&created, "stateMachineArn");
        assert_eq!(
            arn,
            machine_arn(name),
            "case {case:?}: expected the machine's own ARN form, got {arn:?}"
        );
        arn
    }

    /// Start one execution of `machine` under `run` as the ACCOUNT's own credential — which no
    /// row's grant decides — and answer its ARN.
    async fn planted_execution(&self, case: &str, machine: &str, run: &str) -> String {
        let started = self
            .sfn(
                case,
                "StartExecution",
                &json!({ "stateMachineArn": machine, "name": run }),
            )
            .await;
        string(&started, "executionArn")
    }

    /// The status this service reports for `execution`, or `None` where it holds no such
    /// execution at all — which is how a refused start is shown to have created nothing.
    async fn execution_status(&self, case: &str, execution: &str) -> Option<String> {
        let (status, answered) = self
            .sfn_raw("DescribeExecution", &json!({ "executionArn": execution }))
            .await;
        if status == 200 {
            return Some(string(&answered, "status"));
        }
        let code = error_code(&answered);
        assert_eq!(
            code, EXECUTION_DOES_NOT_EXIST,
            "case {case:?}: expected {execution} to be held or absent, got {status} with \
             {answered}"
        );
        None
    }

    /// Every execution ARN this service holds for `machine`, sorted — read as the account, so a
    /// handler's own listing has something independent to agree with.
    async fn held_executions(&self, case: &str, machine: &str) -> Vec<String> {
        let listed = self
            .sfn(
                case,
                "ListExecutions",
                &json!({ "stateMachineArn": machine }),
            )
            .await;
        let mut arns: Vec<String> = listed["executions"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|entry| entry["executionArn"].as_str().map(str::to_string))
            .collect();
        arns.sort();
        arns
    }
}

/// The ARN of a state machine of this account in this Region, which is what a statement names
/// and what a refusal of a machine-addressed call reports.
fn machine_arn(name: &str) -> String {
    format!("arn:aws:states:{REGION}:{ACCOUNT}:stateMachine:{name}")
}

/// The ARN of one named execution of that machine — the resource an execution-addressed call is
/// judged against.
fn execution_arn(machine: &str, run: &str) -> String {
    format!("arn:aws:states:{REGION}:{ACCOUNT}:execution:{machine}:{run}")
}

/// The session a function's own call is signed as: a session of the role it runs as, named
/// after the function.
fn session_arn(role: &str, function: &str) -> String {
    format!("arn:aws:sts::{ACCOUNT}:assumed-role/{role}/{function}")
}

/// The sentence this service refuses a call nothing in force allows with. Written out here
/// rather than built from the service's own helper, so the wording has something to be wrong
/// about: the principal is the session the function's call is signed as, the action is
/// `states:<OperationName>` for the operation it called, and the resource is the ARN the
/// request named.
fn refusal_message(session: &str, action: &str, resource: &str) -> String {
    format!(
        "User: {session} is not authorized to perform: {action} on resource: {resource} \
         because no identity-based policy allows the {action} action"
    )
}

/// What the handler reports for a call nothing in force allows.
fn refused(session: &str, action: &str, resource: &str) -> Value {
    json!({
        "ok": false,
        "code": ACCESS_DENIED,
        "message": refusal_message(session, action, resource),
        "status": ACCESS_DENIED_STATUS,
    })
}

/// The statement every execution role carries whatever else it is granted: the log actions a
/// function writes its own log stream with, which grant nothing of this service.
fn log_statement() -> Value {
    json!({
        "Effect": "Allow",
        "Action": ["logs:CreateLogGroup", "logs:CreateLogStream", "logs:PutLogEvents"],
        "Resource": "*",
    })
}

/// A document allowing `actions` on `resources`, beside that baseline.
fn allowing(actions: &[&str], resources: &[String]) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [
            log_statement(),
            { "Effect": "Allow", "Action": actions, "Resource": resources },
        ],
    })
    .to_string()
}

/// The least-privilege baseline on its own: the log actions, and nothing of this service.
fn logs_only() -> String {
    json!({ "Version": "2012-10-17", "Statement": [log_statement()] }).to_string()
}

/// WHICH operation of this service one probe's handler calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Start,
    List,
    Describe,
    Stop,
}

impl Op {
    /// The word the event names it by, which the handler reads.
    fn event(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::List => "list",
            Self::Describe => "describe",
            Self::Stop => "stop",
        }
    }

    /// The IAM action the operation performs: an action of this service is its operation's own
    /// name under this service's action namespace.
    fn action(self) -> &'static str {
        match self {
            Self::Start => "states:StartExecution",
            Self::List => "states:ListExecutions",
            Self::Describe => "states:DescribeExecution",
            Self::Stop => "states:StopExecution",
        }
    }

    /// Whether the request names its resource by an EXECUTION ARN (else by the state machine's)
    /// — which decides which ARN a statement has to cover and which one a refusal reports.
    fn addresses_an_execution(self) -> bool {
        matches!(self, Self::Describe | Self::Stop)
    }
}

/// WHICH resource a row's statement names.
#[derive(Debug, Clone, Copy)]
enum Scope {
    /// The state machine the handler's calls address.
    Machine,
    /// Another state machine of the same account.
    OtherMachine,
    /// Every execution of the machine the handler's calls address.
    ExecutionsOfMachine,
    /// Every resource, which the bare `*` wildcard stands for.
    Every,
}

/// One probe of a row's role: which operation the handler calls, and whether the documents in
/// force let it through.
struct Probe {
    case: &'static str,
    op: Op,
    allowed: bool,
}

/// One row: what the role's inline document allows, which resource that statement names, and
/// the probes taken against it.
struct Row {
    actions: &'static [&'static str],
    scope: Scope,
    probes: &'static [Probe],
}

const ROWS: &[Row] = &[
    // The case the rule is stated about: a least-privilege role carrying only the log actions a
    // function needs is granted no operation of this service at all.
    Row {
        actions: &[],
        scope: Scope::Every,
        probes: &[
            Probe {
                case: "a role granting only the log actions starts nothing",
                op: Op::Start,
                allowed: false,
            },
            Probe {
                case: "the same role lists no execution of the machine",
                op: Op::List,
                allowed: false,
            },
            Probe {
                case: "the same role reads no execution",
                op: Op::Describe,
                allowed: false,
            },
            Probe {
                case: "the same role stops no execution",
                op: Op::Stop,
                allowed: false,
            },
        ],
    },
    Row {
        actions: &["states:StartExecution", "states:ListExecutions"],
        scope: Scope::Machine,
        probes: &[
            Probe {
                case: "a role granted the start on the machine starts the execution",
                op: Op::Start,
                allowed: true,
            },
            Probe {
                case: "and lists the machine's executions by the same statement",
                op: Op::List,
                allowed: true,
            },
            Probe {
                case: "the same role is granted no read of one execution by it",
                op: Op::Describe,
                allowed: false,
            },
            Probe {
                case: "the same role is granted no stop by it",
                op: Op::Stop,
                allowed: false,
            },
        ],
    },
    // The two halves of the resource question: an execution-addressed call is judged against the
    // EXECUTION's ARN, so a statement naming the state machine does not cover it and one naming
    // that machine's executions does.
    Row {
        actions: &["states:DescribeExecution", "states:StopExecution"],
        scope: Scope::Machine,
        probes: &[
            Probe {
                case: "a grant naming the state machine does not cover reading its execution",
                op: Op::Describe,
                allowed: false,
            },
            Probe {
                case: "nor stopping it",
                op: Op::Stop,
                allowed: false,
            },
        ],
    },
    Row {
        actions: &["states:DescribeExecution", "states:StopExecution"],
        scope: Scope::ExecutionsOfMachine,
        probes: &[
            Probe {
                case: "a grant naming the machine's executions reads one",
                op: Op::Describe,
                allowed: true,
            },
            Probe {
                case: "and stops it by the same statement",
                op: Op::Stop,
                allowed: true,
            },
            Probe {
                case: "the same role is granted no start by it",
                op: Op::Start,
                allowed: false,
            },
        ],
    },
    Row {
        actions: &["states:StartExecution"],
        scope: Scope::OtherMachine,
        probes: &[Probe {
            case: "a role granted the start on ANOTHER machine starts nothing",
            op: Op::Start,
            allowed: false,
        }],
    },
    Row {
        actions: &["states:*"],
        scope: Scope::Every,
        probes: &[
            Probe {
                case: "a role granted every action of the service starts",
                op: Op::Start,
                allowed: true,
            },
            Probe {
                case: "lists",
                op: Op::List,
                allowed: true,
            },
            Probe {
                case: "reads",
                op: Op::Describe,
                allowed: true,
            },
            Probe {
                case: "and stops by the same statement",
                op: Op::Stop,
                allowed: true,
            },
        ],
    },
];

/// The machine every probe of the table addresses, and the second machine of the account one
/// row's statement names instead.
const TABLE_MACHINE: &str = "in-fn-authz-held";
const TABLE_OTHER_MACHINE: &str = "in-fn-authz-elsewhere";

/// The role one row runs as, and the function it runs in — named here because a refusal's own
/// sentence names both.
fn table_role(index: usize) -> String {
    format!("in-fn-authz-role-{index}")
}
fn table_function(index: usize) -> String {
    format!("in-fn-authz-probe-{index}")
}

/// The ARNs a row's statement names.
fn scoped(scope: Scope) -> Vec<String> {
    match scope {
        Scope::Machine => vec![machine_arn(TABLE_MACHINE)],
        Scope::OtherMachine => vec![machine_arn(TABLE_OTHER_MACHINE)],
        Scope::ExecutionsOfMachine => vec![execution_arn(TABLE_MACHINE, "*")],
        Scope::Every => vec!["*".to_string()],
    }
}

/// Every probe of the table: the operation of this service a running function may perform is
/// the one the documents in force on its execution role allow on the ARN the request names, and
/// what this service holds afterwards follows that decision.
#[tokio::test]
async fn an_in_function_call_is_decided_against_the_functions_execution_role() {
    let emulator = Emulator::new();
    let case = "the machines every probe addresses";
    let machine_role = emulator
        .role(case, "in-fn-authz-machines", SFN_SERVICE_PRINCIPAL)
        .await;
    let machine = emulator
        .holding_machine(case, TABLE_MACHINE, &machine_role)
        .await;
    let other = emulator
        .holding_machine(case, TABLE_OTHER_MACHINE, &machine_role)
        .await;
    assert_ne!(
        machine, other,
        "case {case:?}: the two machines must be two machines, got {machine} for both"
    );

    for (index, row) in ROWS.iter().enumerate() {
        let role = table_role(index);
        let function = table_function(index);
        let role_arn = emulator
            .role("setup", &role, LAMBDA_SERVICE_PRINCIPAL)
            .await;
        emulator
            .given_function("setup", &function, &role_arn)
            .await;
        let session = session_arn(&role, &function);

        let document = if row.actions.is_empty() {
            logs_only()
        } else {
            allowing(row.actions, &scoped(row.scope))
        };
        emulator.put_role_policy("setup", &role, &document).await;

        for (probe_index, probe) in row.probes.iter().enumerate() {
            let case = probe.case;
            let run = format!("run-{index}-{probe_index}");
            // What the probe addresses: a start names the run it is about, and an
            // execution-addressed call needs one already running to name.
            let execution = if probe.op.addresses_an_execution() {
                emulator.planted_execution(case, &machine, &run).await
            } else {
                execution_arn(TABLE_MACHINE, &run)
            };

            let event = json!({
                "op": probe.op.event(),
                "machine": machine,
                "execution": execution,
                "run": run,
                "token": "",
                "output": "{}",
            });
            let payload = emulator.invoke(case, &function, &event).await;

            if probe.allowed {
                let expected = match probe.op {
                    Op::Start => json!({ "ok": true, "execution": execution }),
                    Op::List => {
                        let held = emulator.held_executions(case, &machine).await;
                        json!({ "ok": true, "executions": held })
                    }
                    Op::Describe => json!({ "ok": true, "status": RUNNING }),
                    Op::Stop => json!({ "ok": true, "stopped": true }),
                };
                assert_eq!(
                    payload, expected,
                    "case {case:?}: expected the handler to report {expected}, got {payload}"
                );
            } else {
                let resource = if probe.op.addresses_an_execution() {
                    execution.clone()
                } else {
                    machine.clone()
                };
                let expected = refused(&session, probe.op.action(), &resource);
                assert_eq!(
                    payload, expected,
                    "case {case:?}: expected the handler to report {expected}, got {payload}"
                );
            }

            // And what THIS SERVICE holds for the execution the probe is about: a served start
            // created it and a refused one created nothing; a served stop aborted it and a
            // refused one left it exactly as it was. A listing addresses no one execution, so
            // what it must agree with was asserted above.
            let wanted = match (probe.op, probe.allowed) {
                (Op::List, _) => None,
                (Op::Start, false) => Some(None),
                (Op::Stop, true) => Some(Some(ABORTED.to_string())),
                _ => Some(Some(RUNNING.to_string())),
            };
            if let Some(wanted) = wanted {
                let held = emulator.execution_status(case, &execution).await;
                assert_eq!(
                    held, wanted,
                    "case {case:?}: expected {wanted:?} to be held for {execution}, got {held:?}"
                );
            }
            // Nothing any row did reached the OTHER machine: the statement that names it grants
            // the handler nothing on the machine it actually calls.
            let elsewhere = emulator.held_executions(case, &other).await;
            assert!(
                elsewhere.is_empty(),
                "case {case:?}: expected the other machine to hold no execution, got \
                 {elsewhere:?}"
            );
        }
    }
}

/// The role, the function and the machine the revocation drill below runs over.
const CYCLE_ROLE: &str = "in-fn-authz-cycle-role";
const CYCLE_FUNCTION: &str = "in-fn-authz-cycle-probe";
const CYCLE_MACHINE: &str = "in-fn-authz-cycle";

/// RULE, taken over the one thing the table holds fixed: the decision is taken per request
/// against the documents IN FORCE AT THAT INSTANT, so the identical in-function call is served
/// while the grant stands, refused once it is withdrawn and served again once it is put back —
/// and an execution exists for each served start and for no refused one. Nothing about the
/// function, its code or the machine changes between the three calls.
#[tokio::test]
async fn the_document_in_force_at_the_instant_of_the_call_decides_it() {
    let emulator = Emulator::new();
    let case = "setup";
    let machine_role = emulator
        .role(case, "in-fn-authz-cycle-machines", SFN_SERVICE_PRINCIPAL)
        .await;
    let machine = emulator
        .holding_machine(case, CYCLE_MACHINE, &machine_role)
        .await;
    let role_arn = emulator
        .role(case, CYCLE_ROLE, LAMBDA_SERVICE_PRINCIPAL)
        .await;
    emulator
        .given_function(case, CYCLE_FUNCTION, &role_arn)
        .await;
    let session = session_arn(CYCLE_ROLE, CYCLE_FUNCTION);
    let granted = allowing(&["states:StartExecution"], std::slice::from_ref(&machine));

    // (case, the document in force, whether the start is served)
    let phases: [(&str, String, bool); 3] = [
        ("granted", granted.clone(), true),
        ("withdrawn", logs_only(), false),
        ("restored", granted, true),
    ];

    let mut expected_held: Vec<String> = Vec::new();
    for (case, document, allowed) in phases {
        emulator.put_role_policy(case, CYCLE_ROLE, &document).await;
        let run = format!("cycle-{case}");
        let execution = execution_arn(CYCLE_MACHINE, &run);
        let payload = emulator
            .invoke(
                case,
                CYCLE_FUNCTION,
                &json!({
                    "op": Op::Start.event(),
                    "machine": machine,
                    "execution": execution,
                    "run": run,
                    "token": "",
                    "output": "{}",
                }),
            )
            .await;

        let expected = if allowed {
            json!({ "ok": true, "execution": execution })
        } else {
            refused(&session, Op::Start.action(), &machine)
        };
        assert_eq!(
            payload, expected,
            "case {case:?}: expected the handler to report {expected}, got {payload}"
        );

        if allowed {
            expected_held.push(execution.clone());
            expected_held.sort();
        }
        let held = emulator.held_executions(case, &machine).await;
        assert_eq!(
            held, expected_held,
            "case {case:?}: expected {expected_held:?} to be held, got {held:?}"
        );
    }
}

/// The role, the function and the machines the callback settlement below runs over.
const CALLBACK_ROLE: &str = "in-fn-authz-callback-role";
const CALLBACK_FUNCTION: &str = "in-fn-authz-callback-probe";
const CALLBACK_PARENT: &str = "in-fn-authz-callback-parent";
const CALLBACK_CHILD: &str = "in-fn-authz-callback-child";
const CALLBACK_MACHINE_ROLE: &str = "in-fn-authz-callback-machines";

/// The optimized integration the callback task below is handed over by: a nested execution of
/// this service, which carries the token as the child's own input.
const CALLBACK_RESOURCE: &str = "arn:aws:states:::states:startExecution.waitForTaskToken";

/// The child that nested execution runs: it reaches its own end at once, because what the
/// parent waits for is the TOKEN and not the child.
const CALLBACK_CHILD_DEFINITION: &str =
    r#"{"StartAt":"Done","States":{"Done":{"Type":"Pass","Result":{"ok":true},"End":true}}}"#;

/// The `output` the handler's settlement carries.
const SETTLEMENT_OUTPUT: &str = r#"{"approved":true}"#;

/// The token of the OUTSTANDING callback task of `execution`, read back out of the child the
/// task started — the work outside holding it, exactly as a handler holds the token its
/// invocation was given. Polled, because the hand-over is owed by the clock.
async fn outstanding_token(emulator: &Emulator, case: &str, execution: &str) -> String {
    let began = Instant::now();
    loop {
        let history = emulator
            .sfn(
                case,
                "GetExecutionHistory",
                &json!({ "executionArn": execution }),
            )
            .await;
        let submitted = history["events"]
            .as_array()
            .and_then(|events| {
                events
                    .iter()
                    .find(|event| event["type"].as_str() == Some(EVENT_TASK_SUBMITTED))
            })
            .cloned();
        if let Some(event) = submitted {
            let details = &event["taskSubmittedEventDetails"];
            let answered: Value = serde_json::from_str(&string(details, "output"))
                .unwrap_or_else(|why| {
                    panic!("case {case:?}: the submission's output is JSON: {why}")
                });
            let child = string(&answered, "ExecutionArn");
            let described = emulator
                .sfn(case, "DescribeExecution", &json!({ "executionArn": child }))
                .await;
            let input: Value = serde_json::from_str(&string(&described, "input"))
                .unwrap_or_else(|why| panic!("case {case:?}: the child's input is JSON: {why}"));
            return string(&input, "token");
        }
        assert!(
            began.elapsed() < PATIENCE,
            "case {case:?}: expected the callback task to be handed over within {PATIENCE:?}, \
             last saw {history}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// `execution` once it reports `wanted`, or the last status it reported before the deadline.
async fn awaited_status(
    emulator: &Emulator,
    case: &str,
    execution: &str,
    wanted: &str,
) -> Option<String> {
    let began = Instant::now();
    loop {
        let reported = emulator.execution_status(case, execution).await;
        if reported.as_deref() == Some(wanted) || began.elapsed() >= PATIENCE {
            return reported;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// RULE, taken over the operations that name NO ARN of their own: a `SendTask*` call is judged
/// against the resource the task token it carries belongs to — for a callback
/// (`.waitForTaskToken`) task, the state machine whose execution is waiting at it — so a
/// handler whose execution role is granted nothing of this service is refused HTTP 400
/// `AccessDeniedException` naming that machine, the token is not redeemed and the execution
/// stays `RUNNING`; the identical call made once the grant is in force settles the task, and
/// the execution carries on to the end its definition dictates.
#[tokio::test]
async fn an_in_function_callback_settlement_is_judged_against_the_machine_its_token_names() {
    let emulator = Emulator::new();
    let case = "setup";
    // The machine's own role is what this service acts as to make the integration call the
    // callback task asks for, so it is granted the actions that call performs.
    let machine_role = emulator
        .role(case, CALLBACK_MACHINE_ROLE, SFN_SERVICE_PRINCIPAL)
        .await;
    emulator
        .put_role_policy(
            case,
            CALLBACK_MACHINE_ROLE,
            &allowing(&["states:*"], &["*".to_string()]),
        )
        .await;

    let child = emulator
        .sfn(
            case,
            "CreateStateMachine",
            &json!({
                "name": CALLBACK_CHILD,
                "definition": CALLBACK_CHILD_DEFINITION,
                "roleArn": machine_role,
            }),
        )
        .await;
    let parent_definition = json!({
        "StartAt": "WaitForCallback",
        "States": { "WaitForCallback": {
            "Type": "Task",
            "Resource": CALLBACK_RESOURCE,
            "Parameters": {
                "stateMachineArn": string(&child, "stateMachineArn"),
                "input": { "token.$": "$$.Task.Token" },
            },
            "End": true,
        }},
    });
    let parent = emulator
        .sfn(
            case,
            "CreateStateMachine",
            &json!({
                "name": CALLBACK_PARENT,
                "definition": parent_definition.to_string(),
                "roleArn": machine_role,
            }),
        )
        .await;
    let parent_arn = string(&parent, "stateMachineArn");

    let role_arn = emulator
        .role(case, CALLBACK_ROLE, LAMBDA_SERVICE_PRINCIPAL)
        .await;
    emulator
        .given_function(case, CALLBACK_FUNCTION, &role_arn)
        .await;
    let session = session_arn(CALLBACK_ROLE, CALLBACK_FUNCTION);

    let execution = emulator
        .planted_execution(case, &parent_arn, "callback-run")
        .await;
    let case = "a settlement by a role granting nothing of this service";
    let token = outstanding_token(&emulator, case, &execution).await;
    assert!(
        !token.is_empty(),
        "case {case:?}: expected the callback task's own token, got an empty one"
    );

    let event = json!({
        "op": "send",
        "machine": parent_arn,
        "execution": execution,
        "run": "callback-run",
        "token": token,
        "output": SETTLEMENT_OUTPUT,
    });

    // Refused: the role carries the least-privilege baseline alone, so the token is not
    // redeemed and the execution is still waiting at its callback task afterwards.
    emulator
        .put_role_policy(case, CALLBACK_ROLE, &logs_only())
        .await;
    let payload = emulator.invoke(case, CALLBACK_FUNCTION, &event).await;
    let expected = refused(&session, "states:SendTaskSuccess", &parent_arn);
    assert_eq!(
        payload, expected,
        "case {case:?}: expected the handler to report {expected}, got {payload}"
    );
    let held = emulator.execution_status(case, &execution).await;
    assert_eq!(
        held.as_deref(),
        Some(RUNNING),
        "case {case:?}: expected the refused settlement to leave the execution {RUNNING}, got \
         {held:?}"
    );

    // Served: the identical call, with the grant in force, settles the task — and the execution
    // carries on to the end its definition dictates.
    let case = "the identical settlement once the grant names that machine";
    emulator
        .put_role_policy(
            case,
            CALLBACK_ROLE,
            &allowing(
                &["states:SendTaskSuccess"],
                std::slice::from_ref(&parent_arn),
            ),
        )
        .await;
    let payload = emulator.invoke(case, CALLBACK_FUNCTION, &event).await;
    assert_eq!(
        payload,
        json!({ "ok": true, "settled": true }),
        "case {case:?}: expected the settlement to be answered, got {payload}"
    );
    let settled = awaited_status(&emulator, case, &execution, SUCCEEDED).await;
    assert_eq!(
        settled.as_deref(),
        Some(SUCCEEDED),
        "case {case:?}: expected the settled task to carry the execution to {SUCCEEDED}, got \
         {settled:?}"
    );
}
