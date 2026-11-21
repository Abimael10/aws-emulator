// SPDX-License-Identifier: CC0-1.0

//! The SUBSEGMENT TREE a recorded execution's X-Ray segment carries, read back where a
//! client reads it: Step Functions, AWS Lambda and AWS X-Ray on ONE engine, with
//! `BatchGetTraces` asked for the chain the start's trace header named.
//!
//! The RULE under test: **a recorded run's segment carries one subsegment per state the
//! run entered, nested the way the definition nests those states — a `Parallel` state one
//! subsegment per branch of its `Branches` list and a `Map` state one per iteration, with
//! the states of that unit inside it — and one subsegment per attempt a state's work made
//! at the service it called; each entity carrying the mark its own outcome implies, `fault`
//! for work that failed and `error` with a `Canceled` cause for work another unit's failure
//! stopped.**
//!
//! The rows differ along what the rule generalises over: the kind of state that nests
//! others (a `Parallel`, an inline `Map`), the number of units it fans out into, the
//! outcome each unit reached (every unit succeeded, one failed and stopped its siblings),
//! the number of attempts a state's work made (none, one, two), which FAMILY the work is
//! recorded under (the optimized `lambda:invoke` integration, and a `Resource` that is the
//! function's own ARN), and the terminal status the run reached (succeeded, failed, stopped
//! by the client).
//!
//! Nothing here waits a fixed span for a publication: a row waits for the CONDITION — the
//! run's own segment being retrievable — and the deadline is only a backstop against a
//! publication that never happens.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_sfn_awsstepfunctions::EmulatedService;

use crate::lambda_task_control_flow::{package, parsed};
use crate::state_machine_lifecycle::{string, REGION, ROLE_ARN};

/// The `origin` the segment of a Step Functions execution carries, spelled out here rather
/// than imported from the implementation, so a wrong constant in the service cannot make a
/// test pass.
const STATE_MACHINE_ORIGIN: &str = "AWS::StepFunctions::StateMachine";

/// The name a subsegment covering one call to AWS Lambda carries: the name the AWS SDKs
/// give that service, which is how X-Ray names a call out to it.
const LAMBDA_CALL: &str = "Lambda";

/// The operation such a call made, as the `aws` block of a call out reports it.
const INVOKE_OPERATION: &str = "Invoke";

/// What the `cause` of an entity covering work another unit's failure stopped states.
const CANCELED_CAUSE: &str = "Canceled";

/// Where `CreateFunction` is bound (`POST /2015-03-31/functions`), and the `State` a
/// function reports once it can be invoked.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";
const FUNCTION_ACTIVE: &str = "Active";

/// The runtime and handler of the function the invoking rows call.
const RUNTIME: &str = "python3.12";
const HANDLER: &str = "index.handler";

/// The execution role that function runs as. No IAM is mounted here, so nothing is known
/// about it and Lambda serves the create.
const LAMBDA_ROLE_ARN: &str = "arn:aws:iam::000000000000:role/LambdaExecutionRole";

/// The handler every invoking row calls: it RAISES when the event asks it to and answers
/// otherwise, so one package serves the rows whose attempts succeed and the rows whose
/// attempts fail.
const HANDLER_SOURCE: &str = r#"
def handler(event, context):
    if event.get("boom"):
        raise RuntimeError("the handler was asked to raise")
    return {"ok": True}
"#;

/// How long a row waits for a condition it expects to hold, and how often it looks.
/// Generous: the deadline is a backstop against something that never happens, not a
/// measurement.
const DEADLINE: Duration = Duration::from_secs(90);
const LOOK: Duration = Duration::from_millis(50);

/// The statuses an execution can still leave.
const RUNNING: &str = "RUNNING";

