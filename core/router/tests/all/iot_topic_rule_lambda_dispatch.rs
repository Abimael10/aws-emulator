// SPDX-License-Identifier: CC0-1.0

//! The AWS IoT RULES ENGINE's `lambda` action, end to end over the real `MultiServiceRouter`:
//! the whole emulator on one engine, a message published to the IoT data plane, and the
//! invocation the matched rule made, read from the record AWS Lambda itself keeps of every
//! event it accepted.
//!
//! RULE: a message published to a topic an ENABLED topic rule's `FROM`-clause filter covers is
//! projected by that rule's `sql` and each of the rule's `actions` is performed for it — for an
//! `Action$lambda` ("invoke a Lambda function"), ONE invocation of the function its
//! `functionArn` names, with the projected document as the invocation event, made as the IoT
//! service principal and therefore admitted only by that FUNCTION's own resource-based policy
//! for `lambda:InvokeFunction` with the RULE's ARN as `aws:SourceArn`. An action that cannot be
//! performed is what `TopicRulePayload$errorAction` ("the action to take when an error occurs")
//! is for: the error action is performed instead, with the rule-failure report — the rule's
//! name, the topic, the published payload and the failures — as its message. An
//! `Action$republish` ("publish to another MQTT topic") makes the projected message a NEW broker
//! message on the action's topic, which the rules whose filters cover that topic then match in
//! turn. The publisher is answered 200 whatever the rules did, which is why every assertion
//! below is made on the TARGET.
//!
//! Three crates have to agree for that to hold — the IoT data plane takes the message, the IoT
//! control plane's rules engine projects it and dispatches the action, Lambda holds the
//! function's resource policy and records the invocation, IAM holds the roles an action runs
//! under — so the suite lives here: a crate-local suite can only ever see one side of it.
//!
//! What it asserts on is the EVENT each function was handed, read from Lambda's own record of
//! the asynchronous invocations it accepted — the same observable
//! `events_lambda_target_qualified_arn_envelope.rs` and `events_lambda_target_resource_policy.rs`
//! pin an EventBridge target's invocation with, and the one that says what the rules engine
//! dispatched rather than what a handler then did. (That a dispatched function really RUNS, and
//! that its effect reaches another service, is pinned end to end by
//! `cfn_deployed_iot_topic_rule_invokes_its_lambda.rs`.) Nothing here waits on a handler, so
//! every row is decided by state that exists by the time the publish is answered.
//!
//! The rows differ along what the rule generalises over: whether the rule is ENABLED, still
//! EXISTS, or names a function at all; what the target function's resource policy says (a grant
//! for this rule, a grant for another rule's ARN, a grant to another service principal, no
//! policy at all); what the statement SELECTS (the whole message, a projection of computed
//! expressions and the `topic()` builtin, and a `WHERE` condition that holds for one published
//! message and not for another); and which ACTION the rule carries (a `lambda` action, a
//! `republish` hop into a second rule's own `lambda` action, and a `republish` whose role grants
//! no publish).

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::ComAmazonawsLambdaAwsgirapiserviceState;
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode};

/// The Region every request below is signed for, and so the Region every resource it makes
/// belongs to.
const REGION: &str = "us-east-1";

/// The account this emulator serves, which every ARN below carries.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// Where Lambda binds the collection the functions are created, read and permissioned under.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// A deployment package as `Code.ZipFile` carries it: base64 in a `restJson1` JSON body. Nothing
/// here asserts what a handler computed — what is pinned is the event the function was handed —
/// so the package holds no code at all.
const ZIP_FILE: &str = "UEsDBAoAAAAAAAAAAAA=";

/// The runtime and the execution role every function carries, both well-formed for that model's
/// own members and neither of them what any row is about.
const RUNTIME: &str = "python3.12";
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";
const HANDLER: &str = "index.handler";
const FUNCTION_TIMEOUT: u64 = 30;

/// The `State` a function has to report before an invocation of it is accepted.
const ACTIVE: &str = "Active";

/// The IAM action an invocation of a function performs, and the principal AWS IoT invokes a
/// rule's function as.
const INVOKE_ACTION: &str = "lambda:InvokeFunction";
const IOT_PRINCIPAL: &str = "iot.amazonaws.com";

