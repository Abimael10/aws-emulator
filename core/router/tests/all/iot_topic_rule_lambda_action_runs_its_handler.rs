// SPDX-License-Identifier: CC0-1.0

//! An API-created AWS IoT topic rule whose `lambda` action's function REALLY RUNS, followed over
//! the real `MultiServiceRouter` all the way to the effect the handler itself has: the whole
//! emulator on one engine, a message published to the IoT data plane, the function the matched
//! rule invoked, and the queue that function's own handler forwarded the event it was handed to.
//!
//! RULE: a message published to a topic an ENABLED topic rule's `FROM`-clause filter covers is
//! projected by that rule's `sql` and each of its `actions` is performed for it — for an
//! `Action$lambda` ("invoke a Lambda function"), one invocation of the function its `functionArn`
//! names, with the projected document as the invocation event, made as the IoT service principal
//! and therefore admitted only by that FUNCTION's own resource-based policy for
//! `lambda:InvokeFunction` narrowed by the invoking RULE's ARN as `aws:SourceArn`. The invocation
//! runs the function's deployment package, so whatever the handler then does to another service
//! is what a client observes; and where no such grant is read — a rule that is disabled, a rule
//! that no longer exists, a statement whose `WHERE` the message fails, a grant narrowed to
//! another rule, a grant to another service principal, no grant at all, or an action naming a
//! function that does not exist — nothing runs, so that effect never appears. The publisher is
//! answered 200 either way, which is why every assertion below is made where the HANDLER wrote.
//!
//! Four crates have to agree for that to hold — the IoT data plane takes the message, the IoT
//! control plane's rules engine projects it and dispatches the action, Lambda decides the
//! invocation against the function's resource policy and runs the package, Amazon SQS holds what
//! the running handler sent — so the suite lives here: a crate-local suite can only ever see one
//! side of it.
//!
//! What this suite is about that its neighbours are not: the rule, the grant and the enablement
//! are all made through the SERVICE APIs (`CreateTopicRule`, `AddPermission`, `DeleteTopicRule`),
//! and every row is judged by an effect only a RUNNING handler can produce. The rules engine's
//! dispatch on its own — the record Lambda keeps of the events it accepted — is pinned by
//! `iot_topic_rule_lambda_dispatch.rs`, which waits on no handler; the same leg DECLARED IN A
//! TEMPLATE, with the permission as an `AWS::Lambda::Permission`, by
//! `cfn_deployed_iot_topic_rule_invokes_its_lambda.rs`. Neither pins that an API-created rule's
//! action carries the selected document into a real run, nor that each negative is observable as
//! the ABSENCE of that run's effect rather than merely of an invocation record.
//!
//! A template appears here for one reason only: it is what archives inline handler source into a
//! deployment package this emulator can run, which is how a function whose handler is written out
//! in a suite comes to exist at all. Nothing under test is declared in it.
//!
//! The rows differ along what the rule generalises over: whether the rule is ENABLED, still
//! EXISTS, and names a function that exists; what its statement SELECTS (the whole message, and
//! two named members, one of them renamed) and whether its `WHERE` holds for the published
//! message; and what the target function's policy says about the invoking rule (a grant for it, a
//! grant for another rule's ARN, a grant to another service principal, no grant at all). Each
//! row's message carries its own marker, so no row can pass by reading another's.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{form_encode, opt_element, outputs_of};

/// The Region every request below is signed for, and so the Region every resource it makes
/// belongs to.
const REGION: &str = "us-east-1";

/// The account this emulator serves, which every ARN below carries.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The stack that archives the handler into a deployment package, and the status it has to reach.
const STACK: &str = "iot-rule-lambda-e2e";
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The function every row's rule names, the queue its handler forwards to, and the role it runs
/// as.
const FORWARDER: &str = "iot-rule-e2e-forwarder";
const QUEUE: &str = "iot-rule-e2e-sink";

