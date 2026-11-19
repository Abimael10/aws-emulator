// SPDX-License-Identifier: CC0-1.0

//! What AWS Config does for a CHANGE-TRIGGERED Config Custom Lambda rule, without any
//! further call from the client.
//!
//! The rule under test: while a configuration recorder is recording a resource type, a
//! configuration item Config records for a resource of that type INVOKES every active
//! change-triggered custom rule whose scope covers that resource, handing the rule's
//! evaluator the item's own `ConfigurationItemChangeNotification` as its `invokingEvent`;
//! and a rule naming an evaluator Config may not invoke is refused at write time with
//! `InsufficientPermissionsException` and no rule is created.
//!
//! The suite lives here because no single crate can see the path: CloudFormation archives
//! the evaluator's inline source, Lambda runs it, IAM holds the recorder's role, Amazon SQS
//! is both the resource whose change is recorded and the sink the evaluator writes what it
//! was handed to, and Config records, invokes and refuses. Every service is mounted on ONE
//! engine, exactly as `core/router/src/lib.rs` mounts them.
//!
//! No row calls `StartConfigRulesEvaluation`: everything asserted here is work Config owes
//! by itself.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The credential-scope header builder this crate already owns, taking the Region it signs
// for: a suite that writes its own drifts from the others in what it signs.
use super::cloudformation_provisions_s3vectors_resources::authorization;
// The awsQuery form encoder and the XML element readers this crate writes ONCE
// (`tests/all/query_wire.rs`).
use super::query_wire::{element, form_encode, outputs_of};

/// The Region every request below is signed for, and so the Region every resource belongs
/// to.
const REGION: &str = "us-west-2";

/// Config's `X-Amz-Target` prefix (its model's service shape, `StarlingDoveService`) and
/// the signing name its credential scope carries (`sigv4Name: config`).
const CONFIG_TARGET_PREFIX: &str = "StarlingDoveService";
/// See [`CONFIG_TARGET_PREFIX`].
const CONFIG_SIGNING_NAME: &str = "config";

/// Amazon SQS' `X-Amz-Target` prefix and signing name.
const SQS_TARGET_PREFIX: &str = "AmazonSQS";
/// See [`SQS_TARGET_PREFIX`].
const SQS_SIGNING_NAME: &str = "sqs";

/// The service principal Config invokes a rule's evaluator as, which is the principal the
/// function's own resource-based policy has to admit.
const CONFIG_SERVICE_PRINCIPAL: &str = "config.amazonaws.com";

/// The resource type Config records an Amazon SQS queue under, a
/// `com.amazonaws.configservice#ResourceType` value.
const SQS_QUEUE_RESOURCE_TYPE: &str = "AWS::SQS::Queue";

/// The trigger a change-triggered rule declares, and so the `messageType` of the event its
/// evaluator is handed (`SourceDetail$MessageType`: "ConfigurationItemChangeNotification -
/// Triggers an evaluation when Config delivers a configuration item as a result of a
/// resource change").
const CONFIGURATION_ITEM_CHANGE: &str = "ConfigurationItemChangeNotification";

/// The `EventSource` a rule's source detail names
/// (`com.amazonaws.configservice#EventSource`).
const CONFIG_EVENT_SOURCE: &str = "aws.config";

/// `ComplianceType` values the evaluator delivers
/// (`com.amazonaws.configservice#ComplianceType`).
const NON_COMPLIANT: &str = "NON_COMPLIANT";

/// The error Config refuses a rule whose evaluator it cannot invoke with, and its status
/// (`smithy_query(query="operation",operation="PutConfigRule")`).
const INSUFFICIENT_PERMISSIONS: &str = "InsufficientPermissionsException";
/// See [`INSUFFICIENT_PERMISSIONS`].
const CLIENT_ERROR: u16 = 400;

/// The runtime every evaluator runs, and the module and attribute CloudFormation archives
/// inline source under.
const RUNTIME: &str = "python3.12";
/// See [`RUNTIME`].
const HANDLER: &str = "index.handler";

/// The timeout every evaluator is declared with. Nothing here tests a function's own
/// timeout, so it is given generous room: an interpreted handler on a loaded host must not
/// be stopped by a limit this suite is not about.
const FUNCTION_TIMEOUT: u64 = 120;

