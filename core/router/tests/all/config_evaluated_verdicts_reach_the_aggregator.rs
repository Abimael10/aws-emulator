// SPDX-License-Identifier: CC0-1.0

//! What the aggregator reports about an evaluation AWS Config itself performed.
//!
//! The rule under test: **an evaluation result recorded for a Config rule of this account and
//! Region is projected by EVERY read of it — the rule's own compliance details, the aggregate
//! details for that source, and the roll-ups the local and the aggregate summaries are both
//! derived from — whichever evaluator produced it: the Lambda function a `CUSTOM_LAMBDA` rule
//! points at, or a managed rule a conformance pack's deployment created. So a summary reports
//! no count the details do not support, and a deployed conformance pack is counted once in the
//! aggregate conformance-pack summary, in the bucket its own compliance status names.**
//!
//! Nothing here computes a verdict of its own: the `COMPLIANT`/`NON_COMPLIANT`/`NOT_APPLICABLE`
//! each row asserts is the one its own evaluator decided — in Python for the custom-Lambda
//! rows, and from the password policy IAM holds for the account for the conformance-pack rows —
//! and every count asserted is computed from the row set rather than read back out of the
//! answer being checked.
//!
//! The suite lives here because no single crate can see the whole path: CloudFormation
//! archives the evaluator's inline source into its deployment package, Lambda runs it, IAM
//! holds the recorder's role and the account's password policy, and Config invokes, records and
//! projects. Every service is mounted on ONE engine, exactly as `core/router/src/lib.rs` mounts
//! them.
//!
//! The rows differ along what the rule generalises over: which verdict the evaluator delivers,
//! which evaluator produced it, and — for the pack rows — whether the account's configuration
//! meets the requirement the pack's rule states at all.

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

/// The trigger the custom-Lambda rows declare, and so the `messageType` the invoking event
/// Config hands the evaluator carries (`SourceDetail$MessageType`).
const SCHEDULED_NOTIFICATION: &str = "ScheduledNotification";

/// The `MaximumExecutionFrequency` the periodic rows declare
/// (`com.amazonaws.configservice#MaximumExecutionFrequency`).
const TWENTY_FOUR_HOURS: &str = "TwentyFour_Hours";

/// The managed rule the conformance-pack rows deploy, a `Source$SourceIdentifier` value from
/// the Config managed-rule list: it "checks if the account password policy for users meets the
/// specified requirements indicated in the parameters".
const IAM_PASSWORD_POLICY: &str = "IAM_PASSWORD_POLICY";

/// The resource type a conformance pack's template declares a Config rule as.
const CONFIG_RULE_TYPE: &str = "AWS::Config::ConfigRule";

/// The most characters a rule name Config itself generates may carry. A rule's own name is a
/// `ConfigRuleName` (1..128), but `GetComplianceDetailsByConfigRule$ConfigRuleName` takes a
/// `StringWithCharLimit64` (`smithy_query(query="operation",
/// operation="GetComplianceDetailsByConfigRule")`) — the tightest published bound on a member
/// that names a Config rule — so a name the service mints has to fit it to be readable at all.
const GENERATED_RULE_NAME_MAX: usize = 64;

/// The aggregator every row reads the aggregate surfaces through.
const AGGREGATOR: &str = "every-source";

/// The `StackStatus` a stack that deployed everything it declares settles at.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The `State` Lambda serves invocations in.
const FUNCTION_ACTIVE: &str = "Active";