/// One emulator: Step Functions, AWS Lambda and AWS X-Ray mounted on ONE engine, the shape
/// `core/router/src/lib.rs` mounts them in — so a task's invocation is served by the very
/// function a row created and the run's segments land in the store `BatchGetTraces`
/// answers from.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        router.register_service(service_com_amazonaws_xray_awsxray::EmulatedService::new(
            Arc::clone(&engine),
        ));
        router.register_service(
            service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(engine),
        );
        Self { router }
    }

    /// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what
    /// routes the request. The signature itself is never checked here.
    fn authorization(signing_name: &str) -> String {
        format!(
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/\
             aws4_request, SignedHeaders=host;x-amz-target, Signature=0123456789abcdef"
        )
    }

    /// One Step Functions call over its `awsJson1_0` wire, requiring a 200.
    async fn sfn(&self, operation: &str, body: &Value) -> Value {
        let host = format!("states.{REGION}.amazonaws.com");
        let request = http::Request::builder()
            .method("POST")
            .uri(format!("https://{host}/"))
            .header("host", &host)
            .header("content-type", "application/x-amz-json-1.0")
            .header("x-amz-target", format!("AWSStepFunctions.{operation}"))
            .header("authorization", Self::authorization("states"))
            .body(Bytes::from(body.to_string()))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let answered = parsed(response.into_body());
        assert_eq!(
            status, 200,
            "{operation}: expected 200, got {status} with body {answered}"
        );
        answered
    }

    /// One X-Ray call over its `restJson1` wire, requiring a 200.
    async fn xray(&self, path: &str, body: &Value) -> Value {
        let host = format!("xray.{REGION}.amazonaws.com");
        let request = http::Request::builder()
            .method("POST")
            .uri(format!("https://{host}{path}"))
            .header("host", &host)
            .header("content-type", "application/json")
            .header("authorization", Self::authorization("xray"))
            .body(Bytes::from(body.to_string()))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let answered = parsed(response.into_body());
        assert_eq!(
            status, 200,
            "POST {path}: expected 200, got {status} with body {answered}"
        );
        answered
    }

    /// One `restJson1` call to Lambda, as its status and parsed body.
    async fn lambda(&self, method: &str, uri: &str, body: &Value) -> (u16, Value) {
        let payload = match body.is_null() {
            true => Vec::new(),
            false => body.to_string().into_bytes(),
        };
        let host = format!("lambda.{REGION}.amazonaws.com");
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://{host}{uri}"))
            .header("host", &host)
            .header("content-type", "application/json")
            .header("authorization", Self::authorization("lambda"))
            .body(Bytes::from(payload))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        (status, parsed(response.into_body()))
    }

    /// The function every invoking row calls, waited out to [`FUNCTION_ACTIVE`] so an
    /// invocation of it is one Lambda admits; answers its ARN.
    async fn given_function(&self, name: &str) -> String {
        let (status, created) = self
            .lambda(
                "POST",
                FUNCTIONS_URI,
                &json!({
                    "FunctionName": name,
                    "Role": LAMBDA_ROLE_ARN,
                    "Runtime": RUNTIME,
                    "Handler": HANDLER,
                    "Code": { "ZipFile": package(HANDLER_SOURCE) },
                }),
            )
            .await;
        assert_eq!(
            status, 201,
            "CreateFunction must answer 201, got {status} with {created}"
        );
        let arn = string(&created, "FunctionArn");
        let deadline = Instant::now() + DEADLINE;
        loop {
            let (status, configuration) = self
                .lambda(
                    "GET",
                    &format!("{FUNCTIONS_URI}/{name}/configuration"),
                    &Value::Null,
                )
                .await;
            assert_eq!(
                status, 200,
                "GetFunctionConfiguration must resolve the created function, got {status} with \
                 {configuration}"
            );
            if configuration.get("State").and_then(Value::as_str) == Some(FUNCTION_ACTIVE) {
                return arn;
            }
            assert!(
                Instant::now() < deadline,
                "the function must reach {FUNCTION_ACTIVE}, last saw {configuration}"
            );
            ::tokio::time::sleep(LOOK).await;
        }
    }

    /// One TRACING state machine running `definition`; answers its ARN.
    async fn given_state_machine(&self, name: &str, definition: &Value) -> String {
        let created = self
            .sfn(
                "CreateStateMachine",
                &json!({
                    "name": name,
                    "definition": definition.to_string(),
                    "roleArn": ROLE_ARN,
                    "tracingConfiguration": { "enabled": true },
                }),
            )
            .await;
        string(&created, "stateMachineArn")
    }

    /// Start one execution of `machine` on `input`, under the chain `root`; answers its
    /// ARN.
    async fn started(&self, machine: &str, name: &str, input: &Value, root: &str) -> String {
        let started = self
            .sfn(
                "StartExecution",
                &json!({
                    "stateMachineArn": machine,
                    "name": name,
                    "input": input.to_string(),
                    "traceHeader": format!("Root={root};Sampled=1"),
                }),
            )
            .await;
        string(&started, "executionArn")
    }

    /// What `DescribeExecution` reports for `execution_arn`.
    async fn described(&self, execution_arn: &str) -> Value {
        self.sfn("DescribeExecution", &json!({ "executionArn": execution_arn }))
            .await
    }

    /// Wait until `execution_arn` has left `RUNNING`, and answer the status it reached.
    async fn settled(&self, case: &str, execution_arn: &str) -> String {
        let deadline = Instant::now() + DEADLINE;
        loop {
            let described = self.described(execution_arn).await;
            let status = string(&described, "status");
            if status != RUNNING {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: the run {execution_arn:?} never left {RUNNING}"
            );
            ::tokio::time::sleep(LOOK).await;
        }
    }

    /// Wait until X-Ray holds the SEGMENT of the run the chain `root` was started under —
    /// the one segment of that trace whose `origin` names a state machine — and answer its
    /// document.
    ///
    /// The wait is for the CONDITION: a publication that has happened is observable, so
    /// this looks again until it is.
    async fn awaited_segment(&self, case: &str, root: &str) -> Value {
        let deadline = Instant::now() + DEADLINE;
        loop {
            let answered = self.xray("/Traces", &json!({ "TraceIds": [root] })).await;
            let documents = segment_documents(&answered);
            if let Some(document) = documents.iter().find(|document| {
                document.get("origin").and_then(Value::as_str) == Some(STATE_MACHINE_ORIGIN)
            }) {
                return document.clone();
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: X-Ray never held a segment of the chain {root:?}, last saw \
                 {answered}"
            );
            ::tokio::time::sleep(LOOK).await;
        }
    }
}

