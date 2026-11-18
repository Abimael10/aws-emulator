// SPDX-License-Identifier: CC0-1.0

//! ONE EventBridge rule carrying a LAMBDA target and a STEP FUNCTIONS target, where the
//! function's own handler does its work with the AWS SDK its managed Python runtime supplies
//! — end to end over the real assembled `MultiServiceRouter`, with EventBridge, Lambda,
//! Amazon S3, Step Functions, IAM, the token service and CloudFormation mounted on ONE
//! engine.
//!
//! RULE 1: every entry a publication accepts is matched against each enabled rule of the bus
//! it was published onto and handed to EVERY target of a matching rule ON ITS OWN, once per
//! matching entry — a function target is invoked as `events.amazonaws.com`, which its own
//! resource-based policy decides ("for Lambda functions, Amazon SNS topics and Amazon SQS
//! queues, EventBridge relies on resource-based policies", `PutTargets`), and a state-machine
//! target is started under the role the target names ("for EC2 instances, Kinesis data
//! streams, Step Functions state machines and API Gateway APIs, EventBridge relies on IAM
//! roles that you specify in the `RoleARN` argument in `PutTargets`") — so N matching entries
//! produce N handovers per target, each carrying that entry's own envelope, and a handover the
//! documents in force refuse withholds NOTHING from its sibling target.
//!
//! RULE 2: a managed `python3.x` runtime puts the AWS SDK on the handler's import path with
//! the catalog of the services the runtime can reach, so a client the handler builds for ANY
//! of them — the object store and the workflow service included, whichever protocol it speaks
//! — is CONSTRUCTED AND USED rather than refused, and the call it makes reaches that service
//! as a session of the function's execution role, with its effect in that service's own
//! state.
//!
//! Seven crates have to agree for the two to hold together, so the suite lives here. What the
//! neighbours pin and this one does not repeat:
//! [`crate::events_rule_lambda_target_writes_the_detail_into_dynamodb`] pins the single-target
//! rule → function → table chain, [`crate::events_fan_out_to_each_matched_target`] the
//! MULTIPLICITY of a publication's targets with no handler-side work,
//! [`crate::sfn_lambda_task_handler_writes_objects`] what a handler invoked by a `Task` state
//! can write, and the Lambda crate's `handler_sdk_service_inventory` WHICH service tokens the
//! bundled SDK answers to at all. What only this suite reaches is the two together: a
//! heterogeneous fan-out whose function leg's evidence is work the in-function SDK performed
//! in two OTHER services, so a fan-out that never dispatches and an SDK that cannot build the
//! client the handler needs are told apart — the first leaves the sibling state machine
//! started and the object missing, the second leaves both legs' own records in place and the
//! object missing.
//!
//! The rows differ along what the rules generalise over: HOW MANY matching entries one
//! publication carries (one, two, three), so a count is a count rather than a flag; the ORDER
//! the two targets are written in (the function first, the state machine first); the
//! `detail-type` each rule's pattern names; and WHETHER the function target's own
//! resource-based policy admits this service at all, which is the one thing that may withhold
//! the function leg.
//!
//! Every observation is one a client makes: what `PutEvents` answered, the objects the
//! handler's own `PutObject` left in the bucket, the executions Step Functions reports for the
//! machine the handler STARTED, and the executions it reports for the machine the rule's other
//! target names.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The reader of one element of an awsQuery answer, the percent-encoder for an
// `application/x-www-form-urlencoded` component and the stack poll: the `all` target is one
// crate root, so this suite borrows them rather than carrying a second copy.
use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{form_encode, opt_element};

/// The Region every request here is signed for, and so the Region the bucket, the functions,
/// the rules and the state machines belong to.
const REGION: &str = "us-east-1";

/// The bucket the invoked handler writes its marker objects into, and the prefix it nests them
/// under.
const BUCKET: &str = "xsvc-events-fanout-markers";
const PREFIX: &str = "markers/";

/// The `Source` every rule here names and every published entry carries.
const SOURCE: &str = "com.demo.events.fanout.markers";

