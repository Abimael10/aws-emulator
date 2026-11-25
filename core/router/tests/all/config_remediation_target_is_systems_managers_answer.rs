// SPDX-License-Identifier: CC0-1.0

//! WHICH `SSM_DOCUMENT` remediation targets `PutRemediationConfigurations` accepts.
//!
//! RULE: "The target (SSM document) must exist", and the only component that can say
//! whether a document exists is the service that OWNS the document registry — so a
//! configuration's `SSM_DOCUMENT` `TargetId` is accepted exactly when Systems Manager's own
//! read resolves that name at the endpoint the request was signed for, which includes every
//! document that service PUBLISHES (those exist in each account and Region with nobody
//! creating them), and is refused only when that read answers that it holds no document of
//! the name. The name's PREFIX decides nothing either way.
//!
//! This suite lives beside the router because the rule spans two services: the decision is
//! Config's, the fact it decides on is Systems Manager's. Each row therefore asks Systems
//! Manager FIRST — `DescribeDocument` for the very name the configuration targets — and
//! then requires Config's answer to agree with it, so the two cannot drift apart: a target
//! Systems Manager resolves is written and read back, and one it answers `InvalidDocument`
//! for is refused `InvalidParameterValueException` with Config's missing-target sentence and
//! persisted nowhere.
//!
//! What this suite adds beside
//! `services/service-com-amazonaws-configservice-starlingdoveservice/tests/all/registry_resource_types_and_remediation_targets.rs`,
//! which drives the same rule from inside Config's own crate: the per-row `DescribeDocument`
//! cross-check, so no expectation here is a claim this file makes about the registry, and the
//! published families beyond the `AWS-*` one — together with the names that carry a published
//! family's prefix and resolve to nothing, which is what a gate keyed on the prefix instead of
//! on the registry would wrongly accept.
//!
//! The rows differ along what the rule generalises over: which family of published document
//! the target names (the `AWS-*` Automation runbooks, the `AWSConfigRemediation-*` ones Config
//! remediation is documented against, the `AWSSupport-*` ones), a document this account
//! created through Systems Manager's own API, and the ways a name can resolve to nothing —
//! a name carrying a published family's prefix that the registry publishes nothing for, a
//! published name mis-cased (a document name is case-sensitive) and a name of no family at
//! all.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for: the Region Config asks Systems Manager
/// about a document in is the one the request's credential scope names.
const REGION: &str = "us-east-1";

/// The `X-Amz-Target` prefix of Config (that crate's `OPERATIONS.md`).
const CONFIG_TARGET_PREFIX: &str = "StarlingDoveService";

/// The `X-Amz-Target` prefix of Systems Manager (that crate's `OPERATIONS.md`).
const SSM_TARGET_PREFIX: &str = "AmazonSSM";

/// The only value of `com.amazonaws.configservice#RemediationTargetType`
/// (`smithy_query(query="shape", shape="RemediationTargetType")`): the target is an SSM
/// document.
const SSM_DOCUMENT: &str = "SSM_DOCUMENT";

/// The `DocumentType` of an Automation runbook
/// (`com.amazonaws.ssm#DocumentType`), which is what every target below names.
const AUTOMATION: &str = "Automation";

/// The `DocumentIdentifier$Owner` Systems Manager reports for a document it publishes
/// itself, rather than one an account created.
const AMAZON: &str = "Amazon";

/// A document created through Systems Manager's own API, so the positive rows are not only
/// published ones.
const CREATED_RUNBOOK: &str = "Sim2Real-TargetRunbook";

/// One row: the `TargetId` the remediation configuration carries, and whether Systems
/// Manager is expected to resolve it.
struct Case {
    /// What the row is called in every assertion of it.
    case: &'static str,
    /// The `TargetId` of the remediation configuration under test.
    target_id: &'static str,
    /// Whether Systems Manager holds a document of that name, which is what decides
    /// Config's answer.
    resolvable: bool,
    /// Whether Systems Manager PUBLISHES it (as opposed to the account having created it),
    /// which is what its reported owner must say.
    published: bool,
}

/// Config and Systems Manager over ONE shared engine, as the assembled router mounts them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_configservice_starlingdoveservice::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_ssm_amazonssm::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router
}

