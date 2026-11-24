// SPDX-License-Identifier: CC0-1.0

//! How often an EventBridge rule's handover RUNS the handler of the function it was handed to,
//! end to end over the assembled router with CloudFormation, IAM, the token service,
//! EventBridge, Lambda and Amazon SQS on one engine.
//!
//! RULE: **a rule hands a Lambda target its event ASYNCHRONOUSLY, so how often the handler runs
//! for ONE published event is decided by the asynchronous-invocation policy in force on the
//! function the event was handed to: a handler that RETURNS is run once, and one that RAISES is
//! run again for each retry that policy allows — "the maximum number of times to retry when the
//! function returns an error" (`MaximumRetryAttempts`), and two retries where the function
//! configures none ("if you invoke a function asynchronously and it returns an error, Lambda
//! executes the function up to two more times", `Invoke`).**
//!
//! "Event – Invoke the function asynchronously" (`Invoke$InvocationType`) is the invocation a
//! rule makes of a function target, so the handover ends at Lambda's own queue and the attempts
//! of that event are Lambda's to make afterwards, with no caller asking for them.
//!
//! What follows for anything OBSERVING a handover is that a run of the handler is not an
//! arrival to be counted once: a probe a failing handler writes before it raises is written
//! once per attempt, so a reading that expects exactly one is decided by whether the host got
//! through the ladder before the reader looked rather than by the service.
//! [`crate::cfn_relay_execution_role_decides_its_queue_delivery`] reads an under-permissioned
//! relay's refusal over exactly that ladder; this suite is what pins the ladder's LENGTH, over
//! the policies a function can carry.
//!
//! Five crates have to agree for one row to hold — CloudFormation deploys the queue, the role,
//! the functions, the rules and the invoke statements together, IAM holds the role and its
//! document, the token service vends each function's session, EventBridge matches a publication
//! and hands it over, and Lambda runs the handler and spends the budget — so the suite lives
//! here. The count itself is a client's own observation: the handler writes a witness message
//! through the SDK its runtime supplies BEFORE it decides whether to raise, so the witness
//! queue holds one message per RUN and `ReceiveMessage` is what counts them.
//!
//! The rows differ along what the rule generalises over: whether the handler returns or raises,
//! and the retry budget in force on the function — none stated at all (the documented default),
//! the floor of `MaximumRetryAttempts`, its ceiling, and the value between them.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::MAX_ASYNC_DELIVERY_ATTEMPTS;
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{elements, form_encode};

/// The Region every request here is signed for, and so the Region every queue, role, function
/// and rule below belongs to.
const REGION: &str = "us-east-1";

/// The account this emulator serves, which the witness queue's ARN carries.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The stack the whole topology is deployed by, and the status it must reach.
const STACK: &str = "handover-retry-budget";
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The one queue every handler writes, whose messages are the RUNS this suite counts, and the
/// logical id it is declared under — which the handlers' `WITNESS_URL` is `Ref`erenced from.
const WITNESS_QUEUE: &str = "handover-retry-witness";
const WITNESS_LOGICAL: &str = "WitnessQueue";

/// The execution role every function is deployed as, and the logical id it is declared under.
const ROLE: &str = "handover-retry-execution";
const ROLE_LOGICAL: &str = "ExecutionRole";

/// The runtime and handler every declared function carries.
const RUNTIME: &str = "python3.12";
const HANDLER: &str = "index.handler";

/// How long one invocation is given, as the function's own `Timeout`: well past what any run
/// needs on any host, because what is under test is how MANY runs a handover earns and never
/// how fast an interpreter gets through one.
const FUNCTION_TIMEOUT: u64 = 120;

/// The state an invocation needs: "during this time, you can't invoke or modify the function"
/// (`CreateFunction`).
const FUNCTION_ACTIVE: &str = "Active";

/// How often to look again for work the clock owes — the stack settling, a function reaching
/// `Active`, a queue being handed a message — and how many looks to take. The spans are the
/// owning services' own, so no fixed sleep names the instant one ends, and the budget is
/// generous because every case asserts the CONDITION and never the time it took.
const LOOK: Duration = Duration::from_millis(100);
const LOOKS: usize = 1_800;

/// How many looks an observation that must find NOTHING MORE takes before concluding the
/// ladders are spent. Nothing rests on this window alone: the runs counted before it are what
/// show every handover was being delivered while it was open.
const QUIET_LOOKS: usize = 30;