/// The `State` Lambda serves invocations in.
const FUNCTION_ACTIVE: &str = "Active";

/// The `StackStatus` a stack that deployed everything it declares settles at.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// How long a poll waits for work the clock owes — a stack settling, a function becoming
/// active, a configuration item being recorded, an invocation and the delivery it makes.
/// Many passes' worth of slack, not a span any behaviour here depends on.
const BUDGET: Duration = Duration::from_secs(180);

/// How long a poll waits between two reads.
const LOOK: Duration = Duration::from_millis(50);

/// This emulator's account.
fn account() -> String {
    engine::emulator_account_id().to_string()
}

/// CloudFormation, IAM, Lambda, Amazon SQS and Config over one shared engine.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        router.register_service(
            service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        router.register_service(
            service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
            Arc::clone(&engine),
        ));
        router.register_service(
            service_com_amazonaws_configservice_starlingdoveservice::EmulatedService::new(engine),
        );
        Self { router }
    }

    /// One awsQuery call (CloudFormation, IAM), answering the HTTP status and the XML body.
    async fn query(
        &self,
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
            .header("authorization", authorization(service, REGION))
            .body(Bytes::from(body))
            .expect("the test request is well formed");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        (status, String::from_utf8_lossy(response.body()).to_string())
    }

    /// An awsQuery call that must answer 200.
    async fn query_ok(
        &self,
        case: &str,
        service: &str,
        version: &str,
        action: &str,
        members: &[(&str, &str)],
    ) -> String {
        let (status, body) = self.query(service, version, action, members).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from {action}, got {status} with {body}"
        );
        body
    }

    /// One `awsJson` RPC call, answering the HTTP status and the parsed body.
    async fn rpc(
        &self,
        signing_name: &str,
        target_prefix: &str,
        json_version: &str,
        operation: &str,
        body: &Value,
    ) -> (u16, Value) {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
            .header(
                "content-type",
                format!("application/x-amz-json-{json_version}"),
            )
            .header("x-amz-target", format!("{target_prefix}.{operation}"))
            .header("authorization", authorization(signing_name, REGION))
            .body(Bytes::from(
                serde_json::to_vec(body).expect("the request serializes"),
            ))
            .expect("the test request is well formed");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let parsed = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
        (status, parsed)
    }

    /// One Config call (`awsJson1_1`).
    async fn config(&self, operation: &str, body: &Value) -> (u16, Value) {
        self.rpc(
            CONFIG_SIGNING_NAME,
            CONFIG_TARGET_PREFIX,
            "1.1",
            operation,
            body,
        )
        .await
    }

    /// A Config call that must answer 200.
    async fn config_ok(&self, case: &str, operation: &str, body: &Value) -> Value {
        let (status, parsed) = self.config(operation, body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from {operation}, got {status} with {parsed}"
        );
        parsed
    }

    /// An Amazon SQS call (`awsJson1_0`) that must answer 200.
    async fn sqs_ok(&self, case: &str, operation: &str, body: &Value) -> Value {
        let (status, parsed) = self
            .rpc(SQS_SIGNING_NAME, SQS_TARGET_PREFIX, "1.0", operation, body)
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from {operation}, got {status} with {parsed}"
        );
        parsed
    }

    /// One Lambda `restJson1` GET, answering the HTTP status and the parsed body.
    async fn lambda_get(&self, uri: &str) -> (u16, Value) {
        let request = http::Request::builder()
            .method("GET")
            .uri(uri)
            .header("host", format!("lambda.{REGION}.amazonaws.com"))
            .header("authorization", authorization("lambda", REGION))
            .body(Bytes::new())
            .expect("the test request is well formed");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let bytes = response.into_body();
        let parsed = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, parsed)
    }

    /// Create the configuration recorder's own role in IAM, trusting the Config service
    /// principal, and answer its ARN.
    async fn recorder_role(&self, case: &str) -> String {
        let trust = json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Principal": { "Service": CONFIG_SERVICE_PRINCIPAL },
                "Action": "sts:AssumeRole",
            }],
        })
        .to_string();
        let created = self
            .query_ok(
                case,
                "iam",
                "2010-05-08",
                "CreateRole",
                &[
                    ("RoleName", "config-recorder-role"),
                    ("AssumeRolePolicyDocument", &trust),
                ],
            )
            .await;
        element(&created, "Arn")
    }

    /// Put a customer managed configuration recorder recording every supported resource
    /// type, its delivery channel, and START it — which is what makes Config record a
    /// configuration item for a resource that changes. Answers the ARN of the role the
    /// recorder runs as, which is the role Config assumes to invoke a rule's evaluator.
    async fn given_recording(&self, case: &str) -> String {
        let role = self.recorder_role(case).await;
        self.config_ok(
            case,
            "PutConfigurationRecorder",
            &json!({
                "ConfigurationRecorder": {
                    "name": "the-recorder",
                    "roleARN": role,
                    "recordingGroup": { "allSupported": true },
                }
            }),
        )
        .await;
        self.config_ok(
            case,
            "PutDeliveryChannel",
            &json!({ "DeliveryChannel": { "s3BucketName": "config-delivery" } }),
        )
        .await;
        self.config_ok(
            case,
            "StartConfigurationRecorder",
            &json!({ "ConfigurationRecorderName": "the-recorder" }),
        )
        .await;
        role
    }
}