/// Every segment document a `BatchGetTraces` answer carries, parsed.
fn segment_documents(answered: &Value) -> Vec<Value> {
    answered
        .get("Traces")
        .and_then(Value::as_array)
        .map(|traces| {
            traces
                .iter()
                .flat_map(|trace| {
                    trace
                        .get("Segments")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default()
                })
                .filter_map(|segment| {
                    segment
                        .get("Document")
                        .and_then(Value::as_str)
                        .and_then(|text| serde_json::from_str::<Value>(text).ok())
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A chain of X-Ray's own form (`1-<8 hex>-<24 hex>`), one per row, so a row's own trace is
/// asked for by the id its start supplied rather than looked for among the others.
fn chain(row: usize) -> String {
    format!("1-5759e988-bd862e3fe1be46a99427{row:04x}")
}

/// The subsegments `entity` carries, in the order it carries them.
fn children(entity: &Value) -> &[Value] {
    entity
        .get("subsegments")
        .and_then(Value::as_array)
        .map(|array| array.as_slice())
        .unwrap_or_default()
}

/// The names of those subsegments.
fn child_names(entity: &Value) -> Vec<String> {
    children(entity)
        .iter()
        .filter_map(|child| child.get("name").and_then(Value::as_str))
        .map(str::to_string)
        .collect()
}

/// The entity at `path` below `document`: each step naming a subsegment of the one before
/// it.
fn entity_at<'a>(document: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut here = document;
    for name in path {
        here = children(here)
            .iter()
            .find(|child| child.get("name").and_then(Value::as_str) == Some(*name))?;
    }
    Some(here)
}

/// Which MARK an entity carries, read off the document alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mark {
    /// Work that did what it was asked, which carries neither flag.
    Clean,
    /// Work that FAILED at what it was doing: `fault`, "indicating that a server error
    /// occurred".
    Fault,
    /// Work a service REFUSED for what the request asked of it: `error`, "indicating that
    /// a client error occurred".
    Error,
    /// Work another unit's failure STOPPED: the same `error`, with a `Canceled` cause.
    Canceled,
}

/// The mark `entity` carries. A `Canceled` one is told from any other client error by the
/// cause it states, which the rows that expect one assert.
fn mark_of(entity: &Value) -> Mark {
    let flag = |name: &str| entity.get(name).and_then(Value::as_bool).unwrap_or_default();
    match (flag("fault"), flag("error")) {
        (true, _) => Mark::Fault,
        (_, true) => match cause_message(entity).as_deref() {
            Some(CANCELED_CAUSE) => Mark::Canceled,
            _ => Mark::Error,
        },
        _ => Mark::Clean,
    }
}

/// What the `cause` of an entity states, where it carries one.
fn cause_message(entity: &Value) -> Option<String> {
    entity
        .get("cause")
        .and_then(|cause| cause.get("message"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// The calls OUT the state at `path` reports: its subsegments that cover a call to
/// `service`.
fn calls_of<'a>(document: &'a Value, path: &[&str], service: &str) -> Vec<&'a Value> {
    entity_at(document, path)
        .map(|entity| {
            children(entity)
                .iter()
                .filter(|child| child.get("name").and_then(Value::as_str) == Some(service))
                .collect()
        })
        .unwrap_or_default()
}

/// One row of the nesting table: the definition a run walks, and the tree its segment must
/// then carry.
struct Shape {
    case: &'static str,
    definition: Value,
    input: Value,
    /// The status the run must reach, so a row whose unit fails is one that really failed.
    status: &'static str,
    /// The names of the subsegments at each path, in the order they must be carried: the
    /// empty path is the run's own segment.
    levels: Vec<(Vec<&'static str>, Vec<&'static str>)>,
    /// The mark the entity at each path must carry.
    marks: Vec<(Vec<&'static str>, Mark)>,
}

/// A branch or processor of one `Pass` state named `name`.
fn passing(name: &str) -> Value {
    json!({ "StartAt": name, "States": { name: { "Type": "Pass", "End": true } } })
}

/// A branch of one `Fail` state.
fn failing(name: &str) -> Value {
    json!({
        "StartAt": name,
        "States": { name: { "Type": "Fail", "Error": "Boom", "Cause": "on purpose" } },
    })
}

/// A branch whose state is a `Wait` far longer than this suite runs, so a row that
/// reported an outcome at all is one whose state machine did not wait for it.
fn waiting(name: &str) -> Value {
    json!({
        "StartAt": name,
        "States": {
            name: { "Type": "Wait", "Seconds": 600, "Next": "Never" },
            "Never": { "Type": "Pass", "End": true },
        },
    })
}

/// A state machine whose only state is a `Parallel` over `branches`.
fn parallel(branches: Value) -> Value {
    json!({
        "StartAt": "Fan",
        "States": { "Fan": { "Type": "Parallel", "Branches": branches, "End": true } },
    })
}

/// A state machine whose only state is an inline `Map` running `processor` over its input.
fn inline_map(processor: Value) -> Value {
    json!({
        "StartAt": "Each",
        "States": { "Each": { "Type": "Map", "ItemProcessor": processor, "End": true } },
    })
}

/// The table the nesting rule is driven over.
fn shapes() -> Vec<Shape> {
    vec![
        Shape {
            case: "a flat definition, whose states are the segment's own subsegments",
            definition: json!({
                "StartAt": "First",
                "States": {
                    "First": { "Type": "Pass", "Next": "Second" },
                    "Second": { "Type": "Pass", "End": true },
                },
            }),
            input: json!({}),
            status: "SUCCEEDED",
            levels: vec![(vec![], vec!["First", "Second"])],
            marks: vec![
                (vec!["First"], Mark::Clean),
                (vec!["Second"], Mark::Clean),
            ],
        },
        Shape {
            case: "a Parallel whose two branches both succeed",
            definition: parallel(json!([passing("Left"), passing("Right")])),
            input: json!({}),
            status: "SUCCEEDED",
            levels: vec![
                (vec![], vec!["Fan"]),
                (vec!["Fan"], vec!["Branch 0", "Branch 1"]),
                (vec!["Fan", "Branch 0"], vec!["Left"]),
                (vec!["Fan", "Branch 1"], vec!["Right"]),
            ],
            marks: vec![
                (vec!["Fan"], Mark::Clean),
                (vec!["Fan", "Branch 0", "Left"], Mark::Clean),
                (vec!["Fan", "Branch 1", "Right"], Mark::Clean),
            ],
        },
        Shape {
            case: "a branch and an iteration whose own Catch answers a state's failure",
            definition: json!({
                "StartAt": "Fan",
                "States": {
                    "Fan": {
                        "Type": "Parallel",
                        "Branches": [
                            {
                                "StartAt": "Risky",
                                "States": {
                                    "Risky": {
                                        "Type": "Task",
                                        "Resource": "arn:aws:states:::lambda:invoke",
                                        "Parameters": {
                                            "FunctionName": "no-such-function",
                                            "Payload": {},
                                        },
                                        "Catch": [{
                                            "ErrorEquals": ["States.ALL"],
                                            "Next": "Handled",
                                        }],
                                        "End": true,
                                    },
                                    "Handled": { "Type": "Pass", "End": true },
                                },
                            },
                            {
                                "StartAt": "Each",
                                "States": {
                                    "Each": {
                                        "Type": "Map",
                                        "ItemsPath": "$.items",
                                        "ItemProcessor": {
                                            "StartAt": "Item",
                                            "States": {
                                                "Item": {
                                                    "Type": "Task",
                                                    "Resource": "arn:aws:states:::lambda:invoke",
                                                    "Parameters": {
                                                        "FunctionName": "no-such-function",
                                                        "Payload": {},
                                                    },
                                                    "Catch": [{
                                                        "ErrorEquals": ["States.ALL"],
                                                        "Next": "ItemHandled",
                                                    }],
                                                    "End": true,
                                                },
                                                "ItemHandled": { "Type": "Pass", "End": true },
                                            },
                                        },
                                        "End": true,
                                    },
                                },
                            }
                        ],
                        "End": true,
                    },
                },
            }),
            input: json!({ "items": ["a"] }),
            status: "SUCCEEDED",
            levels: vec![
                (vec!["Fan"], vec!["Branch 0", "Branch 1"]),
                (vec!["Fan", "Branch 0"], vec!["Risky", "Handled"]),
                (vec!["Fan", "Branch 1", "Each", "Iteration 0"], vec!["Item", "ItemHandled"]),
            ],
            // The state that failed reports the error of the request it made, because its
            // own `Catch` answered that failure — and the branch and the iteration around
            // it went on and finished, so neither reports a mark of its own.
            marks: vec![
                (vec!["Fan"], Mark::Clean),
                (vec!["Fan", "Branch 0"], Mark::Clean),
                (vec!["Fan", "Branch 0", "Risky"], Mark::Error),
                (vec!["Fan", "Branch 0", "Handled"], Mark::Clean),
                (vec!["Fan", "Branch 1"], Mark::Clean),
                (vec!["Fan", "Branch 1", "Each"], Mark::Clean),
                (vec!["Fan", "Branch 1", "Each", "Iteration 0"], Mark::Clean),
                (vec!["Fan", "Branch 1", "Each", "Iteration 0", "Item"], Mark::Error),
            ],
        },
        Shape {
            case: "a Parallel whose first branch fails, which stops the waiting sibling",
            definition: parallel(json!([failing("Bang"), waiting("Hold")])),
            input: json!({}),
            status: "FAILED",
            levels: vec![
                (vec![], vec!["Fan"]),
                (vec!["Fan"], vec!["Branch 0", "Branch 1"]),
                (vec!["Fan", "Branch 0"], vec!["Bang"]),
                (vec!["Fan", "Branch 1"], vec!["Hold"]),
            ],
            marks: vec![
                (vec!["Fan"], Mark::Fault),
                (vec!["Fan", "Branch 0"], Mark::Fault),
                (vec!["Fan", "Branch 0", "Bang"], Mark::Fault),
                (vec!["Fan", "Branch 1"], Mark::Canceled),
                (vec!["Fan", "Branch 1", "Hold"], Mark::Canceled),
            ],
        },
        Shape {
            case: "a Parallel of three branches, the last of which fails",
            definition: parallel(json!([
                passing("Left"),
                passing("Middle"),
                failing("Bang")
            ])),
            input: json!({}),
            status: "FAILED",
            levels: vec![
                (vec!["Fan"], vec!["Branch 0", "Branch 1", "Branch 2"]),
                (vec!["Fan", "Branch 2"], vec!["Bang"]),
            ],
            marks: vec![
                (vec!["Fan", "Branch 0"], Mark::Clean),
                (vec!["Fan", "Branch 0", "Left"], Mark::Clean),
                (vec!["Fan", "Branch 1", "Middle"], Mark::Clean),
                (vec!["Fan", "Branch 2"], Mark::Fault),
                (vec!["Fan", "Branch 2", "Bang"], Mark::Fault),
            ],
        },
        Shape {
            case: "an inline Map over two items, whose iterations both succeed",
            definition: inline_map(passing("Item")),
            input: json!(["a", "b"]),
            status: "SUCCEEDED",
            levels: vec![
                (vec![], vec!["Each"]),
                (vec!["Each"], vec!["Iteration 0", "Iteration 1"]),
                (vec!["Each", "Iteration 0"], vec!["Item"]),
                (vec!["Each", "Iteration 1"], vec!["Item"]),
            ],
            marks: vec![
                (vec!["Each"], Mark::Clean),
                (vec!["Each", "Iteration 0"], Mark::Clean),
                (vec!["Each", "Iteration 1", "Item"], Mark::Clean),
            ],
        },
        Shape {
            case: "an inline Map over three items, whose last iteration fails",
            definition: inline_map(json!({
                "StartAt": "Check",
                "States": {
                    "Check": {
                        "Type": "Choice",
                        "Choices": [{ "Variable": "$", "StringEquals": "boom", "Next": "Bang" }],
                        "Default": "Done",
                    },
                    "Done": { "Type": "Pass", "End": true },
                    "Bang": { "Type": "Fail", "Error": "Boom", "Cause": "on purpose" },
                },
            })),
            input: json!(["fine", "fine", "boom"]),
            status: "FAILED",
            levels: vec![
                (vec![], vec!["Each"]),
                (
                    vec!["Each"],
                    vec!["Iteration 0", "Iteration 1", "Iteration 2"],
                ),
                (vec!["Each", "Iteration 2"], vec!["Check", "Bang"]),
            ],
            marks: vec![
                (vec!["Each"], Mark::Fault),
                (vec!["Each", "Iteration 2"], Mark::Fault),
                (vec!["Each", "Iteration 2", "Bang"], Mark::Fault),
            ],
        },
        Shape {
            case: "a Map nested inside a Parallel branch, which nests both",
            definition: parallel(json!([
                passing("Left"),
                {
                    "StartAt": "Each",
                    "States": {
                        "Each": {
                            "Type": "Map",
                            "ItemsPath": "$.items",
                            "ItemProcessor": passing("Item"),
                            "End": true,
                        },
                    },
                }
            ])),
            input: json!({ "items": ["a"] }),
            status: "SUCCEEDED",
            levels: vec![
                (vec!["Fan"], vec!["Branch 0", "Branch 1"]),
                (vec!["Fan", "Branch 1"], vec!["Each"]),
                (vec!["Fan", "Branch 1", "Each"], vec!["Iteration 0"]),
                (vec!["Fan", "Branch 1", "Each", "Iteration 0"], vec!["Item"]),
            ],
            marks: vec![(
                vec!["Fan", "Branch 1", "Each", "Iteration 0", "Item"],
                Mark::Clean,
            )],
        },
    ]
}

/// A recorded run's segment carries one subsegment per state the run entered, nested the
/// way the definition nests those states, with the mark each outcome implies.
#[tokio::test]
async fn the_segment_tree_mirrors_the_states_the_run_entered() {
    for (index, row) in shapes().into_iter().enumerate() {
        let emulator = Emulator::new();
        let machine = emulator
            .given_state_machine(&format!("shape-{index}"), &row.definition)
            .await;
        let root = chain(index);
        let execution = emulator
            .started(&machine, "only", &row.input, &root)
            .await;
        let status = emulator.settled(row.case, &execution).await;
        assert_eq!(
            status, row.status,
            "case {:?}: expected the run to reach {}, got {status}",
            row.case, row.status
        );
        let document = emulator.awaited_segment(row.case, &root).await;
        for (path, expected) in &row.levels {
            let entity = entity_at(&document, path).unwrap_or_else(|| {
                panic!(
                    "case {:?}: expected an entity at {path:?}, got {document}",
                    row.case
                )
            });
            let names = child_names(entity);
            assert_eq!(
                names, *expected,
                "case {:?}: expected the subsegments of {path:?} to be {expected:?}, got \
                 {names:?} in {document}",
                row.case
            );
        }
        for (path, expected) in &row.marks {
            let entity = entity_at(&document, path).unwrap_or_else(|| {
                panic!(
                    "case {:?}: expected an entity at {path:?}, got {document}",
                    row.case
                )
            });
            let mark = mark_of(entity);
            assert_eq!(
                mark, *expected,
                "case {:?}: expected {path:?} to carry {expected:?}, got {mark:?} in {entity}",
                row.case
            );
            if *expected == Mark::Canceled {
                assert_eq!(
                    cause_message(entity).as_deref(),
                    Some(CANCELED_CAUSE),
                    "case {:?}: expected the stopped work at {path:?} to state its cause, got \
                     {entity}",
                    row.case
                );
            }
            // Every entity spans the work it covers, however it ended.
            let span = (
                entity.get("start_time").and_then(Value::as_f64),
                entity.get("end_time").and_then(Value::as_f64),
            );
            assert!(
                matches!(span, (Some(from), Some(to)) if to >= from),
                "case {:?}: expected {path:?} to span the work it covers, got {entity}",
                row.case
            );
        }
    }
}

/// One row of the attempts table: the state machine, and what each of its states must
/// then report about the calls its work made.
struct Attempts {
    case: &'static str,
    /// Built from the function's ARN, which is only known once it is created.
    definition: fn(&str) -> Value,
    /// The status the run must reach, so a row whose failure nothing answers is one that
    /// really failed.
    run_status: &'static str,
    states: Vec<Invoking>,
}

/// What one state of such a row must report.
struct Invoking {
    state: &'static str,
    /// How many calls out the state reports: one per attempt its work made.
    calls: usize,
    /// The function those calls name — `None` for the function the row created, which
    /// its definition names by ARN.
    function: Option<&'static str>,
    /// The status those calls report having answered, and `None` where the answer stated
    /// none: a call whose work ANSWERED and reported a failure of its own.
    status: Option<u64>,
    /// The mark the state's own subsegment carries.
    mark: Mark,
    /// The ERROR the state's own cause names, for a state whose work failed: the name the
    /// failure was classified under.
    error: Option<&'static str>,
    /// What the cause of its CALLS states, for calls a service refused: the exception
    /// that service answered, which names the service and the status it came back with.
    answered: Option<&'static str>,
    /// The FUNCTION ERROR its calls report, for calls whose function raised.
    function_error: Option<&'static str>,
}

/// The table the per-attempt rule is driven over: the states of a definition whose
/// failures a catcher answers, under each of the two families a `Task` state's work is
/// recorded under, and one whose failure nothing answers.
fn attempts() -> Vec<Attempts> {
    vec![
        Attempts {
            case: "the optimized lambda:invoke integration",
            run_status: "SUCCEEDED",
            definition: |function| {
                json!({
                    "StartAt": "Route",
                    "States": {
                        "Route": {
                            "Type": "Choice",
                            "Choices": [{
                                "Variable": "$.route",
                                "StringEquals": "invoke",
                                "Next": "InvokeOk",
                            }],
                            "Default": "Recovered",
                        },
                        "InvokeOk": {
                            "Type": "Task",
                            "Resource": "arn:aws:states:::lambda:invoke",
                            "Parameters": { "FunctionName": function, "Payload": {} },
                            "Next": "InvokeMissing",
                        },
                        "InvokeMissing": {
                            "Type": "Task",
                            "Resource": "arn:aws:states:::lambda:invoke",
                            "Parameters": {
                                "FunctionName": "no-such-function",
                                "Payload": {},
                            },
                            "Catch": [{
                                "ErrorEquals": ["States.ALL"],
                                "Next": "InvokeRetry",
                            }],
                            "Next": "InvokeRetry",
                        },
                        "InvokeRetry": {
                            "Type": "Task",
                            "Resource": "arn:aws:states:::lambda:invoke",
                            "Parameters": {
                                "FunctionName": function,
                                "Payload": { "boom": true },
                            },
                            "Retry": [{
                                "ErrorEquals": ["States.ALL"],
                                "MaxAttempts": 1,
                                "IntervalSeconds": 0,
                            }],
                            "Catch": [{
                                "ErrorEquals": ["States.ALL"],
                                "Next": "Recovered",
                            }],
                            "Next": "Recovered",
                        },
                        "Recovered": { "Type": "Pass", "End": true },
                    },
                })
            },
            states: vec![
                Invoking {
                    state: "Route",
                    calls: 0,
                    function: None,
                    status: None,
                    mark: Mark::Clean,
                    error: None,
                    answered: None,
                    function_error: None,
                },
                Invoking {
                    state: "InvokeOk",
                    calls: 1,
                    function: None,
                    status: Some(200),
                    mark: Mark::Clean,
                    error: None,
                    answered: None,
                    function_error: None,
                },
                // The function this one names does not exist, so the call is REFUSED
                // with a client status — an error of the request the task made.
                Invoking {
                    state: "InvokeMissing",
                    calls: 1,
                    function: Some("no-such-function"),
                    status: Some(404),
                    mark: Mark::Error,
                    error: Some("Lambda.ResourceNotFoundException"),
                    answered: Some("(Service: Lambda, Status Code: 404"),
                    function_error: None,
                },
                // This one's function RAISED, and its own `Catch` ANSWERED that failure:
                // the invocation answered and reported a function error, which is a fault
                // of the work that answer covers, while the state's own failure is the
                // error of the request it made.
                Invoking {
                    state: "InvokeRetry",
                    calls: 2,
                    function: None,
                    status: Some(200),
                    mark: Mark::Error,
                    error: Some("RuntimeError"),
                    answered: None,
                    function_error: Some("Unhandled"),
                },
                Invoking {
                    state: "Recovered",
                    calls: 0,
                    function: None,
                    status: None,
                    mark: Mark::Clean,
                    error: None,
                    answered: None,
                    function_error: None,
                },
            ],
        },
        Attempts {
            case: "a Resource that is the function's own ARN",
            run_status: "SUCCEEDED",
            definition: |function| {
                json!({
                    "StartAt": "Route",
                    "States": {
                        "Route": {
                            "Type": "Choice",
                            "Choices": [{
                                "Variable": "$.route",
                                "StringEquals": "invoke",
                                "Next": "InvokeOk",
                            }],
                            "Default": "Recovered",
                        },
                        "InvokeOk": {
                            "Type": "Task",
                            "Resource": function,
                            "Parameters": { "ok": true },
                            "Next": "InvokeRetry",
                        },
                        "InvokeRetry": {
                            "Type": "Task",
                            "Resource": function,
                            "Parameters": { "boom": true },
                            "Retry": [{
                                "ErrorEquals": ["States.ALL"],
                                "MaxAttempts": 1,
                                "IntervalSeconds": 0,
                            }],
                            "Catch": [{
                                "ErrorEquals": ["States.ALL"],
                                "Next": "Recovered",
                            }],
                            "Next": "Recovered",
                        },
                        "Recovered": { "Type": "Pass", "End": true },
                    },
                })
            },
            states: vec![
                Invoking {
                    state: "Route",
                    calls: 0,
                    function: None,
                    status: None,
                    mark: Mark::Clean,
                    error: None,
                    answered: None,
                    function_error: None,
                },
                Invoking {
                    state: "InvokeOk",
                    calls: 1,
                    function: None,
                    status: Some(200),
                    mark: Mark::Clean,
                    error: None,
                    answered: None,
                    function_error: None,
                },
                Invoking {
                    state: "InvokeRetry",
                    calls: 2,
                    function: None,
                    status: Some(200),
                    mark: Mark::Error,
                    error: Some("RuntimeError"),
                    answered: None,
                    function_error: Some("Unhandled"),
                },
                Invoking {
                    state: "Recovered",
                    calls: 0,
                    function: None,
                    status: None,
                    mark: Mark::Clean,
                    error: None,
                    answered: None,
                    function_error: None,
                },
            ],
        },
        // And the other side of the state rule: a failure NOTHING answers, which is the
        // fault of the work the state was doing and the failure of the run itself.
        Attempts {
            case: "a task failure no catcher answers",
            run_status: "FAILED",
            definition: |function| {
                json!({
                    "StartAt": "InvokeBoom",
                    "States": {
                        "InvokeBoom": {
                            "Type": "Task",
                            "Resource": "arn:aws:states:::lambda:invoke",
                            "Parameters": {
                                "FunctionName": function,
                                "Payload": { "boom": true },
                            },
                            "End": true,
                        },
                    },
                })
            },
            states: vec![Invoking {
                state: "InvokeBoom",
                calls: 1,
                function: None,
                status: Some(200),
                mark: Mark::Fault,
                error: Some("RuntimeError"),
                answered: None,
                function_error: Some("Unhandled"),
            }],
        },
    ]
}

/// A state whose work is a call to another service reports one subsegment per ATTEMPT it
/// made, named as a call out to that service is named — so a retried state reports as many
/// as it attempted and a state that called nothing reports none.
#[tokio::test]
async fn a_state_reports_one_subsegment_per_attempt_at_its_work() {
    for (index, row) in attempts().into_iter().enumerate() {
        let emulator = Emulator::new();
        let function_name = format!("traced-{index}");
        let function = emulator.given_function(&function_name).await;
        let machine = emulator
            .given_state_machine(&format!("attempts-{index}"), &(row.definition)(&function))
            .await;
        let root = chain(100 + index);
        let execution = emulator
            .started(&machine, "only", &json!({ "route": "invoke" }), &root)
            .await;
        let status = emulator.settled(row.case, &execution).await;
        assert_eq!(
            status, row.run_status,
            "case {:?}: expected the run to reach {}, got {status}",
            row.case, row.run_status
        );
        let document = emulator.awaited_segment(row.case, &root).await;
        for expected in &row.states {
            let state = expected.state;
            let invoked = expected.function.unwrap_or(function.as_str());
            let calls = calls_of(&document, &[state], LAMBDA_CALL);
            assert_eq!(
                calls.len(),
                expected.calls,
                "case {:?}: expected {state:?} to report {} call(s) to {LAMBDA_CALL}, got {} \
                 in {document}",
                row.case,
                expected.calls,
                calls.len()
            );
            for call in calls {
                assert_eq!(
                    call.get("namespace").and_then(Value::as_str),
                    Some("aws"),
                    "case {:?}: expected the call of {state:?} to be a call out, got {call}",
                    row.case
                );
                assert_eq!(
                    call.get("aws")
                        .and_then(|aws| aws.get("operation"))
                        .and_then(Value::as_str),
                    Some(INVOKE_OPERATION),
                    "case {:?}: expected the call of {state:?} to name the operation it made, \
                     got {call}",
                    row.case
                );
                assert_eq!(
                    call.get("aws")
                        .and_then(|aws| aws.get("function_name"))
                        .and_then(Value::as_str),
                    Some(invoked),
                    "case {:?}: expected the call of {state:?} to name the function it \
                     invoked, got {call}",
                    row.case
                );
                // A call the trace reports is one that ENDED: its span is closed, so a
                // reader is not left with work in flight for a run that is over.
                assert!(
                    call.get("end_time").and_then(Value::as_f64).is_some(),
                    "case {:?}: expected the call of {state:?} to have ended, got {call}",
                    row.case
                );
                // And it reports the response the invoked service answered, where that
                // answer stated one.
                let answered = call
                    .get("http")
                    .and_then(|http| http.get("response"))
                    .and_then(|response| response.get("status"))
                    .and_then(Value::as_u64);
                assert_eq!(
                    answered, expected.status,
                    "case {:?}: expected the call of {state:?} to report the response {:?} it \
                     answered, got {call}",
                    row.case, expected.status
                );
                // A call a service REFUSED states what that service answered, which names
                // the service and the status it came back with.
                if let Some(stated) = expected.answered {
                    let cause = cause_message(call).unwrap_or_default();
                    assert!(
                        cause.contains(stated),
                        "case {:?}: expected the cause of {state:?}'s call to state {stated:?}, \
                         got {cause:?}"
                        ,
                        row.case
                    );
                }
                // And a call whose FUNCTION errored reports the signal the invocation's
                // answer named that error by.
                assert_eq!(
                    call.get("aws")
                        .and_then(|aws| aws.get("function_error"))
                        .and_then(Value::as_str),
                    expected.function_error,
                    "case {:?}: expected the call of {state:?} to report the function error \
                     {:?} its answer named, got {call}",
                    row.case,
                    expected.function_error
                );
            }
            let state_entity = entity_at(&document, &[state]).unwrap_or_else(|| {
                panic!(
                    "case {:?}: expected a subsegment for {state:?}, got {document}",
                    row.case
                )
            });
            let mark = mark_of(state_entity);
            assert_eq!(
                mark, expected.mark,
                "case {:?}: expected {state:?} to carry {:?}, got {mark:?} in {state_entity}",
                row.case, expected.mark
            );
            // A state's own cause names the ERROR its failure was classified under, which
            // is what a `Retry` and a `Catch` match on — not the sentence the service
            // answered, which the call it made states.
            if let Some(classified) = expected.error {
                assert_eq!(
                    cause_message(state_entity).as_deref(),
                    Some(classified),
                    "case {:?}: expected {state:?} to name the error it failed with, got \
                     {state_entity}",
                    row.case
                );
            }
        }
        // A run that reached its end carries no mark of its own, and one its own states
        // failed is a fault of the work the segment covers.
        let expected_run = match row.run_status {
            "SUCCEEDED" => Mark::Clean,
            _ => Mark::Fault,
        };
        assert_eq!(
            mark_of(&document),
            expected_run,
            "case {:?}: expected the run's own segment to carry {expected_run:?}, got \
             {document}",
            row.case
        );
    }
}

/// A run the CLIENT stopped reports a fault of the work its segment covers, and the state
/// the stop caught in flight is reported as work that was canceled.
#[tokio::test]
async fn a_stopped_run_reports_a_fault_and_the_work_the_stop_caught() {
    let emulator = Emulator::new();
    let machine = emulator
        .given_state_machine("stopped", &waiting("Hold"))
        .await;
    let root = chain(200);
    let execution = emulator
        .started(&machine, "only", &json!({}), &root)
        .await;
    // The run is stopped once it is really running, so the state the stop catches is one
    // the walk had entered.
    let deadline = Instant::now() + DEADLINE;
    loop {
        let described = emulator.described(&execution).await;
        if string(&described, "status") == RUNNING {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the run must reach {RUNNING} before it is stopped, last saw {described}"
        );
        ::tokio::time::sleep(LOOK).await;
    }
    emulator
        .sfn("StopExecution", &json!({ "executionArn": execution }))
        .await;
    let status = emulator.settled("a run the client stopped", &execution).await;
    assert_eq!(status, "ABORTED", "expected the stopped run to be ABORTED");
    let document = emulator
        .awaited_segment("a run the client stopped", &root)
        .await;
    assert_eq!(
        mark_of(&document),
        Mark::Fault,
        "expected the stopped run's own segment to report a fault of its work, got {document}"
    );
    let state = entity_at(&document, &["Hold"])
        .unwrap_or_else(|| panic!("expected a subsegment for the state in flight, got {document}"));
    assert_eq!(
        mark_of(state),
        Mark::Canceled,
        "expected the state the stop caught to be reported as canceled, got {state}"
    );
    assert_eq!(
        cause_message(state).as_deref(),
        Some(CANCELED_CAUSE),
        "expected the canceled state to state its cause, got {state}"
    );
}

/// An inline `Map` whose iterations CALL another service reports the work of each
/// iteration under that iteration's own subsegment: the one whose call failed reports the
/// fault of its own work, and the one the failure stopped reports the cancellation.
///
/// This is the rule's other concurrent state driven over work that really leaves this
/// service: the iterations run at the same time, so their states' events are interleaved,
/// and each state is still reported inside the iteration that ran it.
#[tokio::test]
async fn a_map_reports_the_work_each_of_its_iterations_ran() {
    let case = "an inline Map whose first iteration's call fails";
    let emulator = Emulator::new();
    let function_name = "map-fn";
    let function = emulator.given_function(function_name).await;
    let definition = json!({
        "StartAt": "RunMap",
        "States": {
            "RunMap": {
                "Type": "Map",
                "ItemProcessor": {
                    "StartAt": "MapTask",
                    "States": {
                        "MapTask": {
                            "Type": "Task",
                            "Resource": "arn:aws:states:::lambda:invoke",
                            "Parameters": { "FunctionName": function, "Payload.$": "$" },
                            "End": true,
                        },
                    },
                },
                "End": true,
            },
        },
    });
    let machine = emulator.given_state_machine("mapped", &definition).await;
    let root = chain(300);
    // The first item asks the handler to raise and the second does not, so the state's
    // first iteration fails and the work of the second is stopped with it.
    let execution = emulator
        .started(
            &machine,
            "only",
            &json!([{ "boom": true }, { "ok": true }]),
            &root,
        )
        .await;
    let status = emulator.settled(case, &execution).await;
    assert_eq!(
        status, "FAILED",
        "case {case:?}: expected the state's failure to fail the run, got {status}"
    );
    let document = emulator.awaited_segment(case, &root).await;
    for (path, expected) in [
        (vec![], vec!["RunMap"]),
        (vec!["RunMap"], vec!["Iteration 0", "Iteration 1"]),
        (vec!["RunMap", "Iteration 0"], vec!["MapTask"]),
        (vec!["RunMap", "Iteration 1"], vec!["MapTask"]),
        (
            vec!["RunMap", "Iteration 0", "MapTask"],
            vec![LAMBDA_CALL],
        ),
    ] {
        let entity = entity_at(&document, &path)
            .unwrap_or_else(|| panic!("case {case:?}: expected an entity at {path:?}, got {document}"));
        let names = child_names(entity);
        assert_eq!(
            names, expected,
            "case {case:?}: expected the subsegments of {path:?} to be {expected:?}, got \
             {names:?} in {document}"
        );
    }
    for (path, expected) in [
        (vec!["RunMap"], Mark::Fault),
        (vec!["RunMap", "Iteration 0"], Mark::Fault),
        (vec!["RunMap", "Iteration 0", "MapTask"], Mark::Fault),
        (vec!["RunMap", "Iteration 0", "MapTask", LAMBDA_CALL], Mark::Fault),
        (vec!["RunMap", "Iteration 1"], Mark::Canceled),
        (vec!["RunMap", "Iteration 1", "MapTask"], Mark::Canceled),
    ] {
        let entity = entity_at(&document, &path)
            .unwrap_or_else(|| panic!("case {case:?}: expected an entity at {path:?}, got {document}"));
        let mark = mark_of(entity);
        assert_eq!(
            mark, expected,
            "case {case:?}: expected {path:?} to carry {expected:?}, got {mark:?} in {entity}"
        );
    }
    // The iteration that was stopped made no call at all: the work it would have asked
    // for was never asked for.
    let stopped_calls = calls_of(&document, &["RunMap", "Iteration 1", "MapTask"], LAMBDA_CALL);
    assert!(
        stopped_calls.is_empty(),
        "case {case:?}: expected the stopped iteration to report no call, got \
         {stopped_calls:?}"
    );
}