/// The envelope members every publication carries, and the member of the detail that tells one
/// row's entry from its siblings' — which is what each row's rule matches on.
const SOURCE: &str = "com.example.handover.probe";
const DETAIL_TYPE: &str = "Handover Probe";
const PROBE_MEMBER: &str = "probe";

/// The service principal EventBridge hands a function target over as, and the action the
/// handover performs: "for Lambda functions … EventBridge relies on resource-based policies"
/// (`PutTargets`).
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// The IAM action the write every handler makes performs — Amazon SQS publishes one
/// `sqs:<OperationName>` action per operation — which the one execution role is granted on the
/// witness queue alone.
const SEND_ACTION: &str = "sqs:SendMessage";

/// Where `PutFunctionEventInvokeConfig` is bound: `PUT
/// /2019-09-25/functions/{FunctionName}/event-invoke-config`
/// (`smithy_query(query="operation", operation="PutFunctionEventInvokeConfig")`).
const EVENT_INVOKE_CONFIG_URI: &str = "/2019-09-25/functions";

/// The retries `MaximumRetryAttempts` admits, floor and ceiling: `@range(min: 0, max: 2)`
/// (`smithy_query(query="shape", shape="MaximumRetryAttempts")`). The rows state both and the
/// value between them, beside the function that states none at all.
const RETRY_BUDGET_FLOOR: i64 = 0;
const RETRY_BUDGET_CEILING: i64 = 2;

/// The handler every function runs, archived into the deployment package by CloudFormation.
///
/// The witness write comes FIRST and the decision to raise after it, so a run is counted
/// whichever way it ends: the queue holds one message per run of this handler, carrying the
/// event that run was handed.
const PROBE_HANDLER: &str = r#"
import json
import os

import boto3

CLIENT = boto3.client("sqs")
WITNESS_URL = os.environ["WITNESS_URL"]
PROBE = os.environ["PROBE"]
RAISES = os.environ["RAISES"]


def handler(event, context):
    CLIENT.send_message(
        QueueUrl=WITNESS_URL,
        MessageBody=json.dumps({"probe": PROBE, "event": event}, sort_keys=True),
    )
    if RAISES == "yes":
        raise RuntimeError("the handler of " + PROBE + " raises on every run")
    return {"ran": PROBE}
"#;

/// One row: the function the handover is made to, whether its handler raises after it has
/// written the witness, and the retry budget in force on it.
#[derive(Debug, Clone, Copy)]
struct Row {
    case: &'static str,
    /// The function's name, the value its `PROBE` carries and the detail member its rule
    /// matches — one string, so a run is attributable to the row that earned it.
    probe: &'static str,
    /// The suffix of the logical ids this row's resources are declared under.
    logical: &'static str,
    /// Whether this row's handler raises once it has written the witness.
    raises: bool,
    /// The `MaximumRetryAttempts` this row puts on its function, or `None` where it states
    /// none and the documented default is in force.
    budget: Option<i64>,
}

impl Row {
    /// How many RUNS of this row's handler ONE handover earns it, and so how many witness
    /// messages that handover leaves.
    ///
    /// A handler that returned is done with its event. One that raised is handed it again for
    /// each retry the policy in force allows — the budget this row states, or
    /// [`MAX_ASYNC_DELIVERY_ATTEMPTS`] attempts in all where it states none, read off the
    /// owning crate rather than written here so the two cannot drift.
    fn runs(&self) -> usize {
        if !self.raises {
            return 1;
        }
        match self.budget {
            Some(retries) => 1 + usize::try_from(retries).unwrap_or_default(),
            None => usize::try_from(MAX_ASYNC_DELIVERY_ATTEMPTS).unwrap_or(1),
        }
    }
}

const ROWS: &[Row] = &[
    Row {
        case: "a handler that returns, with no budget configured at all",
        probe: "handover-returns",
        logical: "Returns",
        raises: false,
        budget: None,
    },
    Row {
        case: "a handler that raises, with no budget configured at all",
        probe: "handover-default-budget",
        logical: "DefaultBudget",
        raises: true,
        budget: None,
    },
    Row {
        case: "a handler that raises, at the floor of MaximumRetryAttempts",
        probe: "handover-no-retry",
        logical: "NoRetry",
        raises: true,
        budget: Some(RETRY_BUDGET_FLOOR),
    },
    Row {
        case: "a handler that raises, with one retry allowed",
        probe: "handover-one-retry",
        logical: "OneRetry",
        raises: true,
        budget: Some(1),
    },
    Row {
        case: "a handler that raises, at the ceiling of MaximumRetryAttempts",
        probe: "handover-two-retries",
        logical: "TwoRetries",
        raises: true,
        budget: Some(RETRY_BUDGET_CEILING),
    },
];

