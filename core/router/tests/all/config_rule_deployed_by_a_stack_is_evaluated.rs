// SPDX-License-Identifier: CC0-1.0

//! What becomes of a Config rule a CloudFormation stack provisioned.
//!
//! The rule under test: **a Config rule a CloudFormation stack provisions is held by Config
//! exactly as a directly written one is, so Config owes the same evaluation passes for it — the
//! one the rule's creation starts and the one every `StartConfigRulesEvaluation` asks for — and
//! for a rule whose verdicts are delivered back (`Source.Owner` = `CUSTOM_LAMBDA`) each pass
//! invokes the function `Source.SourceIdentifier` names with a usable `ResultToken`, so the
//! verdict that function delivers with `PutEvaluations` is what the rule's evaluation results
//! report for the resource it evaluated.**
//!
//! Nothing here computes a verdict: the `ComplianceType` and the `Annotation` every row asserts
//! are the ones that row's evaluator decided in Python, and the resource they are about is the
//! one the invoking event Config handed it named.
//!
//! The suite lives here because no single crate can see the whole path: CloudFormation evaluates
//! the template and provisions the role, the function, the invoke grant and the rule, Lambda
//! archives and runs the evaluator, IAM holds the roles, and Config registers, dispatches,
//! records and projects. Every service is mounted on ONE engine, exactly as
//! `core/router/src/lib.rs` mounts them.
//!
//! The rows differ along what the rule generalises over: which verdict the evaluator delivers,
//! and whether the annotation it delivers is one the template carried to it through the rule's
//! declared `InputParameters`. Each row is then evaluated twice — once by the pass the stack's
//! own provisioning started, and once by the pass an explicit `StartConfigRulesEvaluation`
//! asks for after the rule's results have been discarded — so a dispatch that happens only at
//! creation fails the row.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The credential-scope header builder this crate already owns, taking the Region it signs
// for: a suite that writes its own drifts from the others in what it signs.
use super::cloudformation_provisions_s3vectors_resources::authorization;
// The awsQuery form encoder and the XML element readers this crate writes ONCE
// (`tests/all/query_wire.rs`): a suite that writes its own drifts from the others.
use super::query_wire::{element, form_encode, outputs_of};

/// The Region every request below is signed for, and so the Region every resource belongs to.
const REGION: &str = "us-west-2";

/// Config's `X-Amz-Target` prefix, from its model's service shape (`StarlingDoveService`), and
/// the signing name its credential scope carries (`sigv4Name: config`).
const CONFIG_TARGET_PREFIX: &str = "StarlingDoveService";
/// See [`CONFIG_TARGET_PREFIX`].
const CONFIG_SIGNING_NAME: &str = "config";

/// The service principal Config invokes a rule's evaluator and delivers as, which is the
/// principal a function's resource policy has to admit before Config will take a rule pointing
/// at it.
const CONFIG_SERVICE_PRINCIPAL: &str = "config.amazonaws.com";

/// The `Source$Owner` of a rule whose verdicts are delivered back to Config by the Lambda
/// function it names (`com.amazonaws.configservice#Owner`).
const CUSTOM_LAMBDA: &str = "CUSTOM_LAMBDA";

/// `ComplianceType` values (`com.amazonaws.configservice#ComplianceType`).
const COMPLIANT: &str = "COMPLIANT";
/// See [`COMPLIANT`].
const NON_COMPLIANT: &str = "NON_COMPLIANT";
/// See [`COMPLIANT`].
const NOT_APPLICABLE: &str = "NOT_APPLICABLE";

/// The resource type a verdict about the account itself carries — the account pseudo-resource
/// `ExternalEvaluation$ComplianceResourceType` names ("Config accepts `AWS::::Account` resource
/// type"), which is what a periodic rule's evaluator reports about.
const ACCOUNT_RESOURCE_TYPE: &str = "AWS::::Account";

/// The trigger every row declares, and so the `messageType` of the invoking event Config hands
/// the evaluator (`SourceDetail$MessageType`).
const SCHEDULED_NOTIFICATION: &str = "ScheduledNotification";

/// The `MaximumExecutionFrequency` every row declares
/// (`com.amazonaws.configservice#MaximumExecutionFrequency`).
const TWENTY_FOUR_HOURS: &str = "TwentyFour_Hours";

/// The `EventSource` of a rule's trigger (`SourceDetail$EventSource`: "aws.config").
const AWS_CONFIG_EVENT_SOURCE: &str = "aws.config";