/// A function name no function holds, which is what the wrong-ARN row's action names.
const ABSENT_FUNCTION: &str = "iot-rule-e2e-no-such-forwarder";

/// A rule the grant of the wrong-source row names instead of the rule that invokes.
const UNRELATED_RULE: &str = "iot_e2e_unrelated";

/// Where Lambda binds the collection the function is read and permissioned under.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The runtime the function carries, and how long one invocation is given: well past what a run
/// needs on any host, because what is under test is the rule's wiring and not how fast an
/// interpreter gets through the handler.
const RUNTIME: &str = "python3.12";
const FUNCTION_TIMEOUT: u64 = 120;

/// The `State` the function has to report before an invocation of it is accepted.
const ACTIVE: &str = "Active";

/// The IAM action an invocation of a function performs, the principal AWS IoT invokes a rule's
/// function as, and the principal Lambda runs a function as.
const INVOKE_ACTION: &str = "lambda:InvokeFunction";
const IOT_PRINCIPAL: &str = "iot.amazonaws.com";
const LAMBDA_PRINCIPAL: &str = "lambda.amazonaws.com";

/// A service principal that is NOT the one AWS IoT invokes as, which is what the wrong-principal
/// row's grant names.
const OTHER_PRINCIPAL: &str = "sns.amazonaws.com";

/// How long an observation of work the CLOCK owes — the stack settling, the function going
/// `Active`, a forwarded event reaching the queue — waits before calling it a hang, and how long
/// between two looks. Generous: a row fails only when the work did not happen at all, so a loaded
/// host cannot decide the outcome.
const DEADLINE: Duration = Duration::from_secs(120);
/// See [`DEADLINE`].
const LOOK_AGAIN: Duration = Duration::from_millis(100);

/// The long poll one drain of the queue makes, in seconds, and how many consecutive polls have to
/// answer nothing — AFTER every expected event has arrived — before the queue is read as quiet.
/// That is what makes "nothing was forwarded for this row" an observation rather than a race: the
/// sentinel's own event has already been forwarded by then, and the queue has been silent for
/// whole polls since.
const WAIT_TIME_SECONDS: u64 = 1;
const QUIET_POLLS: u32 = 2;

/// The marker the last message published carries: every negative row is judged only once this
/// one's event has been forwarded, so each negative has had at least a full publish-to-effect
/// round trip longer than the positive that proves the path works.
const SENTINEL: &str = "sentinel";

/// The handler the function runs: it forwards the event it was handed to the queue its
/// environment names, which is the only thing a client can see of an invocation the rules engine
/// made.
const FORWARDING_HANDLER: &str = r#"
import json
import os

import boto3

SQS = boto3.client("sqs")


def handler(event, context):
    SQS.send_message(
        QueueUrl=os.environ["QUEUE_URL"],
        MessageBody=json.dumps({"event": event}),
    )
    return {"ok": True}
"#;

/// The ARN of one function, which is what a rule's `lambda` action names.
fn function_arn(function: &str) -> String {
    format!("arn:aws:lambda:{REGION}:{ACCOUNT}:function:{function}")
}

/// The ARN of one topic rule, which is the `aws:SourceArn` a grant for its actions is narrowed
/// by.
fn rule_arn(rule: &str) -> String {
    format!("arn:aws:iot:{REGION}:{ACCOUNT}:rule/{rule}")
}