/// One awsJson1.1 call, as its status and parsed body. The credential scope names the
/// signing name that routes the request and the Region it is served in; the signature is
/// never checked.
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
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/\
                 aws4_request, SignedHeaders=host, Signature=0000"
            ),
        )
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .body(bytes::Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// A Config call.
async fn config(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    rpc(router, "config", CONFIG_TARGET_PREFIX, operation, body).await
}

/// A Config call required to answer 200.
async fn config_ok(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = config(router, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {operation}, got {status} with {parsed}"
    );
    parsed
}

/// A Systems Manager call.
async fn ssm(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    rpc(router, "ssm", SSM_TARGET_PREFIX, operation, body).await
}

/// The wire error code of a refusal body (`__type`, without its namespace).
fn error_code(body: &Value) -> String {
    let rendered = body["__type"].as_str().unwrap_or("<no __type>");
    rendered.rsplit('#').next().unwrap_or(rendered).to_string()
}

/// The message of a refusal body.
fn error_message(body: &Value) -> String {
    body["message"]
        .as_str()
        .or_else(|| body["Message"].as_str())
        .unwrap_or("<no message>")
        .to_string()
}

/// How Config words a remediation configuration whose target could not be found, spelled
/// out here rather than imported from the service crate so a change to the wording fails
/// this suite. Its one variable part is the rule the configuration was sent for.
fn target_not_found_message(config_rule_name: &str) -> String {
    format!(
        "The remediation target for {config_rule_name} could not be found. Specify the \
         appropriate remediation target and try again."
    )
}

/// An Automation runbook body, the shape a Systems Manager Automation document declares.
fn automation_content() -> String {
    json!({
        "schemaVersion": "0.3",
        "description": "a remediation this account owns",
        "assumeRole": "{{ AutomationAssumeRole }}",
        "parameters": {
            "AutomationAssumeRole": { "type": "String" },
            "BucketName": { "type": "String" },
        },
        "mainSteps": [{
            "name": "putBucketVersioning",
            "action": "aws:executeAwsApi",
            "inputs": { "Service": "s3", "Api": "PutBucketVersioning" },
        }],
    })
    .to_string()
}

/// The `PutRemediationConfigurations` body for one configuration of `rule` targeting
/// `target_id`.
fn remediation(rule: &str, target_id: &str) -> Value {
    json!({
        "RemediationConfigurations": [{
            "ConfigRuleName": rule,
            "TargetType": SSM_DOCUMENT,
            "TargetId": target_id,
        }],
    })
}

/// The `TargetId` of every remediation configuration the account holds for `rule`.
async fn stored_targets(router: &MultiServiceRouter, case: &str, rule: &str) -> Vec<String> {
    let body = config_ok(
        router,
        case,
        "DescribeRemediationConfigurations",
        &json!({ "ConfigRuleNames": [rule] }),
    )
    .await;
    body["RemediationConfigurations"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: expected RemediationConfigurations, got {body}"))
        .iter()
        .map(|configuration| {
            configuration["TargetId"]
                .as_str()
                .unwrap_or("<no TargetId>")
                .to_string()
        })
        .collect()
}

#[tokio::test]
async fn a_remediation_target_is_accepted_exactly_when_systems_manager_resolves_it() {
    // The published names below are Automation runbooks of the "Systems Manager Automation
    // runbook reference", one per family that reference publishes: the `AWS-*` runbooks, the
    // `AWSConfigRemediation-*` runbooks the AWS Config remediation documentation is written
    // against, and the `AWSSupport-*` self-service automations. Whether the registry really
    // holds each one is not taken on this list's word: every row asks Systems Manager below.
    let cases: [Case; 9] = [
        Case {
            case: "a published runbook that acts on an instance",
            target_id: "AWS-StartEC2Instance",
            resolvable: true,
            published: true,
        },
        Case {
            case: "a published runbook that acts on a bucket",
            target_id: "AWS-ConfigureS3BucketLogging",
            resolvable: true,
            published: true,
        },
        Case {
            case: "a published Config remediation runbook",
            target_id: "AWSConfigRemediation-EnableKeyRotation",
            resolvable: true,
            published: true,
        },
        Case {
            case: "a published support automation",
            target_id: "AWSSupport-EnableVPCFlowLogs",
            resolvable: true,
            published: true,
        },
        Case {
            case: "a runbook this account created",
            target_id: CREATED_RUNBOOK,
            resolvable: true,
            published: false,
        },
        Case {
            case: "an AWS- prefixed name the registry publishes nothing for",
            target_id: "AWS-Sim2RealNoSuchRunbook",
            resolvable: false,
            published: false,
        },
        Case {
            case: "an AWSConfigRemediation- prefixed name the registry publishes nothing for",
            target_id: "AWSConfigRemediation-Sim2RealNoSuchRunbook",
            resolvable: false,
            published: false,
        },
        Case {
            case: "a published name mis-cased",
            target_id: "aws-startec2instance",
            resolvable: false,
            published: false,
        },
        Case {
            case: "a name of no family at all",
            target_id: "Sim2Real-NoSuchRunbook",
            resolvable: false,
            published: false,
        },
    ];

    let router = emulator();

    // The customer managed configuration recorder an account holds before it writes rules.
    config_ok(
        &router,
        "setup",
        "PutConfigurationRecorder",
        &json!({
            "ConfigurationRecorder": {
                "roleARN": format!(
                    "arn:aws:iam::{}:role/config-role",
                    emulator_server::emulator_account_id(),
                ),
            },
        }),
    )
    .await;

    // The one document this account owns, created through the API of the service that owns
    // the registry — so a resolvable target is not only a published one.
    let (status, parsed) = ssm(
        &router,
        "CreateDocument",
        &json!({
            "Name": CREATED_RUNBOOK,
            "DocumentType": AUTOMATION,
            "Content": automation_content(),
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: creating {CREATED_RUNBOOK} must succeed, got {status} with {parsed}"
    );

    for (index, case) in cases.into_iter().enumerate() {
        // What Systems Manager — the only component that can answer it — says about the
        // name this row targets. Every expectation about Config below is checked against
        // THIS answer, so the two services cannot disagree about one name.
        let (ssm_status, described) = ssm(
            &router,
            "DescribeDocument",
            &json!({ "Name": case.target_id }),
        )
        .await;
        if case.resolvable {
            // A document the service publishes reports it as its owner; one the account
            // created reports the account.
            let owner = if case.published {
                AMAZON
            } else {
                emulator_server::emulator_account_id()
            };
            assert_eq!(
                (
                    ssm_status,
                    described["Document"]["Name"].as_str(),
                    described["Document"]["DocumentType"].as_str(),
                    described["Document"]["Owner"].as_str(),
                ),
                (
                    200,
                    Some(case.target_id),
                    Some(AUTOMATION),
                    Some(owner),
                ),
                "case {:?}: Systems Manager must hold {} as an {AUTOMATION} document \
                 (published: {}), got {ssm_status} with {described}",
                case.case,
                case.target_id,
                case.published
            );
        } else {
            assert_eq!(
                (ssm_status, error_code(&described).as_str()),
                (400, "InvalidDocument"),
                "case {:?}: Systems Manager must hold no document named {}, got {ssm_status} \
                 with {described}",
                case.case,
                case.target_id
            );
        }

        // One rule per row, so each row's acceptance is read back against its own rule and
        // the refusal's variable part differs from row to row.
        let rule = format!("remediation-target-rule-{index}");
        config_ok(
            &router,
            case.case,
            "PutConfigRule",
            &json!({
                "ConfigRule": {
                    "ConfigRuleName": &rule,
                    "Source": {
                        "Owner": "AWS",
                        "SourceIdentifier": "S3_BUCKET_VERSIONING_ENABLED",
                    },
                },
            }),
        )
        .await;

        let (status, parsed) = config(
            &router,
            "PutRemediationConfigurations",
            &remediation(&rule, case.target_id),
        )
        .await;

        if case.resolvable {
            assert_eq!(
                (status, parsed["FailedBatches"].as_array().map(Vec::len)),
                (200, Some(0)),
                "case {:?} ({}): a target Systems Manager resolves is written, got {status} \
                 with {parsed}",
                case.case,
                case.target_id
            );
            // Read back through the operation that reports it: the configuration is
            // persisted with the target it was sent with.
            let stored = stored_targets(&router, case.case, &rule).await;
            assert_eq!(
                stored,
                vec![case.target_id.to_string()],
                "case {:?}: expected the stored target {:?}, got {stored:?}",
                case.case,
                case.target_id
            );
        } else {
            assert_eq!(
                (status, error_code(&parsed), error_message(&parsed)),
                (
                    400,
                    "InvalidParameterValueException".to_string(),
                    target_not_found_message(&rule)
                ),
                "case {:?} ({}): a target Systems Manager holds no document for is refused \
                 naming the rule, got {status} with {parsed}",
                case.case,
                case.target_id
            );
            let stored = stored_targets(&router, case.case, &rule).await;
            assert!(
                stored.is_empty(),
                "case {:?}: the refused configuration must not be stored, got {stored:?}",
                case.case
            );
        }
    }
}