/// The `ConfigRuleState` a rule Config holds and evaluates reports
/// (`com.amazonaws.configservice#ConfigRuleState`).
const RULE_ACTIVE: &str = "ACTIVE";

/// The error code `StartConfigRulesEvaluation` answers while the pass an earlier call of it
/// started is still running ("For StartConfigRulesEvaluation API, this exception is thrown if an
/// evaluation is in progress").
const LIMIT_EXCEEDED: &str = "LimitExceededException";

/// The logical id the evaluators' shared execution role is declared under.
const EVALUATOR_ROLE_ID: &str = "EvaluatorRole";

/// The `StackStatus` a stack that deployed everything it declares settles at.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The `State` Lambda reports for a function that can serve an invocation.
const FUNCTION_ACTIVE: &str = "Active";

/// The runtime every evaluator runs, and the module and attribute CloudFormation archives
/// inline source under.
const RUNTIME: &str = "python3.12";
/// See [`RUNTIME`].
const HANDLER: &str = "index.handler";

/// The timeout every evaluator is declared with. Nothing here tests a function's own timeout,
/// so it is given generous room: a run shares this process with every other test of this
/// target, and an interpreted handler on a loaded host must not be stopped by a limit this
/// suite is not about.
const FUNCTION_TIMEOUT: u64 = 120;

/// How long a poll waits for work the clock owes — a stack settling, a function becoming
/// active, an evaluation pass, the invocation it makes and the delivery that invocation makes.
/// Many passes' worth of slack, not a span any behaviour here depends on.
const BUDGET: Duration = Duration::from_secs(120);

/// How long a poll waits between two reads.
const LOOK: Duration = Duration::from_millis(50);

/// This emulator's account, the account whose Config data the store holds.
fn account() -> String {
    engine::emulator_account_id().to_string()
}

/// CloudFormation, IAM, Lambda and Config over one shared engine.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    /// The four services this suite exercises, mounted on ONE engine exactly as
    /// `core/router/src/lib.rs` mounts them. Request authentication is off, as in every other
    /// suite here; the in-process calls CloudFormation, Config and the handlers make are signed
    /// either way, and the credential scope is what routes them.
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

    /// One Config call (`awsJson1_1`), answering the HTTP status and the parsed body.
    async fn config(&self, operation: &str, body: &Value) -> (u16, Value) {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header(
                "host",
                format!("{CONFIG_SIGNING_NAME}.{REGION}.amazonaws.com"),
            )
            .header("content-type", "application/x-amz-json-1.1")
            .header("x-amz-target", format!("{CONFIG_TARGET_PREFIX}.{operation}"))
            .header("authorization", authorization(CONFIG_SIGNING_NAME, REGION))
            .body(Bytes::from(
                serde_json::to_vec(body).expect("the request serializes"),
            ))
            .expect("the test request is well formed");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let parsed = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
        (status, parsed)
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

    /// Put the customer managed configuration recorder a Config rule needs — "you must create a
    /// configuration recorder before you can create or update a Config rule" — on a role IAM
    /// holds and shows Config can assume.
    async fn given_recorder(&self, case: &str) {
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
        let role = element(&created, "Arn");
        self.config_ok(
            case,
            "PutConfigurationRecorder",
            &json!({
                "ConfigurationRecorder": {
                    "name": "the-recorder",
                    "roleARN": role,
                    "recordingGroup": {
                        "allSupported": true,
                        "includeGlobalResourceTypes": true,
                    },
                }
            }),
        )
        .await;
    }
}

/// The source every evaluator runs: it reads the invoking event Config handed it and delivers
/// ONE evaluation back with `PutEvaluations`, spending the `ResultToken` that event carries —
/// which is what "this operation is required in every Lambda function that is invoked by an
/// Config rule" asks of it.
///
/// The verdict delivered is the row's; the subject is the account the event names, which is what
/// a periodic rule's evaluator reports about; and the annotation is the one the rule's declared
/// parameters carried to the function, or the row's own when the rule declares none — so what the
/// template declared is observable in what Config then reports. A handler reaching this with no
/// `resultToken`, or an event carrying no notification, raises: that fails the row rather than
/// passing it with a verdict nothing asked for.
const EVALUATOR_SOURCE: &str = r#"
import json

import boto3

CONFIG = boto3.client("config")


