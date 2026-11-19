// SPDX-License-Identifier: CC0-1.0

//! What a remediation execution DOES to the resource it names — the whole arc a
//! guardrail client drives: a running recorder records a bucket, a managed rule places
//! it `NON_COMPLIANT`, and the rule's remediation then runs and makes the bucket comply.
//!
//! RULE: a remediation execution RUNS the Systems Manager automation document its
//! remediation configuration names, by starting an execution of that document in Systems
//! Manager with the configuration's parameters resolved for the resource — so the run exists
//! where the service that owns automation documents holds it, that service's own steps are
//! what change the resource, and the Config execution reports the state that automation
//! reached. One run per started execution, whatever the document is: a run Systems Manager
//! refuses — a document that is not an automation runbook, a configuration that leaves out a
//! parameter the runbook requires — is reported as the failure it is, carrying that service's
//! own error code and sentence, and changes nothing.
//!
//! The rows differ along what the rule generalises over: which document the configuration
//! names (two of the published runbooks, a document that is not a runbook at all, and — in
//! the second test — a document created through Systems Manager's own API, which this crate
//! can hold no action for), which input each one's parameters resolve (a value the
//! configuration states, a value the document defaults, and the resource value
//! `RESOURCE_ID`), and which state the resulting change leaves the bucket in — including
//! one that leaves it still NON_COMPLIANT, because the state the configuration asked for is
//! not the one the rule checks for.
//!
//! Config, S3 and Systems Manager are mounted on ONE engine, reusing the harness
//! `config_recorded_resource_compliance` builds: the recorder reads the buckets it records
//! from S3's own API, the runbook's own steps act on them through that same API, and
//! Systems Manager is where the run itself lives.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::config_recorded_resource_compliance::{Emulator, ACCOUNT_ID};

/// The resource type Config records an S3 bucket under.
const BUCKET_RESOURCE_TYPE: &str = "AWS::S3::Bucket";

/// The only published `RemediationTargetType`: an SSM automation document.
const TARGET_TYPE: &str = "SSM_DOCUMENT";

/// The `X-Amz-Target` prefix of Systems Manager (that crate's `OPERATIONS.md`), which is
/// how this suite asks that service about the runs Config started in it.
const SSM_TARGET_PREFIX: &str = "AmazonSSM";

/// The `Parameters` key carrying the role a remediation runs as, which the published
/// runbooks declare as a parameter of their own.
const AUTOMATION_ASSUME_ROLE: &str = "AutomationAssumeRole";

/// The `Parameters` key the S3 runbooks take the bucket's name in.
const BUCKET_NAME: &str = "BucketName";

/// The `ResourceValue` a parameter resolves from the resource the execution is about
/// (`com.amazonaws.configservice#ResourceValueType`, whose one value this is).
const RESOURCE_ID: &str = "RESOURCE_ID";

/// `ComplianceType` values a managed rule places a recorded bucket at.
const COMPLIANT: &str = "COMPLIANT";
/// See [`COMPLIANT`].
const NON_COMPLIANT: &str = "NON_COMPLIANT";

/// `RemediationExecutionState` values this suite reads back
/// (`smithy_query(query="shape", shape="RemediationExecutionState")`).
const SUCCEEDED: &str = "SUCCEEDED";
/// See [`SUCCEEDED`].
const FAILED: &str = "FAILED";

/// The Config rule every row writes, which the remediation under test remediates, and
/// the managed rule it runs: "Checks if versioning is enabled for your S3 buckets".
const RULE_NAME: &str = "bucket-guardrail";
/// See [`RULE_NAME`].
const VERSIONING_RULE: &str = "S3_BUCKET_VERSIONING_ENABLED";

/// The default encryption Amazon S3 reports for a bucket nothing has configured
/// encryption on: "Amazon S3 now applies server-side encryption with Amazon S3 managed
/// keys (SSE-S3) as the base level of encryption for every bucket", whose algorithm is
/// `AES256` (`ServerSideEncryption`).
const DEFAULT_ENCRYPTION: &str = "AES256";