/// A service principal that is NOT the one AWS IoT invokes as, which is what the wrong-principal
/// row grants to.
const OTHER_PRINCIPAL: &str = "sns.amazonaws.com";

/// How long an observation of work the CLOCK owes — a function going `Active`, and the record of
/// an accepted invocation appearing — waits before calling it a hang, and how long between two
/// looks. Generous: a row fails only when the work did not happen at all, so a loaded host
/// cannot decide the outcome, and bounded, so an exhausted wait cannot make the suite slow.
const DEADLINE: Duration = Duration::from_secs(60);
/// See [`DEADLINE`].
const LOOK_AGAIN: Duration = Duration::from_millis(25);

/// The role a `republish` action runs under, which has to trust the IoT service principal and
/// grant the publish on the topic the action names — and one that grants neither, which is the
/// only observable that action's permission boundary has.
const REPUBLISH_ROLE: &str = "iot-rule-republish";
const BARRED_ROLE: &str = "iot-rule-republish-barred";

/// The four functions the rows point at, which differ ONLY in what their resource policy says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    /// Carries a `lambda:InvokeFunction` grant to the IoT service principal for each rule that
    /// names it.
    Granted,
    /// Carries no resource policy at all.
    NoPolicy,
    /// Carries a grant whose `aws:SourceArn` names ANOTHER rule.
    OtherRule,
    /// Carries a grant to another service principal.
    OtherPrincipal,
}

impl Target {
    const ALL: &'static [Self] = &[
        Self::Granted,
        Self::NoPolicy,
        Self::OtherRule,
        Self::OtherPrincipal,
    ];

    /// The name the function is created under.
    fn function(self) -> &'static str {
        match self {
            Self::Granted => "iot-rule-fwd-granted",
            Self::NoPolicy => "iot-rule-fwd-no-policy",
            Self::OtherRule => "iot-rule-fwd-other-rule",
            Self::OtherPrincipal => "iot-rule-fwd-other-principal",
        }
    }
}

/// The ARN of one function, which is what a rule's `lambda` action names.
fn function_arn(function: &str) -> String {
    format!("arn:aws:lambda:{REGION}:{ACCOUNT}:function:{function}")
}

/// The ARN of one topic rule, which is the `aws:SourceArn` a grant for its actions is narrowed
/// by.
fn rule_arn(rule: &str) -> String {
    format!("arn:aws:iot:{REGION}:{ACCOUNT}:rule/{rule}")
}

/// The ARN of one topic, which is the resource a `republish` action's role is granted
/// `iot:Publish` on.
fn topic_arn(topic: &str) -> String {
    format!("arn:aws:iot:{REGION}:{ACCOUNT}:topic/{topic}")
}

/// One row: the rule it stores, the message it publishes, and whether the function that rule
/// names has to have been handed it.
struct Row {
    case: &'static str,
    /// The rule's name. `RuleName`'s pattern is `^[a-zA-Z0-9_]+$`, so a rule name carries
    /// letters, digits and underscores alone.
    rule: &'static str,
    /// The topic the rule selects from and the message is published to.
    topic: &'static str,
    /// The rule's `sql`.
    sql: String,
    /// The rule's actions.
    actions: Value,
    /// The rule's `errorAction`, for a row that has one.
    error_action: Option<Value>,
    /// Whether the rule is created disabled.
    disabled: bool,
    /// Whether the rule is deleted again before anything is published.
    deleted: bool,
    /// The function whose resource policy must carry a grant for this rule, for a row that
    /// expects an invocation.
    permitted: Option<Target>,
    /// The payload published to the topic.
    payload: Value,
    /// The marker that payload carries, and whether the function it reaches has to have been
    /// handed it. `""` for a row whose DELIVERED document carries no marker of its own — a
    /// projection that does not select one — which its own assertions below judge instead.
    marker: &'static str,
    delivered: bool,
}

impl Row {
    /// A row whose rule selects the whole message and invokes `target`'s function.
    fn lambda(case: &'static str, rule: &'static str, topic: &'static str, target: Target) -> Self {
        Self {
            case,
            rule,
            topic,
            sql: format!("SELECT * FROM '{topic}'"),
            actions: json!([{ "lambda": { "functionArn": function_arn(target.function()) } }]),
            error_action: None,
            disabled: false,
            deleted: false,
            permitted: None,
            payload: json!({ "marker": case }),
            marker: case,
            delivered: false,
        }
    }

