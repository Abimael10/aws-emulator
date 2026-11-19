// SPDX-License-Identifier: CC0-1.0

//! WHAT decides where a Step Functions execution whose states do REAL work ends up: the
//! definition and the work itself, never how much interpretation the run took.
//!
//! RULE: a STANDARD execution is DRIVEN to a terminal status decided by its definition and
//! by the work its states perform — `SUCCEEDED` once the chain's last state ends, `FAILED`
//! under the error name the failed work itself reported — however many states the definition
//! chains, however many calls they make and however long interpreting them takes; the only
//! span that may cut an execution short is the definition's own `TimeoutSeconds`, which ends
//! it `TIMED_OUT` and names no error and no cause of its own. ASL's `States.Runtime` belongs
//! to a genuine fault in the definition, so no run that reaches its end or whose work raised
//! carries it — while a definition that asks for more than one execution may do is stopped by
//! the QUOTA on that work and fails under it, with a cause naming what it asked too much of.
//!
//! Three crates have to agree for that to hold — Step Functions interprets the chain and
//! makes each invocation, IAM holds the documents the invocation is judged against, and
//! Lambda runs the handler whose answer (or whose raise) the task reports — so the suite
//! lives here rather than in any one of them. The crate-local
//! `definition_decides_the_outcome` suite pins the same rule for a definition the interpreter
//! runs on its own; what only this suite can reach is a chain whose every state's outcome is
//! computed by caller code running in another service.
//!
//! The rows differ along what the rule generalises over: how many `Task` states the chain
//! carries (one, two, four), how much work each handler does for its state (a short
//! computation, one thousands of times longer — the dimension a ceiling on interpretation
//! would decide and the definition does not), and how the run ENDS — the chain's own end, a
//! handler that raises on a key the input omits (the tenant-less probe a role barrier is built
//! from), the definition's own `TimeoutSeconds`, and a definition whose loop its own data
//! never leaves, which the quota on the work one execution may do stops: the bound that
//! replaced the clock has to FAIL CLOSED, so a definition that does not terminate reaches a
//! terminal status rather than holding the run open.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::stack_status;
use crate::query_wire::{form_encode, opt_element};

/// The Region every request here is signed for, and so the Region the function and the state
/// machines belong to.
const REGION: &str = "us-east-1";

/// The runtime the handler is written for, and the module the archived inline source lands
/// in, which the handler name resolves against.
const RUNTIME: &str = "python3.12";
/// See [`RUNTIME`].
const HANDLER: &str = "index.handler";

/// The stack the function is declared in, and the `StackStatus` it must reach.
const FUNCTION_STACK: &str = "xsvc-driven-chain-stack";
/// See [`FUNCTION_STACK`].
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The function every `Task` state of every row invokes.
const FUNCTION: &str = "xsvc-driven-chain-step";

/// How long one invocation is given, as the function's own `Timeout`: far past what any row's
/// handler needs on any host, so nothing but the handler decides what an invocation answers.
const FUNCTION_TIMEOUT: u64 = 300;

/// The modulus the handler reduces its running total by, and the one the expectation is
/// recomputed with: written once so the two cannot drift.
const MODULUS: u64 = 1_000_003;

/// The rounds of arithmetic the HEAVY row asks each of its handlers for, and the few the
/// other rows ask for: the work a state's own code does is what the rows vary, and neither
/// figure may decide where a run ends. Both are recomputed by [`total_of`], so a row's answer
/// reports work that really ran.
const HEAVY_ROUNDS: u64 = 20_000;
/// See [`HEAVY_ROUNDS`].
const LIGHT_ROUNDS: u64 = 64;

/// The seconds the one row that declares a definition-level `TimeoutSeconds` gives its whole
/// execution — "the maximum number of seconds an execution of the state machine can run" —
/// and the seconds the `Wait` placed before its chain holds it for, which is far past that.
const DEFINITION_TIMEOUT_SECONDS: u64 = 1;
/// See [`DEFINITION_TIMEOUT_SECONDS`].
const HOLD_SECONDS: u64 = 3_600;

/// The tenant the rows that name one are started for: the segment the handler's own trail
/// entries are built from.
const TENANT: &str = "basalt-tenant";