/// How long a poll waits for a span the clock owes — a recording pass, an evaluation, an
/// automation execution being picked up and walked. Every one of them is due a few seconds
/// after the call that made it due at the outside, so this is several times over and not a
/// span the behaviour depends on.
const CONVERGENCE_BUDGET: Duration = Duration::from_secs(30);

/// How long a poll waits between two reads.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// One row: the document the rule's remediation names, the parameters that configuration
/// carries beside the role, and what the run of it leaves behind.
struct Case {
    /// What the row is called in every assertion of it.
    case: &'static str,
    /// The `TargetId` of the remediation configuration: the document it runs.
    target_id: &'static str,
    /// Parameters the configuration states as static values, beside the role.
    statics: &'static [(&'static str, &'static str)],
    /// Whether the configuration names the bucket through the resource value, which is how
    /// a remediation hands the resource it is about to the runbook.
    bucket_by_resource_value: bool,
    /// The state the execution settles at: a run Systems Manager made has succeeded, one it
    /// refused has failed.
    settles_at: &'static str,
    /// The error code Systems Manager refuses the start with, which is what the execution's
    /// one step then carries as its `ErrorMessage`; `None` for a row that is run.
    refused_with: Option<&'static str>,
    /// The bucket's versioning status after the remediation, absent while its versioning
    /// has never been set.
    versioning: Option<&'static str>,
    /// The bucket's default encryption algorithm after the remediation.
    encryption: Option<&'static str>,
    /// The verdict the rule places the bucket at once the change has been recorded.
    verdict_after: &'static str,
}

/// The `PutConfigRule` body for the managed rule under test, scoped to `bucket`.
fn bucket_rule(bucket: &str) -> Value {
    json!({
        "ConfigRule": {
            "ConfigRuleName": RULE_NAME,
            "Source": { "Owner": "AWS", "SourceIdentifier": VERSIONING_RULE },
            "Scope": {
                "ComplianceResourceTypes": [BUCKET_RESOURCE_TYPE],
                "ComplianceResourceId": bucket,
            },
        }
    })
}

/// The `PutRemediationConfigurations` body for the document `target_id` with `parameters`.
fn remediation_body(target_id: &str, parameters: Value) -> Value {
    json!({
        "RemediationConfigurations": [{
            "ConfigRuleName": RULE_NAME,
            "TargetType": TARGET_TYPE,
            "TargetId": target_id,
            "ResourceType": BUCKET_RESOURCE_TYPE,
            "Parameters": parameters,
        }],
    })
}

/// The role a remediation runs as, as the one parameter every configuration here carries.
fn assume_role_parameter() -> Value {
    json!({
        AUTOMATION_ASSUME_ROLE: {
            "StaticValue": {
                "Values": [format!("arn:aws:iam::{ACCOUNT_ID}:role/config-remediation")],
            },
        },
    })
}

/// The `PutRemediationConfigurations` body for this row, built from its document and its
/// parameters.
fn remediation(case: &Case) -> Value {
    let mut parameters = assume_role_parameter();
    for (name, value) in case.statics {
        parameters[*name] = json!({ "StaticValue": { "Values": [value] } });
    }
    if case.bucket_by_resource_value {
        parameters[BUCKET_NAME] = json!({ "ResourceValue": { "Value": RESOURCE_ID } });
    }
    remediation_body(case.target_id, parameters)
}

/// The `StartRemediationExecution` body for `bucket`.
fn start(bucket: &str) -> Value {
    json!({
        "ConfigRuleName": RULE_NAME,
        "ResourceKeys": [{
            "resourceType": BUCKET_RESOURCE_TYPE,
            "resourceId": bucket,
        }],
    })
}

/// One Systems Manager call (`awsJson1_1`), required to answer 200.
async fn ssm(emulator: &Emulator, operation: &str, body: &Value) -> Value {
    let (status, parsed) = emulator
        .call("ssm", SSM_TARGET_PREFIX, "1.1", operation, body)
        .await;
    assert_eq!(
        status, 200,
        "{operation}: expected HTTP 200, got {status} with {parsed}"
    );
    parsed
}

/// The bucket's versioning status, absent while its versioning has never been set.
async fn versioning_of(emulator: &Emulator, bucket: &str) -> Option<String> {
    emulator
        .s3()
        .get_bucket_versioning()
        .bucket(bucket)
        .send()
        .await
        .ok()
        .and_then(|answer| answer.status().map(|status| status.as_str().to_string()))
}