def handler(event, context):
    token = event["resultToken"]
    invoking = json.loads(event["invokingEvent"])
    declared = event.get("ruleParameters")
    parameters = json.loads(declared) if declared else {}
    annotation = parameters.get("annotation", "BAKED_ANNOTATION")
    CONFIG.put_evaluations(
        Evaluations=[
            {
                "ComplianceResourceType": "ACCOUNT_RESOURCE_TYPE",
                "ComplianceResourceId": event["accountId"],
                "ComplianceType": "VERDICT",
                "Annotation": annotation,
                "OrderingTimestamp": 0,
            }
        ],
        ResultToken=token,
    )
    return {"messageType": invoking["messageType"]}
"#;

/// One row: the verdict its evaluator delivers, the annotation that evaluator carries itself,
/// and the annotation the rule's declared `InputParameters` carry to it instead, when the row
/// declares any.
struct Row {
    case: &'static str,
    verdict: &'static str,
    baked_annotation: &'static str,
    declared_annotation: Option<&'static str>,
}

impl Row {
    /// The annotation this row's evaluator ends up delivering: the declared one wins, because
    /// the function prefers the parameters it was handed over the one in its own source.
    fn expected_annotation(&self) -> &'static str {
        self.declared_annotation.unwrap_or(self.baked_annotation)
    }

    /// The `InputParameters` this row's rule declares, as a template document.
    fn input_parameters(&self) -> Option<Value> {
        self.declared_annotation
            .map(|annotation| json!({ "annotation": annotation }))
    }

    /// [`EVALUATOR_SOURCE`] with this row's verdict and own annotation written into it.
    fn evaluator_source(&self) -> String {
        EVALUATOR_SOURCE
            .replace("ACCOUNT_RESOURCE_TYPE", ACCOUNT_RESOURCE_TYPE)
            .replace("BAKED_ANNOTATION", self.baked_annotation)
            .replace("VERDICT", self.verdict)
    }
}

/// The function one row's evaluator is declared under.
fn function_name(index: usize) -> String {
    format!("stack-rule-evaluator-{index}")
}

/// The Config rule one row's stack declares.
fn rule_name(index: usize) -> String {
    format!("stack-deployed-rule-{index}")
}

/// The stack output one row's evaluator ARN is published as.
fn evaluator_arn_output(index: usize) -> String {
    format!("Evaluator{index}Arn")
}

/// The stack output one row's rule ARN is published as — an attribute of the rule that is
/// CONFIG's own to mint, so a stack can only publish it by reading the rule back out of Config.
fn rule_arn_output(index: usize) -> String {
    format!("Rule{index}Arn")
}

/// The stack output one row's rule id is published as; see [`rule_arn_output`].
fn rule_id_output(index: usize) -> String {
    format!("Rule{index}Id")
}

/// The ONE stack every row is deployed by: the execution role the evaluators run as, and per row
/// a function carrying that row's inline source, the resource-policy statement that admits Config
/// as its invoker, and the `AWS::Config::ConfigRule` pointing at the ARN Lambda minted for it.
///
/// The rule DEPENDS ON its function's invoke grant: Config takes a Custom Lambda rule only while
/// the function's own policy admits it as the invoker, so the grant has to be in place before the
/// rule is provisioned.
fn evaluator_stack(rows: &[Row]) -> String {
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
            },
        }),
    );
    let mut outputs = serde_json::Map::new();
    for (index, row) in rows.iter().enumerate() {
        let function_id = format!("Evaluator{index}");
        let grant_id = format!("Invoke{index}");
        let rule_id = format!("Rule{index}");
        resources.insert(
            function_id.clone(),
            json!({
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": function_name(index),
                    "Runtime": RUNTIME,
                    "Handler": HANDLER,
                    "Role": { "Fn::GetAtt": [EVALUATOR_ROLE_ID, "Arn"] },
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": { "ZipFile": row.evaluator_source() },
                },
            }),
        );
        resources.insert(
            grant_id.clone(),
            json!({
                "Type": "AWS::Lambda::Permission",
                "Properties": {
                    "FunctionName": { "Ref": function_id.clone() },
                    "Action": "lambda:InvokeFunction",
                    "Principal": CONFIG_SERVICE_PRINCIPAL,
                    "SourceAccount": account(),
                },
            }),
        );
        let mut rule_properties = serde_json::Map::new();
        rule_properties.insert(
            "ConfigRuleName".to_string(),
            Value::String(rule_name(index)),
        );
        rule_properties.insert(
            "Source".to_string(),
            json!({
                "Owner": CUSTOM_LAMBDA,
                "SourceIdentifier": { "Fn::GetAtt": [function_id.clone(), "Arn"] },
                "SourceDetails": [{
                    "EventSource": AWS_CONFIG_EVENT_SOURCE,
                    "MessageType": SCHEDULED_NOTIFICATION,
                    "MaximumExecutionFrequency": TWENTY_FOUR_HOURS,
                }],
            }),
        );
        rule_properties.insert(
            "MaximumExecutionFrequency".to_string(),
            Value::String(TWENTY_FOUR_HOURS.to_string()),
        );
        if let Some(parameters) = row.input_parameters() {
            rule_properties.insert("InputParameters".to_string(), parameters);
        }
        resources.insert(
            rule_id.clone(),
            json!({
                "Type": "AWS::Config::ConfigRule",
                "DependsOn": [grant_id],
                "Properties": Value::Object(rule_properties),
            }),
        );
        outputs.insert(
            evaluator_arn_output(index),
            json!({ "Value": { "Fn::GetAtt": [function_id, "Arn"] } }),
        );
        outputs.insert(
            rule_arn_output(index),
            json!({ "Value": { "Fn::GetAtt": [rule_id.clone(), "Arn"] } }),
        );
        outputs.insert(
            rule_id_output(index),
            json!({ "Value": { "Fn::GetAtt": [rule_id, "ConfigRuleId"] } }),
        );
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "the evaluators and the Config rules of this account's compliance baseline",
        "Resources": Value::Object(resources),
        "Outputs": Value::Object(outputs),
    })
    .to_string()
}