/// The modelled `ExecutionStatus` values a row ends in, and the one it sits in while its
/// tasks are still running (`ExecutionStatus`: RUNNING | SUCCEEDED | FAILED | TIMED_OUT |
/// ABORTED | PENDING_REDRIVE).
const RUNNING: &str = "RUNNING";
/// See [`RUNNING`].
const SUCCEEDED: &str = "SUCCEEDED";
/// See [`RUNNING`].
const FAILED: &str = "FAILED";
/// See [`RUNNING`].
const TIMED_OUT: &str = "TIMED_OUT";

/// The class Python raises for a subscript the mapping does not carry, which is the
/// `errorType` of the document that raise answers and so the name the failed task and the
/// failed execution carry.
const RAISED_CLASS: &str = "KeyError";

/// ASL's own name for a runtime fault in the DEFINITION, which a definition that asks for
/// more work than one execution may do is reported under. The rows that declare no such fault
/// may not end under it: a run abandoned by an interpreter would land exactly here.
const RUNTIME_ERROR: &str = "States.Runtime";

/// What the cause of a run the quota stopped has to name: the history it filled, which is
/// counted in the events one execution's history holds and in no span of time.
const QUOTA_WORDING: &str = "maximum number of history events";

/// The turn count the spinning row's `Choice` waits for, which its loop reaches no sooner than
/// the quota on one execution's history stops it: the definition declares a terminal state
/// its own data never gets to.
const TURNS_NEVER_REACHED: u64 = 1_000_000;

/// Wording no `cause` may carry: an interpretation ceiling of this emulator's own is about
/// the emulator rather than about the definition or its work.
const CLOCK_WORDING: [&str; 3] = ["interpreted for longer", "seconds this emulator", "budget"];

/// The `State` a function reports once it can be invoked (`FunctionConfiguration$State`:
/// "Active" once "the function is ready to be invoked").
const FUNCTION_ACTIVE: &str = "Active";

/// How often to look while waiting for work the clock owes — a created function reaching
/// [`FUNCTION_ACTIVE`], a stack reaching [`CREATE_COMPLETE`], an execution leaving
/// [`RUNNING`] — and how many looks to take. The spans are those services' own, so no fixed
/// sleep names the instant one ends, and the deadline is generous so a loaded host cannot
/// decide any row's outcome.
const LOOK: Duration = Duration::from_millis(100);
/// See [`LOOK`].
const LOOKS: usize = 1_200;

/// The handler every `Task` state of every row runs: it reads the key its caller is expected
/// to have named, does the work the state handed it, and answers the document the chain
/// carries on — so each state's result is COMPUTED by caller code and the trail records that
/// the state ran at all.
///
/// Reading the key first is what the tenant-less probe is: a run started without it ends on
/// the handler's own raise rather than on anything the interpreter decided.
fn handler_source() -> String {
    format!(
        r#"
def handler(event, context):
    tenant = event["tenant"]
    rounds = int(event.get("rounds", 0))
    total = 0
    for n in range(rounds):
        total = (total + n * n) % {MODULUS}
    trail = list(event.get("trail", []))
    trail.append("%s:%d" % (tenant, len(trail) + 1))
    answered = dict(event)
    answered["trail"] = trail
    answered["total"] = total
    return answered
"#
    )
}

/// What the handler's loop computes for `rounds`, recomputed here: a row whose answer carries
/// this had its work really performed rather than a result synthesized for it.
fn total_of(rounds: u64) -> u64 {
    let mut total = 0;
    for n in 0..rounds {
        total = (total + n * n) % MODULUS;
    }
    total
}

/// The trail a chain of `tasks` states leaves for [`TENANT`]: one entry per state that ran,
/// in the order they ran.
fn trail_of(tasks: usize) -> Vec<String> {
    (1..=tasks).map(|step| format!("{TENANT}:{step}")).collect()
}