    fn with_marker(mut self, marker: &'static str) -> Self {
        self.payload = json!({ "marker": marker });
        self.marker = marker;
        self
    }

    fn permitted_on(mut self, target: Target) -> Self {
        self.permitted = Some(target);
        self
    }

    fn delivers(mut self) -> Self {
        self.delivered = true;
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
}

/// The topic rule A republishes to, and the one the barred republish would republish to.
const STAGED_TOPIC: &str = "iot/rule/staged";
const BARRED_STAGE_TOPIC: &str = "iot/rule/barred-stage";

/// The rule whose `lambda` action the republish chain's second hop runs, the rule over the
/// barred republish's stage topic, and the rule the wrong-source-arn grant names instead of the
/// rule that invokes.
const CHAINED_RULE: &str = "iot_rule_chained";
const BARRED_STAGE_RULE: &str = "iot_rule_barred_stage";
const UNRELATED_RULE: &str = "iot_rule_unrelated";

/// The rule the error-action row stores, the topic it is published to, and the function name no
/// function holds — which is what makes that row's primary action fail.
const ERROR_ACTION_RULE: &str = "iot_rule_error_action";
const ERROR_ACTION_TOPIC: &str = "iot/rule/failing";
const ABSENT_FUNCTION: &str = "iot-rule-no-such-function";

/// The topic the projection row publishes to, and the payload and keys its statement is about.
///
/// The statement computes `temperature * 9 / 5 + 32`, renames `humidity` and calls the `topic()`
/// builtin, and names nothing else — so the delivered document carries exactly those three keys
/// and the published `extraField` is dropped.
const PROJECTION_TOPIC: &str = "iot/rule/projected";
const PROJECTED_TEMPERATURE: i64 = 20;
const PROJECTED_HUMIDITY: i64 = 55;
const PROJECTED_FAHRENHEIT: i64 = 68;

/// The topic the two `WHERE` rows publish to.
const FILTERED_TOPIC: &str = "iot/rule/filtered";

/// Every row, in the order they are stored and published.
fn rows() -> Vec<Row> {
    vec![
        Row::lambda(
            "an enabled rule whose target grants the invocation for this rule",
            "iot_rule_enabled",
            "iot/rule/enabled",
            Target::Granted,
        )
        .with_marker("enabled")
        .permitted_on(Target::Granted)
        .delivers(),
        Row::lambda(
            "a rule created disabled",
            "iot_rule_disabled",
            "iot/rule/disabled",
            Target::Granted,
        )
        .with_marker("disabled")
        .permitted_on(Target::Granted)
        .disabled(),
        Row::lambda(
            "a rule deleted again before the publish",
            "iot_rule_deleted",
            "iot/rule/deleted",
            Target::Granted,
        )
        .with_marker("deleted")
        .permitted_on(Target::Granted)
        .deleted(),
        Row::lambda(
            "a target carrying no resource policy at all",
            "iot_rule_no_policy",
            "iot/rule/no-policy",
            Target::NoPolicy,
        )
        .with_marker("no-policy"),
        Row::lambda(
            "a target whose grant names another rule's ARN",
            "iot_rule_other_rule",
            "iot/rule/other-rule",
            Target::OtherRule,
        )
        .with_marker("other-rule"),
        Row::lambda(
            "a target whose grant names another service principal",
            "iot_rule_other_principal",
            "iot/rule/other-principal",
            Target::OtherPrincipal,
        )
        .with_marker("other-principal"),
        Row {
            error_action: Some(
                json!({ "lambda": { "functionArn": function_arn(Target::Granted.function()) } }),
            ),
            actions: json!([{ "lambda": { "functionArn": function_arn(ABSENT_FUNCTION) } }]),
            ..Row::lambda(
                "a rule whose functionArn names no function, with an errorAction that does",
                ERROR_ACTION_RULE,
                ERROR_ACTION_TOPIC,
                Target::Granted,
            )
            .with_marker("error-action")
            .permitted_on(Target::Granted)
        },
        Row {
            sql: format!(
                "SELECT temperature * 9 / 5 + 32 AS temperatureF, humidity AS humidityPct, \
                 topic() AS sourceTopic FROM '{PROJECTION_TOPIC}'"
            ),
            payload: json!({
                "temperature": PROJECTED_TEMPERATURE,
                "humidity": PROJECTED_HUMIDITY,
                "extraField": "dropped-by-the-select",
            }),
            // The projection selects no marker, so this row is judged by the document it
            // delivered rather than by the marker loop.
            marker: "",
            delivered: false,
            ..Row::lambda(
                "a statement that projects computed expressions and the topic() builtin",
                "iot_rule_projection",
                PROJECTION_TOPIC,
                Target::Granted,
            )
            .permitted_on(Target::Granted)
        },
        Row {
            sql: format!(
                "SELECT * FROM '{FILTERED_TOPIC}' WHERE temperature > {PROJECTED_TEMPERATURE}"
            ),
            payload: json!({ "marker": "where-holds", "temperature": 70 }),
            ..Row::lambda(
                "a WHERE condition that holds for the published message",
                "iot_rule_filtered",
                FILTERED_TOPIC,
                Target::Granted,
            )
            .with_marker("where-holds")
            .permitted_on(Target::Granted)
            .delivers()
        },
        Row {
            sql: format!(
                "SELECT * FROM '{FILTERED_TOPIC}' WHERE temperature > {PROJECTED_TEMPERATURE}"
            ),
            payload: json!({ "marker": "where-fails", "temperature": 1 }),
            ..Row::lambda(
                "the same condition for a message it does not hold for",
                "iot_rule_filtered",
                FILTERED_TOPIC,
                Target::Granted,
            )
            .with_marker("where-fails")
            .permitted_on(Target::Granted)
        },
        Row {
            actions: json!([{
                "republish": {
                    "roleArn": format!("arn:aws:iam::{ACCOUNT}:role/{REPUBLISH_ROLE}"),
                    "topic": STAGED_TOPIC,
                    "qos": 1,
                },
            }]),
            ..Row::lambda(
                "a republish hop into a second rule's own lambda action",
                "iot_rule_republisher",
                "iot/rule/source",
                Target::Granted,
            )
            .with_marker("republished")
            .delivers()
        },
        Row {
            actions: json!([{
                "republish": {
                    "roleArn": format!("arn:aws:iam::{ACCOUNT}:role/{BARRED_ROLE}"),
                    "topic": BARRED_STAGE_TOPIC,
                    "qos": 1,
                },
            }]),
            ..Row::lambda(
                "a republish whose role grants no publish on the action's topic",
                "iot_rule_barred_republisher",
                "iot/rule/barred-source",
                Target::Granted,
            )
            .with_marker("republish-barred")
        },
    ]
}

/// The whole emulator over one engine, exactly as the binary assembles it, with the engine kept
/// so the record Lambda holds of the invocations it accepted can be read.
fn emulator() -> (MultiServiceRouter, Arc<::engine::InMemoryEngine>) {
    let engine = aws_emulator_router::new_engine();
    let mut router = aws_emulator_router::build_router(Arc::clone(&engine));
    // Signature verification is the front door's concern, not this suite's: the credential
    // scope is what routes a request.
    router.set_request_authentication(false);
    (router, engine)
}

/// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what routes
/// the request. The signature is never checked here.
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
    body: Vec<u8>,
) -> (u16, Vec<u8>) {
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("authorization", authorization(signing_name))
        .header("content-type", content_type)
        .body(Bytes::from(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
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
        body.into_bytes(),
    )
    .await;
    (status, String::from_utf8_lossy(&payload).to_string())
}

/// One IAM call that must be served, answered with its XML body.
async fn iam(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> String {
    let (status, body) = query(router, "iam", "2010-05-08", action, params).await;
    assert_eq!(
        status, 200,
        "setup: IAM {action} must succeed, got {status} with {body}"
    );
    body
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

/// A trust policy admitting one service principal.
fn trust_policy(principal: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": principal },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// Create one role trusting the IoT service principal with `document` in force on it, and answer
/// the ARN IAM minted for it.
async fn given_role(router: &MultiServiceRouter, name: &str, document: &Value) -> String {
    let created = iam(
        router,
        "CreateRole",
        &[
            ("RoleName", name),
            ("AssumeRolePolicyDocument", &trust_policy(IOT_PRINCIPAL)),
        ],
    )
    .await;
    let arn = element(&created, "Arn");
    iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", name),
            ("PolicyName", "grants"),
            ("PolicyDocument", &document.to_string()),
        ],
    )
    .await;
    arn
}

/// Create one function carrying no code, which is all a row that asserts the EVENT it was handed
/// needs.
async fn create_function(router: &MultiServiceRouter, function: &str) {
    let (status, answered) = rest(
        router,
        "lambda",
        "POST",
        FUNCTIONS_URI,
        Some(
            json!({
                "FunctionName": function,
                "Role": ROLE,
                "Runtime": RUNTIME,
                "Handler": HANDLER,
                "Timeout": FUNCTION_TIMEOUT,
                "Code": { "ZipFile": ZIP_FILE },
            })
            .to_string()
            .into_bytes(),
        ),
    )
    .await;
    assert_eq!(
        status, 201,
        "setup: CreateFunction({function}) must succeed, got {status} with {answered}"
    );
}

/// Look again until `function` reports `Active`, which is the state "you can't invoke or modify
/// the function" before. Waits on the CONDITION, not on a span.
async fn wait_for_active(router: &MultiServiceRouter, function: &str) {
    let giving_up_at = Instant::now() + DEADLINE;
    let mut last = Value::Null;
    while Instant::now() < giving_up_at {
        let (status, configuration) = rest(
            router,
            "lambda",
            "GET",
            &format!("{FUNCTIONS_URI}/{function}/configuration"),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "setup: GetFunctionConfiguration({function}) must succeed, got {status} with \
             {configuration}"
        );
        if configuration["State"] == json!(ACTIVE) {
            return;
        }
        last = configuration;
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    panic!("setup: {function} never reported {ACTIVE}, last {last}");
}

/// Add one statement to a function's resource policy, as `AddPermission` writes one.
async fn add_permission(
    router: &MultiServiceRouter,
    function: &str,
    statement_id: &str,
    principal: &str,
    source_arn: &str,
) {
    let (status, answered) = rest(
        router,
        "lambda",
        "POST",
        &format!("{FUNCTIONS_URI}/{function}/policy"),
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
        "setup: AddPermission({function}, {statement_id}) must succeed, got {status} with \
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

/// Publish one message to the IoT data plane, which is what the rules engine runs on. An
/// accepted publish is answered 200 whatever the rules did with it.
///
/// The topic travels as ONE path segment: a `{topic}` label carries no `/` of its own, so a
/// topic name's separators are escaped by this crate's one encoder of an unreserved-set
/// component ([`crate::query_wire::form_encode`]) — which is how an SDK sends one.
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

/// The event document of every asynchronous invocation Lambda has ACCEPTED for `function`,
/// oldest first — the owning service's own record of what it was handed.
fn accepted(engine: &Arc<::engine::InMemoryEngine>, function: &str) -> Vec<Value> {
    let state = engine.state::<ComAmazonawsLambdaAwsgirapiserviceState>();
    let store = state.lock();
    store
        .functions
        .get(function)
        .map(|record| {
            record
                .async_invocations
                .iter()
                .map(|entry| {
                    serde_json::from_slice(&entry.payload).unwrap_or_else(|error| {
                        panic!(
                            "the event handed to {function} must be a JSON document ({error}): \
                             {:?}",
                            String::from_utf8_lossy(&entry.payload)
                        )
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Look again until Lambda holds `expected` accepted invocations for `function`, and report
/// them.
///
/// The record exists by the time the publish that caused it is answered — the rules engine makes
/// the invocation on that request's own path — so this is a guard against a future change
/// rather than a wait the suite depends on: it waits on the CONDITION either way, and reports
/// what Lambda held when it gave up.
async fn accepted_once_settled(
    engine: &Arc<::engine::InMemoryEngine>,
    function: &str,
    expected: usize,
) -> Vec<Value> {
    let giving_up_at = Instant::now() + DEADLINE;
    loop {
        let held = accepted(engine, function);
        if held.len() >= expected || Instant::now() >= giving_up_at {
            return held;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The marker one accepted event carries, for an event that carries one — a projected document
/// and a rule-failure report carry none, and are found by their own members instead.
fn marker_of(event: &Value) -> Option<String> {
    event.get("marker")?.as_str().map(str::to_string)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_matching_publish_runs_a_permitted_rules_lambda_action_and_nothing_else() {
    let (router, engine) = emulator();

    // The role a `republish` action runs under: AWS IoT acts AS it, so it has to trust this
    // service principal and grant the publish on the topic the action names.
    given_role(
        &router,
        REPUBLISH_ROLE,
        &json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Action": "iot:Publish",
                "Resource": topic_arn(STAGED_TOPIC),
            }],
        }),
    )
    .await;
    // The same role without that grant, which is what the barred republish row runs under.
    given_role(
        &router,
        BARRED_ROLE,
        &json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Action": "iot:DescribeEndpoint",
                "Resource": "*",
            }],
        }),
    )
    .await;

    for target in Target::ALL {
        create_function(&router, target.function()).await;
    }
    for target in Target::ALL {
        wait_for_active(&router, target.function()).await;
    }

    // What each function's resource policy says, which is the whole of the authorization gate.
    // One statement per (function, rule): two rows over one rule share the grant that rule's
    // invocations are admitted by.
    let mut granted: BTreeSet<(&'static str, &'static str)> = BTreeSet::new();
    for row in rows() {
        let Some(target) = row.permitted else {
            continue;
        };
        if !granted.insert((target.function(), row.rule)) {
            continue;
        }
        add_permission(
            &router,
            target.function(),
            row.rule,
            IOT_PRINCIPAL,
            &rule_arn(row.rule),
        )
        .await;
    }
    add_permission(
        &router,
        Target::OtherRule.function(),
        "AllowAnotherRule",
        IOT_PRINCIPAL,
        &rule_arn(UNRELATED_RULE),
    )
    .await;
    add_permission(
        &router,
        Target::OtherPrincipal.function(),
        "AllowAnotherPrincipal",
        OTHER_PRINCIPAL,
        &rule_arn("iot_rule_other_principal"),
    )
    .await;

    // The second hop of the republish chain, whose own `lambda` action is what makes the
    // republished message observable, and a rule over the BARRED republish's stage topic — so
    // the only reason that row delivers nothing is its action's role.
    for rule in [CHAINED_RULE, BARRED_STAGE_RULE] {
        add_permission(
            &router,
            Target::Granted.function(),
            rule,
            IOT_PRINCIPAL,
            &rule_arn(rule),
        )
        .await;
    }
    create_topic_rule(
        &router,
        "the republish chain's second rule",
        CHAINED_RULE,
        &json!({
            "sql": format!("SELECT * FROM '{STAGED_TOPIC}'"),
            "actions": [{
                "lambda": { "functionArn": function_arn(Target::Granted.function()) },
            }],
        }),
    )
    .await;
    create_topic_rule(
        &router,
        "a rule over the barred republish's stage topic",
        BARRED_STAGE_RULE,
        &json!({
            "sql": format!("SELECT * FROM '{BARRED_STAGE_TOPIC}'"),
            "actions": [{
                "lambda": { "functionArn": function_arn(Target::Granted.function()) },
            }],
        }),
    )
    .await;

    // Each row's rule, stored once even where two rows publish to it.
    let mut stored: BTreeSet<&'static str> = BTreeSet::new();
    for row in rows() {
        if !stored.insert(row.rule) {
            continue;
        }
        let mut payload = json!({ "sql": row.sql, "actions": row.actions });
        if row.disabled {
            payload["ruleDisabled"] = json!(true);
        }
        if let Some(error_action) = &row.error_action {
            payload["errorAction"] = error_action.clone();
        }
        create_topic_rule(&router, row.case, row.rule, &payload).await;
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

    // Every row's publish, in order.
    for row in rows() {
        publish(&router, row.case, row.topic, &row.payload).await;
    }

    // What the permitted function was handed: one event per row that delivers, plus the two that
    // carry no marker of their own — the projected document and the rule-failure report.
    let expected: Vec<&'static str> = rows()
        .iter()
        .filter(|row| row.delivered && !row.marker.is_empty())
        .map(|row| row.marker)
        .collect();
    let events = accepted_once_settled(
        &engine,
        Target::Granted.function(),
        expected.len() + 2,
    )
    .await;
    let markers: BTreeSet<String> = events.iter().filter_map(marker_of).collect();

    for row in rows() {
        if row.marker.is_empty() {
            continue;
        }
        let seen = markers.contains(row.marker);
        assert_eq!(
            seen,
            row.delivered,
            "case {:?}: expected the rule's lambda action {} the message marked {:?}, got the \
             markers {markers:?} in {events:?}",
            row.case,
            if row.delivered {
                "to deliver"
            } else {
                "not to deliver"
            },
            row.marker,
        );
    }

    // Nothing was delivered twice, and nothing beyond what the rows expect was delivered at
    // all: one matching message runs a rule's action ONCE.
    assert_eq!(
        events.len(),
        expected.len() + 2,
        "case \"one invocation per matched message\": expected {} events, got {events:?}",
        expected.len() + 2
    );

    // The three targets whose resource policy admits NOTHING for the rule that named them were
    // never invoked, judged after every expected invocation has been recorded.
    for target in Target::ALL {
        if *target == Target::Granted {
            continue;
        }
        let held = accepted(&engine, target.function());
        assert!(
            held.is_empty(),
            "case {:?}: expected the rules engine to invoke it for no message at all, got \
             {held:?}",
            target.function()
        );
    }

    // The PROJECTION: the delivered document carries exactly the keys the select clause names,
    // with the arithmetic evaluated and `topic()` resolved to the topic published to.
    let projected = events
        .iter()
        .find(|event| event.get("temperatureF").is_some())
        .unwrap_or_else(|| {
            panic!(
                "case \"a projected document\": expected the rule's projection to be delivered, \
                 got {events:?}"
            )
        });
    let keys: BTreeSet<&str> = projected
        .as_object()
        .map(|members| members.keys().map(String::as_str).collect())
        .unwrap_or_default();
    assert_eq!(
        keys,
        BTreeSet::from(["temperatureF", "humidityPct", "sourceTopic"]),
        "case \"a projected document\": expected exactly the selected keys, got {projected}"
    );
    assert_eq!(
        projected["temperatureF"],
        json!(PROJECTED_FAHRENHEIT),
        "case \"a projected document\": expected the evaluated arithmetic, got {projected}"
    );
    assert_eq!(
        projected["humidityPct"],
        json!(PROJECTED_HUMIDITY),
        "case \"a projected document\": expected the renamed member, got {projected}"
    );
    assert_eq!(
        projected["sourceTopic"],
        json!(PROJECTION_TOPIC),
        "case \"a projected document\": expected topic() to resolve to the topic published to, \
         got {projected}"
    );

    // The ERROR ACTION: the fallback function was handed the rule-failure report for the
    // primary action that could not be performed.
    let report = events
        .iter()
        .find(|event| event.get("failures").is_some())
        .unwrap_or_else(|| {
            panic!(
                "case \"a failing primary action\": expected the rule's errorAction to deliver a \
                 failure report, got {events:?}"
            )
        });
    assert_eq!(
        report["ruleName"],
        json!(ERROR_ACTION_RULE),
        "case \"a failing primary action\": expected the report to name the rule, got {report}"
    );
    assert_eq!(
        report["topic"],
        json!(ERROR_ACTION_TOPIC),
        "case \"a failing primary action\": expected the report to name the topic, got {report}"
    );
    let encoded = report["base64OriginalPayload"].as_str().unwrap_or_default();
    let decoded = aws_smithy_types::base64::decode(encoded).unwrap_or_else(|error| {
        panic!(
            "case \"a failing primary action\": expected base64 of the published payload \
             ({error}), got {report}"
        )
    });
    assert_eq!(
        String::from_utf8_lossy(&decoded),
        json!({ "marker": "error-action" }).to_string(),
        "case \"a failing primary action\": expected the published payload, got {report}"
    );
    let failures = report["failures"].as_array().cloned().unwrap_or_default();
    let first = failures.first().cloned().unwrap_or(Value::Null);
    assert!(
        !failures.is_empty()
            && !first["errorMessage"]
                .as_str()
                .unwrap_or_default()
                .trim()
                .is_empty(),
        "case \"a failing primary action\": expected a non-empty failure carrying a non-empty \
         errorMessage, got {report}"
    );
    assert_eq!(
        first["failedResource"],
        json!(function_arn(ABSENT_FUNCTION)),
        "case \"a failing primary action\": expected the failure to name the target it failed \
         on, got {report}"
    );
}