/// A `Source` no rule here names, which is how an entry that must be delivered NOWHERE is
/// published beside one that must be delivered.
const OTHER_SOURCE: &str = "com.demo.events.fanout.unmatched";

/// The member of each entry's `detail` an observation is keyed by.
const MARKER: &str = "marker";

/// The function the granted rows target, and the one the barred row targets: the same handler,
/// told apart only by the statement each one's own policy carries.
const GRANTED_FUNCTION: &str = "xsvc-fanout-marker-writer";
const BARRED_FUNCTION: &str = "xsvc-fanout-marker-barred";

/// The stack both functions are deployed as: inline template source is the form a template
/// declares a RUNNABLE deployment package in, which is how a handler written out in a suite
/// comes to exist at all.
const FUNCTION_STACK: &str = "xsvc-fanout-marker-functions";

/// The `StackStatus` that stack has to reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The state the function has to report before an invocation is served: "during this time, you
/// can't invoke or modify the function".
const FUNCTION_ACTIVE: &str = "Active";

/// The state machine the HANDLER starts with the SDK its runtime supplies — the witness that
/// the in-function workflow client was built and used, rather than refused as unknown.
const DOWNSTREAM_MACHINE: &str = "xsvc-fanout-marker-downstream";

/// The roles here: the function's own execution role, the role a state-machine target is
/// started under, and the role the machines themselves run as.
const FUNCTION_ROLE: &str = "xsvc-fanout-marker-function-role";
const TARGET_ROLE: &str = "xsvc-fanout-marker-target-role";
const MACHINE_ROLE: &str = "xsvc-fanout-marker-machine-role";

/// The principals the three roles above are assumed by.
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";
const LAMBDA_PRINCIPAL: &str = "lambda.amazonaws.com";
const STATES_PRINCIPAL: &str = "states.amazonaws.com";

/// The action an invocation of a function performs, which the function's own policy admits.
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// The action a state-machine handover performs, which the target role admits.
const START_EXECUTION_ACTION: &str = "states:StartExecution";

/// A machine that terminates on its own, so an execution a handover — or the handler — started
/// is a real execution rather than one waiting on a task.
const DEFINITION: &str = r#"{"StartAt":"Done","States":{"Done":{"Type":"Succeed"}}}"#;

/// The statuses an execution of those machines is read in: the one it sits in while its work
/// is outstanding, and the terminal one a machine whose only state succeeds reaches
/// (`smithy_query(query="shape",shape="ExecutionStatus",model_path="services/service-com-amazonaws-sfn-awsstepfunctions/smithy-model/com-amazonaws-sfn-awsstepfunctions.json")`).
const RUNNING: &str = "RUNNING";
/// See [`RUNNING`].
const SUCCEEDED: &str = "SUCCEEDED";

/// How long one invocation is given, as the function's own `Timeout`: well past what the
/// handler needs on any host.
const FUNCTION_TIMEOUT: u64 = 120;

/// How long to keep looking for work the CLOCK owes — a stack settling, a function reaching
/// [`FUNCTION_ACTIVE`], a handover running, the handler's own calls landing — and how long
/// between two looks. The deadline is generous: what is under test is that the work happens at
/// all, never how fast this host gets to it.
const BUDGET: Duration = Duration::from_secs(180);
/// See [`BUDGET`].
const LOOK: Duration = Duration::from_millis(50);

/// The handler both functions run: for the ONE entry it is handed it builds a client for the
/// object store and one for the workflow service with the SDK its runtime supplies, writes the
/// envelope it received as an object keyed by the entry's own marker, and starts an execution
/// of the downstream machine named after that marker.
///
/// Both clients are built at module scope, so a runtime whose SDK could not resolve either
/// token fails the import rather than the call — which `Invoke` reports as a function error and
/// a `Task` reports as a failed execution.
const MARKER_HANDLER: &str = r#"
import json
import os

import boto3

OBJECTS = boto3.client("s3")
WORKFLOWS = boto3.client("stepfunctions")