/// One row: the chain it runs, the work its handlers do, whether its input names the key the
/// handler reads, and what the definition and that work decide the execution ends as.
struct Row {
    case: &'static str,
    /// How many `Task` states the definition chains.
    tasks: usize,
    /// The rounds of arithmetic each handler does for its state.
    rounds: u64,
    /// Whether the input carries the key the handler reads first — the tenant-less probe is
    /// the row where it does not.
    names_its_tenant: bool,
    /// The definition's own `TimeoutSeconds`, for the one row that declares one, whose chain
    /// is held behind a `Wait` far longer than it.
    timeout_seconds: Option<u64>,
    /// Whether the chain sits behind a loop whose exit the definition's own data never
    /// reaches, which is what the work quota has to stop.
    spins: bool,
    status: &'static str,
    error: Option<&'static str>,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "one task, whose handler answers",
            tasks: 1,
            rounds: LIGHT_ROUNDS,
            names_its_tenant: true,
            timeout_seconds: None,
            spins: false,
            status: SUCCEEDED,
            error: None,
        },
        Row {
            case: "two tasks chained, each handing its answer to the next",
            tasks: 2,
            rounds: LIGHT_ROUNDS,
            names_its_tenant: true,
            timeout_seconds: None,
            spins: false,
            status: SUCCEEDED,
            error: None,
        },
        Row {
            case: "four tasks chained, the shape a provisioning workflow has",
            tasks: 4,
            rounds: LIGHT_ROUNDS,
            names_its_tenant: true,
            timeout_seconds: None,
            spins: false,
            status: SUCCEEDED,
            error: None,
        },
        Row {
            // The row whose states do orders of magnitude more work than the others': four
            // invocations, each computing over tens of thousands of rounds, and the outcome
            // is still the one the definition decides.
            case: "four tasks whose handlers each compute over tens of thousands of rounds",
            tasks: 4,
            rounds: HEAVY_ROUNDS,
            names_its_tenant: true,
            timeout_seconds: None,
            spins: false,
            status: SUCCEEDED,
            error: None,
        },
        Row {
            // The tenant-less probe: the run ends on the HANDLER's own error class, which is
            // what tells a client the invocation happened at all.
            case: "four tasks whose first handler raises on a key the input omits",
            tasks: 4,
            rounds: LIGHT_ROUNDS,
            names_its_tenant: false,
            timeout_seconds: None,
            spins: false,
            status: FAILED,
            error: Some(RAISED_CLASS),
        },
        Row {
            // The ONE span that may cut a run short, and the status it reports: the
            // execution's own allowance ran out rather than anything a state did, so there is
            // no failure to name. The status-alone report is what this service's driver
            // records for a machine-level timeout, which the workflow crate's own
            // `definition_decides_the_outcome` suite pins for a definition run without tasks;
            // a STATE's `TimeoutSeconds` is the bound that fails its state under an error
            // name instead.
            case: "four tasks held behind a wait past the definition's own TimeoutSeconds",
            tasks: 4,
            rounds: LIGHT_ROUNDS,
            names_its_tenant: true,
            timeout_seconds: Some(DEFINITION_TIMEOUT_SECONDS),
            spins: false,
            status: TIMED_OUT,
            error: None,
        },
        Row {
            // The bound that replaced the clock, and the row that shows it FAILS CLOSED: a
            // definition whose loop its own data never leaves reaches a terminal status —
            // under ASL's runtime error, with a cause naming the work it asked too much of —
            // rather than holding the execution open or being abandoned on a span of time.
            case: "a loop whose exit its own data never reaches, before the chain is entered",
            tasks: 4,
            rounds: LIGHT_ROUNDS,
            names_its_tenant: true,
            timeout_seconds: None,
            spins: true,
            status: FAILED,
            error: Some(RUNTIME_ERROR),
        },
    ]
}

/// The emulator as it is ASSEMBLED, on one engine: the task's invocation is dispatched back
/// into this same router, so the function it reaches is the one this suite deployed.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate; the
    // credential scope is what routes a request.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what
/// routes the request.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One Lambda `restJson1` request, as its status and its parsed body.
async fn lambda(router: &MultiServiceRouter, method: &str, uri: &str) -> (u16, Value) {
    let host = format!("lambda.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{uri}"))
        .header("host", host)
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"))
        .body(Bytes::new())
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