/// The source every evaluator runs: it reads the parameters the rule was defined with the
/// way an evaluator does — a JSON-encoded string member a rule that declares none does not
/// carry at all — writes the event Config handed it onto the queue the row watches, and
/// delivers one evaluation back with `PutEvaluations`, spending the `ResultToken` that event
/// carries, which is what "this operation is required in every Lambda function that is
/// invoked by an Config rule" asks of it.
///
/// A handler reaching this with no `resultToken`, or with a `ruleParameters` that is not the
/// string it travels as, raises — which fails the row rather than passing it with a verdict
/// nothing asked for.
const EVALUATOR_SOURCE: &str = r#"
import json

import boto3

CONFIG = boto3.client("config")
QUEUES = boto3.client("sqs")


def handler(event, context):
    token = event["resultToken"]
    parameters = json.loads(event.get("ruleParameters", "{}"))
    QUEUES.send_message(QueueUrl="SINK_QUEUE_URL", MessageBody=json.dumps(event))
    invoking = json.loads(event["invokingEvent"])
    item = invoking.get("configurationItem") or {}
    CONFIG.put_evaluations(
        Evaluations=[
            {
                "ComplianceResourceType": item.get("resourceType", "AWS::::Account"),
                "ComplianceResourceId": item.get("resourceId", event["accountId"]),
                "ComplianceType": "VERDICT",
                "Annotation": invoking["messageType"],
                "OrderingTimestamp": 0,
            }
        ],
        ResultToken=token,
    )
    return {"delivered": "VERDICT", "parameters": parameters}
"#;

/// [`EVALUATOR_SOURCE`] pointed at the queue the row watches, delivering `verdict`.
fn evaluator_source(sink_queue_url: &str, verdict: &str) -> String {
    EVALUATOR_SOURCE
        .replace("SINK_QUEUE_URL", sink_queue_url)
        .replace("VERDICT", verdict)
}

/// The logical id the evaluators' execution role is declared under.
const EVALUATOR_ROLE_ID: &str = "EvaluatorRole";

/// The stack output a function's ARN is published as.
const ARN_OUTPUT: &str = "EvaluatorArn";