def handler(event, context):
    detail = event["detail"]
    marker = detail["marker"]
    key = os.environ["PREFIX"] + marker + ".json"
    OBJECTS.put_object(
        Bucket=os.environ["BUCKET"],
        Key=key,
        Body=json.dumps(
            {
                "marker": marker,
                "source": event["source"],
                "detail-type": event["detail-type"],
                "id": event["id"],
            }
        ).encode("utf-8"),
        ContentType="application/json",
    )
    started = WORKFLOWS.start_execution(
        stateMachineArn=os.environ["DOWNSTREAM"],
        name=marker,
        input=json.dumps({"marker": marker}),
    )
    return {"key": key, "started": started["executionArn"]}
"#;

/// One row: the rule it publishes through, the state machine that rule's OTHER target names,
/// the `detail-type` its pattern names, the markers its publication carries, which order the
/// two targets are written in, and whether the function target's own policy admits this
/// service.
struct Row {
    case: &'static str,
    rule: &'static str,
    machine: &'static str,
    detail_type: &'static str,
    markers: &'static [&'static str],
    /// Whether the function target is written BEFORE the state-machine target.
    function_first: bool,
    /// The function the rule targets, and whether a statement admitting this service is put on
    /// it.
    function: &'static str,
    granted: bool,
}

/// The rows, in the order they run: the barred row sits between two granted ones, so its
/// missing objects are read after a delivery of a LATER publication has already landed.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "one entry, the function target written first",
            rule: "xsvc-fanout-rule-single",
            machine: "xsvc-fanout-machine-single",
            detail_type: "Fan Out Single",
            markers: &["single-0"],
            function_first: true,
            function: GRANTED_FUNCTION,
            granted: true,
        },
        Row {
            case: "two entries for a function whose policy admits nothing of this service",
            rule: "xsvc-fanout-rule-barred",
            machine: "xsvc-fanout-machine-barred",
            detail_type: "Fan Out Barred",
            markers: &["barred-0", "barred-1"],
            function_first: true,
            function: BARRED_FUNCTION,
            granted: false,
        },
        Row {
            case: "three entries in one publication, the state-machine target written first",
            rule: "xsvc-fanout-rule-batch",
            machine: "xsvc-fanout-machine-batch",
            detail_type: "Fan Out Batch",
            markers: &["batch-0", "batch-1", "batch-2"],
            function_first: false,
            function: GRANTED_FUNCTION,
            granted: true,
        },
    ]
}

/// The emulator as it is ASSEMBLED, on one engine: the handler's own SDK calls are dispatched
/// back into this same router, so the bucket and the machine it addresses are the ones this
/// suite reads.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The suite exercises the services, not SigV4: the credential scope is what routes a
    // request, and the signature itself is never checked here.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what routes
/// the request.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One request as it arrives on the wire, with the status and the body it was answered with.
async fn dispatch(
    router: &MultiServiceRouter,
    method: &str,
    host: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Bytes,
) -> (u16, Vec<u8>) {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let response = router
        .dispatch(builder.body(body).expect("a well-formed request"))
        .await;
    (response.status().as_u16(), response.body().to_vec())
}

/// One call to an AWS JSON service, as its status and parsed body.
async fn rpc(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    json_version: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let (status, answered) = dispatch(
        router,
        "POST",
        &format!("{signing_name}.{REGION}.amazonaws.com"),
        "/",
        &[
            (
                "content-type",
                &format!("application/x-amz-json-{json_version}"),
            ),
            ("x-amz-target", &format!("{target_prefix}.{operation}")),
            ("authorization", &authorization(signing_name)),
        ],
        Bytes::from(body.to_string()),
    )
    .await;
    let parsed: Value = if answered.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&answered).unwrap_or_else(|_| {
            Value::String(String::from_utf8_lossy(&answered).into_owned())
        })
    };
    (status, parsed)
}

/// One EventBridge call (`awsJson1_1`, `AWSEvents.<Operation>`) that must answer 200.
async fn events(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = rpc(router, "events", "AWSEvents", "1.1", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from EventBridge {operation}, got {status} with \
         {parsed}"
    );
    parsed
}