/// One call to an AWS JSON service, as its status and parsed body.
async fn rpc(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host)
        .header("content-type", "application/x-amz-json-1.0")
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
    (status, parsed)
}

/// One Step Functions call, requiring a 200.
async fn sfn(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = rpc(router, "states", "AWSStepFunctions", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {operation} must answer 200, got {status} with {parsed}"
    );
    parsed
}

/// One awsQuery call to IAM, requiring a 200, answered with its XML body.
async fn iam(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let mut body = format!("Action={action}&Version=2010-05-08");
    for (name, value) in members {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("https://iam.amazonaws.com/")
        .header("host", "iam.amazonaws.com")
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("iam"))
        .body(Bytes::from(body))
        .expect("a well-formed awsQuery request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: {action} must answer 200 from IAM, got {status} with {text}"
    );
    text
}

/// One awsQuery call to CloudFormation, answered with its status and its XML body.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (name, value) in members {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let host = format!("cloudformation.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation"))
        .body(Bytes::from(body))
        .expect("a well-formed awsQuery request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// A role `service` may assume, carrying `statements` as its inline policy; answers its ARN.
///
/// A state machine's execution role is trusted by Step Functions and a function's own
/// execution role by Lambda — which `CreateFunction` checks — so the trusted service is the
/// caller's.
async fn given_role(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    service: &str,
    statements: Value,
) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": service },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let created = iam(
        router,
        case,
        "CreateRole",
        &[("RoleName", name), ("AssumeRolePolicyDocument", &trust)],
    )
    .await;
    let arn = opt_element(&created, "Arn")
        .unwrap_or_else(|| panic!("case {case:?}: CreateRole({name:?}) must answer an ARN"));
    let document = json!({ "Version": "2012-10-17", "Statement": statements }).to_string();
    iam(
        router,
        case,
        "PutRolePolicy",
        &[
            ("RoleName", name),
            ("PolicyName", "inline"),
            ("PolicyDocument", &document),
        ],
    )
    .await;
    arn
}

/// Deploy the function every row's tasks invoke, wait until Lambda reports it
/// [`FUNCTION_ACTIVE`], and answer its ARN.
///
/// The handler is declared as a stack's inline source, which CloudFormation archives into the
/// deployment package — the way a function whose handler is written out in a suite comes to
/// exist at all.
async fn given_function(router: &MultiServiceRouter, case: &str, role_arn: &str) -> String {
    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Step": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": FUNCTION,
                    "Runtime": RUNTIME,
                    "Handler": HANDLER,
                    "Role": role_arn,
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": { "ZipFile": handler_source() },
                },
            },
        },
    })
    .to_string();
    let (status, answered) = cloudformation(
        router,
        "CreateStack",
        &[("StackName", FUNCTION_STACK), ("TemplateBody", &template)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateStack({FUNCTION_STACK:?}) must be accepted, got {status} with \
         {answered}"
    );
    // The stack's completion is owed from the instant Lambda reports the function created,
    // which no fixed sleep names: this looks until it reports it.
    let mut reported = stack_status(router, FUNCTION_STACK).await;
    for _ in 0..LOOKS {
        if reported.0 == CREATE_COMPLETE {
            break;
        }
        tokio::time::sleep(LOOK).await;
        reported = stack_status(router, FUNCTION_STACK).await;
    }
    assert_eq!(
        reported.0, CREATE_COMPLETE,
        "case {case:?}: the function's stack must reach {CREATE_COMPLETE}, last saw {}",
        reported.1
    );

    let mut configuration = Value::Null;
    for _ in 0..LOOKS {
        let (_, answered) = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{FUNCTION}/configuration"),
        )
        .await;
        configuration = answered;
        if configuration["State"].as_str() == Some(FUNCTION_ACTIVE) {
            let arn = configuration["FunctionArn"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: the function must report its ARN, got {configuration}")
                })
                .to_string();
            return arn;
        }
        tokio::time::sleep(LOOK).await;
    }
    panic!("case {case:?}: the function must reach {FUNCTION_ACTIVE}, last saw {configuration}");
}