/// The `ConformancePackState` a deployed pack settles at.
const PACK_CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The logical id the evaluator functions' execution role is declared under.
const EVALUATOR_ROLE_ID: &str = "EvaluatorRole";

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
/// active, an evaluation pass, an invocation and the delivery it makes. Many passes' worth of
/// slack, not a span any behaviour here depends on.
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

    /// Create the configuration recorder's own role in IAM, trusting the Config service
    /// principal, and answer its ARN.
    ///
    /// With IAM mounted beside Config, a delivery channel is taken only while IAM shows Config
    /// can assume the recorder's role, so every row that puts a recorder needs this role to
    /// exist.
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

    /// Put the configuration recorder a Config rule needs — "you must create a configuration
    /// recorder before you can create or update a Config rule" — on a role IAM holds.
    async fn given_recorder(&self, case: &str) {
        let role = self.recorder_role(case).await;
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

    /// The same recorder, with the delivery channel a recorder needs and recording STARTED,
    /// which is what a conformance pack is deployed onto.
    async fn given_recording(&self, case: &str) {
        self.given_recorder(case).await;
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
    }

    /// Create the aggregator every aggregate read below goes through: one account aggregation
    /// source naming this account and this Region, which is the source this store answers for.
    async fn given_aggregator(&self, case: &str) {
        self.config_ok(
            case,
            "PutConfigurationAggregator",
            &json!({
                "ConfigurationAggregatorName": AGGREGATOR,
                "AccountAggregationSources": [{
                    "AccountIds": [account()],
                    "AwsRegions": [REGION],
                }],
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
/// The verdict delivered is `verdict`; the annotation is the `messageType` of the event the
/// function was handed, and the subject is the configuration item that event carried or the
/// account when it carried none — so what Config handed the function is observable in what
/// Config then reports. A handler reaching this with no `resultToken` raises, which fails the
/// row rather than passing it with a verdict nothing asked for.
const EVALUATOR_SOURCE: &str = r#"
import json

import boto3

CONFIG = boto3.client("config")


def handler(event, context):
    token = event["resultToken"]
    invoking = json.loads(event["invokingEvent"])
    item = invoking.get("configurationItem")
    if item is None:
        resource_type = "ACCOUNT_RESOURCE_TYPE"
        resource_id = event["accountId"]
    else:
        resource_type = item["resourceType"]
        resource_id = item["resourceId"]
    CONFIG.put_evaluations(
        Evaluations=[
            {
                "ComplianceResourceType": resource_type,
                "ComplianceResourceId": resource_id,
                "ComplianceType": "VERDICT",
                "Annotation": invoking["messageType"],
                "OrderingTimestamp": 0,
            }
        ],
        ResultToken=token,
    )
    return {"delivered": "VERDICT"}
"#;

/// [`EVALUATOR_SOURCE`] with the verdict this evaluator delivers written into it.
fn evaluator_source(verdict: &str) -> String {
    EVALUATOR_SOURCE
        .replace("ACCOUNT_RESOURCE_TYPE", ACCOUNT_RESOURCE_TYPE)
        .replace("VERDICT", verdict)
}

/// The function one row's evaluator is declared under.
fn function_name(index: usize) -> String {
    format!("rule-evaluator-{index}")
}

/// The Config rule one row writes.
fn rule_name(index: usize) -> String {
    format!("evaluated-rule-{index}")
}

/// The stack output one row's evaluator ARN is published as.
fn arn_output(index: usize) -> String {
    format!("Evaluator{index}Arn")
}

/// The stack the custom-Lambda rows deploy: the execution role every evaluator runs as, one
/// function per row carrying that row's inline source, and the resource-policy statement that
/// admits Config as the invoker of each — the grant Config needs before it will take a rule
/// pointing at the function at all. Each function's ARN is published as a stack output, so the
/// rule is written against the ARN Lambda itself minted.
fn evaluator_stack(verdicts: &[(&str, &str)]) -> String {
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
    for (index, (_, verdict)) in verdicts.iter().enumerate() {
        let function_id = format!("Evaluator{index}");
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
                    "Code": { "ZipFile": evaluator_source(verdict) },
                },
            }),
        );
        resources.insert(
            format!("Invoke{index}"),
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
        outputs.insert(
            arn_output(index),
            json!({ "Value": { "Fn::GetAtt": [function_id, "Arn"] } }),
        );
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "the evaluators of this account's Custom Lambda Config rules",
        "Resources": Value::Object(resources),
        "Outputs": Value::Object(outputs),
    })
    .to_string()
}

/// Deploy `stack` and answer its `DescribeStacks` body once it has settled at
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
/// first evaluation pass reaches a function that can serve it.
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

/// One evaluation result, as the members every compliance read reports it by:
/// `(ComplianceType, Annotation, ResourceType, ResourceId)`.
type ReportedResult = (String, String, String, String);

/// `results`, each an `EvaluationResult`-shaped value, as [`ReportedResult`] rows.
fn rows_of(results: &[Value]) -> Vec<ReportedResult> {
    results
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

/// The rule `rule`'s own evaluation results, read until it has at least one.
async fn own_details(emulator: &Emulator, case: &str, rule: &str) -> Vec<ReportedResult> {
    let deadline = Instant::now() + BUDGET;
    loop {
        let body = emulator
            .config_ok(
                case,
                "GetComplianceDetailsByConfigRule",
                &json!({ "ConfigRuleName": rule }),
            )
            .await;
        let results = body["EvaluationResults"].as_array().cloned();
        if let Some(results) = results.filter(|results| !results.is_empty()) {
            return rows_of(&results);
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the rule {rule:?} to be evaluated, got {body}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// The same rows the AGGREGATE details report for `rule`, each checked to name the source
/// account and Region it was aggregated from.
async fn aggregate_details(emulator: &Emulator, case: &str, rule: &str) -> Vec<ReportedResult> {
    let body = emulator
        .config_ok(
            case,
            "GetAggregateComplianceDetailsByConfigRule",
            &json!({
                "ConfigurationAggregatorName": AGGREGATOR,
                "ConfigRuleName": rule,
                "AccountId": account(),
                "AwsRegion": REGION,
            }),
        )
        .await;
    let results = body["AggregateEvaluationResults"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    for result in &results {
        assert_eq!(
            (result["AccountId"].as_str(), result["AwsRegion"].as_str()),
            (Some(account().as_str()), Some(REGION)),
            "case {case:?}: every aggregate result names the source it came from, got {result}"
        );
    }
    rows_of(&results)
}

/// The `(CompliantResourceCount, NonCompliantResourceCount)` of one `ComplianceSummary`, as the
/// capped counts it reports them.
fn counts_of(case: &str, summary: &Value) -> (i64, i64) {
    let capped = |member: &str| {
        let count = &summary[member];
        assert_eq!(
            count["CapExceeded"],
            json!(false),
            "case {case:?}: expected {member} to be under the cap, got {count}"
        );
        count["CappedCount"].as_i64().unwrap_or_else(|| {
            panic!("case {case:?}: expected a CappedCount in {member}, got {count}")
        })
    };
    (
        capped("CompliantResourceCount"),
        capped("NonCompliantResourceCount"),
    )
}

#[tokio::test]
async fn a_custom_lambda_rules_delivered_verdict_is_what_every_compliance_read_reports() {
    // (case, the verdict this row's evaluator delivers). The rows differ along that verdict,
    // which is what decides both what the details report and which of the summary's two counts
    // the rule contributes to — `NOT_APPLICABLE` contributing to neither.
    let rows: [(&str, &str); 4] = [
        (
            "an evaluator that finds the account non-compliant",
            NON_COMPLIANT,
        ),
        ("an evaluator that finds the account compliant", COMPLIANT),
        (
            "a second evaluator that finds the account non-compliant",
            NON_COMPLIANT,
        ),
        (
            "an evaluator that reports the account's data is not applicable",
            NOT_APPLICABLE,
        ),
    ];
    let suite = "a verdict delivered by each rule's own evaluator";

    let emulator = Emulator::new();
    emulator.given_recorder(suite).await;
    emulator.given_aggregator(suite).await;

    // One stack for the whole row set: each row's evaluator is its own function, with its own
    // inline source and its own invoke grant.
    let stack = deployed(&emulator, suite, "evaluators", &evaluator_stack(&rows)).await;
    let outputs = outputs_of(&stack);

    for (index, (case, verdict)) in rows.iter().enumerate() {
        active(&emulator, case, &function_name(index)).await;
        let function = outputs.get(&arn_output(index)).unwrap_or_else(|| {
            panic!("case {case:?}: expected the stack to publish {}", arn_output(index))
        });
        let rule = rule_name(index);
        emulator
            .config_ok(
                case,
                "PutConfigRule",
                &json!({
                    "ConfigRule": {
                        "ConfigRuleName": rule,
                        "Source": {
                            "Owner": "CUSTOM_LAMBDA",
                            "SourceIdentifier": function,
                            "SourceDetails": [{
                                "EventSource": "aws.config",
                                "MessageType": SCHEDULED_NOTIFICATION,
                                "MaximumExecutionFrequency": TWENTY_FOUR_HOURS,
                            }],
                        },
                        "MaximumExecutionFrequency": TWENTY_FOUR_HOURS,
                    }
                }),
            )
            .await;

        // The pass the WRITE of the rule owes invokes the evaluator, and the verdict it
        // delivered is what the rule's own details report: one result, about the account, with
        // the annotation naming the trigger whose event the function was handed.
        let expected = vec![(
            (*verdict).to_string(),
            SCHEDULED_NOTIFICATION.to_string(),
            ACCOUNT_RESOURCE_TYPE.to_string(),
            account(),
        )];
        let own = own_details(&emulator, case, &rule).await;
        assert_eq!(
            own, expected,
            "case {case:?}: expected the delivered verdict, got {own:?}"
        );

        // The aggregate details of the source the data came from report the SAME record: one
        // result, two reports of it.
        let aggregated = aggregate_details(&emulator, case, &rule).await;
        assert_eq!(
            aggregated, own,
            "case {case:?}: expected the aggregate details to report the rule's own results \
             {own:?}, got {aggregated:?}"
        );
    }

    // The roll-ups are derived from exactly those results, so each count is the number of rows
    // whose verdict places their rule that way — computed here from the row set, never read out
    // of the answer being checked.
    let compliant = rows.iter().filter(|(_, v)| *v == COMPLIANT).count() as i64;
    let non_compliant = rows.iter().filter(|(_, v)| *v == NON_COMPLIANT).count() as i64;

    let summary = emulator
        .config_ok(
            suite,
            "GetAggregateConfigRuleComplianceSummary",
            &json!({
                "ConfigurationAggregatorName": AGGREGATOR,
                "Filters": { "AccountId": account(), "AwsRegion": REGION },
            }),
        )
        .await;
    let groups = summary["AggregateComplianceCounts"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        groups.len(),
        1,
        "case {suite:?}: expected exactly one group for this account, got {groups:?}"
    );
    assert_eq!(
        groups[0]["GroupName"].as_str(),
        Some(account().as_str()),
        "case {suite:?}: expected the group to name the source account, got {}",
        groups[0]
    );
    let aggregate_counts = counts_of(suite, &groups[0]["ComplianceSummary"]);
    assert_eq!(
        aggregate_counts,
        (compliant, non_compliant),
        "case {suite:?}: expected the aggregate summary to count the rules the details place, \
         got {aggregate_counts:?}"
    );

    // And the account's own roll-up of the same results cannot disagree with it.
    let local = emulator
        .config_ok(suite, "GetComplianceSummaryByConfigRule", &json!({}))
        .await;
    let local_counts = counts_of(suite, &local["ComplianceSummary"]);
    assert_eq!(
        local_counts, aggregate_counts,
        "case {suite:?}: expected the account's own summary to agree with the aggregate one \
         {aggregate_counts:?}, got {local_counts:?}"
    );
}

/// A conformance pack template declaring ONE Config rule of the managed rule
/// `source_identifier`, carrying `parameters` as its `InputParameters` when the row states any.
fn pack_template(rule: &str, source_identifier: &str, parameters: Option<&Value>) -> String {
    let mut body = format!(
        "Resources:\n  Rule0:\n    Type: {CONFIG_RULE_TYPE}\n    Properties:\n      \
         ConfigRuleName: {rule}\n      Source:\n        Owner: AWS\n        \
         SourceIdentifier: {source_identifier}\n"
    );
    if let Some(parameters) = parameters {
        body.push_str(&format!("      InputParameters: '{parameters}'\n"));
    }
    body
}

/// Deploy `pack` from a template declaring one rule, and answer the pack's own id once the
/// deployment the clock owes has settled.
async fn deployed_pack(
    emulator: &Emulator,
    case: &str,
    pack: &str,
    rule: &str,
    parameters: Option<&Value>,
) -> String {
    emulator
        .config_ok(
            case,
            "PutConformancePack",
            &json!({
                "ConformancePackName": pack,
                "TemplateBody": pack_template(rule, IAM_PASSWORD_POLICY, parameters),
            }),
        )
        .await;
    let deadline = Instant::now() + BUDGET;
    loop {
        let body = emulator
            .config_ok(
                case,
                "DescribeConformancePackStatus",
                &json!({ "ConformancePackNames": [pack] }),
            )
            .await;
        let detail = &body["ConformancePackStatusDetails"][0];
        if detail["ConformancePackState"] == json!(PACK_CREATE_COMPLETE) {
            let id = detail["ConformancePackId"].as_str().unwrap_or_else(|| {
                panic!("case {case:?}: expected the pack's own id, got {body}")
            });
            return id.to_string();
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the pack's deployment to settle, got {body}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// The name Config gave the rule a pack deployed, checked for the form such a name has to
/// take: the name the template DECLARED for it, suffixed with the pack's own id so two packs
/// declaring one rule name cannot deploy one rule over the other, and short enough for every
/// path that takes a rule name — `GetComplianceDetailsByConfigRule$ConfigRuleName` is a
/// `StringWithCharLimit64`, the tightest published bound, so a name the service mints and then
/// cannot accept back names a rule no caller can read the evaluations of.
fn checked_pack_rule_name(case: &str, reported: &str, declared: &str, pack_id: &str) -> String {
    let suffix = format!("-{pack_id}");
    let head = reported.strip_suffix(&suffix).unwrap_or_else(|| {
        panic!("case {case:?}: expected the rule name {reported:?} to carry the pack's id as {suffix:?}")
    });
    assert!(
        declared.starts_with(head),
        "case {case:?}: expected the rule name to keep the declared {declared:?}, got {head:?}"
    );
    assert!(
        reported.chars().count() <= GENERATED_RULE_NAME_MAX,
        "case {case:?}: expected a name of at most {GENERATED_RULE_NAME_MAX} characters, got \
         {reported:?}"
    );
    reported.to_string()
}

/// The pack's own evaluation results, read until it has at least one.
async fn pack_details(emulator: &Emulator, case: &str, pack: &str) -> Vec<Value> {
    let deadline = Instant::now() + BUDGET;
    loop {
        let body = emulator
            .config_ok(
                case,
                "GetConformancePackComplianceDetails",
                &json!({ "ConformancePackName": pack }),
            )
            .await;
        let results = body["ConformancePackRuleEvaluationResults"].as_array().cloned();
        if let Some(results) = results.filter(|results| !results.is_empty()) {
            return results;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the pack's rule to be evaluated, got {body}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// One row of the conformance-pack table: what it is called, the password policy to put in
/// force as `UpdateAccountPasswordPolicy` members (none at all for an account that never set
/// one), the requirements the pack's rule states, and the compliance the account's
/// configuration places the pack at.
type PackCase = (
    &'static str,
    &'static [(&'static str, &'static str)],
    Option<Value>,
    &'static str,
);

#[tokio::test]
async fn a_deployed_packs_rule_is_counted_once_by_the_aggregate_conformance_pack_summary() {
    // (case, the password policy to put in force as `UpdateAccountPasswordPolicy` members, the
    // requirements the pack's rule states, the verdict that account configuration places the
    // pack at). The rows differ along whether the account has a password policy at all, which
    // requirement the rule states, and whether the policy in force meets it. Each parameter
    // name is one `IAM_PASSWORD_POLICY` takes, as the managed rule's documentation spells it.
    let rows: [PackCase; 5] = [
        (
            "no password policy at all, and no requirement stated",
            &[],
            None,
            NON_COMPLIANT,
        ),
        (
            "a policy in force, with no requirement stated",
            &[("MinimumPasswordLength", "8")],
            None,
            COMPLIANT,
        ),
        (
            "a minimum length the policy meets exactly",
            &[("MinimumPasswordLength", "14")],
            Some(json!({ "MinimumPasswordLength": "14" })),
            COMPLIANT,
        ),
        (
            "a minimum length the policy falls short of",
            &[("MinimumPasswordLength", "8")],
            Some(json!({ "MinimumPasswordLength": "14" })),
            NON_COMPLIANT,
        ),
        (
            "a character class the policy does not require",
            &[("RequireNumbers", "true")],
            Some(json!({ "RequireSymbols": "true" })),
            NON_COMPLIANT,
        ),
    ];

    for (case, policy, parameters, expected) in rows {
        // One account's configuration per row: the password policy is account-wide, so each row
        // gets its own store.
        let emulator = Emulator::new();
        if !policy.is_empty() {
            emulator
                .query_ok(
                    case,
                    "iam",
                    "2010-05-08",
                    "UpdateAccountPasswordPolicy",
                    policy,
                )
                .await;
        }
        emulator.given_recording(case).await;
        emulator.given_aggregator(case).await;

        let pack = "password-policy-pack";
        let declared_rule = "account-password-policy";
        let pack_id = deployed_pack(&emulator, case, pack, declared_rule, parameters.as_ref()).await;

        // One rule, one subject: the account. So the pack reports exactly one result, and it
        // carries the verdict the account's own configuration places that rule at.
        let results = pack_details(&emulator, case, pack).await;
        assert_eq!(
            results.len(),
            1,
            "case {case:?}: expected one evaluation result, got {results:?}"
        );
        assert_eq!(
            results[0]["ComplianceType"].as_str(),
            Some(expected),
            "case {case:?}: expected the pack's rule to report {expected}, got {:?}",
            results[0]
        );
        // The rule the result is about is the one the pack's deployment created, named the way
        // such a name has to be.
        let reported = results[0]["EvaluationResultIdentifier"]["EvaluationResultQualifier"]
            ["ConfigRuleName"]
            .as_str()
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected the result to name its rule, got {:?}",
                    results[0]
                )
            });
        let rule = checked_pack_rule_name(case, reported, declared_rule, &pack_id);

        // The pack's own compliance reads are the roll-up of exactly those results, so neither
        // can disagree with them.
        let summary = emulator
            .config_ok(
                case,
                "GetConformancePackComplianceSummary",
                &json!({ "ConformancePackNames": [pack] }),
            )
            .await;
        assert_eq!(
            summary["ConformancePackComplianceSummaryList"][0]["ConformancePackComplianceStatus"]
                .as_str(),
            Some(expected),
            "case {case:?}: expected the pack's compliance status to be {expected}, got {summary}"
        );
        let described = emulator
            .config_ok(
                case,
                "DescribeConformancePackCompliance",
                &json!({ "ConformancePackName": pack }),
            )
            .await;
        let per_rule = described["ConformancePackRuleComplianceList"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            per_rule.len(),
            1,
            "case {case:?}: expected one member rule's compliance, got {per_rule:?}"
        );
        assert_eq!(
            (
                per_rule[0]["ConfigRuleName"].as_str(),
                per_rule[0]["ComplianceType"].as_str(),
            ),
            (Some(rule.as_str()), Some(expected)),
            "case {case:?}: expected the member rule {rule:?} to report {expected}, got {:?}",
            per_rule[0]
        );

        // And the aggregator counts this account's ONE pack once, in the bucket the pack's own
        // status names — the counts computed here from what the row declares.
        let (compliant, non_compliant) = if expected == COMPLIANT { (1, 0) } else { (0, 1) };
        let aggregate = emulator
            .config_ok(
                case,
                "GetAggregateConformancePackComplianceSummary",
                &json!({
                    "ConfigurationAggregatorName": AGGREGATOR,
                    "Filters": { "AccountId": account(), "AwsRegion": REGION },
                }),
            )
            .await;
        let groups = aggregate["AggregateConformancePackComplianceSummaries"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            groups.len(),
            1,
            "case {case:?}: expected exactly one group for this account, got {groups:?}"
        );
        assert_eq!(
            groups[0]["GroupName"].as_str(),
            Some(account().as_str()),
            "case {case:?}: expected the group to name the source account, got {}",
            groups[0]
        );
        let counted = (
            groups[0]["ComplianceSummary"]["CompliantConformancePackCount"]
                .as_i64()
                .unwrap_or_default(),
            groups[0]["ComplianceSummary"]["NonCompliantConformancePackCount"]
                .as_i64()
                .unwrap_or_default(),
        );
        assert_eq!(
            counted,
            (compliant, non_compliant),
            "case {case:?}: expected the aggregate pack summary to count the pack as its own \
             status reports it, got {counted:?}"
        );
    }
}