/// Deploy `template` as `stack` and answer its `DescribeStacks` body once it has settled at
/// `CREATE_COMPLETE`.
///
/// The stack's completion is owed by the clock, which no fixed wait names: this looks until it
/// holds, and fails the row at once on a status that says the deployment will never get there.
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
                // The template declares the evaluators' execution role, which CloudFormation
                // takes only once the caller acknowledges it ("Requires capabilities :
                // [CAPABILITY_IAM]").
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

/// Wait until Lambda reports `function` active, so the invocation Config makes for the rule's
/// evaluation pass reaches a function that can serve it.
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

/// One evaluation result, as the members a compliance read reports it by:
/// `(ComplianceType, Annotation, ResourceType, ResourceId)`.
type ReportedResult = (String, String, String, String);

/// The rule `rule`'s evaluation results as [`ReportedResult`] rows, from ONE read.
async fn details_now(emulator: &Emulator, case: &str, rule: &str) -> Vec<ReportedResult> {
    let body = emulator
        .config_ok(
            case,
            "GetComplianceDetailsByConfigRule",
            &json!({ "ConfigRuleName": rule }),
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

/// The rule `rule`'s evaluation results, read until it has at least one.
async fn evaluated(emulator: &Emulator, case: &str, rule: &str) -> Vec<ReportedResult> {
    let deadline = Instant::now() + BUDGET;
    loop {
        let rows = details_now(emulator, case, rule).await;
        if !rows.is_empty() {
            return rows;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the rule {rule:?} to be evaluated, got no evaluation result"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// Discard every verdict recorded against `rule` and wait until the rule reads as unevaluated.
///
/// The delete is repeated while a pass that was already in flight is still landing its delivery:
/// what the row then goes on to assert is that a NEW evaluation puts a result back, so the rule
/// has to start from none.
async fn cleared(emulator: &Emulator, case: &str, rule: &str) {
    let deadline = Instant::now() + BUDGET;
    loop {
        emulator
            .config_ok(
                case,
                "DeleteEvaluationResults",
                &json!({ "ConfigRuleName": rule }),
            )
            .await;
        let rows = details_now(emulator, case, rule).await;
        if rows.is_empty() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the rule {rule:?} to hold no evaluation result after the \
             delete, got {rows:?}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// Ask Config to evaluate `rule` on demand, waiting out the one precondition that call states
/// about itself: "An existing StartConfigRulesEvaluation call for the specified rules must
/// complete before you can call the API again", which is answered `LimitExceededException`.
async fn evaluate_now(emulator: &Emulator, case: &str, rule: &str) {
    let deadline = Instant::now() + BUDGET;
    loop {
        let (status, body) = emulator
            .config(
                "StartConfigRulesEvaluation",
                &json!({ "ConfigRuleNames": [rule] }),
            )
            .await;
        if status == 200 {
            return;
        }
        let code = body["__type"].as_str().unwrap_or_default().to_string();
        assert!(
            code.contains(LIMIT_EXCEEDED),
            "case {case:?}: expected StartConfigRulesEvaluation to be accepted or to report an \
             evaluation in progress, got {status} with {body}"
        );
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected StartConfigRulesEvaluation to be accepted, got {status} with \
             {body}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// The one `ConfigRule` Config holds under `rule`.
async fn described(emulator: &Emulator, case: &str, rule: &str) -> Value {
    let body = emulator
        .config_ok(
            case,
            "DescribeConfigRules",
            &json!({ "ConfigRuleNames": [rule] }),
        )
        .await;
    let rules = body["ConfigRules"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        rules.len(),
        1,
        "case {case:?}: expected exactly one rule named {rule:?}, got {body}"
    );
    rules[0].clone()
}

#[tokio::test]
async fn a_stack_deployed_custom_lambda_rule_is_dispatched_to_its_evaluator() {
    // The rows differ along the verdict the evaluator delivers and along whether the annotation
    // it delivers reached it through the rule's declared parameters.
    let rows = [
        Row {
            case: "a stack-deployed rule whose evaluator finds the account non-compliant",
            verdict: NON_COMPLIANT,
            baked_annotation: "the-evaluators-own-finding",
            declared_annotation: None,
        },
        Row {
            case: "a stack-deployed rule whose evaluator finds the account compliant",
            verdict: COMPLIANT,
            baked_annotation: "nothing-to-report",
            declared_annotation: None,
        },
        Row {
            case: "a stack-deployed rule whose declared parameters reach its evaluator",
            verdict: NOT_APPLICABLE,
            baked_annotation: "no-parameters-were-handed-over",
            declared_annotation: Some("the-annotation-the-template-declared"),
        },
    ];
    let suite = "a Config rule a stack deployed";

    let emulator = Emulator::new();
    emulator.given_recorder(suite).await;

    let stack = deployed(&emulator, suite, "compliance-baseline", &evaluator_stack(&rows)).await;
    let outputs = outputs_of(&stack);

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let rule = rule_name(index);
        let published = |output: String| -> String {
            outputs
                .get(&output)
                .unwrap_or_else(|| {
                    panic!("case {case:?}: expected the stack to publish {output:?}")
                })
                .clone()
        };
        let function = published(evaluator_arn_output(index));

        // The rule the stack provisioned is one CONFIG holds: it answers for it, points it at the
        // ARN Lambda minted for the evaluator, and the identity the stack published for the
        // resource is the one that service assigned the rule.
        let described = described(&emulator, case, &rule).await;
        assert_eq!(
            (
                described["Source"]["Owner"].as_str(),
                described["Source"]["SourceIdentifier"].as_str(),
                described["ConfigRuleState"].as_str(),
            ),
            (Some(CUSTOM_LAMBDA), Some(function.as_str()), Some(RULE_ACTIVE)),
            "case {case:?}: expected the deployed rule to evaluate through its function, got \
             {described}"
        );
        assert_eq!(
            (
                described["ConfigRuleArn"].as_str(),
                described["ConfigRuleId"].as_str(),
            ),
            (
                Some(published(rule_arn_output(index)).as_str()),
                Some(published(rule_id_output(index)).as_str()),
            ),
            "case {case:?}: expected the stack to publish the identity Config minted, got \
             {described}"
        );
        assert_eq!(
            described["InputParameters"].as_str(),
            row.input_parameters()
                .as_ref()
                .map(Value::to_string)
                .as_deref(),
            "case {case:?}: expected the declared parameters to reach the rule, got {described}"
        );

        // The verdict every read of the rule reports is the one its evaluator delivered, about
        // the account the invoking event named, with the annotation that evaluator supplied.
        let expected = vec![(
            row.verdict.to_string(),
            row.expected_annotation().to_string(),
            ACCOUNT_RESOURCE_TYPE.to_string(),
            account(),
        )];

        // First the pass the rule's own provisioning started, which needs no call from the
        // caller: "when you create a rule, Config evaluates your resources against the rule
        // automatically".
        active(&emulator, case, &function_name(index)).await;
        let provisioned = evaluated(&emulator, case, &rule).await;
        assert_eq!(
            provisioned, expected,
            "case {case:?}: expected the deployed rule's evaluator to have been invoked, got \
             {provisioned:?}"
        );

        // Then the pass an explicit on-demand evaluation asks for: with the rule's results
        // discarded, the result a read reports again can only come from the invocation THAT call
        // caused.
        cleared(&emulator, case, &rule).await;
        evaluate_now(&emulator, case, &rule).await;
        let reevaluated = evaluated(&emulator, case, &rule).await;
        assert_eq!(
            reevaluated, expected,
            "case {case:?}: expected StartConfigRulesEvaluation to have the rule evaluated \
             again, got {reevaluated:?}"
        );
    }
}
