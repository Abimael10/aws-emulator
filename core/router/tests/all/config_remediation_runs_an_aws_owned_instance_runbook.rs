// SPDX-License-Identifier: CC0-1.0

//! What a Config remediation whose target is an AWS-OWNED automation document DOES to the
//! Amazon EC2 instance it names.
//!
//! RULE: a remediation execution reaches a terminal `RemediationExecutionState` only from
//! the Systems Manager automation execution that RUNS it, so a `SUCCEEDED` execution means
//! that service really ran the document the remediation configuration names — the AWS-owned
//! `AWS-*` runbooks included — and the document's own steps have already landed their effect
//! in the service that OWNS the resource. For the published runbooks whose step changes an
//! instance's state, that effect is Amazon EC2 reporting the instance at the state the
//! runbook asked for by the time Config reports `SUCCEEDED`.
//!
//! This suite lives beside the router because the rule spans three services: the finding
//! comes from Config, the run exists only where Systems Manager holds it, and the instance
//! belongs to Amazon EC2 — the only component that can move it between states and the only
//! one that can say which state it is in. A single-service test cannot see the hand-off at
//! all, and a `SUCCEEDED` record with no side effect is exactly what one would miss.
//!
//! The rows differ along what the rule generalises over: which AWS-owned runbook the
//! configuration names, how many state-changing steps that runbook declares (one, two),
//! which state the instance is in when the remediation starts, and which state the run
//! leaves it at. Every row also drives the tail of the same contract: deleting the
//! configuration leaves the rule with none, and a start against a rule that has none is
//! refused.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{block_of, element, elements, form_encode};

/// The Region every request below is signed for: a configuration recorder records the
/// resources of its own Region, and the launches use the same one.
const REGION: &str = "us-west-2";

/// The canonical emulator account (`engine::EMULATOR_ACCOUNT_ID`) every ARN below carries.
const ACCOUNT_ID: &str = "000000000000";

/// The EC2 API version an ec2Query body carries.
const EC2_API_VERSION: &str = "2016-11-15";

/// The AMI the launches below run from, in the long form EC2 issues AMI ids in.
const AMI: &str = "ami-0123456789abcdef0";

/// The instance type the launches name.
const INSTANCE_TYPE: &str = "t3.micro";

/// The resource type Config records an Amazon EC2 instance under — an entry of
/// `com.amazonaws.configservice#ResourceType`.
const INSTANCE_RESOURCE_TYPE: &str = "AWS::EC2::Instance";

/// The only published `RemediationTargetType`: an SSM automation document
/// (`com.amazonaws.configservice#RemediationTargetType`, whose one value this is).
const TARGET_TYPE: &str = "SSM_DOCUMENT";

/// The recorder every row writes.
const RECORDER_NAME: &str = "the-recorder";

/// The Config rule every row writes, and the AWS managed rule it runs: "Checks if your
/// resources have the tags that you specify", which places a resource carrying none of the
/// tags the rule names at `NON_COMPLIANT`.
const RULE_NAME: &str = "instance-guardrail";
/// See [`RULE_NAME`].
const REQUIRED_TAGS_RULE: &str = "REQUIRED_TAGS";

/// The tag key the rule requires, which no instance below carries — so the rule FINDS the
/// instance, which is what a remediation acts on.
const REQUIRED_TAG_KEY: &str = "guardrail-marker";

/// The `Parameters` keys the published instance runbooks declare: the instance they act on,
/// and the role the automation runs as.
const INSTANCE_ID_PARAMETER: &str = "InstanceId";
/// See [`INSTANCE_ID_PARAMETER`].
const AUTOMATION_ASSUME_ROLE: &str = "AutomationAssumeRole";

/// The `ResourceValue` that resolves to the resource the execution is about
/// (`com.amazonaws.configservice#ResourceValueType`, whose one value this is).
const RESOURCE_ID: &str = "RESOURCE_ID";

/// The `ComplianceType` the rule places an instance carrying none of the required tags at.
const NON_COMPLIANT: &str = "NON_COMPLIANT";