/// The template the stack is deployed with: the queue the handler forwards to, the execution role
/// that grants the forward, and the function whose inline source it archives into a deployment
/// package. Nothing under test is declared here.
fn template() -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Sink": {
                "Type": "AWS::SQS::Queue",
                "Properties": { "QueueName": QUEUE },
            },
            "Role": {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Principal": { "Service": LAMBDA_PRINCIPAL },
                            "Action": "sts:AssumeRole",
                        }],
                    },
                    // A function calls another service as its execution role, so the role GRANTS
                    // the forward its handler makes: without it the handler's own
                    // `sqs:SendMessage` is refused, which is a different rule than the invocation
                    // boundary these rows read.
                    "Policies": [{
                        "PolicyName": "forward-to-the-sink",
                        "PolicyDocument": {
                            "Version": "2012-10-17",
                            "Statement": [{
                                "Effect": "Allow",
                                "Action": "sqs:SendMessage",
                                "Resource": "*",
                            }],
                        },
                    }],
                },
            },
            "Forwarder": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": FORWARDER,
                    "Runtime": RUNTIME,
                    "Handler": "index.handler",
                    "Timeout": FUNCTION_TIMEOUT,
                    "Role": { "Fn::GetAtt": ["Role", "Arn"] },
                    "Environment": { "Variables": { "QUEUE_URL": { "Ref": "Sink" } } },
                    "Code": { "ZipFile": FORWARDING_HANDLER },
                },
            },
        },
        "Outputs": {
            "SinkUrl": { "Value": { "Ref": "Sink" } },
        },
    })
    .to_string()
}

/// What the target function's resource policy says about one row's rule, which is the whole of
/// the IoT -> Lambda authorization gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Grant {
    /// `AddPermission` for the IoT service principal, narrowed to THIS rule's ARN.
    ForThisRule,
    /// A grant for the IoT service principal narrowed to ANOTHER rule's ARN.
    ForAnotherRule,
    /// A grant for this rule's ARN to another service principal.
    ToAnotherPrincipal,
    /// No statement naming this rule at all.
    Withheld,
}

/// One row: the rule it stores, the message it publishes, and whether the handler has to have
/// run — and therefore forwarded — for it.
struct Row {
    case: &'static str,
    /// The rule's name. `RuleName`'s pattern is `^[a-zA-Z0-9_]+$`, so a rule name carries
    /// letters, digits and underscores alone.
    rule: &'static str,
    /// The topic the rule selects from and the message is published to.
    topic: &'static str,
    /// The rule's `sql`.
    sql: String,
    /// The `functionArn` the rule's single `lambda` action names.
    target: String,
    /// What the target function's policy says about this rule.
    grant: Grant,
    /// Whether the rule is created disabled.
    disabled: bool,
    /// Whether the rule is deleted again before anything is published.
    deleted: bool,
    /// The payload published to the topic, which carries this row's marker.
    payload: Value,
    /// The marker that payload carries, and whether the handler has to have forwarded an event
    /// carrying it.
    marker: &'static str,
    forwarded: bool,
}

impl Row {
    /// A row whose rule selects the whole message and invokes the forwarder.
    fn rule(
        case: &'static str,
        rule: &'static str,
        topic: &'static str,
        marker: &'static str,
    ) -> Self {
        Self {
            case,
            rule,
            topic,
            sql: format!("SELECT * FROM '{topic}'"),
            target: function_arn(FORWARDER),
            grant: Grant::ForThisRule,
            disabled: false,
            deleted: false,
            payload: json!({
                "marker": marker,
                "deviceId": "sensor-1",
                "temperature": READING,
                "unit": "C",
            }),
            marker,
            forwarded: false,
        }
    }

    fn forwards(mut self) -> Self {
        self.forwarded = true;
        self
    }

    fn disabled(mut self) -> Self {
        self.disabled = true;
        self
    }

    fn deleted(mut self) -> Self {
        self.deleted = true;
        self
    }

    fn granted(mut self, grant: Grant) -> Self {
        self.grant = grant;
        self
    }
}

/// The reading every message that is not about a `WHERE` condition carries, which is also what
/// the renaming row's projection has to deliver.
const READING: f64 = 21.5;

/// The topic the two `WHERE` rows publish to, the bound their statement compares against, and the
/// readings on each side of it.
const FILTERED_TOPIC: &str = "iot/e2e/filtered";
const FILTER_BOUND: i64 = 30;
const OVER_THE_BOUND: i64 = 70;
const UNDER_THE_BOUND: i64 = 1;