/// The emulator as it is ASSEMBLED, on one engine: a handler's own SDK call is dispatched back
/// into this same router, so the queue it writes is the queue this suite reads.
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
/// are the account's own and no function's role's.
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

/// One Lambda `restJson1` call, as its status and its parsed body.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let payload = body.map_or_else(Bytes::new, |value| Bytes::from(value.to_string()));
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"))
        .body(payload)
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body())
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(response.body()).to_string()))
    };
    (status, parsed)
}

/// One Amazon SQS operation over its `awsJson1_0` wire, made as the ACCOUNT's own credential —
/// the queue's owner, whose reads no function's grant decides.
async fn sqs(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("sqs.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("authorization", authorization("sqs"))
        .header("x-amz-target", format!("AmazonSQS.{operation}"))
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

/// One EventBridge `awsJson1_1` call (`AWSEvents.<Operation>`), as its status and parsed body.
async fn events(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("events.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("authorization", authorization("events"))
        .header("x-amz-target", format!("AWSEvents.{operation}"))
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

/// The ARN the witness queue carries, which is what the execution role's one statement names.
fn witness_arn() -> String {
    format!("arn:aws:sqs:{REGION}:{ACCOUNT}:{WITNESS_QUEUE}")
}

/// The trust policy the execution role needs: Lambda has to be able to assume it for a function
/// to be handed it at all.
fn trust_policy() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": "lambda.amazonaws.com"},
            "Action": "sts:AssumeRole",
        }],
    })
}

/// The inline document the execution role carries: the log writes every execution role makes,
/// and the witness write every handler is granted. No row's grant is under test here, so one
/// role serves them all.
fn role_document() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [
            {
                "Effect": "Allow",
                "Action": ["logs:CreateLogGroup", "logs:CreateLogStream", "logs:PutLogEvents"],
                "Resource": "*",
            },
            {
                "Effect": "Allow",
                "Action": SEND_ACTION,
                "Resource": witness_arn(),
            },
        ],
    })
}

/// The pattern one row's rule carries: the envelope members every publication names, and the
/// probe that tells this row's entry from its siblings'.
fn pattern(row: &Row) -> Value {
    json!({
        "source": [SOURCE],
        "detail-type": [DETAIL_TYPE],
        "detail": {PROBE_MEMBER: [row.probe]},
    })
}

/// The `detail` one row's entry publishes.
fn detail(row: &Row) -> Value {
    json!({PROBE_MEMBER: row.probe})
}