/// One Step Functions call (`awsJson1_0`, `AWSStepFunctions.<Operation>`) that must answer 200.
async fn sfn(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = rpc(router, "states", "AWSStepFunctions", "1.0", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Step Functions {operation}, got {status} with \
         {parsed}"
    );
    parsed
}

/// One Lambda `restJson1` call, as its status and parsed body.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let payload = body.map(Value::to_string).unwrap_or_default();
    let (status, answered) = dispatch(
        router,
        method,
        &format!("lambda.{REGION}.amazonaws.com"),
        path,
        &[
            ("content-type", "application/json"),
            ("authorization", &authorization("lambda")),
        ],
        Bytes::from(payload),
    )
    .await;
    let parsed: Value = if answered.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&answered).unwrap_or_else(|_| {
            Value::String(String::from_utf8_lossy(&answered).into_owned())
        })
    };
    (status, parsed)
}

/// One request against Amazon S3's REST surface, path-style, with the bytes it answered.
async fn s3(router: &MultiServiceRouter, method: &str, path: &str) -> (u16, Vec<u8>) {
    dispatch(
        router,
        method,
        &format!("s3.{REGION}.amazonaws.com"),
        path,
        &[
            ("content-type", "application/octet-stream"),
            ("authorization", &authorization("s3")),
        ],
        Bytes::new(),
    )
    .await
}

/// One awsQuery call of `signing_name` at `host`, as its status and XML body.
async fn query(
    router: &MultiServiceRouter,
    signing_name: &str,
    host: &str,
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
    let (status, answered) = dispatch(
        router,
        "POST",
        host,
        "/",
        &[
            ("content-type", "application/x-www-form-urlencoded"),
            ("authorization", &authorization(signing_name)),
        ],
        Bytes::from(body),
    )
    .await;
    (status, String::from_utf8_lossy(&answered).into_owned())
}

/// One IAM call that must answer 200.
async fn iam(router: &MultiServiceRouter, action: &str, members: &[(&str, &str)]) -> String {
    let (status, body) = query(
        router,
        "iam",
        "iam.amazonaws.com",
        "2010-05-08",
        action,
        members,
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: expected HTTP 200 from IAM {action}, got {status} with {body}"
    );
    body
}

/// One CloudFormation call that must answer 200.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let (status, body) = query(
        router,
        "cloudformation",
        &format!("cloudformation.{REGION}.amazonaws.com"),
        "2010-05-15",
        action,
        members,
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: expected HTTP 200 from CloudFormation {action}, got {status} with {body}"
    );
    body
}

/// A role `principal` may assume, carrying `statements` as its one inline policy; answers the
/// ARN IAM reports for it.
async fn given_role(
    router: &MultiServiceRouter,
    name: &str,
    principal: &str,
    statements: Value,
) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": principal },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let created = iam(
        router,
        "CreateRole",
        &[("RoleName", name), ("AssumeRolePolicyDocument", &trust)],
    )
    .await;
    let arn = opt_element(&created, "Arn")
        .unwrap_or_else(|| panic!("setup: CreateRole({name:?}) must answer an ARN, got {created}"));
    let document = json!({ "Version": "2012-10-17", "Statement": statements }).to_string();
    iam(
        router,
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

/// Create the bucket the handler writes into.
async fn given_bucket(router: &MultiServiceRouter) {
    let (status, body) = s3(router, "PUT", &format!("/{BUCKET}")).await;
    assert_eq!(
        status,
        200,
        "setup: CreateBucket({BUCKET:?}) must answer 200, got {status} with {}",
        String::from_utf8_lossy(&body)
    );
}

/// A state machine named `name` running as `role_arn`, whose one state succeeds; answers the
/// ARN Step Functions reports for it.
async fn given_machine(router: &MultiServiceRouter, name: &str, role_arn: &str) -> String {
    let created = sfn(
        router,
        "setup",
        "CreateStateMachine",
        &json!({ "name": name, "definition": DEFINITION, "roleArn": role_arn }),
    )
    .await;
    created["stateMachineArn"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("setup: CreateStateMachine({name:?}) must answer an ARN, got {created}")
        })
        .to_string()
}