/// Every row, in the order they are stored and published.
fn rows() -> Vec<Row> {
    vec![
        Row::rule(
            "an enabled rule whose target grants the invocation for it",
            "iot_e2e_granted",
            "iot/e2e/granted",
            "granted",
        )
        .forwards(),
        Row {
            sql: "SELECT marker, temperature AS celsius FROM 'iot/e2e/projected'".to_string(),
            ..Row::rule(
                "a statement that selects two members and renames one",
                "iot_e2e_projected",
                "iot/e2e/projected",
                "projected",
            )
            .forwards()
        },
        Row {
            sql: format!("SELECT * FROM '{FILTERED_TOPIC}' WHERE temperature > {FILTER_BOUND}"),
            payload: json!({ "marker": "where-holds", "temperature": OVER_THE_BOUND }),
            ..Row::rule(
                "a WHERE condition that holds for the published message",
                "iot_e2e_filtered",
                FILTERED_TOPIC,
                "where-holds",
            )
            .forwards()
        },
        Row {
            sql: format!("SELECT * FROM '{FILTERED_TOPIC}' WHERE temperature > {FILTER_BOUND}"),
            payload: json!({ "marker": "where-fails", "temperature": UNDER_THE_BOUND }),
            ..Row::rule(
                "the same condition for a message it does not hold for",
                "iot_e2e_filtered",
                FILTERED_TOPIC,
                "where-fails",
            )
        },
        Row::rule(
            "a rule created disabled",
            "iot_e2e_disabled",
            "iot/e2e/disabled",
            "disabled",
        )
        .disabled(),
        Row::rule(
            "a rule deleted again before the publish",
            "iot_e2e_deleted",
            "iot/e2e/deleted",
            "deleted",
        )
        .deleted(),
        Row::rule(
            "a target carrying no grant for the invoking rule",
            "iot_e2e_withheld",
            "iot/e2e/withheld",
            "withheld",
        )
        .granted(Grant::Withheld),
        Row::rule(
            "a grant whose aws:SourceArn names another rule",
            "iot_e2e_wrong_source",
            "iot/e2e/wrong-source",
            "wrong-source",
        )
        .granted(Grant::ForAnotherRule),
        Row::rule(
            "a grant to another service principal",
            "iot_e2e_wrong_principal",
            "iot/e2e/wrong-principal",
            "wrong-principal",
        )
        .granted(Grant::ToAnotherPrincipal),
        Row {
            target: function_arn(ABSENT_FUNCTION),
            ..Row::rule(
                "a permitted rule whose functionArn names no function",
                "iot_e2e_absent_target",
                "iot/e2e/absent-target",
                "absent-target",
            )
        },
    ]
}

/// The whole emulator over one engine, exactly as the binary assembles it.
fn emulator() -> MultiServiceRouter {
    let engine = aws_emulator_router::new_engine();
    let mut router = aws_emulator_router::build_router(engine);
    // Signature verification is the front door's concern, not this suite's: the credential scope
    // is what routes a request, and a handler's OWN calls are signed by the runtime with the
    // credential it was given.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what routes the
/// request. The signature is never checked here.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

/// One HTTP call into the emulator: the status and the body.
async fn call(
    router: &MultiServiceRouter,
    method: &str,
    signing_name: &str,
    uri: &str,
    content_type: &str,
    headers: &[(&str, &str)],
    body: Vec<u8>,
) -> (u16, Vec<u8>) {
    let mut request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("authorization", authorization(signing_name))
        .header("content-type", content_type);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = router
        .dispatch(
            request
                .body(Bytes::from(body))
                .expect("the test request is well formed"),
        )
        .await;
    let status = response.status().as_u16();
    (status, response.into_body().to_vec())
}

/// One awsQuery call, answered with its status and its XML body.
async fn query(
    router: &MultiServiceRouter,
    signing_name: &str,
    version: &str,
    action: &str,
    params: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let (status, payload) = call(
        router,
        "POST",
        signing_name,
        "/",
        "application/x-www-form-urlencoded",
        &[],
        body.into_bytes(),
    )
    .await;
    (status, String::from_utf8_lossy(&payload).to_string())
}

/// One Amazon SQS call (`awsJson1_0`) that must be served, answered with its document.
async fn sqs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, payload) = call(
        router,
        "POST",
        "sqs",
        "/",
        "application/x-amz-json-1.0",
        &[("x-amz-target", &format!("AmazonSQS.{operation}"))],
        body.to_string().into_bytes(),
    )
    .await;
    let parsed: Value = serde_json::from_slice(&payload).unwrap_or(Value::Null);
    assert_eq!(
        status, 200,
        "case {case:?}: SQS {operation} must succeed, got {status} with {parsed}"
    );
    parsed
}