/// A stack declaring the evaluator's execution role, the function carrying `source`, and —
/// when `admits_config` — the resource-policy statement that admits Config as its invoker.
fn evaluator_stack(function: &str, source: &str, admits_config: bool) -> String {
    let mut resources = serde_json::Map::new();
    resources.insert(
        EVALUATOR_ROLE_ID.to_string(),
        json!({
            "Type": "AWS::IAM::Role",
            "Properties": {
                "AssumeRolePolicyDocument": {
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Effect": "Allow",
                        "Principal": { "Service": "lambda.amazonaws.com" },
                        "Action": "sts:AssumeRole",
                    }],
                },
                // What the evaluator's own handler calls: the queue it writes what it was
                // handed to, and the delivery of its verdict back to Config.
                "Policies": [{
                    "PolicyName": "evaluator",
                    "PolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Action": ["sqs:SendMessage", "config:PutEvaluations"],
                            "Resource": "*",
                        }],
                    },
                }],
            },
        }),
    );
    resources.insert(
        "Evaluator".to_string(),
        json!({
            "Type": "AWS::Lambda::Function",
            "Properties": {
                "FunctionName": function,
                "Runtime": RUNTIME,
                "Handler": HANDLER,
                "Role": { "Fn::GetAtt": [EVALUATOR_ROLE_ID, "Arn"] },
                "Timeout": FUNCTION_TIMEOUT,
                "Code": { "ZipFile": source },
            },
        }),
    );
    if admits_config {
        resources.insert(
            "InvokeFromConfig".to_string(),
            json!({
                "Type": "AWS::Lambda::Permission",
                "Properties": {
                    "FunctionName": { "Ref": "Evaluator" },
                    "Action": "lambda:InvokeFunction",
                    "Principal": CONFIG_SERVICE_PRINCIPAL,
                    "SourceAccount": account(),
                },
            }),
        );
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "the evaluator of a change-triggered Config Custom Lambda rule",
        "Resources": Value::Object(resources),
        "Outputs": { ARN_OUTPUT: { "Value": { "Fn::GetAtt": ["Evaluator", "Arn"] } } },
    })
    .to_string()
}