/// The template the suite deploys: the witness queue, the execution role, and per row a function
/// running that row's handler, a rule matching that row's probe and the statement admitting
/// EventBridge's handover of it.
fn stack_template() -> String {
    let mut resources = serde_json::Map::new();
    resources.insert(
        WITNESS_LOGICAL.to_string(),
        json!({
            "Type": "AWS::SQS::Queue",
            "Properties": {"QueueName": WITNESS_QUEUE},
        }),
    );
    resources.insert(
        ROLE_LOGICAL.to_string(),
        json!({
            "Type": "AWS::IAM::Role",
            "Properties": {
                "RoleName": ROLE,
                "AssumeRolePolicyDocument": trust_policy(),
                "Policies": [{
                    "PolicyName": "probe-grants",
                    "PolicyDocument": role_document(),
                }],
            },
        }),
    );
    for row in ROWS {
        let function_logical = format!("Probe{}", row.logical);
        let rule_logical = format!("Rule{}", row.logical);
        // What the handler reads to decide whether to raise once it has written the witness.
        let raises = if row.raises { "yes" } else { "no" };
        resources.insert(
            function_logical.clone(),
            json!({
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": row.probe,
                    "Runtime": RUNTIME,
                    "Handler": HANDLER,
                    "Role": {"Fn::GetAtt": [ROLE_LOGICAL, "Arn"]},
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": {"ZipFile": PROBE_HANDLER},
                    "Environment": {"Variables": {
                        "WITNESS_URL": {"Ref": WITNESS_LOGICAL},
                        "PROBE": row.probe,
                        "RAISES": raises,
                    }},
                },
            }),
        );
        resources.insert(
            rule_logical.clone(),
            json!({
                "Type": "AWS::Events::Rule",
                "Properties": {
                    "Name": format!("{STACK}-{}", row.logical),
                    "EventPattern": pattern(row),
                    "State": "ENABLED",
                    "Targets": [{
                        "Id": "probe",
                        "Arn": {"Fn::GetAtt": [function_logical.clone(), "Arn"]},
                    }],
                },
            }),
        );
        resources.insert(
            format!("Invoke{}", row.logical),
            json!({
                "Type": "AWS::Lambda::Permission",
                "Properties": {
                    "FunctionName": {"Fn::GetAtt": [function_logical, "Arn"]},
                    "Action": INVOKE_ACTION,
                    "Principal": EVENTS_PRINCIPAL,
                    "SourceArn": {"Fn::GetAtt": [rule_logical, "Arn"]},
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

/// Deploy the stack and wait for `CREATE_COMPLETE`, which is owed from the instant the owning
/// services report its resources ready — an instant no fixed sleep can name, so this looks until
/// the stack reports it.
async fn deploy(router: &MultiServiceRouter) {
    let template = stack_template();
    let (status, answered) = query(
        router,
        "cloudformation",
        "2010-05-15",
        "CreateStack",
        &[
            ("StackName", STACK),
            ("TemplateBody", &template),
            // The template declares a named IAM role, which a create acknowledges before
            // CloudFormation provisions it.
            ("Capabilities.member.1", "CAPABILITY_NAMED_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateStack({STACK:?}) must be accepted, got {status} with {answered}"
    );
    let reported = wait_for_stack_status(router, STACK, CREATE_COMPLETE).await;
    let failures = if reported.0 == CREATE_COMPLETE {
        Vec::new()
    } else {
        let (_, happened) = query(
            router,
            "cloudformation",
            "2010-05-15",
            "DescribeStackEvents",
            &[("StackName", STACK)],
        )
        .await;
        elements(&happened, "ResourceStatusReason")
    };
    assert_eq!(
        reported.0, CREATE_COMPLETE,
        "setup: the stack must reach {CREATE_COMPLETE}, got {} with resource failures {failures:?}",
        reported.1
    );
}

/// The URL the witness queue is reached at, read back from the service that holds it, with its
/// ARN asserted to be the one the role's statement names.
async fn witness_url(router: &MultiServiceRouter) -> String {
    let (status, answered) = sqs(router, "GetQueueUrl", &json!({"QueueName": WITNESS_QUEUE})).await;
    assert_eq!(
        status, 200,
        "setup: GetQueueUrl({WITNESS_QUEUE:?}) must succeed, got {status} with {answered}"
    );
    let url = answered["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: the queue must report a URL, got {answered}"))
        .to_string();
    let (status, attributes) = sqs(
        router,
        "GetQueueAttributes",
        &json!({"QueueUrl": url, "AttributeNames": ["QueueArn"]}),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: GetQueueAttributes({WITNESS_QUEUE:?}) must succeed, got {status} with {attributes}"
    );
    assert_eq!(
        attributes["Attributes"]["QueueArn"].as_str(),
        Some(witness_arn().as_str()),
        "setup: the queue's ARN is what the role's statement names, got {attributes}"
    );
    url
}

/// Wait until Lambda first reports `name` `Active` — "during this time, you can't invoke or
/// modify the function" (`CreateFunction`) — which is the state a handover needs.
async fn wait_until_active(router: &MultiServiceRouter, name: &str) {
    let mut last = Value::Null;
    for _ in 0..LOOKS {
        let (_, configuration) = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{name}/configuration"),
            None,
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

/// Put the row's retry budget on its function, and read the answer back: the configuration the
/// policy is then applied from is the one the service reports holding.
async fn configure_budget(router: &MultiServiceRouter, row: &Row, retries: i64) {
    let case = row.case;
    let (status, reported) = lambda(
        router,
        "PUT",
        &format!("{EVENT_INVOKE_CONFIG_URI}/{}/event-invoke-config", row.probe),
        Some(&json!({"MaximumRetryAttempts": retries})),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutFunctionEventInvokeConfig must be served, got {status} with {reported}"
    );
    assert_eq!(
        reported["MaximumRetryAttempts"],
        json!(retries),
        "case {case:?}: expected the configuration to hold the budget it was put, got {reported}"
    );
}

/// Take everything the queue at `url` hands over, deleting each message as it is read so a later
/// look answers what arrived after this one — which is what lets a run be counted ONCE — and
/// answer the bodies parsed.
///
/// `wanted` is how many the caller expects: the loop looks again until it has them, so a host
/// slow to run a handler cannot decide the outcome. A caller expecting none keeps looking for
/// the short quiet window instead, so a run that did happen is seen.
async fn drained(router: &MultiServiceRouter, case: &str, url: &str, wanted: usize) -> Vec<Value> {
    let mut held: Vec<Value> = Vec::new();
    let looks = if wanted == 0 { QUIET_LOOKS } else { LOOKS };
    for look in 0..looks {
        let (status, answered) = sqs(
            router,
            "ReceiveMessage",
            &json!({"QueueUrl": url, "MaxNumberOfMessages": 10}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: draining {url} must succeed, got {status} with {answered}"
        );
        let handed = answered["Messages"].as_array().cloned().unwrap_or_default();
        for message in &handed {
            let body = message["Body"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: a message with no body {message}"));
            held.push(
                serde_json::from_str(body).unwrap_or_else(|_| Value::String(body.to_string())),
            );
            let (status, deleted) = sqs(
                router,
                "DeleteMessage",
                &json!({
                    "QueueUrl": url,
                    "ReceiptHandle": message["ReceiptHandle"].as_str().unwrap_or_default(),
                }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: deleting a drained message must succeed, got {status} with \
                 {deleted}"
            );
        }
        if held.len() >= wanted.max(1) || look + 1 == looks {
            break;
        }
        tokio::time::sleep(LOOK).await;
    }
    held
}

/// Every row: one publication earns each function as many runs of its handler as the policy in
/// force on it allows, each run handed the very event that was published.
#[tokio::test]
async fn a_handover_runs_the_handler_once_per_attempt_its_policy_allows() {
    let router = build_router();
    deploy(&router).await;
    let witness = witness_url(&router).await;
    for row in ROWS {
        wait_until_active(&router, row.probe).await;
        if let Some(retries) = row.budget {
            configure_budget(&router, row, retries).await;
        }
    }

    // One publication, carrying an entry per row, each matched by that row's own rule and handed
    // over to that row's function.
    let entries: Vec<Value> = ROWS
        .iter()
        .map(|row| {
            json!({
                "Source": SOURCE,
                "DetailType": DETAIL_TYPE,
                "Detail": detail(row).to_string(),
            })
        })
        .collect();
    let (status, published) = events(&router, "PutEvents", &json!({"Entries": entries})).await;
    assert_eq!(
        status, 200,
        "publication: PutEvents must be served, got {status} with {published}"
    );
    assert_eq!(
        published["FailedEntryCount"],
        json!(0),
        "publication: an accepted publication reports no failed entry, got {published}"
    );

    // Every run of every handler leaves one witness message, so the whole ladder of every row is
    // waited for and then read: a count, never an arrival.
    let ladders: usize = ROWS.iter().map(Row::runs).sum();
    let witnessed = drained(&router, "every row", &witness, ladders).await;
    let mut runs_of: BTreeMap<&str, Vec<&Value>> = BTreeMap::new();
    for written in &witnessed {
        let probe = written[PROBE_MEMBER].as_str().unwrap_or_default();
        runs_of.entry(probe).or_default().push(written);
    }
    assert_eq!(
        witnessed.len(),
        ladders,
        "every row: expected {ladders} run(s) of a handler in all, got {witnessed:?}"
    );
    for row in ROWS {
        let case = row.case;
        let runs = runs_of.get(row.probe).cloned().unwrap_or_default();
        assert_eq!(
            runs.len(),
            row.runs(),
            "case {case:?}: expected the handover to run this handler {} time(s) under the \
             budget {:?}, got {runs:?}",
            row.runs(),
            row.budget
        );
        for run in &runs {
            let envelope = &run["event"];
            assert_eq!(
                envelope["source"],
                json!(SOURCE),
                "case {case:?}: expected every run to be handed the published source, got \
                 {envelope}"
            );
            assert_eq!(
                envelope["detail-type"],
                json!(DETAIL_TYPE),
                "case {case:?}: expected every run to be handed the published detail-type, got \
                 {envelope}"
            );
            assert_eq!(
                envelope["detail"],
                detail(row),
                "case {case:?}: expected every run to be handed the published detail unchanged, \
                 got {envelope}"
            );
        }
    }

    // And the ladders are SPENT: every attempt the policies allow is accounted for above, so the
    // quiet window that follows them finds no further run of any handler.
    let beyond = drained(&router, "every row", &witness, 0).await;
    assert!(
        beyond.is_empty(),
        "every row: expected no run past the attempts each policy allows, got {beyond:?}"
    );
}