/// The bucket's default encryption algorithm, absent while it has no default encryption.
async fn encryption_of(emulator: &Emulator, bucket: &str) -> Option<String> {
    emulator
        .s3()
        .get_bucket_encryption()
        .bucket(bucket)
        .send()
        .await
        .ok()
        .and_then(|answer| answer.server_side_encryption_configuration)
        .and_then(|configuration| {
            configuration
                .rules
                .iter()
                .filter_map(|rule| {
                    rule.apply_server_side_encryption_by_default
                        .as_ref()
                        .map(|default| default.sse_algorithm.as_str().to_string())
                })
                .next()
        })
}

/// Wait until the recorder has captured a configuration item for `bucket`, which is what
/// every leg downstream of discovery reads.
async fn await_configuration_item(case: &str, emulator: &Emulator, bucket: &str) {
    let deadline = Instant::now() + CONVERGENCE_BUDGET;
    loop {
        let (status, body) = emulator
            .call(
                "config",
                "StarlingDoveService",
                "1.1",
                "GetResourceConfigHistory",
                &json!({ "resourceType": BUCKET_RESOURCE_TYPE, "resourceId": bucket }),
            )
            .await;
        let items = body["configurationItems"]
            .as_array()
            .map(Vec::len)
            .unwrap_or_default();
        if status == 200 && items > 0 {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: a running recorder must capture a configuration item for \
             {bucket:?}; the last read answered {status} with {body}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Wait until the rule places `bucket` at `wanted`, and answer what it placed it at.
async fn await_verdict(case: &str, emulator: &Emulator, bucket: &str, wanted: &str) -> String {
    let deadline = Instant::now() + CONVERGENCE_BUDGET;
    loop {
        let body = emulator
            .config(
                "GetComplianceDetailsByResource",
                &json!({ "ResourceType": BUCKET_RESOURCE_TYPE, "ResourceId": bucket }),
            )
            .await;
        let placed: Vec<String> = body["EvaluationResults"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|result| result["ComplianceType"].as_str().map(ToString::to_string))
            .collect();
        if placed.iter().any(|verdict| verdict == wanted) {
            return wanted.to_string();
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the rule must place {bucket:?} at {wanted}, got {placed:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The rule's remediation execution for `bucket`, as `DescribeRemediationExecutionStatus`
/// reports it right now.
async fn execution(case: &str, emulator: &Emulator, bucket: &str) -> Value {
    let body = emulator
        .config(
            "DescribeRemediationExecutionStatus",
            &json!({ "ConfigRuleName": RULE_NAME }),
        )
        .await;
    let reported = body["RemediationExecutionStatuses"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    reported
        .iter()
        .find(|status| status["ResourceKey"]["resourceId"].as_str() == Some(bucket))
        .cloned()
        .unwrap_or_else(|| {
            panic!("case {case:?}: an execution must be reported for {bucket:?}, got {reported:?}")
        })
}

/// Wait until the rule's remediation execution for `bucket` reports `wanted`, and answer
/// the execution that reported it.
async fn await_execution_state(
    case: &str,
    emulator: &Emulator,
    bucket: &str,
    wanted: &str,
) -> Value {
    let deadline = Instant::now() + CONVERGENCE_BUDGET;
    loop {
        let reported = execution(case, emulator, bucket).await;
        if reported["State"].as_str() == Some(wanted) {
            return reported;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the remediation execution for {bucket:?} must reach {wanted}, \
             got {reported}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The automation executions Systems Manager holds for the document `document`, each as its
/// id and the status it is at.
async fn automation_executions(emulator: &Emulator, document: &str) -> Vec<(String, String)> {
    let body = ssm(
        emulator,
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
            let text = |value: &Value| value.as_str().unwrap_or_default().to_string();
            (
                text(&execution["AutomationExecutionId"]),
                text(&execution["AutomationExecutionStatus"]),
            )
        })
        .collect()
}

/// Drive the arc every row shares up to the finding a remediation acts on: a recording
/// recorder, a recorded bucket, the rule, and its `NON_COMPLIANT` verdict for the bucket.
async fn with_a_non_compliant_bucket(case: &str, emulator: &Emulator, bucket: &str) {
    emulator.recording().await;
    emulator.create_bucket(bucket, &[]).await;
    await_configuration_item(case, emulator, bucket).await;
    emulator.config("PutConfigRule", &bucket_rule(bucket)).await;
    await_verdict(case, emulator, bucket, NON_COMPLIANT).await;
}

#[tokio::test]
async fn a_remediation_execution_runs_the_runbook_its_configuration_names() {
    // The published runbooks exercised below are the AWS-owned automation documents a
    // Config remediation configuration names on an S3 bucket, from the Systems Manager
    // Automation runbook reference: `AWS-ConfigureS3BucketVersioning` (parameters
    // `BucketName` and `VersioningState`, whose default is `Enabled`) and
    // `AWS-EnableS3BucketEncryption` (parameters `BucketName` and `SSEAlgorithm`, whose
    // default is `AES256`). `AWS-EnableS3BucketEncryption` is the entry this issue did not
    // cite, and `AWS-UpdateSSMAgent` is a published document that is a COMMAND document
    // rather than a runbook, so no automation execution of it can be started at all.
    let cases: [Case; 5] = [
        Case {
            case: "the versioning runbook, left to its default state",
            target_id: "AWS-ConfigureS3BucketVersioning",
            statics: &[],
            bucket_by_resource_value: true,
            settles_at: SUCCEEDED,
            refused_with: None,
            versioning: Some("Enabled"),
            encryption: Some(DEFAULT_ENCRYPTION),
            verdict_after: COMPLIANT,
        },
        Case {
            case: "the versioning runbook, asked to suspend versioning",
            target_id: "AWS-ConfigureS3BucketVersioning",
            statics: &[("VersioningState", "Suspended")],
            bucket_by_resource_value: true,
            settles_at: SUCCEEDED,
            refused_with: None,
            versioning: Some("Suspended"),
            encryption: Some(DEFAULT_ENCRYPTION),
            // Suspended versioning is not versioning enabled, so the rule still places
            // the bucket as NON_COMPLIANT after the remediation ran.
            verdict_after: NON_COMPLIANT,
        },
        Case {
            case: "the encryption runbook, asked for KMS encryption",
            target_id: "AWS-EnableS3BucketEncryption",
            statics: &[("SSEAlgorithm", "aws:kms")],
            bucket_by_resource_value: true,
            settles_at: SUCCEEDED,
            refused_with: None,
            // The runbook this configuration names sets no versioning state, so the
            // bucket the versioning rule checks is left NON_COMPLIANT.
            versioning: None,
            encryption: Some("aws:kms"),
            verdict_after: NON_COMPLIANT,
        },
        Case {
            case: "a published document that is not an automation runbook",
            target_id: "AWS-UpdateSSMAgent",
            statics: &[],
            bucket_by_resource_value: false,
            settles_at: FAILED,
            refused_with: Some("AutomationDefinitionNotFoundException"),
            versioning: None,
            encryption: Some(DEFAULT_ENCRYPTION),
            verdict_after: NON_COMPLIANT,
        },
        Case {
            // The runbook declares `BucketName` with no default, so a configuration that
            // resolves no value for it states no bucket to act on and Systems Manager will
            // not run the document at all.
            case: "a configuration that leaves out a parameter the runbook requires",
            target_id: "AWS-ConfigureS3BucketVersioning",
            statics: &[],
            bucket_by_resource_value: false,
            settles_at: FAILED,
            refused_with: Some("InvalidAutomationExecutionParametersException"),
            versioning: None,
            encryption: Some(DEFAULT_ENCRYPTION),
            verdict_after: NON_COMPLIANT,
        },
    ];

    for case in cases {
        let emulator = Emulator::new();
        let bucket = "remediated-bucket";
        with_a_non_compliant_bucket(case.case, &emulator, bucket).await;

        // A rule whose remediation configuration has been DELETED has no remediation to
        // run, which is the one precondition this refusal states — and the resource is
        // left exactly as it was.
        let put = emulator
            .config("PutRemediationConfigurations", &remediation(&case))
            .await;
        assert_eq!(
            put["FailedBatches"].as_array().map(Vec::len),
            Some(0),
            "case {:?}: the remediation configuration must be stored, got {put}",
            case.case
        );
        emulator
            .config(
                "DeleteRemediationConfiguration",
                &json!({ "ConfigRuleName": RULE_NAME }),
            )
            .await;
        let (status, code) = emulator
            .refused("StartRemediationExecution", &start(bucket))
            .await;
        assert_eq!(
            (status, code.as_str()),
            (400, "NoSuchRemediationConfigurationException"),
            "case {:?}: a disarmed rule must refuse the execution",
            case.case
        );
        assert_eq!(
            (
                versioning_of(&emulator, bucket).await.as_deref(),
                encryption_of(&emulator, bucket).await.as_deref(),
            ),
            (None, Some(DEFAULT_ENCRYPTION)),
            "case {:?}: the refused execution must leave the bucket untouched",
            case.case
        );

        // The identical configuration, put again, re-arms the identical execution: it is
        // run as an automation execution of the document it names, and settles where that
        // run does.
        emulator
            .config("PutRemediationConfigurations", &remediation(&case))
            .await;
        let started = emulator
            .config("StartRemediationExecution", &start(bucket))
            .await;
        assert_eq!(
            started["FailedItems"].as_array().map(Vec::len),
            Some(0),
            "case {:?}: the execution must start for the bucket, got {started}",
            case.case
        );

        let settled = await_execution_state(case.case, &emulator, bucket, case.settles_at).await;
        let step = &settled["StepDetails"][0];
        // A run that was made reports no error; a run Systems Manager refused reports what
        // that service answered — its own error code and sentence.
        let reported_error = step["ErrorMessage"].as_str();
        if let Some(code) = case.refused_with {
            let expected_prefix = format!("{code}: ");
            assert!(
                reported_error.is_some_and(|message| message.starts_with(&expected_prefix)
                    && message.len() > expected_prefix.len()),
                "case {:?}: the failed step must carry the refusal of the service that owns \
                 the document, got {reported_error:?}",
                case.case
            );
            assert!(
                automation_executions(&emulator, case.target_id)
                    .await
                    .is_empty(),
                "case {:?}: a refused start leaves no automation execution behind",
                case.case
            );
        } else {
            assert_eq!(
                reported_error, None,
                "case {:?}: a step that did not fail reports no error, got {reported_error:?}",
                case.case
            );
        }

        assert_eq!(
            (
                versioning_of(&emulator, bucket).await.as_deref(),
                encryption_of(&emulator, bucket).await.as_deref(),
            ),
            (case.versioning, case.encryption),
            "case {:?}: the execution must leave the bucket as its document specifies",
            case.case
        );

        // The recorder captures the state the remediation produced, and the rule places
        // the bucket from THAT configuration item.
        let placed = await_verdict(case.case, &emulator, bucket, case.verdict_after).await;
        assert_eq!(
            placed, case.verdict_after,
            "case {:?}: the rule must place the remediated bucket at {}",
            case.case, case.verdict_after
        );
    }
}

/// The document this suite CREATES through Systems Manager's own API, which no table in the
/// Config crate can hold an action for: its one step writes the name of the bucket it
/// remediated into a Systems Manager parameter, so the parameter's value is proof that the
/// document's own step ran.
const NOTING_RUNBOOK: &str = "SuiteNoteTheRemediatedBucket";

/// The parameter [`NOTING_RUNBOOK`] writes the remediated bucket's name into.
const NOTED_PARAMETER: &str = "/suite/remediated-bucket";

/// The body of [`NOTING_RUNBOOK`], whose parameters are the role the automation runs as and
/// the bucket the remediation is about.
fn noting_runbook() -> Value {
    json!({
        "schemaVersion": "0.3",
        "description": "Notes the bucket a remediation acted on.",
        "assumeRole": "{{ AutomationAssumeRole }}",
        "parameters": {
            AUTOMATION_ASSUME_ROLE: { "type": "String", "default": "" },
            BUCKET_NAME: { "type": "String" },
        },
        "mainSteps": [{
            "name": "noteTheBucket",
            "action": "aws:executeAwsApi",
            "inputs": {
                "Service": "ssm",
                "Api": "PutParameter",
                "Name": NOTED_PARAMETER,
                "Value": "{{ BucketName }}",
                "Type": "String",
                "Overwrite": true,
            },
        }],
    })
}

/// The value Systems Manager holds for [`NOTED_PARAMETER`], absent while nothing has
/// written it.
async fn noted_bucket(emulator: &Emulator) -> Option<String> {
    let (status, body) = emulator
        .call(
            "ssm",
            SSM_TARGET_PREFIX,
            "1.1",
            "GetParameter",
            &json!({ "Name": NOTED_PARAMETER }),
        )
        .await;
    if status != 200 {
        return None;
    }
    body["Parameter"]["Value"]
        .as_str()
        .map(ToString::to_string)
}

#[tokio::test]
async fn concurrent_starts_run_one_automation_execution_of_the_document_they_name() {
    let emulator = Emulator::new();
    let bucket = "noted-bucket";
    let case = "a document created through Systems Manager's own API";
    with_a_non_compliant_bucket(case, &emulator, bucket).await;

    // The document is the account's own, created the way a client creates one, so nothing
    // in the Config crate can know an action for it: whatever it does is what Systems
    // Manager's walk of its steps does.
    ssm(
        &emulator,
        "CreateDocument",
        &json!({
            "Name": NOTING_RUNBOOK,
            "DocumentType": "Automation",
            "DocumentFormat": "JSON",
            "Content": noting_runbook().to_string(),
        }),
    )
    .await;
    let mut parameters = assume_role_parameter();
    parameters[BUCKET_NAME] = json!({ "ResourceValue": { "Value": RESOURCE_ID } });
    emulator
        .config(
            "PutRemediationConfigurations",
            &remediation_body(NOTING_RUNBOOK, parameters),
        )
        .await;

    // THREE starts at once for the same resource key: "An existing
    // StartRemediationExecution call for the specified resource keys must complete before
    // you can call the API again", so one of them starts the run and the others report the
    // key back — and the document is run ONCE.
    let request = start(bucket);
    let (first, second, third) = tokio::join!(
        emulator.config("StartRemediationExecution", &request),
        emulator.config("StartRemediationExecution", &request),
        emulator.config("StartRemediationExecution", &request),
    );
    let answers = [&first, &second, &third];
    let accepted = answers
        .iter()
        .filter(|answer| answer["FailedItems"].as_array().map(Vec::len) == Some(0))
        .count();
    assert_eq!(
        accepted, 1,
        "case {case:?}: exactly one of the concurrent starts may start the run, got \
         {answers:?}"
    );

    // The run itself: ONE automation execution of the document the configuration names,
    // which settles at success and carries the bucket the remediation was started for as
    // the value its `BucketName` parameter resolved to.
    let settled = await_execution_state(case, &emulator, bucket, SUCCEEDED).await;
    let running = automation_executions(&emulator, NOTING_RUNBOOK).await;
    assert_eq!(
        running.len(),
        1,
        "case {case:?}: the document must have been run exactly once, got {running:?}"
    );
    assert_eq!(
        running[0].1, "Success",
        "case {case:?}: the automation execution must have succeeded, got {running:?}"
    );
    let automation = ssm(
        &emulator,
        "GetAutomationExecution",
        &json!({ "AutomationExecutionId": running[0].0 }),
    )
    .await;
    assert_eq!(
        (
            automation["AutomationExecution"]["DocumentName"].as_str(),
            automation["AutomationExecution"]["Parameters"][BUCKET_NAME][0].as_str(),
        ),
        (Some(NOTING_RUNBOOK), Some(bucket)),
        "case {case:?}: the run must be of the document the configuration names, for the \
         resource the execution names, got {automation}"
    );
    assert_eq!(
        settled["StepDetails"][0]["ErrorMessage"].as_str(),
        None,
        "case {case:?}: a run that succeeded reports no error, got {settled}"
    );

    // What the document's own step DID, read from the service that served it: nothing in
    // the Config crate could have produced this.
    assert_eq!(
        noted_bucket(&emulator).await.as_deref(),
        Some(bucket),
        "case {case:?}: the document's own step must have run"
    );
}