/// One restJson1 call to a service of this emulator, answered with its status and its document:
/// the two IoT planes and Lambda all bind their operations that way.
async fn rest(
    router: &MultiServiceRouter,
    signing_name: &str,
    method: &str,
    uri: &str,
    body: Option<Vec<u8>>,
) -> (u16, Value) {
    let (status, payload) = call(
        router,
        method,
        signing_name,
        uri,
        "application/json",
        &[],
        body.unwrap_or_default(),
    )
    .await;
    let parsed: Value = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&payload).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// Look again until the stack first reports `wanted`, and answer the last status it reported with
/// the answer it was read from. Waits on that CONDITION rather than on a span.
async fn wait_for_stack(router: &MultiServiceRouter, wanted: &str) -> (String, String) {
    let giving_up_at = Instant::now() + DEADLINE;
    loop {
        let (status, body) = query(
            router,
            "cloudformation",
            "2010-05-15",
            "DescribeStacks",
            &[("StackName", STACK)],
        )
        .await;
        assert_eq!(
            status, 200,
            "setup: DescribeStacks({STACK}) must succeed, got {status} with {body}"
        );
        let reported = opt_element(&body, "StackStatus").unwrap_or_default();
        if reported == wanted || Instant::now() >= giving_up_at {
            return (reported, body);
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Look again until the forwarder reports `Active`, which is the state "you can't invoke or
/// modify the function" before. Waits on the CONDITION, not on a span.
async fn wait_for_active(router: &MultiServiceRouter) {
    let giving_up_at = Instant::now() + DEADLINE;
    let mut last = Value::Null;
    while Instant::now() < giving_up_at {
        let (status, configuration) = rest(
            router,
            "lambda",
            "GET",
            &format!("{FUNCTIONS_URI}/{FORWARDER}/configuration"),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "setup: GetFunctionConfiguration({FORWARDER}) must succeed, got {status} with \
             {configuration}"
        );
        if configuration["State"] == json!(ACTIVE) {
            return;
        }
        last = configuration;
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    panic!("setup: {FORWARDER} never reported {ACTIVE}, last {last}");
}

/// Add one statement to the forwarder's resource policy, as `AddPermission` writes one.
async fn add_permission(
    router: &MultiServiceRouter,
    statement_id: &str,
    principal: &str,
    source_arn: &str,
) {
    let (status, answered) = rest(
        router,
        "lambda",
        "POST",
        &format!("{FUNCTIONS_URI}/{FORWARDER}/policy"),
        Some(
            json!({
                "StatementId": statement_id,
                "Action": INVOKE_ACTION,
                "Principal": principal,
                "SourceArn": source_arn,
            })
            .to_string()
            .into_bytes(),
        ),
    )
    .await;
    assert_eq!(
        status, 201,
        "setup: AddPermission({FORWARDER}, {statement_id}) must succeed, got {status} with \
         {answered}"
    );
}

/// Store one topic rule, as `CreateTopicRule` takes it: the body IS the `topicRulePayload`.
async fn create_topic_rule(router: &MultiServiceRouter, case: &str, rule: &str, payload: &Value) {
    let (status, answered) = rest(
        router,
        "iot",
        "POST",
        &format!("/rules/{rule}"),
        Some(payload.to_string().into_bytes()),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateTopicRule({rule}) must succeed, got {status} with {answered}"
    );
}

/// Read one stored rule back, which is what says the create landed as declared.
async fn get_topic_rule(router: &MultiServiceRouter, case: &str, rule: &str) -> Value {
    let (status, read) = rest(router, "iot", "GET", &format!("/rules/{rule}"), None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetTopicRule({rule}) must resolve it, got {status} with {read}"
    );
    read
}

/// Publish one message to the IoT data plane, which is what the rules engine runs on. An accepted
/// publish is answered 200 whatever the rules did with it.
///
/// The topic travels as ONE path segment: a `{topic}` label carries no `/` of its own, so a topic
/// name's separators are escaped by this crate's one encoder of an unreserved-set component
/// ([`crate::query_wire::form_encode`]) — which is how an SDK sends one.
async fn publish(router: &MultiServiceRouter, case: &str, topic: &str, payload: &Value) {
    let (status, answered) = rest(
        router,
        "iotdata",
        "POST",
        &format!("/topics/{}?qos=1", form_encode(topic)),
        Some(payload.to_string().into_bytes()),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: Publish({topic}) must be accepted, got {status} with {answered}"
    );
}

/// Drain the sink until every marker in `wanted` has been forwarded AND the queue has answered
/// nothing [`QUIET_POLLS`] times running, and answer every event the running handlers sent.
///
/// Waits on that CONDITION rather than on a span, and reports what it did see when a wanted
/// marker never arrives.
async fn forwarded_events(
    router: &MultiServiceRouter,
    queue_url: &str,
    wanted: &BTreeSet<&'static str>,
) -> Vec<Value> {
    let case = "the queue the invoked handlers forward to";
    let giving_up_at = Instant::now() + DEADLINE;
    let mut events: Vec<Value> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut quiet = 0;
    while Instant::now() < giving_up_at {
        let received = sqs(
            router,
            case,
            "ReceiveMessage",
            &json!({
                "QueueUrl": queue_url,
                "MaxNumberOfMessages": 10,
                "WaitTimeSeconds": WAIT_TIME_SECONDS,
            }),
        )
        .await;
        let messages = received["Messages"].as_array().cloned().unwrap_or_default();
        if messages.is_empty() {
            quiet += 1;
        } else {
            quiet = 0;
        }
        for message in messages {
            let body = message["Body"].as_str().unwrap_or_default();
            let parsed: Value = serde_json::from_str(body).unwrap_or_else(|error| {
                panic!("case {case:?}: a forwarded message must be a JSON document ({error}): {body}")
            });
            if let Some(marker) = parsed["event"]["marker"].as_str() {
                seen.insert(marker.to_string());
            }
            events.push(parsed["event"].clone());
        }
        if quiet >= QUIET_POLLS && wanted.iter().all(|marker| seen.contains(*marker)) {
            return events;
        }
    }
    panic!(
        "case {case:?}: expected a running handler to forward an event for each of {wanted:?}, saw \
         {seen:?} in {events:?}"
    );
}

/// The event one marker's message was projected into, for a marker that was forwarded at all.
fn event_marked<'a>(events: &'a [Value], marker: &str) -> Option<&'a Value> {
    events.iter().find(|event| event["marker"] == json!(marker))
}

/// The member names one delivered document carries, which is what a statement selecting named
/// members is judged by.
fn keys_of(document: &Value) -> BTreeSet<&str> {
    document
        .as_object()
        .map(|members| members.keys().map(String::as_str).collect())
        .unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_api_created_rules_lambda_action_runs_the_handler_its_grant_admits() {
    let router = emulator();

    // The queue, the execution role and the FUNCTION, whose inline source a template is what
    // archives into a deployment package this emulator can run. Nothing under test is declared
    // here: every rule, grant and enablement below is made through the service APIs.
    let (status, answered) = query(
        &router,
        "cloudformation",
        "2010-05-15",
        "CreateStack",
        &[
            ("StackName", STACK),
            ("TemplateBody", &template()),
            ("Capabilities.member.1", "CAPABILITY_IAM"),
            ("Capabilities.member.2", "CAPABILITY_NAMED_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateStack({STACK}) must succeed, got {status} with {answered}"
    );
    let (reached, described) = wait_for_stack(&router, CREATE_COMPLETE).await;
    assert_eq!(
        reached, CREATE_COMPLETE,
        "setup: expected the stack holding the forwarder to reach {CREATE_COMPLETE}, got \
         {reached:?} in {described}"
    );
    let outputs = outputs_of(&described);
    let sink = outputs
        .get("SinkUrl")
        .cloned()
        .unwrap_or_else(|| panic!("setup: the stack must publish its sink's URL, got {outputs:?}"));
    wait_for_active(&router).await;

    // What the function's resource policy says about each rule, which is the whole of the
    // authorization gate. One statement per rule: two rows over one rule share its grant.
    let mut granted: BTreeSet<&'static str> = BTreeSet::new();
    for row in rows() {
        if !granted.insert(row.rule) {
            continue;
        }
        match row.grant {
            Grant::ForThisRule => {
                add_permission(&router, row.rule, IOT_PRINCIPAL, &rule_arn(row.rule)).await;
            }
            Grant::ForAnotherRule => {
                add_permission(&router, row.rule, IOT_PRINCIPAL, &rule_arn(UNRELATED_RULE)).await;
            }
            Grant::ToAnotherPrincipal => {
                add_permission(&router, row.rule, OTHER_PRINCIPAL, &rule_arn(row.rule)).await;
            }
            Grant::Withheld => {}
        }
    }

    // Each row's rule, stored once even where two rows publish to it, and read back through
    // `GetTopicRule` so the stored statement, action and enablement are the declared ones.
    let mut stored: BTreeSet<&'static str> = BTreeSet::new();
    for row in rows() {
        if !stored.insert(row.rule) {
            continue;
        }
        let mut payload = json!({
            "sql": row.sql,
            "actions": [{ "lambda": { "functionArn": row.target } }],
        });
        if row.disabled {
            payload["ruleDisabled"] = json!(true);
        }
        create_topic_rule(&router, row.case, row.rule, &payload).await;
        let read = get_topic_rule(&router, row.case, row.rule).await;
        assert_eq!(
            read["ruleArn"],
            json!(rule_arn(row.rule)),
            "case {:?}: expected the rule's ARN to be the one a grant for it names, got {read}",
            row.case
        );
        assert_eq!(
            read["rule"]["sql"],
            json!(row.sql),
            "case {:?}: expected the stored statement to be the declared one, got {read}",
            row.case
        );
        assert_eq!(
            read["rule"]["actions"][0]["lambda"]["functionArn"],
            json!(row.target),
            "case {:?}: expected the stored action to name the declared function, got {read}",
            row.case
        );
        assert_eq!(
            read["rule"]["ruleDisabled"],
            json!(row.disabled),
            "case {:?}: expected the stored enablement to be the declared one, got {read}",
            row.case
        );
        if row.deleted {
            let (status, answered) =
                rest(&router, "iot", "DELETE", &format!("/rules/{}", row.rule), None).await;
            assert_eq!(
                status, 200,
                "case {:?}: DeleteTopicRule({}) must succeed, got {status} with {answered}",
                row.case, row.rule
            );
        }
    }

    // Every row's publish, in order, and the sentinel last — so by the time the sentinel's own
    // event has been forwarded every earlier publish has had longer than it did.
    for row in rows() {
        publish(&router, row.case, row.topic, &row.payload).await;
    }
    let sentinel = json!({
        "marker": SENTINEL,
        "deviceId": "sensor-1",
        "temperature": READING,
        "unit": "C",
    });
    publish(&router, "the sentinel publish", "iot/e2e/granted", &sentinel).await;

    let mut wanted: BTreeSet<&'static str> = rows()
        .iter()
        .filter(|row| row.forwarded)
        .map(|row| row.marker)
        .collect();
    wanted.insert(SENTINEL);
    let events = forwarded_events(&router, &sink, &wanted).await;

    // Each row, judged where the HANDLER wrote: a run happened for exactly the rows a grant
    // admitted an invocation for on a rule that matched.
    for row in rows() {
        let forwarded = event_marked(&events, row.marker).is_some();
        assert_eq!(
            forwarded,
            row.forwarded,
            "case {:?}: expected the rule's lambda action {} the message marked {:?}, got the \
             events {events:?}",
            row.case,
            if row.forwarded {
                "to run the handler for"
            } else {
                "to run nothing for"
            },
            row.marker,
        );
    }

    // Nothing ran twice, and nothing beyond the rows that match and are granted ran at all: one
    // matching message runs a rule's action ONCE.
    assert_eq!(
        events.len(),
        wanted.len(),
        "case \"one run per matched and granted message\": expected {} forwarded events, got \
         {events:?}",
        wanted.len()
    );

    // The EVENT the run was handed, for the statement that selects the whole message: the
    // published document itself, member for member.
    let whole = event_marked(&events, "granted").unwrap_or_else(|| {
        panic!(
            "case \"an enabled rule whose target grants the invocation for it\": expected its \
             handler to have run, got {events:?}"
        )
    });
    assert_eq!(
        keys_of(whole),
        BTreeSet::from(["marker", "deviceId", "temperature", "unit"]),
        "case \"an enabled rule whose target grants the invocation for it\": expected the whole \
         published message, got {whole}"
    );
    assert_eq!(
        whole["deviceId"],
        json!("sensor-1"),
        "case \"an enabled rule whose target grants the invocation for it\": expected the \
         published member, got {whole}"
    );
    assert_eq!(
        whole["unit"],
        json!("C"),
        "case \"an enabled rule whose target grants the invocation for it\": expected the \
         published member, got {whole}"
    );
    assert_eq!(
        whole["temperature"].as_f64(),
        Some(READING),
        "case \"an enabled rule whose target grants the invocation for it\": expected the \
         published reading, got {whole}"
    );

    // And for the statement that selects two members, one of them renamed: exactly those, with
    // the published member dropped.
    let projected = event_marked(&events, "projected").unwrap_or_else(|| {
        panic!(
            "case \"a statement that selects two members and renames one\": expected its handler \
             to have run, got {events:?}"
        )
    });
    assert_eq!(
        keys_of(projected),
        BTreeSet::from(["marker", "celsius"]),
        "case \"a statement that selects two members and renames one\": expected exactly the \
         selected keys, got {projected}"
    );
    assert_eq!(
        projected["celsius"].as_f64(),
        Some(READING),
        "case \"a statement that selects two members and renames one\": expected the renamed \
         member's value, got {projected}"
    );

    // The row whose `WHERE` holds was handed the whole message, which is what says the condition
    // decided the match rather than the projection.
    let filtered = event_marked(&events, "where-holds").unwrap_or_else(|| {
        panic!(
            "case \"a WHERE condition that holds for the published message\": expected its handler \
             to have run, got {events:?}"
        )
    });
    assert_eq!(
        filtered["temperature"],
        json!(OVER_THE_BOUND),
        "case \"a WHERE condition that holds for the published message\": expected the published \
         reading, got {filtered}"
    );
}