/// The terminal `RemediationExecutionState` values
/// (`com.amazonaws.configservice#RemediationExecutionState`).
const SUCCEEDED: &str = "SUCCEEDED";
/// See [`SUCCEEDED`].
const FAILED: &str = "FAILED";

/// The `AutomationExecutionStatus` an automation that ended well reports
/// (`com.amazonaws.ssm#AutomationExecutionStatus`).
const AUTOMATION_SUCCESS: &str = "Success";

/// `InstanceStateName` values this suite names
/// (`com.amazonaws.ec2#InstanceStateName`, whose members are `pending`, `running`,
/// `shutting-down`, `terminated`, `stopping` and `stopped`).
const RUNNING: &str = "running";
/// See [`RUNNING`].
const STOPPED: &str = "stopped";
/// See [`RUNNING`].
const TERMINATED: &str = "terminated";

/// The sentence Config refuses a start for a rule with no remediation configuration with,
/// whose only variable part is the rule the request named. Spelled out here rather than
/// imported from the service crate, so a change to the wording fails this suite.
fn no_remediation_configuration_message(config_rule_name: &str) -> String {
    format!(
        "There is no applicable RemediationConfiguration setup for AWS Config rule \
         {config_rule_name}"
    )
}

/// How long a poll waits for a condition the clock owes — a launch reaching its state, a
/// recording pass, an evaluation, an automation being picked up and walked. Generous by
/// design: it is a deadline, not a measurement, and nothing below asserts how long anything
/// took.
const CONVERGENCE_BUDGET: Duration = Duration::from_secs(60);

/// How long a poll waits between two reads.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// One row: the AWS-owned runbook the rule's remediation names, the state the instance is
/// brought to before the remediation runs, and the state the run leaves it at.
struct Case {
    /// What the row is called in every assertion of it.
    case: &'static str,
    /// The `TargetId` of the remediation configuration: the document it runs.
    target_id: &'static str,
    /// The `InstanceStateName` the instance is in when the remediation starts.
    starts_from: &'static str,
    /// The `InstanceStateName` Amazon EC2 must report once Config reports the execution
    /// `SUCCEEDED`: the state the runbook's last state-changing step asked for.
    leaves_at: &'static str,
}

/// Config, Amazon EC2 and Systems Manager over ONE shared engine, as the assembled router
/// mounts them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_configservice_starlingdoveservice::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(service_com_amazonaws_ssm_amazonssm::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name`, which is what
/// routes the request and which Region it is served in; the signature is never checked.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}\
         /aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One form field of an ec2Query body.
fn param(name: &str, value: &str) -> (String, String) {
    (name.to_string(), value.to_string())
}