/// The definition one row runs: `tasks` `Task` states chained, each invoking `function_arn`
/// with the document the chain has built so far, and — for the rows that declare them — a
/// `Wait` the chain sits behind with the `TimeoutSeconds` that bounds the whole execution, or
/// a loop in front of the chain whose exit the definition's own data never reaches.
fn definition(
    function_arn: &str,
    tasks: usize,
    timeout_seconds: Option<u64>,
    spins: bool,
) -> String {
    let name_of = |step: usize| format!("Step{step}");
    let mut states = serde_json::Map::new();
    for step in 1..=tasks {
        let mut state = json!({ "Type": "Task", "Resource": function_arn });
        if step == tasks {
            state["End"] = json!(true);
        } else {
            state["Next"] = json!(name_of(step + 1));
        }
        states.insert(name_of(step), state);
    }
    let mut start_at = name_of(1);
    if timeout_seconds.is_some() {
        states.insert(
            "Hold".to_string(),
            json!({ "Type": "Wait", "Seconds": HOLD_SECONDS, "Next": name_of(1) }),
        );
        start_at = "Hold".to_string();
    }
    if spins {
        // A turn counted with an intrinsic, and a `Choice` that goes round again until a turn
        // count no run of this loop reaches: the chain beyond it is declared and never
        // entered.
        states.insert(
            "Spin".to_string(),
            json!({
                "Type": "Pass",
                "Parameters": {
                    "tenant.$": "$.tenant",
                    "rounds.$": "$.rounds",
                    "n.$": "States.MathAdd($.n, 1)",
                },
                "Next": "Turn",
            }),
        );
        states.insert(
            "Turn".to_string(),
            json!({
                "Type": "Choice",
                "Choices": [
                    { "Variable": "$.n", "NumericLessThan": TURNS_NEVER_REACHED, "Next": "Spin" },
                ],
                "Default": name_of(1),
            }),
        );
        start_at = "Spin".to_string();
    }
    let mut written = json!({ "StartAt": start_at, "States": Value::Object(states) });
    if let Some(seconds) = timeout_seconds {
        written["TimeoutSeconds"] = json!(seconds);
    }
    written.to_string()
}

/// Start one execution with `input` and report it once it has left [`RUNNING`].
async fn ran(
    router: &MultiServiceRouter,
    case: &str,
    machine_arn: &str,
    name: &str,
    input: &Value,
) -> Value {
    let started = sfn(
        router,
        case,
        "StartExecution",
        &json!({
            "stateMachineArn": machine_arn,
            "name": name,
            "input": input.to_string(),
        }),
    )
    .await;
    let execution_arn = started["executionArn"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: StartExecution must answer an ARN, got {started}")
        })
        .to_string();
    let mut described = Value::Null;
    for _ in 0..LOOKS {
        described = sfn(
            router,
            case,
            "DescribeExecution",
            &json!({ "executionArn": execution_arn }),
        )
        .await;
        if described["status"].as_str() != Some(RUNNING) {
            return described;
        }
        tokio::time::sleep(LOOK).await;
    }
    panic!("case {case:?}: the execution must leave {RUNNING}, last saw {described}");
}

