// SPDX-License-Identifier: CC0-1.0

//! Which CHANGE-TRIGGERED Custom Lambda rule Config takes for a recorded resource, and
//! what the rule it takes then does.
//!
//! The rule under test: **a Config rule whose `Source.Owner` is `CUSTOM_LAMBDA` is
//! evaluated by the AWS Lambda function its `SourceIdentifier` names rather than by Config,
//! so the write is taken only where Config could INVOKE that function — the identifier
//! names a function the account holds and that function's own resource-based policy allows
//! the Config service principal `lambda:InvokeFunction` on it. Anything else is refused
//! `InsufficientPermissionsException` (HTTP 400, from
//! `smithy_query(query="operation", operation="PutConfigRule")`), naming the function it
//! could not invoke, and leaves the account holding no such rule — whatever the account's
//! configuration recorder is doing. A rule that IS taken is one Config evaluates: a
//! change-triggered rule's evaluator is invoked for the configuration item of each recorded
//! resource in the rule's scope, with no further call, and the verdict that function
//! delivers is what the rule's compliance reads then report.**
//!
//! And that invocation is owed until it is MADE: a call that did not reach the function ran
//! nothing and delivered no verdict, so a later pass makes it and the verdict still
//! arrives, with no call from the client
//! ([`an_invocation_a_pass_could_not_make_is_made_by_a_later_pass`]).
//!
//! Config, IAM, AWS Lambda and Amazon SQS are mounted on ONE engine, as
//! `core/router/src/lib.rs` mounts them: the queue in the rule's scope is a real Amazon SQS
//! queue a RUNNING configuration recorder observed through Amazon SQS' own API, the policy
//! Config reads is the one `AddPermission` actually wrote, and the verdict is the one the
//! function's own Python handler decided from the event Config handed it — nothing here
//! fakes a grant, an item or a verdict.
//!
//! The rows differ along what the function's own policy says about Config: nothing at all,
//! a grant to another service principal, a grant of another action, the documented grant —
//! and along whether the `SourceIdentifier` names a function of the account at all.

use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_configservice_starlingdoveservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

use crate::custom_lambda_rule_evaluation::package;

/// The Region every request below is signed for, and the Region the queue lives in: a
/// configuration recorder records the resources of its own Region.
const REGION: &str = "us-west-2";

/// Where `CreateFunction` is bound (`POST /2015-03-31/functions`).
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The runtime every function below runs, and the module and attribute its package exposes
/// its handler as.
const RUNTIME: &str = "python3.12";
/// See [`RUNTIME`].
const HANDLER: &str = "lambda_function.lambda_handler";

/// How long one invocation of the handler is given, as the function's own `Timeout`: well
/// past what an interpreted run needs.
const FUNCTION_TIMEOUT: i64 = 30;

/// The service principal Config invokes a rule's evaluator as, which is the principal the
/// function's own policy has to name.
const CONFIG_PRINCIPAL: &str = "config.amazonaws.com";

/// The IAM action invoking an evaluator is.
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// The error code Config refuses a rule whose evaluator it cannot invoke with.
const INSUFFICIENT_PERMISSIONS: &str = "InsufficientPermissionsException";

/// The resource type Config records an Amazon SQS queue under, an entry of
/// `com.amazonaws.configservice#ResourceType`.
const QUEUE_RESOURCE_TYPE: &str = "AWS::SQS::Queue";

/// The `SourceDetail$MessageType` a CHANGE-TRIGGERED rule declares ("Triggers an evaluation
/// when Config delivers a configuration item as a result of a resource change"), which is
/// also the `messageType` of the event its evaluator is handed.
const CHANGE_NOTIFICATION: &str = "ConfigurationItemChangeNotification";

/// The verdict the rows' handler delivers for the queue it is handed.
const NON_COMPLIANT: &str = "NON_COMPLIANT";

/// How long a poll waits for work the clock owes — the function becoming `Active`, the
/// recording pass, the invocation and the delivery it makes. Many passes' worth of slack,
/// not a span the behaviour depends on: every wait ends on the CONDITION it is about.
const CONVERGENCE_BUDGET: Duration = Duration::from_secs(180);