/// POST one ec2Query action that must succeed, answered with its XML body.
async fn ec2(router: &MultiServiceRouter, action: &str, params: &[(String, String)]) -> String {
    let mut body = format!("Action={action}&Version={EC2_API_VERSION}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("ec2.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("ec2"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let answered = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "EC2 {action} must succeed, got {status} with {answered}"
    );
    answered
}

/// One awsJson1.1 RPC call, as its status and parsed body.
async fn rpc(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("authorization", authorization(signing_name))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One Config call, required to answer 200.
async fn config(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let (status, parsed) = rpc(router, "config", "StarlingDoveService", operation, body).await;
    assert_eq!(
        status, 200,
        "{operation}: expected HTTP 200, got {status} with {parsed}"
    );
    parsed
}

/// One Config call required to be REFUSED, as its status, error code and error message.
async fn config_refusal(
    router: &MultiServiceRouter,
    operation: &str,
    body: &Value,
) -> (u16, String, String) {
    let (status, parsed) = rpc(router, "config", "StarlingDoveService", operation, body).await;
    let code = parsed
        .get("__type")
        .and_then(Value::as_str)
        .map(|wire| wire.rsplit('#').next().unwrap_or(wire).to_string())
        .unwrap_or_else(|| format!("<no __type in {parsed}>"));
    let message = parsed
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    (status, code, message)
}

/// One Systems Manager call, required to answer 200. `AmazonSSM` is that service's
/// `X-Amz-Target` prefix (its crate's `OPERATIONS.md`).
async fn ssm(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let (status, parsed) = rpc(router, "ssm", "AmazonSSM", operation, body).await;
    assert_eq!(
        status, 200,
        "{operation}: expected HTTP 200, got {status} with {parsed}"
    );
    parsed
}

/// Launch ONE instance through EC2's own `RunInstances` and answer the id that service
/// minted. Config is told nothing about it: the recorder discovers it.
async fn launch(router: &MultiServiceRouter) -> String {
    let launched = ec2(
        router,
        "RunInstances",
        &[
            param("ImageId", AMI),
            param("InstanceType", INSTANCE_TYPE),
            param("MinCount", "1"),
            param("MaxCount", "1"),
        ],
    )
    .await;
    element(&launched, "instanceId")
}

/// The `InstanceState$Name` Amazon EC2 reports for `instance_id`, absent while that service
/// reports no state for it.
///
/// Read from the service that OWNS the instance, which is the only component that can say
/// which state it is in.
async fn instance_state(router: &MultiServiceRouter, instance_id: &str) -> Option<String> {
    let described = ec2(
        router,
        "DescribeInstances",
        &[param("InstanceId.1", instance_id)],
    )
    .await;
    let state_block = block_of(&described, "instanceState")?;
    elements(state_block, "name").into_iter().next()
}

/// Wait until Amazon EC2 reports `instance_id` at `wanted`.
///
/// What is waited for is that service's own transition, never a span: the assertion that
/// fails at the deadline names the state it last reported.
async fn await_instance_state(
    case: &str,
    router: &MultiServiceRouter,
    instance_id: &str,
    wanted: &str,
) {
    let deadline = Instant::now() + CONVERGENCE_BUDGET;
    loop {
        let reported = instance_state(router, instance_id).await;
        if reported.as_deref() == Some(wanted) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: Amazon EC2 must report {instance_id} at {wanted}, got {reported:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Put a recorder recording every supported type, with the delivery channel a recorder
/// needs, and start it.
async fn start_recording(router: &MultiServiceRouter) {
    config(
        router,
        "PutConfigurationRecorder",
        &json!({
            "ConfigurationRecorder": {
                "name": RECORDER_NAME,
                "roleARN": format!("arn:aws:iam::{ACCOUNT_ID}:role/config-role"),
                "recordingGroup": { "allSupported": true },
            }
        }),
    )
    .await;
    config(
        router,
        "PutDeliveryChannel",
        &json!({ "DeliveryChannel": { "s3BucketName": "config-delivery" } }),
    )
    .await;
    config(
        router,
        "StartConfigurationRecorder",
        &json!({ "ConfigurationRecorderName": RECORDER_NAME }),
    )
    .await;
}

/// The `PutConfigRule` body for the managed rule under test, scoped to `instance_id`.
fn instance_rule(instance_id: &str) -> Value {
    json!({
        "ConfigRule": {
            "ConfigRuleName": RULE_NAME,
            "Source": { "Owner": "AWS", "SourceIdentifier": REQUIRED_TAGS_RULE },
            "Scope": {
                "ComplianceResourceTypes": [INSTANCE_RESOURCE_TYPE],
                "ComplianceResourceId": instance_id,
            },
            "InputParameters": json!({ "tag1Key": REQUIRED_TAG_KEY }).to_string(),
        }
    })
}

/// The `PutRemediationConfigurations` body for `target_id`: the instance comes from the
/// resource value and the role from a static value, which is how the two parameters the
/// published instance runbooks declare are supplied.
fn remediation(target_id: &str) -> Value {
    json!({
        "RemediationConfigurations": [{
            "ConfigRuleName": RULE_NAME,
            "TargetType": TARGET_TYPE,
            "TargetId": target_id,
            "ResourceType": INSTANCE_RESOURCE_TYPE,
            "Parameters": {
                INSTANCE_ID_PARAMETER: { "ResourceValue": { "Value": RESOURCE_ID } },
                AUTOMATION_ASSUME_ROLE: {
                    "StaticValue": {
                        "Values": [format!("arn:aws:iam::{ACCOUNT_ID}:role/config-remediation")],
                    },
                },
            },
        }],
    })
}

/// The `StartRemediationExecution` body for `instance_id`.
fn start_body(instance_id: &str) -> Value {
    json!({
        "ConfigRuleName": RULE_NAME,
        "ResourceKeys": [{
            "resourceType": INSTANCE_RESOURCE_TYPE,
            "resourceId": instance_id,
        }],
    })
}

/// Wait until the rule places `instance_id` at `NON_COMPLIANT`, which is the finding a
/// remediation acts on.
async fn await_finding(case: &str, router: &MultiServiceRouter, instance_id: &str) {
    let deadline = Instant::now() + CONVERGENCE_BUDGET;
    loop {
        let body = config(
            router,
            "GetComplianceDetailsByResource",
            &json!({
                "ResourceType": INSTANCE_RESOURCE_TYPE,
                "ResourceId": instance_id,
            }),
        )
        .await;
        let placed: Vec<String> = body["EvaluationResults"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|result| result["ComplianceType"].as_str().map(ToString::to_string))
            .collect();
        if placed.iter().any(|verdict| verdict == NON_COMPLIANT) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the rule must place {instance_id} at {NON_COMPLIANT}, got {placed:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Wait until the rule's remediation execution for `instance_id` reports a TERMINAL state,
/// and answer the execution that reported it.
///
/// Terminal rather than `SUCCEEDED`: a run that failed is reported as the failure it is by
/// the caller's own assertion, rather than waited out to the deadline.
async fn await_settled_execution(
    case: &str,
    router: &MultiServiceRouter,
    instance_id: &str,
) -> Value {
    let deadline = Instant::now() + CONVERGENCE_BUDGET;
    loop {
        let body = config(
            router,
            "DescribeRemediationExecutionStatus",
            &json!({ "ConfigRuleName": RULE_NAME }),
        )
        .await;
        let reported = body["RemediationExecutionStatuses"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let mine = reported
            .iter()
            .find(|status| status["ResourceKey"]["resourceId"].as_str() == Some(instance_id))
            .cloned();
        if let Some(execution) = mine.as_ref() {
            let state = execution["State"].as_str().unwrap_or_default();
            if state == SUCCEEDED || state == FAILED {
                return execution.clone();
            }
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the remediation execution for {instance_id} must settle, got {mine:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The automation executions Systems Manager holds for the document `document`, each as the
/// status it is at.
async fn automation_statuses(router: &MultiServiceRouter, document: &str) -> Vec<String> {
    let body = ssm(
        router,
        "DescribeAutomationExecutions",
        // `DocumentNamePrefix` is the `AutomationExecutionFilterKey` that selects the
        // executions of one document.
        &json!({ "Filters": [{ "Key": "DocumentNamePrefix", "Values": [document] }] }),
    )
    .await;
    body["AutomationExecutionMetadataList"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|execution| {
            execution["AutomationExecutionStatus"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

#[tokio::test]
async fn a_succeeded_remediation_of_an_aws_owned_runbook_has_already_moved_the_instance() {
    // The runbooks exercised below are the AWS-owned automation documents of the "Systems
    // Manager Automation runbook reference" that act on an Amazon EC2 instance, each
    // declaring `InstanceId` and `AutomationAssumeRole` as its parameters:
    // `AWS-StartEC2Instance`, `AWS-StopEC2Instance`, `AWS-RestartEC2Instance` (whose two
    // state-changing steps stop the instance and start it again) and
    // `AWS-TerminateEC2Instance`.
    let cases: [Case; 4] = [
        Case {
            case: "the start runbook on a stopped instance",
            target_id: "AWS-StartEC2Instance",
            starts_from: STOPPED,
            leaves_at: RUNNING,
        },
        Case {
            case: "the stop runbook on a running instance",
            target_id: "AWS-StopEC2Instance",
            starts_from: RUNNING,
            leaves_at: STOPPED,
        },
        Case {
            case: "the restart runbook on a stopped instance",
            target_id: "AWS-RestartEC2Instance",
            starts_from: STOPPED,
            leaves_at: RUNNING,
        },
        Case {
            case: "the terminate runbook on a running instance",
            target_id: "AWS-TerminateEC2Instance",
            starts_from: RUNNING,
            leaves_at: TERMINATED,
        },
    ];

    for case in cases {
        let router = emulator();

        // The instance is launched and brought to the state the row starts from through
        // Amazon EC2's own API.
        let instance = launch(&router).await;
        await_instance_state(case.case, &router, &instance, RUNNING).await;
        if case.starts_from == STOPPED {
            ec2(
                &router,
                "StopInstances",
                &[param("InstanceId.1", &instance)],
            )
            .await;
            await_instance_state(case.case, &router, &instance, STOPPED).await;
        }

        start_recording(&router).await;
        config(&router, "PutConfigRule", &instance_rule(&instance)).await;
        await_finding(case.case, &router, &instance).await;

        let put = config(
            &router,
            "PutRemediationConfigurations",
            &remediation(case.target_id),
        )
        .await;
        assert_eq!(
            put["FailedBatches"].as_array().map(Vec::len),
            Some(0),
            "case {:?}: the configuration naming an AWS-owned document must be stored, got \
             {put}",
            case.case
        );

        let started = config(
            &router,
            "StartRemediationExecution",
            &start_body(&instance),
        )
        .await;
        assert_eq!(
            started["FailedItems"].as_array().map(Vec::len),
            Some(0),
            "case {:?}: the execution must start for {instance}, got {started}",
            case.case
        );

        // The pairing the rule is about: the instant the execution reports a terminal state,
        // the state Amazon EC2 reports is read WITHOUT waiting for anything — a SUCCEEDED
        // execution whose effect had not landed is caught here.
        let settled = await_settled_execution(case.case, &router, &instance).await;
        let state_at_settlement = instance_state(&router, &instance).await;
        assert_eq!(
            (
                settled["State"].as_str(),
                state_at_settlement.as_deref(),
                settled["StepDetails"][0]["Name"].as_str(),
            ),
            (Some(SUCCEEDED), Some(case.leaves_at), Some(case.target_id)),
            "case {:?}: a {SUCCEEDED} remediation of {} must come with Amazon EC2 reporting \
             {instance} at {}, got the execution {settled}",
            case.case,
            case.target_id,
            case.leaves_at
        );

        // The run itself exists where the service that owns the document holds it: ONE
        // automation execution of that document, which ended well.
        let statuses = automation_statuses(&router, case.target_id).await;
        assert_eq!(
            statuses,
            vec![AUTOMATION_SUCCESS.to_string()],
            "case {:?}: Systems Manager must hold one {AUTOMATION_SUCCESS} automation \
             execution of {}, got {statuses:?}",
            case.case,
            case.target_id
        );

        // The tail of the same contract: the configuration is DELETED, which leaves the rule
        // with none to read back and nothing to run.
        config(
            &router,
            "DeleteRemediationConfiguration",
            &json!({ "ConfigRuleName": RULE_NAME }),
        )
        .await;
        let described = config(
            &router,
            "DescribeRemediationConfigurations",
            &json!({ "ConfigRuleNames": [RULE_NAME] }),
        )
        .await;
        assert_eq!(
            described["RemediationConfigurations"]
                .as_array()
                .map(Vec::len),
            Some(0),
            "case {:?}: a deleted configuration must be read back as none, got {described}",
            case.case
        );
        let (status, code, message) = config_refusal(
            &router,
            "StartRemediationExecution",
            &start_body(&instance),
        )
        .await;
        assert_eq!(
            (status, code.as_str(), message.as_str()),
            (
                400,
                "NoSuchRemediationConfigurationException",
                no_remediation_configuration_message(RULE_NAME).as_str(),
            ),
            "case {:?}: a rule with no remediation configuration must refuse the start",
            case.case
        );
    }
}