/// Deploy `stack` and answer its `DescribeStacks` body once it has settled at
/// `CREATE_COMPLETE`.
async fn deployed(emulator: &Emulator, case: &str, stack: &str, template: &str) -> String {
    emulator
        .query_ok(
            case,
            "cloudformation",
            "2010-05-15",
            "CreateStack",
            &[
                ("StackName", stack),
                ("TemplateBody", template),
                ("Capabilities.member.1", "CAPABILITY_IAM"),
            ],
        )
        .await;
    let deadline = Instant::now() + BUDGET;
    loop {
        let body = emulator
            .query_ok(
                case,
                "cloudformation",
                "2010-05-15",
                "DescribeStacks",
                &[("StackName", stack)],
            )
            .await;
        let status = element(&body, "StackStatus");
        if status == CREATE_COMPLETE {
            return body;
        }
        assert!(
            !status.contains("FAILED") && !status.starts_with("ROLLBACK"),
            "case {case:?}: expected the stack to deploy its resources, got {status} with {body}"
        );
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the stack to reach {CREATE_COMPLETE}, got {status}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// Wait until Lambda reports `function` active, so an invocation Config makes reaches a
/// function that can serve it.
async fn active(emulator: &Emulator, case: &str, function: &str) {
    let uri = format!("/2015-03-31/functions/{function}/configuration");
    let deadline = Instant::now() + BUDGET;
    loop {
        let (status, body) = emulator.lambda_get(&uri).await;
        if status == 200 && body["State"] == json!(FUNCTION_ACTIVE) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {function} to become {FUNCTION_ACTIVE}, got {status} with \
             {body}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// Create the queue `name` and answer its url.
async fn queue(emulator: &Emulator, case: &str, name: &str) -> String {
    let created = emulator
        .sqs_ok(case, "CreateQueue", &json!({ "QueueName": name }))
        .await;
    created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a QueueUrl, got {created}"))
        .to_string()
}

/// The deployed evaluator's ARN, with the stack, the function and the queue it writes to in
/// place, once Lambda reports the function able to serve an invocation.
async fn deployed_evaluator(
    emulator: &Emulator,
    case: &str,
    stack: &str,
    function: &str,
    sink_queue_url: &str,
    admits_config: bool,
) -> String {
    let evaluator =
        deployed_evaluator_unawaited(emulator, case, stack, function, sink_queue_url, admits_config)
            .await;
    active(emulator, case, function).await;
    evaluator
}

/// The same evaluator, answered as soon as its stack has deployed it — WITHOUT waiting for
/// Lambda to report it able to serve an invocation, which is the window
/// `CreateFunction` documents ("During this time, you can't invoke or modify the function").
async fn deployed_evaluator_unawaited(
    emulator: &Emulator,
    case: &str,
    stack: &str,
    function: &str,
    sink_queue_url: &str,
    admits_config: bool,
) -> String {
    let template = evaluator_stack(
        function,
        &evaluator_source(sink_queue_url, NON_COMPLIANT),
        admits_config,
    );
    let body = deployed(emulator, case, stack, &template).await;
    outputs_of(&body)
        .get(ARN_OUTPUT)
        .unwrap_or_else(|| panic!("case {case:?}: expected the stack to publish {ARN_OUTPUT}"))
        .clone()
}

/// Every event the evaluator wrote onto `queue_url`, read until at least one is there.
///
/// The invocation is work Config owes by the clock, which no fixed wait names: this looks
/// until one has arrived or the budget is spent.
async fn pushed_events(emulator: &Emulator, case: &str, queue_url: &str) -> Vec<Value> {
    let deadline = Instant::now() + BUDGET;
    loop {
        let received = emulator
            .sqs_ok(
                case,
                "ReceiveMessage",
                &json!({ "QueueUrl": queue_url, "MaxNumberOfMessages": 10 }),
            )
            .await;
        let messages = received["Messages"].as_array().cloned().unwrap_or_default();
        let events: Vec<Value> = messages
            .iter()
            .filter_map(|message| serde_json::from_str(message["Body"].as_str()?).ok())
            .collect();
        if !events.is_empty() {
            return events;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected Config to invoke the change-triggered rule's evaluator by \
             itself, got no event on {queue_url}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// The `(ComplianceType, Annotation, ResourceType, ResourceId)` of each evaluation result
/// the rule `rule` holds, read until it holds at least one.
async fn delivered_verdicts(
    emulator: &Emulator,
    case: &str,
    rule: &str,
) -> Vec<(String, String, String, String)> {
    let deadline = Instant::now() + BUDGET;
    loop {
        let body = emulator
            .config_ok(
                case,
                "GetComplianceDetailsByConfigRule",
                &json!({ "ConfigRuleName": rule }),
            )
            .await;
        let results = body["EvaluationResults"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let rows: Vec<(String, String, String, String)> = results
            .iter()
            .map(|result| {
                let qualifier =
                    &result["EvaluationResultIdentifier"]["EvaluationResultQualifier"];
                (
                    result["ComplianceType"].as_str().unwrap_or_default().to_string(),
                    result["Annotation"].as_str().unwrap_or_default().to_string(),
                    qualifier["ResourceType"].as_str().unwrap_or_default().to_string(),
                    qualifier["ResourceId"].as_str().unwrap_or_default().to_string(),
                )
            })
            .collect();
        if !rows.is_empty() {
            return rows;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the evaluator's delivered verdict to be recorded for \
             {rule}, got {body}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// One row's change-triggered rule, scoped as the row states and carrying
/// `input_parameters` as the rule's own `InputParameters` where the row states any.
async fn put_change_triggered_rule(
    emulator: &Emulator,
    rule: &str,
    evaluator: &str,
    scope: &Value,
    input_parameters: Option<&str>,
) -> (u16, Value) {
    let mut config_rule = json!({
        "ConfigRuleName": rule,
        "Source": {
            "Owner": "CUSTOM_LAMBDA",
            "SourceIdentifier": evaluator,
            "SourceDetails": [{
                "EventSource": CONFIG_EVENT_SOURCE,
                "MessageType": CONFIGURATION_ITEM_CHANGE,
            }],
        },
        "Scope": scope.clone(),
    });
    if let Some(parameters) = input_parameters {
        config_rule["InputParameters"] = json!(parameters);
    }
    emulator
        .config("PutConfigRule", &json!({ "ConfigRule": config_rule }))
        .await
}

/// One row of the push table: how the rule's scope selects the queue whose change triggers
/// it.
struct Row {
    case: &'static str,
    /// The rule's `Scope`, built from the queue's own identity.
    scope: fn(queue_url: &str, queue_name: &str) -> Value,
    /// The tags the queue carries, which a scope may narrow by.
    tags: &'static [(&'static str, &'static str)],
}

#[tokio::test]
async fn a_recorded_configuration_item_push_invokes_every_change_triggered_rule_covering_it() {
    // The rows differ along the dimension the rule's scope selects the changed resource by —
    // the resource type alone, that type and the resource's own id, and a tag the resource
    // carries (`Scope$ComplianceResourceTypes`, `Scope$ComplianceResourceId`,
    // `Scope$TagKey`/`Scope$TagValue`) — and so along which recorded item triggers the rule.
    let rows: [Row; 3] = [
        Row {
            case: "a scope naming the changed resource's type",
            scope: |_url, _name| json!({ "ComplianceResourceTypes": [SQS_QUEUE_RESOURCE_TYPE] }),
            tags: &[],
        },
        Row {
            case: "a scope naming the resource's type and its own id",
            scope: |url, _name| {
                json!({
                    "ComplianceResourceTypes": [SQS_QUEUE_RESOURCE_TYPE],
                    "ComplianceResourceId": url,
                })
            },
            tags: &[],
        },
        Row {
            case: "a scope naming a tag the changed resource carries",
            scope: |_url, _name| json!({ "TagKey": "config-watched", "TagValue": "yes" }),
            tags: &[("config-watched", "yes")],
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let emulator = Emulator::new();
        let queue_name = format!("change-triggered-{index}");
        let queue_url = queue(&emulator, case, &queue_name).await;
        if !row.tags.is_empty() {
            let tags: BTreeMap<&str, &str> = row.tags.iter().copied().collect();
            emulator
                .sqs_ok(
                    case,
                    "TagQueue",
                    &json!({ "QueueUrl": queue_url, "Tags": tags }),
                )
                .await;
        }
        let function = format!("change-triggered-evaluator-{index}");
        let evaluator = deployed_evaluator(
            &emulator,
            case,
            &format!("evaluator-{index}"),
            &function,
            &queue_url,
            true,
        )
        .await;
        emulator.given_recording(case).await;

        let rule = format!("change-triggered-rule-{index}");
        let (status, answered) = put_change_triggered_rule(
            &emulator,
            &rule,
            &evaluator,
            &(row.scope)(&queue_url, &queue_name),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected PutConfigRule to take the rule, got {status} with \
             {answered}"
        );

        // Config invoked the evaluator by itself: no row here calls
        // `StartConfigRulesEvaluation`.
        let events = pushed_events(&emulator, case, &queue_url).await;
        let first = &events[0];
        let invoking: Value = serde_json::from_str(
            first["invokingEvent"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: expected an invokingEvent, got {first}")),
        )
        .unwrap_or_else(|_| panic!("case {case:?}: expected a JSON invokingEvent, got {first}"));
        assert_eq!(
            invoking["messageType"].as_str(),
            Some(CONFIGURATION_ITEM_CHANGE),
            "case {case:?}: expected the configuration-item notification, got {invoking}"
        );
        let item = &invoking["configurationItem"];
        assert_eq!(
            (
                item["resourceType"].as_str(),
                item["resourceId"].as_str(),
                item["awsRegion"].as_str(),
                item["awsAccountId"].as_str(),
            ),
            (
                Some(SQS_QUEUE_RESOURCE_TYPE),
                Some(queue_url.as_str()),
                Some(REGION),
                Some(account().as_str()),
            ),
            "case {case:?}: expected the invoking event to carry the changed queue's own \
             configuration item, got {invoking}"
        );
        assert_eq!(
            (
                first["configRuleName"].as_str(),
                first["configRuleArn"].as_str().map(|arn| arn.contains("config-rule")),
                first["eventLeftScope"].as_bool(),
            ),
            (Some(rule.as_str()), Some(true), Some(false)),
            "case {case:?}: expected the event to name the rule it was made for, got {first}"
        );

        // And the verdict the evaluator delivered on the item it was handed is what the
        // rule's own compliance details report.
        let delivered = delivered_verdicts(&emulator, case, &rule).await;
        assert_eq!(
            delivered,
            vec![(
                NON_COMPLIANT.to_string(),
                CONFIGURATION_ITEM_CHANGE.to_string(),
                SQS_QUEUE_RESOURCE_TYPE.to_string(),
                queue_url.clone(),
            )],
            "case {case:?}: expected the delivered verdict about the changed queue, got \
             {delivered:?}"
        );

        // The same result read the other way round — by the RESOURCE it is about — is the
        // one record of it: a verdict a rule reports and the resource does not would be two
        // answers about one evaluation.
        let by_resource = emulator
            .config_ok(
                case,
                "GetComplianceDetailsByResource",
                &json!({
                    "ResourceType": SQS_QUEUE_RESOURCE_TYPE,
                    "ResourceId": queue_url,
                }),
            )
            .await;
        let reported: Vec<(&str, &str)> = by_resource["EvaluationResults"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .map(|result| {
                let qualifier =
                    &result["EvaluationResultIdentifier"]["EvaluationResultQualifier"];
                (
                    qualifier["ConfigRuleName"].as_str().unwrap_or_default(),
                    result["ComplianceType"].as_str().unwrap_or_default(),
                )
            })
            .collect();
        assert_eq!(
            reported,
            vec![(rule.as_str(), NON_COMPLIANT)],
            "case {case:?}: expected the queue's own compliance details to report the \
             delivered verdict, got {by_resource}"
        );
    }
}

#[tokio::test]
async fn a_rule_naming_an_evaluator_config_cannot_invoke_is_refused_and_not_created() {
    let case = "a function whose policy does not admit Config";
    let emulator = Emulator::new();
    let queue_url = queue(&emulator, case, "unreachable-evaluator-sink").await;
    let evaluator = deployed_evaluator(
        &emulator,
        case,
        "unreachable-evaluator",
        "unreachable-evaluator",
        &queue_url,
        false,
    )
    .await;
    emulator.given_recording(case).await;

    let rule = "denied-rule";
    let (status, answered) = put_change_triggered_rule(
        &emulator,
        rule,
        &evaluator,
        &json!({ "ComplianceResourceTypes": [SQS_QUEUE_RESOURCE_TYPE] }),
        None,
    )
    .await;
    assert_eq!(
        status, CLIENT_ERROR,
        "case {case:?}: expected PutConfigRule to refuse the rule, got {status} with {answered}"
    );
    let code = answered["__type"]
        .as_str()
        .or_else(|| answered["code"].as_str())
        .unwrap_or_default();
    assert!(
        code.contains(INSUFFICIENT_PERMISSIONS),
        "case {case:?}: expected {INSUFFICIENT_PERMISSIONS}, got {answered}"
    );

    let (read_status, read) = emulator
        .config("DescribeConfigRules", &json!({ "ConfigRuleNames": [rule] }))
        .await;
    assert_eq!(
        read_status, CLIENT_ERROR,
        "case {case:?}: a refused rule was never created, so reading it must fail, got \
         {read_status} with {read}"
    );
}

#[tokio::test]
async fn the_event_an_evaluator_is_handed_carries_each_member_as_the_type_it_travels_as() {
    // Every member of the event is carried as the type it travels as, and a member Config
    // has no value for is LEFT OUT rather than carried as a JSON null: `ruleParameters` is
    // "if present, the parameters the rule was defined with", a JSON-ENCODED STRING, which an
    // evaluator reads as one. The rows differ along what the rule declares — nothing, and a
    // parameter document — so the member is absent in one and that very document in the
    // other. The handler itself reads it the way an evaluator does, so a row whose event
    // carried something it could not parse never reaches the queue at all.
    let rows: [(&str, Option<&str>); 2] = [
        ("a rule declaring no input parameters", None),
        (
            "a rule declaring a parameter document",
            Some(r#"{"maximumDelaySeconds":"0"}"#),
        ),
    ];

    for (index, (case, parameters)) in rows.iter().enumerate() {
        let emulator = Emulator::new();
        let queue_url = queue(&emulator, case, &format!("event-members-{index}")).await;
        let function = format!("event-members-evaluator-{index}");
        let evaluator = deployed_evaluator(
            &emulator,
            case,
            &format!("event-members-{index}"),
            &function,
            &queue_url,
            true,
        )
        .await;
        let recorder_role = emulator.given_recording(case).await;

        let rule = format!("event-members-rule-{index}");
        let (status, answered) = put_change_triggered_rule(
            &emulator,
            &rule,
            &evaluator,
            &json!({ "ComplianceResourceTypes": [SQS_QUEUE_RESOURCE_TYPE] }),
            *parameters,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected PutConfigRule to take the rule, got {status} with \
             {answered}"
        );

        let events = pushed_events(&emulator, case, &queue_url).await;
        let first = &events[0];
        assert_eq!(
            first.get("ruleParameters").and_then(Value::as_str),
            *parameters,
            "case {case:?}: expected the parameters the rule was defined with, got {first}"
        );
        assert!(
            first.get("ruleParameters").is_none_or(|member| !member.is_null()),
            "case {case:?}: a member Config has no value for is left out, not carried null, \
             got {first}"
        );
        // The role Config ASSUMED to make the invocation, which is the recorder's own.
        assert_eq!(
            first["executionRoleArn"].as_str(),
            Some(recorder_role.as_str()),
            "case {case:?}: expected the event to name the role Config invoked as, got {first}"
        );

        // The configuration item the event carries holds its COLLECTION members, empty where
        // the recorded resource has nothing under them, so an evaluator reads the entries
        // they hold without first asking whether the member is there.
        let invoking: Value = serde_json::from_str(
            first["invokingEvent"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: expected an invokingEvent, got {first}")),
        )
        .unwrap_or_else(|_| panic!("case {case:?}: expected a JSON invokingEvent, got {first}"));
        let item = &invoking["configurationItem"];
        for (member, empty) in [
            ("tags", json!({})),
            ("supplementaryConfiguration", json!({})),
            ("relatedEvents", json!([])),
            ("relationships", json!([])),
        ] {
            assert_eq!(
                item[member], empty,
                "case {case:?}: expected {member} to be carried as {empty}, got {item}"
            );
        }
        assert!(
            item["configurationItemStatus"].as_str().is_some_and(|status| !status.is_empty())
                && item["configurationStateId"].as_str().is_some()
                && item["ARN"].as_str().is_some_and(|arn| arn.contains(":sqs:")),
            "case {case:?}: expected the item Config recorded for the queue, got {item}"
        );

        // And Config reports a status for the rule it invoked, whatever evaluates it.
        let reported = emulator
            .config_ok(
                case,
                "DescribeConfigRuleEvaluationStatus",
                &json!({ "ConfigRuleNames": [rule.clone()] }),
            )
            .await;
        let statuses = reported["ConfigRulesEvaluationStatus"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            statuses
                .iter()
                .map(|status| status["ConfigRuleName"].as_str().unwrap_or_default())
                .collect::<Vec<&str>>(),
            vec![rule.as_str()],
            "case {case:?}: expected a status for the rule Config invoked, got {reported}"
        );
    }
}

#[tokio::test]
async fn a_rule_written_while_its_evaluator_is_still_provisioning_is_evaluated_once_it_serves() {
    // An invocation AWS Lambda REFUSED — the function is still inside the window
    // `CreateFunction` documents ("During this time, you can't invoke or modify the
    // function") — evaluated nothing, so Config still owes it and invokes again. The rule is
    // written here as soon as the function exists, without waiting for Lambda to report it
    // able to serve one, so the evaluation has to arrive whether the first invocation
    // reached a function that could serve it or not.
    let case = "a rule written as soon as its evaluator exists";
    let emulator = Emulator::new();
    let queue_url = queue(&emulator, case, "provisioning-evaluator-sink").await;
    emulator.given_recording(case).await;
    let evaluator = deployed_evaluator_unawaited(
        &emulator,
        case,
        "provisioning-evaluator",
        "provisioning-evaluator",
        &queue_url,
        true,
    )
    .await;

    let rule = "provisioning-evaluator-rule";
    let (status, answered) = put_change_triggered_rule(
        &emulator,
        rule,
        &evaluator,
        &json!({ "ComplianceResourceTypes": [SQS_QUEUE_RESOURCE_TYPE] }),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected PutConfigRule to take the rule, got {status} with {answered}"
    );

    let events = pushed_events(&emulator, case, &queue_url).await;
    assert_eq!(
        events[0]["configRuleName"].as_str(),
        Some(rule),
        "case {case:?}: expected the event to name the rule it was made for, got {}",
        events[0]
    );
    let delivered = delivered_verdicts(&emulator, case, rule).await;
    assert_eq!(
        delivered,
        vec![(
            NON_COMPLIANT.to_string(),
            CONFIGURATION_ITEM_CHANGE.to_string(),
            SQS_QUEUE_RESOURCE_TYPE.to_string(),
            queue_url.clone(),
        )],
        "case {case:?}: expected the delivered verdict about the recorded queue, got \
         {delivered:?}"
    );
}