/// Every row ends in the status its definition and its handlers decide, whatever the run
/// asked of the interpreter on the way.
#[tokio::test]
async fn a_task_chain_is_driven_to_the_terminal_state_its_definition_decides() {
    let setup = "a chain of Lambda tasks";
    let router = build_router();

    // The function runs as a role Lambda trusts; the state machines run as a role allowed the
    // invocation they make.
    let function_role = given_role(
        &router,
        setup,
        "xsvc-driven-chain-handler-role",
        "lambda.amazonaws.com",
        json!([{
            "Effect": "Allow",
            "Action": ["logs:CreateLogGroup", "logs:CreateLogStream", "logs:PutLogEvents"],
            "Resource": "*",
        }]),
    )
    .await;
    let machine_role = given_role(
        &router,
        setup,
        "xsvc-driven-chain-machine-role",
        "states.amazonaws.com",
        json!([{ "Effect": "Allow", "Action": "lambda:InvokeFunction", "Resource": "*" }]),
    )
    .await;
    let function_arn = given_function(&router, setup, &function_role).await;

    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let created = sfn(
            &router,
            case,
            "CreateStateMachine",
            &json!({
                "name": format!("xsvc-driven-chain-{index}"),
                "definition": definition(
                    &function_arn,
                    row.tasks,
                    row.timeout_seconds,
                    row.spins,
                ),
                "roleArn": machine_role,
            }),
        )
        .await;
        let machine_arn = created["stateMachineArn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: CreateStateMachine must answer an ARN"))
            .to_string();

        let mut input = json!({ "rounds": row.rounds, "n": 0 });
        if row.names_its_tenant {
            input["tenant"] = json!(TENANT);
        }
        let described = ran(&router, case, &machine_arn, "only", &input).await;

        assert_eq!(
            (
                described["status"].as_str(),
                described["error"].as_str(),
            ),
            (Some(row.status), row.error),
            "case {case:?}: expected the outcome its definition and its handlers decide, \
             {} {:?}, got {described}",
            row.status,
            row.error
        );

        let cause = described["cause"].as_str().unwrap_or_default().to_string();
        if row.error != Some(RUNTIME_ERROR) {
            assert_ne!(
                described["error"].as_str(),
                Some(RUNTIME_ERROR),
                "case {case:?}: this row declares no definition fault, so it may not end under \
                 {RUNTIME_ERROR}, got {described}"
            );
        }
        for wording in CLOCK_WORDING {
            assert!(
                !cause.contains(wording),
                "case {case:?}: expected no cause about this emulator's own interpretation, got \
                 {cause:?} carrying {wording:?}"
            );
        }

        if row.error == Some(RUNTIME_ERROR) {
            // The quota on the history one execution keeps stopped a definition that does not
            // terminate: the run is TERMINAL, and the cause names the history it filled rather
            // than any span of time.
            assert!(
                cause.contains(QUOTA_WORDING),
                "case {case:?}: expected the cause to name the {QUOTA_WORDING} one execution \
                 may make, got {cause:?}"
            );
            assert!(
                described["output"].as_str().is_none(),
                "case {case:?}: an execution that did not finish has no output, got {described}"
            );
        } else if let Some(class) = row.error {
            // The handler raised: the cause is the error document that raise answered, and it
            // names the class and the key the handler could not read.
            let document: Value = serde_json::from_str(&cause).unwrap_or_else(|error| {
                panic!(
                    "case {case:?}: expected the run's error document, got {error} for {cause:?}"
                )
            });
            assert_eq!(
                document["errorType"].as_str(),
                Some(class),
                "case {case:?}: expected the document to name the raised class, got {document}"
            );
            assert!(
                document["errorMessage"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("tenant"),
                "case {case:?}: expected the message the handler raised to name the key it \
                 read, got {document}"
            );
            assert!(
                described["output"].as_str().is_none(),
                "case {case:?}: a failed execution carries no output, got {described}"
            );
        } else if row.status == SUCCEEDED {
            // The run reached the chain's end: every state ran, in order, and each handler's
            // own computation is in the document the execution answered.
            let output: Value = serde_json::from_str(described["output"].as_str().unwrap_or("null"))
                .unwrap_or(Value::Null);
            let trail: Vec<String> = output["trail"]
                .as_array()
                .map(|entries| {
                    entries
                        .iter()
                        .map(|entry| entry.as_str().unwrap_or_default().to_string())
                        .collect()
                })
                .unwrap_or_default();
            assert_eq!(
                trail,
                trail_of(row.tasks),
                "case {case:?}: expected one trail entry per state that ran, got {output}"
            );
            assert_eq!(
                output["total"].as_u64(),
                Some(total_of(row.rounds)),
                "case {case:?}: expected the handler's own computation over {} rounds, got \
                 {output}",
                row.rounds
            );
        } else {
            // The definition's own allowance ran out: the status is the whole report, and
            // nothing the chain would have computed is claimed.
            assert!(
                cause.is_empty(),
                "case {case:?}: an execution-level timeout names no cause, got {cause:?}"
            );
            assert!(
                described["output"].as_str().is_none(),
                "case {case:?}: an execution that did not finish has no output, got {described}"
            );
        }
    }
}