/// Deploy both functions as one stack and answer the ARN Lambda reports for each, in the order
/// `names` lists them, once Lambda first reports it [`FUNCTION_ACTIVE`].
async fn given_functions(
    router: &MultiServiceRouter,
    role_arn: &str,
    downstream_arn: &str,
    names: &[&str],
) -> Vec<String> {
    let mut resources = serde_json::Map::new();
    for (index, name) in names.iter().enumerate() {
        resources.insert(
            format!("Writer{index}"),
            json!({
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": name,
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": role_arn,
                    "Timeout": FUNCTION_TIMEOUT,
                    "Environment": { "Variables": {
                        "BUCKET": BUCKET,
                        "PREFIX": PREFIX,
                        "DOWNSTREAM": downstream_arn,
                    } },
                    "Code": { "ZipFile": MARKER_HANDLER },
                },
            }),
        );
    }
    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": Value::Object(resources),
    })
    .to_string();
    cloudformation(
        router,
        "CreateStack",
        &[("StackName", FUNCTION_STACK), ("TemplateBody", &template)],
    )
    .await;
    let (reached, described) = wait_for_stack_status(router, FUNCTION_STACK, CREATE_COMPLETE).await;
    assert_eq!(
        reached, CREATE_COMPLETE,
        "setup: expected {FUNCTION_STACK:?} to reach {CREATE_COMPLETE}, got {described}"
    );
    let mut arns = Vec::new();
    for name in names {
        arns.push(await_function_active(router, name).await);
    }
    arns
}