/// How long a poll waits between two reads.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Config, IAM, AWS Lambda and Amazon SQS over one shared engine.
struct Emulator {
    router: MultiServiceRouter,
    /// The engine every service above is mounted on, which is also the store a row reads
    /// Config's own record of its passes out of ([`Emulator::failed_dispatches`]).
    engine: Arc<engine::InMemoryEngine>,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
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
        Self { router, engine }
    }

    /// One RPC call: `POST /` carrying `X-Amz-Target`, signed for `signing_name`.
    async fn call_rpc(
        &self,
        signing_name: &str,
        target_prefix: &str,
        json_version: &str,
        operation: &str,
        body: &Value,
    ) -> (u16, Value) {
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/\
             aws4_request, SignedHeaders=host;x-amz-date, Signature=deadbeef"
        );
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
            .header("authorization", authorization)
            .header(
                "content-type",
                format!("application/x-amz-json-{json_version}"),
            )
            .header("x-amz-target", format!("{target_prefix}.{operation}"))
            .body(bytes::Bytes::from(body.to_string()))
            .expect("the test request is well formed");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let parsed = if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        };
        (status, parsed)
    }

    /// One Config call (`awsJson1_1`), as its status and parsed body.
    async fn call(&self, operation: &str, body: &Value) -> (u16, Value) {
        self.call_rpc("config", "StarlingDoveService", "1.1", operation, body)
            .await
    }

    /// A Config call that must answer 200.
    async fn config(&self, operation: &str, body: &Value) -> Value {
        let (status, parsed) = self.call(operation, body).await;
        assert_eq!(
            status, 200,
            "{operation}: expected HTTP 200, got {status} with {parsed}"
        );
        parsed
    }

    /// An Amazon SQS call (`awsJson1_0`) that must answer 200.
    async fn sqs(&self, operation: &str, body: &Value) -> Value {
        let (status, parsed) = self
            .call_rpc("sqs", "AmazonSQS", "1.0", operation, body)
            .await;
        assert_eq!(
            status, 200,
            "{operation}: expected HTTP 200, got {status} with {parsed}"
        );
        parsed
    }

    /// One AWS Lambda `restJson1` call, as its status and parsed body.
    async fn lambda(&self, method: &str, uri: &str, body: Option<&Value>) -> (u16, Value) {
        let payload = body
            .map(|value| value.to_string().into_bytes())
            .unwrap_or_default();
        let request = http::Request::builder()
            .method(method)
            .uri(uri)
            .header("host", format!("lambda.{REGION}.amazonaws.com"))
            .header("content-type", "application/json")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/lambda/\
                     aws4_request, SignedHeaders=host, Signature=0"
                ),
            )
            .body(bytes::Bytes::from(payload))
            .expect("the test request is well formed");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let bytes = response.into_body();
        let parsed: Value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        (status, parsed)
    }

    /// Put the customer managed configuration recorder a Config rule needs — "you must
    /// create a configuration recorder before you can create or update a Config rule" —
    /// with its own role created in IAM first, and START it, so the account's resources are
    /// recorded and a change-triggered rule has an item to be triggered by.
    async fn recording(&self) {
        let role = service_common::iam_client::iam_client_through(
            &smithy_server_runtime::EmbeddedEmulator::from_router(self.router.clone()),
            REGION,
        )
        .create_role()
        .role_name("config-role")
        .assume_role_policy_document(
            json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Allow",
                    "Principal": { "Service": CONFIG_PRINCIPAL },
                    "Action": "sts:AssumeRole",
                }],
            })
            .to_string(),
        )
        .send()
        .await
        .expect("creating the recorder's role must succeed")
        .role
        .expect("CreateRole answers the role")
        .arn;
        self.config(
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
        // A recorder cannot be started without the delivery channel it delivers through
        // (`NoAvailableDeliveryChannelException`).
        self.config(
            "PutDeliveryChannel",
            &json!({ "DeliveryChannel": { "s3BucketName": "config-delivery" } }),
        )
        .await;
        self.config(
            "StartConfigurationRecorder",
            &json!({ "ConfigurationRecorderName": "the-recorder" }),
        )
        .await;
    }

    /// Create a queue through Amazon SQS' own API, wait until Config's inventory holds it,
    /// and answer its url — the `resourceId` Config records a queue under.
    async fn given_recorded_queue(&self, case: &str, name: &str) -> String {
        let created = self.sqs("CreateQueue", &json!({ "QueueName": name })).await;
        let queue_url = created["QueueUrl"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {case:?}: CreateQueue must answer a QueueUrl, got {created}")
            })
            .to_string();
        let deadline = Instant::now() + CONVERGENCE_BUDGET;
        loop {
            let listed = self
                .config(
                    "ListDiscoveredResources",
                    &json!({
                        "resourceType": QUEUE_RESOURCE_TYPE,
                        "resourceIds": [queue_url],
                    }),
                )
                .await;
            if listed["resourceIdentifiers"]
                .as_array()
                .is_some_and(|identifiers| !identifiers.is_empty())
            {
                return queue_url;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: expected {queue_url} to be recorded, got {listed}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// How many of Config's passes have tried and FAILED to make the invocation of the
    /// rule `name`'s evaluator, read out of the store the handlers share.
    ///
    /// Config publishes no evaluation status for a Custom Lambda rule
    /// (`ConfigRuleEvaluationStatus`: "This operation does not return status information
    /// about Config Custom Lambda rules"), so the store is where a row observes that a
    /// pass really did try and could not reach the function.
    fn failed_dispatches(&self, name: &str) -> u32 {
        let state = service_com_amazonaws_configservice_starlingdoveservice::state::service_state(
            &*self.engine,
        )
        .expect("the test engine is the shared engine");
        let store = state.lock();
        store
            .config_rules
            .get(name)
            .map_or(0, |rule| rule.evaluation.failed_dispatches)
    }
}

/// The handler every row's function runs: it reads the event Config handed it and delivers
/// ONE evaluation back with `PutEvaluations`, spending the `resultToken` that event carries
/// and reporting the verdict ABOUT the configuration item the event carried — so what
/// Config handed the function is observable in what Config then reports.
const HANDLER_SOURCE: &str = r#"
import json

import boto3

CONFIG = boto3.client("config")


def lambda_handler(event, context):
    invoking = json.loads(event["invokingEvent"])
    item = invoking["configurationItem"]
    CONFIG.put_evaluations(
        Evaluations=[
            {
                "ComplianceResourceType": item["resourceType"],
                "ComplianceResourceId": item["resourceId"],
                "ComplianceType": "NON_COMPLIANT",
                "Annotation": invoking["messageType"],
                "OrderingTimestamp": 0,
            }
        ],
        ResultToken=event["resultToken"],
    )
    return {"delivered": "NON_COMPLIANT"}
"#;

/// Create the function `name`, wait until it reports `Active`, and answer the ARN AWS
/// Lambda assigned it. No resource-based policy is written here: the grant is what each row
/// varies.
async fn given_function(case: &str, emulator: &Emulator, name: &str) -> String {
    let (status, created) = emulator
        .lambda(
            "POST",
            FUNCTIONS_URI,
            Some(&json!({
                "FunctionName": name,
                "Role": format!(
                    "arn:aws:iam::{}:role/config-rule-evaluator",
                    engine::emulator_account_id()
                ),
                "Runtime": RUNTIME,
                "Handler": HANDLER,
                "Timeout": FUNCTION_TIMEOUT,
                "Code": {
                    "ZipFile": base64::engine::general_purpose::STANDARD
                        .encode(package(HANDLER_SOURCE)),
                },
            })),
        )
        .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateFunction answered {status} {created}"
    );
    let arn = created["FunctionArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no FunctionArn in {created}"))
        .to_string();
    let deadline = Instant::now() + CONVERGENCE_BUDGET;
    loop {
        let (status, read) = emulator
            .lambda("GET", &format!("{FUNCTIONS_URI}/{name}/configuration"), None)
            .await;
        if status == 200 && read["State"] == json!("Active") {
            return arn;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the function to become Active, got {status} {read}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Grant `principal` the action `action` on the function `name` — one `AddPermission` call,
/// which is what writes the function's resource-based policy.
async fn given_permission(
    case: &str,
    emulator: &Emulator,
    name: &str,
    principal: &str,
    action: &str,
) {
    let (status, granted) = emulator
        .lambda(
            "POST",
            &format!("{FUNCTIONS_URI}/{name}/policy"),
            Some(&json!({
                "StatementId": "AllowExecutionFromConfig",
                "Action": action,
                "Principal": principal,
            })),
        )
        .await;
    assert_eq!(
        status, 201,
        "case {case:?}: AddPermission answered {status} {granted}"
    );
}

/// The names of the Config rules the account holds, through `DescribeConfigRules`.
async fn held_rules(emulator: &Emulator) -> Vec<String> {
    let read = emulator.config("DescribeConfigRules", &json!({})).await;
    read["ConfigRules"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|rule| rule["ConfigRuleName"].as_str().map(str::to_string))
        .collect()
}

/// The error code and message a refused Config call answered under.
fn refusal(body: &Value) -> (String, String) {
    let code = body["__type"]
        .as_str()
        .or_else(|| body["code"].as_str())
        .unwrap_or_default()
        .rsplit(['#', ':'])
        .next()
        .unwrap_or_default()
        .to_string();
    let message = ["message", "Message"]
        .iter()
        .find_map(|key| body.get(*key).and_then(Value::as_str))
        .unwrap_or_default()
        .to_string();
    (code, message)
}

/// How Config words the refusal of a rule whose evaluator it cannot invoke: the sentence
/// `InsufficientPermissionsException` states for this case ("the Lambda function cannot be
/// invoked. Check the function ARN, and check the function's permissions"), around the one
/// function the request named.
fn expected_message(function: &str) -> String {
    format!(
        "The AWS Lambda function {function} cannot be invoked. Check the function ARN, and \
         check the function's permissions."
    )
}

/// The evaluation results the rule `name` reports, as
/// `(ComplianceType, Annotation, ResourceType, ResourceId)` rows.
async fn reported(emulator: &Emulator, name: &str) -> Vec<(String, String, String, String)> {
    let body = emulator
        .config(
            "GetComplianceDetailsByConfigRule",
            &json!({ "ConfigRuleName": name }),
        )
        .await;
    body["EvaluationResults"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|result| {
            let qualifier = &result["EvaluationResultIdentifier"]["EvaluationResultQualifier"];
            (
                result["ComplianceType"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                result["Annotation"].as_str().unwrap_or_default().to_string(),
                qualifier["ResourceType"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                qualifier["ResourceId"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect()
}

/// The grant a row writes on its function: the principal it grants to and the action.
type Grant = (&'static str, &'static str);

#[tokio::test]
async fn a_change_triggered_rule_is_taken_only_when_config_may_invoke_its_evaluator() {
    let cases: [(&str, bool, Option<Grant>, bool); 5] = [
        ("no resource-based policy at all", true, None, false),
        (
            "a grant to another service principal",
            true,
            Some(("events.amazonaws.com", INVOKE_ACTION)),
            false,
        ),
        (
            "a grant of another action",
            true,
            Some((CONFIG_PRINCIPAL, "lambda:GetFunction")),
            false,
        ),
        ("an identifier naming no function at all", false, None, false),
        (
            "the documented grant for Config",
            true,
            Some((CONFIG_PRINCIPAL, INVOKE_ACTION)),
            true,
        ),
    ];

    for (index, (case, created, grant, taken)) in cases.into_iter().enumerate() {
        let emulator = Emulator::new();
        emulator.recording().await;
        let queue_url = emulator
            .given_recorded_queue(case, &format!("evaluated-queue-{index}"))
            .await;

        let name = format!("queue-evaluator-{index}");
        let function = if created {
            let arn = given_function(case, &emulator, &name).await;
            if let Some((principal, action)) = grant {
                given_permission(case, &emulator, &name, principal, action).await;
            }
            arn
        } else {
            format!(
                "arn:aws:lambda:{REGION}:{}:function:{name}",
                engine::emulator_account_id()
            )
        };

        let rule = format!("change-triggered-rule-{index}");
        let (status, body) = emulator
            .call(
                "PutConfigRule",
                &json!({
                    "ConfigRule": {
                        "ConfigRuleName": rule,
                        "Scope": { "ComplianceResourceTypes": [QUEUE_RESOURCE_TYPE] },
                        "Source": {
                            "Owner": "CUSTOM_LAMBDA",
                            "SourceIdentifier": function,
                            "SourceDetails": [{
                                "EventSource": "aws.config",
                                "MessageType": CHANGE_NOTIFICATION,
                            }],
                        },
                    }
                }),
            )
            .await;

        if !taken {
            assert_eq!(
                status, 400,
                "case {case:?}: expected the rule to be refused, got {status} with {body}"
            );
            let (code, message) = refusal(&body);
            assert_eq!(
                code, INSUFFICIENT_PERMISSIONS,
                "case {case:?}: expected {INSUFFICIENT_PERMISSIONS}, got {code:?} in {body}"
            );
            assert_eq!(
                message,
                expected_message(&function),
                "case {case:?}: expected the refusal to name {function:?}, got {message:?}"
            );
            // A refused write creates nothing: the account holds no rule of that name, so
            // nothing is left pointing at a function Config may not invoke.
            let held = held_rules(&emulator).await;
            assert!(
                held.is_empty(),
                "case {case:?}: expected the refusal to leave no rule, got {held:?}"
            );
            continue;
        }

        assert_eq!(
            status, 200,
            "case {case:?}: expected the rule to be created, got {status} with {body}"
        );
        let held = held_rules(&emulator).await;
        assert_eq!(
            held,
            vec![rule.clone()],
            "case {case:?}: expected the account to hold {rule:?}, got {held:?}"
        );

        // The rule Config TOOK is one it evaluates: the recorded queue's configuration item
        // triggers it, Config invokes the function with that item, and the verdict the
        // function delivered is what the rule's compliance read reports — with no further
        // call from this suite.
        let deadline = Instant::now() + CONVERGENCE_BUDGET;
        let results = loop {
            let results = reported(&emulator, &rule).await;
            if !results.is_empty() {
                break results;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: expected the rule's evaluator to be invoked for \
                 {queue_url} and to deliver a verdict, got nothing"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        };
        assert_eq!(
            results,
            vec![(
                NON_COMPLIANT.to_string(),
                CHANGE_NOTIFICATION.to_string(),
                QUEUE_RESOURCE_TYPE.to_string(),
                queue_url.clone(),
            )],
            "case {case:?}: expected the evaluator's verdict on the recorded queue, got \
             {results:?}"
        );
    }
}

/// Where a function's reserved concurrency is bound
/// (`PUT`/`DELETE /2017-10-31/functions/{FunctionName}/concurrency`).
fn concurrency_uri(name: &str) -> String {
    format!("/2017-10-31/functions/{name}/concurrency")
}

/// Reserve `executions` simultaneous executions for the function `name`.
///
/// A reservation of ZERO leaves no slot for an invocation of it to run in, so every
/// invocation of that function is throttled — which is how a row below makes Config's call
/// fail without touching Config itself.
async fn given_reserved_concurrency(case: &str, emulator: &Emulator, name: &str, executions: i64) {
    let (status, answered) = emulator
        .lambda(
            "PUT",
            &concurrency_uri(name),
            Some(&json!({ "ReservedConcurrentExecutions": executions })),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutFunctionConcurrency answered {status} {answered}"
    );
}

/// Give up the function `name`'s own reservation, so its invocations are bounded by the
/// account's capacity again and the call Config makes can be served.
async fn release_reserved_concurrency(case: &str, emulator: &Emulator, name: &str) {
    let (status, answered) = emulator.lambda("DELETE", &concurrency_uri(name), None).await;
    assert_eq!(
        status, 204,
        "case {case:?}: DeleteFunctionConcurrency answered {status} {answered}"
    );
}

#[tokio::test]
async fn an_invocation_a_pass_could_not_make_is_made_by_a_later_pass() {
    // The invocation of a change-triggered rule's evaluator is owed until it is MADE: a
    // call that did not reach the function ran nothing, so a later pass makes it and the
    // verdict the function then delivers is still what the rule's compliance read reports.
    //
    // The rows differ along how many of Config's passes could not reach the evaluator
    // before one could: none at all (the control, where the first pass makes the call),
    // one, and several — so what the verdict's arrival shows is the invocation staying owed
    // rather than one lucky first attempt. The function is made un-invocable through AWS
    // Lambda's own API, so nothing here fakes the failure.
    let cases: [(&str, u32); 3] = [
        ("an evaluator Config can invoke straight away", 0),
        ("one pass that could not reach the evaluator", 1),
        ("several passes that could not reach the evaluator", 3),
    ];

    for (index, (case, unreachable_passes)) in cases.into_iter().enumerate() {
        let emulator = Emulator::new();
        emulator.recording().await;
        let queue_url = emulator
            .given_recorded_queue(case, &format!("owed-queue-{index}"))
            .await;
        let function_name = format!("owed-evaluator-{index}");
        let function = given_function(case, &emulator, &function_name).await;
        given_permission(
            case,
            &emulator,
            &function_name,
            CONFIG_PRINCIPAL,
            INVOKE_ACTION,
        )
        .await;
        if unreachable_passes > 0 {
            given_reserved_concurrency(case, &emulator, &function_name, 0).await;
        }

        let rule = format!("owed-change-triggered-rule-{index}");
        let (status, body) = emulator
            .call(
                "PutConfigRule",
                &json!({
                    "ConfigRule": {
                        "ConfigRuleName": rule,
                        "Scope": { "ComplianceResourceTypes": [QUEUE_RESOURCE_TYPE] },
                        "Source": {
                            "Owner": "CUSTOM_LAMBDA",
                            "SourceIdentifier": function,
                            "SourceDetails": [{
                                "EventSource": "aws.config",
                                "MessageType": CHANGE_NOTIFICATION,
                            }],
                        },
                    }
                }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the rule to be created, got {status} with {body}"
        );

        if unreachable_passes > 0 {
            // Wait for the CONDITION this row is about: Config has tried, and failed, to
            // make the invocation that many times. Each failure is a pass that reached a
            // function with no slot to run in.
            let deadline = Instant::now() + CONVERGENCE_BUDGET;
            loop {
                let tried = emulator.failed_dispatches(&rule);
                if tried >= unreachable_passes {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "case {case:?}: expected {unreachable_passes} pass(es) to fail to reach \
                     the evaluator, got {tried}"
                );
                tokio::time::sleep(POLL_INTERVAL).await;
            }
            // Nothing ran, so nothing delivered a verdict: Config records none of its own
            // for a rule whose verdicts are the function's.
            let while_unreachable = reported(&emulator, &rule).await;
            assert!(
                while_unreachable.is_empty(),
                "case {case:?}: expected no verdict while the evaluator could not be \
                 invoked, got {while_unreachable:?}"
            );
            release_reserved_concurrency(case, &emulator, &function_name).await;
        }

        // The invocation was still OWED, so a later pass makes it — with no further call
        // from this suite — and the verdict the function delivered for the recorded queue
        // is what the rule's compliance read reports.
        let deadline = Instant::now() + CONVERGENCE_BUDGET;
        let results = loop {
            let results = reported(&emulator, &rule).await;
            if !results.is_empty() {
                break results;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: expected the evaluator to be invoked for {queue_url} and to \
                 deliver a verdict, got nothing after {} failed pass(es)",
                emulator.failed_dispatches(&rule)
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        };
        assert_eq!(
            results,
            vec![(
                NON_COMPLIANT.to_string(),
                CHANGE_NOTIFICATION.to_string(),
                QUEUE_RESOURCE_TYPE.to_string(),
                queue_url.clone(),
            )],
            "case {case:?}: expected the evaluator's verdict on the recorded queue, got \
             {results:?}"
        );
        // A pass that REACHED the function has answered the invocation, so nothing owes it
        // again: the control row, whose evaluator was invocable throughout, records no
        // failed try at all.
        let tried = emulator.failed_dispatches(&rule);
        assert!(
            tried >= unreachable_passes,
            "case {case:?}: expected at least {unreachable_passes} failed pass(es) to be \
             recorded, got {tried}"
        );
        if unreachable_passes == 0 {
            assert_eq!(
                tried, 0,
                "case {case:?}: expected an invocable evaluator to cost no failed pass, got \
                 {tried}"
            );
        }
    }
}