/// Wait until Lambda first reports `name` [`FUNCTION_ACTIVE`], and answer the `FunctionArn` it
/// reports.
async fn await_function_active(router: &MultiServiceRouter, name: &str) -> String {
    let deadline = Instant::now() + BUDGET;
    loop {
        let (status, configuration) = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{name}/configuration"),
            None,
        )
        .await;
        if status == 200 && configuration["State"].as_str() == Some(FUNCTION_ACTIVE) {
            return configuration["FunctionArn"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("setup: {name:?} reports no FunctionArn, got {configuration}")
                })
                .to_string();
        }
        assert!(
            Instant::now() < deadline,
            "setup: Lambda never reported {name:?} {FUNCTION_ACTIVE}, last read {status} \
             {configuration}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// The statement on `function` that admits the invocation `rule_arn` makes: the service
/// principal, the invoking action, scoped by `aws:SourceArn` to the rule itself.
async fn allow_the_rule_to_invoke(
    router: &MultiServiceRouter,
    function: &str,
    statement_id: &str,
    rule_arn: &str,
) {
    let (status, granted) = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{function}/policy"),
        Some(&json!({
            "StatementId": statement_id,
            "Action": INVOKE_ACTION,
            "Principal": EVENTS_PRINCIPAL,
            "SourceArn": rule_arn,
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "setup: expected HTTP 201 from AddPermission on {function:?}, got {status} with {granted}"
    );
}

/// The object `key` holds right now: its bytes, or `None` where the bucket holds no such
/// object.
async fn object(router: &MultiServiceRouter, key: &str) -> Option<Vec<u8>> {
    let (status, body) = s3(router, "GET", &format!("/{BUCKET}/{key}")).await;
    (status == 200).then_some(body)
}

/// The document the handler wrote under `key`, looked for again until it is there.
async fn await_object(router: &MultiServiceRouter, case: &str, key: &str) -> Value {
    let deadline = Instant::now() + BUDGET;
    loop {
        if let Some(bytes) = object(router, key).await {
            let text = String::from_utf8_lossy(&bytes).into_owned();
            return serde_json::from_str(&text).unwrap_or_else(|error| {
                panic!("case {case:?}: the handler writes a JSON document, got {text:?} ({error})")
            });
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the rule's Lambda target never wrote the marker object {key:?}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// The executions Step Functions reports for `machine_arn` right now.
async fn executions(router: &MultiServiceRouter, case: &str, machine_arn: &str) -> Vec<Value> {
    let listed = sfn(
        router,
        case,
        "ListExecutions",
        &json!({ "stateMachineArn": machine_arn }),
    )
    .await;
    listed["executions"].as_array().cloned().unwrap_or_default()
}

/// The names of the executions of `machine_arn` that are in `wanted`, looked for again until
/// every one of them is there.
async fn await_executions_named(
    router: &MultiServiceRouter,
    case: &str,
    machine_arn: &str,
    wanted: &BTreeSet<String>,
) -> Vec<Value> {
    let deadline = Instant::now() + BUDGET;
    loop {
        let held = executions(router, case, machine_arn).await;
        let mine: Vec<Value> = held
            .iter()
            .filter(|execution| {
                execution["name"]
                    .as_str()
                    .is_some_and(|name| wanted.contains(name))
            })
            .cloned()
            .collect();
        if mine.len() >= wanted.len() {
            return mine;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {} execution(s) of {machine_arn} named among {wanted:?}, \
             got {held:?}",
            wanted.len()
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// Wait until every execution in `held` has left `RUNNING`, and answer what each was last
/// described as, keyed by its name.
async fn described(
    router: &MultiServiceRouter,
    case: &str,
    held: &[Value],
) -> Vec<(String, Value)> {
    let deadline = Instant::now() + BUDGET;
    loop {
        let mut settled = Vec::new();
        for execution in held {
            let arn = execution["executionArn"].as_str().unwrap_or_default();
            let read = sfn(
                router,
                case,
                "DescribeExecution",
                &json!({ "executionArn": arn }),
            )
            .await;
            settled.push((
                read["name"].as_str().unwrap_or_default().to_string(),
                read,
            ));
        }
        if settled
            .iter()
            .all(|(_name, read)| read["status"].as_str() != Some(RUNNING))
        {
            return settled;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: executions never left RUNNING, last read {settled:?}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// RULES 1 and 2 together: one publication reaches every target of its rule on its own, and
/// the function leg's work is done by clients the runtime's own SDK built for two other
/// services.
#[tokio::test]
async fn a_fan_outs_function_leg_does_its_work_with_the_runtimes_own_sdk() {
    let router = emulator();
    let rows = rows();

    // The bucket, the machine roles, the machine the HANDLER starts, the function role that
    // admits exactly the two calls the handler makes, and both functions.
    given_bucket(&router).await;
    let machine_role = given_role(
        &router,
        MACHINE_ROLE,
        STATES_PRINCIPAL,
        json!([{ "Effect": "Allow", "Action": START_EXECUTION_ACTION, "Resource": "*" }]),
    )
    .await;
    let downstream_arn = given_machine(&router, DOWNSTREAM_MACHINE, &machine_role).await;
    let function_role = given_role(
        &router,
        FUNCTION_ROLE,
        LAMBDA_PRINCIPAL,
        json!([
            {
                "Effect": "Allow",
                "Action": "s3:PutObject",
                "Resource": format!("arn:aws:s3:::{BUCKET}/*"),
            },
            {
                "Effect": "Allow",
                "Action": START_EXECUTION_ACTION,
                "Resource": downstream_arn,
            },
        ]),
    )
    .await;
    let function_arns = given_functions(
        &router,
        &function_role,
        &downstream_arn,
        &[GRANTED_FUNCTION, BARRED_FUNCTION],
    )
    .await;
    let granted_arn = function_arns[0].clone();
    let barred_arn = function_arns[1].clone();

    // One state machine per row for the rule's OTHER target, and the role those handovers are
    // started under.
    let mut machine_arns = Vec::new();
    for row in &rows {
        machine_arns.push(given_machine(&router, row.machine, &machine_role).await);
    }
    let target_role = given_role(
        &router,
        TARGET_ROLE,
        EVENTS_PRINCIPAL,
        json!([{
            "Effect": "Allow",
            "Action": START_EXECUTION_ACTION,
            "Resource": machine_arns.clone(),
        }]),
    )
    .await;

    // One rule per row, carrying both targets in the order the row names, and the statement on
    // the function the row's own policy calls for.
    for (row, machine_arn) in rows.iter().zip(machine_arns.iter()) {
        let created = events(
            &router,
            row.case,
            "PutRule",
            &json!({
                "Name": row.rule,
                "EventPattern": json!({
                    "source": [SOURCE],
                    "detail-type": [row.detail_type],
                })
                .to_string(),
                "State": "ENABLED",
            }),
        )
        .await;
        let rule_arn = created["RuleArn"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {:?}: PutRule reports no RuleArn, got {created}", row.case)
            })
            .to_string();
        let function_target = json!({
            "Id": "function-leg",
            "Arn": if row.granted { &granted_arn } else { &barred_arn },
        });
        let machine_target = json!({
            "Id": "machine-leg",
            "Arn": machine_arn,
            "RoleArn": target_role,
        });
        let targets = if row.function_first {
            vec![function_target, machine_target]
        } else {
            vec![machine_target, function_target]
        };
        let wired = events(
            &router,
            row.case,
            "PutTargets",
            &json!({ "Rule": row.rule, "Targets": targets }),
        )
        .await;
        assert_eq!(
            wired["FailedEntryCount"].as_i64(),
            Some(0),
            "case {:?}: both targets of the rule must be accepted, got {wired}",
            row.case
        );
        if row.granted {
            allow_the_rule_to_invoke(&router, row.function, row.rule, &rule_arn).await;
        }
    }

    // Every marker a granted row claims a delivery for, and every one a barred row must never
    // see delivered.
    let mut delivered: Vec<(&'static str, String)> = Vec::new();
    let mut undelivered: Vec<(&'static str, String)> = Vec::new();

    for (row, machine_arn) in rows.iter().zip(machine_arns.iter()) {
        let case = row.case;
        // One matching entry per marker, and — in every row — one entry whose source no rule
        // here names, so the quiet half is read off the SAME publication.
        let mut entries: Vec<Value> = row
            .markers
            .iter()
            .map(|marker| {
                json!({
                    "Source": SOURCE,
                    "DetailType": row.detail_type,
                    "Detail": json!({ MARKER: marker }).to_string(),
                })
            })
            .collect();
        let unmatched = format!("unmatched-{}", row.rule);
        entries.push(json!({
            "Source": OTHER_SOURCE,
            "DetailType": row.detail_type,
            "Detail": json!({ MARKER: unmatched }).to_string(),
        }));
        undelivered.push((case, unmatched));

        let answered = events(&router, case, "PutEvents", &json!({ "Entries": entries })).await;
        assert_eq!(
            answered["FailedEntryCount"].as_i64(),
            Some(0),
            "case {case:?}: every submitted entry must be accepted, got {answered}"
        );
        let results = answered["Entries"].as_array().cloned().unwrap_or_default();
        assert_eq!(
            results.len(),
            row.markers.len() + 1,
            "case {case:?}: expected one result entry per submitted entry, got {answered}"
        );
        let event_ids: Vec<String> = results
            .iter()
            .map(|result| {
                result["EventId"]
                    .as_str()
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: an accepted entry carries an EventId, got {answered}")
                    })
                    .to_string()
            })
            .collect();

        // The STATE MACHINE leg: it is started once per matching entry whatever the function
        // leg's own policy says, and each start carries that entry's own envelope.
        let wanted_inputs: BTreeSet<String> =
            row.markers.iter().map(|marker| (*marker).to_string()).collect();
        let all = await_execution_count(&router, case, machine_arn, row.markers.len()).await;
        assert_eq!(
            all.len(),
            row.markers.len(),
            "case {case:?}: expected exactly one state-machine handover per matching entry, got \
             {all:?}"
        );
        let settled = described(&router, case, &all).await;
        let carried: BTreeSet<String> = settled
            .iter()
            .map(|(_name, read)| {
                let input: Value =
                    serde_json::from_str(read["input"].as_str().unwrap_or("null")).unwrap_or(Value::Null);
                input["detail"][MARKER]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            })
            .collect();
        assert_eq!(
            carried, wanted_inputs,
            "case {case:?}: expected the state-machine leg to be started once per matching \
             entry, each carrying its own marker, got {settled:?}"
        );

        // The FUNCTION leg: granted, every matching entry's handler run wrote its own object
        // with the envelope it was handed, and started its own downstream execution with the
        // workflow client its runtime supplied.
        if row.granted {
            for (index, marker) in row.markers.iter().enumerate() {
                let key = format!("{PREFIX}{marker}.json");
                let held = await_object(&router, case, &key).await;
                assert_eq!(
                    held[MARKER].as_str(),
                    Some(*marker),
                    "case {case:?}: expected the object to carry the entry's own marker, got \
                     {held}"
                );
                assert_eq!(
                    held["source"].as_str(),
                    Some(SOURCE),
                    "case {case:?}: expected the envelope's published source, got {held}"
                );
                assert_eq!(
                    held["detail-type"].as_str(),
                    Some(row.detail_type),
                    "case {case:?}: expected the envelope's published detail-type, got {held}"
                );
                assert_eq!(
                    held["id"].as_str(),
                    Some(event_ids[index].as_str()),
                    "case {case:?}: expected the envelope's id to be the EventId the \
                     publication answered for this entry, got {held}"
                );
                delivered.push((case, (*marker).to_string()));
            }
            let names: BTreeSet<String> =
                row.markers.iter().map(|marker| (*marker).to_string()).collect();
            let downstream = await_executions_named(&router, case, &downstream_arn, &names).await;
            assert_eq!(
                downstream.len(),
                names.len(),
                "case {case:?}: expected the handler's own workflow client to have started one \
                 execution per delivery, got {downstream:?}"
            );
        } else {
            // Barred: the sibling leg has already run, so the absence below is the function
            // leg's own and not a delivery still in flight.
            for marker in row.markers {
                let key = format!("{PREFIX}{marker}.json");
                assert!(
                    object(&router, &key).await.is_none(),
                    "case {case:?}: a function whose policy admits nothing of this service must \
                     not be invoked, yet {key:?} was written"
                );
                undelivered.push((case, (*marker).to_string()));
            }
        }
    }

    // Nothing a row did not ask for ran: the machine the handler starts holds exactly one
    // execution per delivered marker, every one of them settled, and no object exists for a
    // marker no delivery was owed for — read once more now that every later publication has
    // landed.
    let whole = "the whole fan-out";
    let downstream_held = executions(&router, whole, &downstream_arn).await;
    assert_eq!(
        downstream_held.len(),
        delivered.len(),
        "case {whole:?}: expected one downstream execution per delivery, got {downstream_held:?}"
    );
    for (name, read) in described(&router, whole, &downstream_held).await {
        assert_eq!(
            read["status"].as_str(),
            Some(SUCCEEDED),
            "case {whole:?}: expected the handler-started execution {name:?} to have succeeded, \
             got {read}"
        );
    }
    for (case, marker) in &undelivered {
        let key = format!("{PREFIX}{marker}.json");
        assert!(
            object(&router, &key).await.is_none(),
            "case {case:?}: no handover was owed for {marker:?}, yet {key:?} holds an object"
        );
    }
    for (case, marker) in &delivered {
        let key = format!("{PREFIX}{marker}.json");
        assert!(
            object(&router, &key).await.is_some(),
            "case {case:?}: expected {key:?} to still hold the object its delivery wrote"
        );
    }
}

/// Wait until `machine_arn` holds at least `wanted` executions, and answer them.
async fn await_execution_count(
    router: &MultiServiceRouter,
    case: &str,
    machine_arn: &str,
    wanted: usize,
) -> Vec<Value> {
    let deadline = Instant::now() + BUDGET;
    loop {
        let held = executions(router, case, machine_arn).await;
        if held.len() >= wanted {
            return held;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {wanted} execution(s) of {machine_arn}, got {held:?}"
        );
        tokio::time::sleep(LOOK).await;
    }
}
