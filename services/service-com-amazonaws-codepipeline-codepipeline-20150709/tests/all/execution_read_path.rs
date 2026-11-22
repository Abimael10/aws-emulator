// SPDX-License-Identifier: CC0-1.0

//! What the execution READ path reports: `ListPipelineExecutions`,
//! `GetPipelineExecution`, `ListActionExecutions` and `GetPipelineState`, driven end to
//! end over the wire with CodePipeline, CodeCommit and S3 mounted on one engine — so the
//! execution plane reaches each of them the way the assembled emulator does.
//!
//! The rules under test, each asserted over a SET of inputs rather than one case:
//!
//! * an execution a caller's CALL started records that caller's own principal ARN as
//!   `ExecutionTrigger$triggerDetail` — "detail related to the event that started a
//!   pipeline execution … the user ARN for a user-initiated start-pipeline-execution CLI
//!   command" — whichever call started it, and every operation that reports the trigger
//!   reports it;
//! * a member carrying WHAT A RUN HOLDS is reported as the empty list or map when the run
//!   holds none of it, never omitted: an execution's `artifactRevisions`, an action
//!   execution's `input.inputArtifacts` and `output.outputVariables`, and a stage's
//!   `inboundExecutions`;
//! * an action whose configured branch is not one the repository holds fails with the
//!   action PROVIDER's own `ConfigurationError` sentence about that misconfiguration,
//!   built from the action's configuration, and nothing of the emulator's own call to the
//!   repository service appears in it;
//! * `ActionState$entityUrl` is the action type's published `entityUrlTemplate` with
//!   every `{Config:<property>}` reference replaced by the action's configuration value
//!   for that property, and is absent for an action type that publishes no template;
//! * retrying a stage runs its failed action AGAIN, and each ATTEMPT is an action
//!   execution of its own: the history holds one record per attempt, each under its own
//!   `actionExecutionId`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The `X-Amz-Target` prefix OPERATIONS.md records for CodePipeline.
const PIPELINE_TARGET: &str = "CodePipeline_20150709";

/// The `X-Amz-Target` prefix CodeCommit's OPERATIONS.md records.
const COMMIT_TARGET: &str = "CodeCommit_20150413";

/// A `roleArn` satisfying `RoleArn`'s modelled pattern. IAM is not mounted here, so no
/// role lookup has an answer and the declarations are accepted (only a definitive "no
/// such role" refuses one).
const ROLE_ARN: &str = "arn:aws:iam::123456789012:role/service-role/cp";

/// How long a test waits for the clock-owed steps of one execution: each step is due
/// `state::execution::ACTION_STEP` (300 ms) after the one before it. Generous, because
/// what is waited for is the CONDITION and not a span.
const WAIT: Duration = Duration::from_secs(20);

/// How long between two looks at a condition that is not there yet.
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// The `FailureType` an action carries when its declaration cannot be executed at all.
const CONFIGURATION_ERROR: &str = "ConfigurationError";

/// One emulator: the shared engine with CodePipeline, S3 and CodeCommit on it, and the
/// router that dispatches to all three.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_codepipeline_codepipeline_20150709::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
            Arc::clone(&engine),
        ));
        router.register_service(
            service_com_amazonaws_codecommit_codecommit_20150413::EmulatedService::new(engine),
        );
        Self { router }
    }

    /// One awsJson1_1 call to the service `signing_name` claims, under `target`.
    async fn json_call(
        &self,
        signing_name: &str,
        target: &str,
        operation: &str,
        body: &Value,
    ) -> (u16, Value) {
        let authorization = format!(
            "AWS4-HMAC-SHA256 \
             Credential=AKIDEXAMPLE/20260101/us-east-1/{signing_name}/aws4_request, \
             SignedHeaders=host;x-amz-date, Signature=0123456789abcdef"
        );
        let request = http::Request::builder()
            .method("POST")
            .uri(format!("https://{signing_name}.us-east-1.amazonaws.com/"))
            .header("host", format!("{signing_name}.us-east-1.amazonaws.com"))
            .header("authorization", authorization)
            .header("content-type", "application/x-amz-json-1.1")
            .header("x-amz-target", format!("{target}.{operation}"))
            .body(bytes::Bytes::from(body.to_string()))
            .expect("a valid request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let body = response.into_body();
        let parsed = if body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&body).unwrap_or_else(|error| {
                panic!(
                    "{operation} answered a body that is not JSON ({error}): {}",
                    String::from_utf8_lossy(&body)
                )
            })
        };
        (status, parsed)
    }

    /// One CodePipeline operation that must succeed.
    async fn cp_ok(&self, operation: &str, body: &Value) -> Value {
        let (status, response) = self
            .json_call("codepipeline", PIPELINE_TARGET, operation, body)
            .await;
        assert_eq!(
            status, 200,
            "{operation} must succeed, got {status} with {response}"
        );
        response
    }

    /// One CodeCommit operation that must succeed.
    async fn cc_ok(&self, operation: &str, body: &Value) -> Value {
        let (status, response) = self
            .json_call("codecommit", COMMIT_TARGET, operation, body)
            .await;
        assert_eq!(
            status, 200,
            "{operation} must succeed, got {status} with {response}"
        );
        response
    }

    /// Create the artifact-store bucket `name`.
    async fn bucket(&self, name: &str) {
        let authorization = "AWS4-HMAC-SHA256 \
             Credential=AKIDEXAMPLE/20260101/us-east-1/s3/aws4_request, \
             SignedHeaders=host;x-amz-date, Signature=0123456789abcdef";
        let request = http::Request::builder()
            .method("PUT")
            .uri(format!("https://s3.us-east-1.amazonaws.com/{name}"))
            .header("host", "s3.us-east-1.amazonaws.com")
            .header("authorization", authorization)
            .body(bytes::Bytes::new())
            .expect("a valid request");
        let response = self.router.dispatch(request).await;
        assert_eq!(
            response.status().as_u16(),
            200,
            "the bucket {name:?} must be created"
        );
    }

    /// Create a repository holding `files` on `branch`. With no files the repository is
    /// created empty, so it has no branch at all.
    async fn repository(&self, name: &str, branch: &str, files: &[(&str, &[u8])]) {
        self.cc_ok("CreateRepository", &json!({"repositoryName": name}))
            .await;
        let mut tip: Option<String> = None;
        for (path, content) in files {
            let mut body = json!({
                "repositoryName": name,
                "branchName": branch,
                "filePath": path,
                // `fileContent` is a blob, which awsJson1_1 carries base64-encoded.
                "fileContent": base64::engine::general_purpose::STANDARD.encode(content),
                "commitMessage": format!("add {path}"),
            });
            if let Some(parent) = &tip {
                body["parentCommitId"] = json!(parent);
            }
            let answer = self.cc_ok("PutFile", &body).await;
            tip = Some(
                answer["commitId"]
                    .as_str()
                    .unwrap_or_else(|| panic!("PutFile reports the commit it made, got {answer}"))
                    .to_string(),
            );
        }
    }
}

/// The ARN of the principal every call in this suite speaks for: the IAM user the
/// emulator's own long-term access key belongs to, which is the identity STS reports as
/// the caller. Read from the one definition of it rather than written out here, so the
/// assertion is "the calling principal" and not one particular string.
fn calling_principal() -> String {
    service_common::caller_principal::ambient_user_arn()
}

/// A `Source` stage reading `branch` of `repository`, producing `artifact`.
fn code_commit_source(repository: &str, branch: &str, artifact: &str) -> Value {
    json!({
        "name": "Source",
        "actions": [{
            "name": "Src",
            "actionTypeId": {
                "category": "Source",
                "owner": "AWS",
                "provider": "CodeCommit",
                "version": "1",
            },
            "configuration": {"RepositoryName": repository, "BranchName": branch},
            "outputArtifacts": [{"name": artifact}],
            "namespace": "SourceVariables",
        }],
    })
}

/// A `Deploy` stage that extracts `artifact` into `bucket`.
fn extracting_deploy(artifact: &str, bucket: &str) -> Value {
    json!({
        "name": "Deploy",
        "actions": [{
            "name": "Dep",
            "actionTypeId": {
                "category": "Deploy", "owner": "AWS", "provider": "S3", "version": "1"
            },
            "configuration": {"BucketName": bucket, "Extract": "true"},
            "inputArtifacts": [{"name": artifact}],
        }],
    })
}

/// A manual-approval stage: the non-`Source` stage a declaration has to carry when the
/// row under test delivers nothing. Its action type publishes no URL template at all.
fn approval_gate() -> Value {
    json!({
        "name": "Gate",
        "actions": [{
            "name": "Approve",
            "actionTypeId": {
                "category": "Approval", "owner": "AWS", "provider": "Manual", "version": "1"
            },
        }],
    })
}

/// A pipeline declaration named `name`, writing its artifacts to `artifact_store`.
fn declaration(name: &str, artifact_store: &str, stages: Vec<Value>) -> Value {
    json!({"pipeline": {
        "name": name,
        "roleArn": ROLE_ARN,
        "artifactStore": {"type": "S3", "location": artifact_store},
        "stages": stages,
    }})
}

/// The summaries `ListPipelineExecutions` reports for `pipeline`, newest first.
async fn summaries(emulator: &Emulator, pipeline: &str) -> Vec<Value> {
    let listed = emulator
        .cp_ok("ListPipelineExecutions", &json!({"pipelineName": pipeline}))
        .await;
    listed["pipelineExecutionSummaries"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// The action executions `ListActionExecutions` reports for `pipeline`, newest first.
async fn action_executions(emulator: &Emulator, pipeline: &str) -> Vec<Value> {
    let page = emulator
        .cp_ok("ListActionExecutions", &json!({"pipelineName": pipeline}))
        .await;
    page["actionExecutionDetails"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// Poll `ListPipelineExecutions` until the pipeline's newest execution reaches a terminal
/// status, and answer its summary.
async fn wait_for_terminal(emulator: &Emulator, case: &str, pipeline: &str) -> Value {
    let deadline = Instant::now() + WAIT;
    loop {
        let listed = summaries(emulator, pipeline).await;
        let newest = listed.first().cloned().unwrap_or(Value::Null);
        let status = newest["status"].as_str().unwrap_or_default().to_string();
        if matches!(
            status.as_str(),
            "Succeeded" | "Failed" | "Stopped" | "Superseded" | "Cancelled"
        ) {
            return newest;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the newest execution of {pipeline} never settled, last {newest}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Poll `ListActionExecutions` until `pipeline` reports `expected` records for the action
/// `action`, and answer them, newest first.
async fn wait_for_attempts(
    emulator: &Emulator,
    case: &str,
    pipeline: &str,
    action: &str,
    expected: usize,
) -> Vec<Value> {
    let deadline = Instant::now() + WAIT;
    loop {
        let attempts: Vec<Value> = action_executions(emulator, pipeline)
            .await
            .into_iter()
            .filter(|detail| detail["actionName"] == json!(action))
            .collect();
        let settled = attempts
            .iter()
            .all(|attempt| attempt["status"] == json!("Failed"));
        if attempts.len() == expected && settled {
            return attempts;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: {pipeline} never reported {expected} settled attempt(s) of \
             {action}, last {attempts:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// RULE: an execution a caller's CALL started carries that caller's principal ARN as its
/// trigger detail, and every operation that reports the trigger reports it.
///
/// The rows differ along what the rule generalises over: WHICH call started the
/// execution — and so which `TriggerType` it records — none of which changes whose call
/// it was.
#[tokio::test]
async fn an_execution_records_the_calling_principal_as_its_trigger_detail() {
    // (case, the `TriggerType` the execution records, how the row starts it)
    type Start = fn(&str) -> (&'static str, Value);
    let rows: Vec<(&str, &str, Start)> = vec![
        (
            "the run creating the pipeline owes",
            "CreatePipeline",
            // Nothing more to call: the creation itself starts that run.
            |_| ("GetPipelineState", json!({"name": "unused"})),
        ),
        (
            "a start of the pipeline",
            "StartPipelineExecution",
            |pipeline| ("StartPipelineExecution", json!({"name": pipeline})),
        ),
        (
            "a revision reported for the source action",
            "PutActionRevision",
            |pipeline| {
                (
                    "PutActionRevision",
                    json!({
                        "pipelineName": pipeline,
                        "stageName": "Source",
                        "actionName": "Src",
                        "actionRevision": {
                            "revisionId": "c0ffee",
                            "revisionChangeId": "change-1",
                            "created": 1_700_000_000,
                        },
                    }),
                )
            },
        ),
    ];

    for (index, (case, trigger_type, start)) in rows.into_iter().enumerate() {
        let emulator = Emulator::new();
        let pipeline = format!("triggered-{index}");
        let artifacts = format!("{pipeline}-artifacts");
        let repository = format!("{pipeline}-repo");
        emulator.bucket(&artifacts).await;
        emulator
            .repository(&repository, "main", &[("etl.py", b"print(\"etl\")\n")])
            .await;
        emulator
            .cp_ok(
                "CreatePipeline",
                &declaration(
                    &pipeline,
                    &artifacts,
                    vec![
                        code_commit_source(&repository, "main", "SourceArtifact"),
                        approval_gate(),
                    ],
                ),
            )
            .await;
        let (operation, body) = start(&pipeline);
        if operation != "GetPipelineState" {
            emulator.cp_ok(operation, &body).await;
        }

        // The newest execution is the one this row started.
        let newest = summaries(&emulator, &pipeline)
            .await
            .first()
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: the pipeline carries an execution"));
        assert_eq!(
            (
                newest["trigger"]["triggerType"].clone(),
                newest["trigger"]["triggerDetail"].clone()
            ),
            (json!(trigger_type), json!(calling_principal())),
            "case {case:?}: the summary names what started the execution and whose call \
             it was, got {newest}"
        );

        let id = newest["pipelineExecutionId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: the summary carries an id, got {newest}"))
            .to_string();
        let read = emulator
            .cp_ok(
                "GetPipelineExecution",
                &json!({"pipelineName": pipeline, "pipelineExecutionId": id}),
            )
            .await;
        assert_eq!(
            read["pipelineExecution"]["trigger"],
            newest["trigger"],
            "case {case:?}: the reader reports the trigger the listing does, got {read}"
        );
    }
}

/// RULE: a member carrying what a run HOLDS is reported as the empty list or map when the
/// run holds none of it — an execution's `artifactRevisions`, an action execution's
/// `input.inputArtifacts` and `output.outputVariables`, a stage's `inboundExecutions` —
/// because absent and empty are different facts on the wire.
///
/// The rows differ along what the rule generalises over: whether the source resolves a
/// revision at all, so the same members carry something in one row and nothing in the
/// other.
#[tokio::test]
async fn a_run_reports_what_it_holds_as_an_empty_list_rather_than_omitting_it() {
    // (case, whether the repository holds the configured branch)
    let rows: Vec<(&str, bool)> = vec![
        ("a source that resolves a revision", true),
        ("a source that resolves none", false),
    ];

    for (index, (case, resolves)) in rows.into_iter().enumerate() {
        let emulator = Emulator::new();
        let pipeline = format!("holds-{index}");
        let artifacts = format!("{pipeline}-artifacts");
        let delivered = format!("{pipeline}-delivered");
        let repository = format!("{pipeline}-repo");
        emulator.bucket(&artifacts).await;
        emulator.bucket(&delivered).await;
        let files: Vec<(&str, &[u8])> = if resolves {
            vec![("etl.py", b"print(\"etl\")\n")]
        } else {
            Vec::new()
        };
        emulator.repository(&repository, "main", &files).await;
        emulator
            .cp_ok(
                "CreatePipeline",
                &declaration(
                    &pipeline,
                    &artifacts,
                    vec![
                        code_commit_source(&repository, "main", "SourceArtifact"),
                        extracting_deploy("SourceArtifact", &delivered),
                    ],
                ),
            )
            .await;
        let settled = wait_for_terminal(&emulator, case, &pipeline).await;
        let id = settled["pipelineExecutionId"]
            .as_str()
            .unwrap_or_default()
            .to_string();

        let read = emulator
            .cp_ok(
                "GetPipelineExecution",
                &json!({"pipelineName": pipeline, "pipelineExecutionId": id}),
            )
            .await;
        let revisions = read["pipelineExecution"]["artifactRevisions"]
            .as_array()
            .unwrap_or_else(|| {
                panic!("case {case:?}: artifactRevisions is reported either way, got {read}")
            })
            .clone();
        assert_eq!(
            revisions.len(),
            usize::from(resolves),
            "case {case:?}: one revision per artifact the execution knows, got {read}"
        );

        // The source action: it consumes no artifact at all, so its input artifact list
        // is reported empty, and its output variables are a map either way.
        let source = action_executions(&emulator, &pipeline)
            .await
            .into_iter()
            .find(|detail| detail["actionName"] == json!("Src"))
            .unwrap_or_else(|| panic!("case {case:?}: the source action was reached"));
        assert_eq!(
            source["input"]["inputArtifacts"],
            json!([]),
            "case {case:?}: an action consuming no artifact reports an empty list, got \
             {source}"
        );
        assert_eq!(
            source["input"]["actionTypeId"]["__type"],
            json!("ActionTypeId"),
            "case {case:?}: the nested action type carries its shape discriminator, got \
             {source}"
        );
        let variables = source["output"]["outputVariables"]
            .as_object()
            .unwrap_or_else(|| {
                panic!("case {case:?}: outputVariables is reported either way, got {source}")
            })
            .clone();
        assert_eq!(
            !variables.is_empty(),
            resolves,
            "case {case:?}: a run reports the variables it published and no others, got \
             {source}"
        );

        // Every stage reports the executions held at its inbound transition, which is
        // none at all once the pipeline has settled.
        let state = emulator
            .cp_ok("GetPipelineState", &json!({"name": pipeline}))
            .await;
        for stage in state["stageStates"].as_array().expect("stage states") {
            assert_eq!(
                stage["inboundExecutions"],
                json!([]),
                "case {case:?}: a stage nothing waits at reports an empty list, got {stage}"
            );
        }
    }
}

/// RULE: an action whose configured branch is not one the repository holds fails with the
/// action provider's own `ConfigurationError` sentence about that misconfiguration, whose
/// only variable parts are the branch and repository names the action's configuration
/// states — and the same sentence is what the action's state reports as its error
/// message, while the history reports it once, as the provider's summary for the
/// execution, with the provider's code alone in its error details.
///
/// The rows differ along what the rule generalises over: the branch the declaration
/// names, the repository's name, and whether the repository holds another branch or none
/// at all.
#[tokio::test]
async fn a_missing_branch_fails_with_the_providers_configuration_error_sentence() {
    // (case, the branch the repository holds, the branch the declaration names)
    let rows: Vec<(&str, Option<&str>, &str)> = vec![
        ("an empty repository, the default branch named", None, "main"),
        (
            "a repository whose only branch is another",
            Some("develop"),
            "release-1.0",
        ),
        (
            "a branch whose name is punctuated",
            Some("main"),
            "feature/new-etl",
        ),
    ];

    for (index, (case, held, configured)) in rows.into_iter().enumerate() {
        let emulator = Emulator::new();
        let pipeline = format!("unbranched-{index}");
        let artifacts = format!("{pipeline}-artifacts");
        let repository = format!("{pipeline}-repo");
        emulator.bucket(&artifacts).await;
        let files: Vec<(&str, &[u8])> = match held {
            Some(_) => vec![("etl.py", b"print(\"etl\")\n")],
            None => Vec::new(),
        };
        emulator
            .repository(&repository, held.unwrap_or("main"), &files)
            .await;
        emulator
            .cp_ok(
                "CreatePipeline",
                &declaration(
                    &pipeline,
                    &artifacts,
                    vec![
                        code_commit_source(&repository, configured, "SourceArtifact"),
                        approval_gate(),
                    ],
                ),
            )
            .await;
        wait_for_terminal(&emulator, case, &pipeline).await;

        // The sentence the CodeCommit source action reports for a branch the repository
        // does not hold: fixed text, with the branch and the repository taken from the
        // action's own configuration and the repository service's own message (none here,
        // which a relayed message states as `null`) closing it.
        let expected = format!(
            "The action failed because no branch named {configured} was found in the \
             selected AWS CodeCommit repository {repository}. Make sure you are using the \
             correct branch name, and then try again. Error: null"
        );
        let source = action_executions(&emulator, &pipeline)
            .await
            .into_iter()
            .find(|detail| detail["actionName"] == json!("Src"))
            .unwrap_or_else(|| panic!("case {case:?}: the source action was reached"));
        let result = &source["output"]["executionResult"];
        assert_eq!(
            (
                result["externalExecutionSummary"].clone(),
                result["errorDetails"]["code"].clone(),
                result["errorDetails"]["message"].clone()
            ),
            (json!(expected), json!(CONFIGURATION_ERROR), Value::Null),
            "case {case:?}: the provider's sentence is its summary for the execution, and \
             the history states its code alone beside it, got {source}"
        );

        let state = emulator
            .cp_ok("GetPipelineState", &json!({"name": pipeline}))
            .await;
        let action = state["stageStates"][0]["actionStates"][0].clone();
        assert_eq!(
            (
                action["latestExecution"]["errorDetails"]["message"].clone(),
                action["latestExecution"]["summary"].clone()
            ),
            (json!(expected), json!(expected)),
            "case {case:?}: the state reports the same sentence the history does, got \
             {state}"
        );
        // None of the emulator's own call to the repository service reaches the caller:
        // no Rust type name, no response dump.
        for leaked in ["ServiceError", "SdkBody", "StatusCode", "ErrorMetadata"] {
            assert!(
                !expected.contains(leaked)
                    && !action["latestExecution"]["errorDetails"]["message"]
                        .as_str()
                        .unwrap_or_default()
                        .contains(leaked),
                "case {case:?}: {leaked} is the emulator's own client object, not a fact \
                 about the action, got {action}"
            );
        }
    }
}

/// RULE: `ActionState$entityUrl` is the deep link the action's type publishes
/// (`ActionTypeSettings$entityUrlTemplate`) with every `{Config:<property>}` reference
/// replaced by the action's configuration value for that property; an action type that
/// publishes no template reports no URL at all.
///
/// The template is read back from `ListActionTypes` rather than written out here, so the
/// assertion is that the state resolves the PUBLISHED template for the action rather than
/// that it equals one particular string. The rows differ along what the rule generalises
/// over: the action type (one with a template, one without) and the configuration values
/// the template's references resolve to.
#[tokio::test]
async fn an_action_state_reports_its_action_types_entity_url_resolved_for_the_action() {
    // (case, the repository and branch the source action is configured with)
    let rows: Vec<(&str, &str, &str)> = vec![
        ("the default branch", "etl-repo", "main"),
        ("another branch of another repository", "web.app", "release-1.0"),
    ];

    for (index, (case, repository, branch)) in rows.into_iter().enumerate() {
        let emulator = Emulator::new();
        let pipeline = format!("linked-{index}");
        let artifacts = format!("{pipeline}-artifacts");
        emulator.bucket(&artifacts).await;
        emulator
            .repository(repository, branch, &[("etl.py", b"print(\"etl\")\n")])
            .await;
        emulator
            .cp_ok(
                "CreatePipeline",
                &declaration(
                    &pipeline,
                    &artifacts,
                    vec![
                        code_commit_source(repository, branch, "SourceArtifact"),
                        approval_gate(),
                    ],
                ),
            )
            .await;

        // What the action type PUBLISHES as its deep link, from the catalog the listing
        // reports — and the approval action type, which publishes none.
        let catalog = emulator
            .cp_ok("ListActionTypes", &json!({"actionOwnerFilter": "AWS"}))
            .await;
        let published = |category: &str, provider: &str| -> Option<String> {
            catalog["actionTypes"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|entry| {
                    entry["id"]["category"] == json!(category)
                        && entry["id"]["provider"] == json!(provider)
                })
                .and_then(|entry| entry["settings"]["entityUrlTemplate"].as_str())
                .map(str::to_string)
        };
        let template = published("Source", "CodeCommit").unwrap_or_else(|| {
            panic!("case {case:?}: the CodeCommit source type publishes a deep link")
        });
        let expected = template
            .replace("{Config:RepositoryName}", repository)
            .replace("{Config:BranchName}", branch);
        assert!(
            !expected.contains("{Config:"),
            "case {case:?}: every reference of the template is resolved by the action's \
             configuration, got {expected}"
        );

        let state = emulator
            .cp_ok("GetPipelineState", &json!({"name": pipeline}))
            .await;
        assert_eq!(
            state["stageStates"][0]["actionStates"][0]["entityUrl"],
            json!(expected),
            "case {case:?}: the action's state links the resource its configuration \
             names, got {state}"
        );
        assert_eq!(
            published("Approval", "Manual"),
            None,
            "case {case:?}: the approval action type publishes no deep link, so the \
             catalog carries none"
        );
        assert!(
            state["stageStates"][1]["actionStates"][0]
                .get("entityUrl")
                .is_none(),
            "case {case:?}: an action type publishing no template reports no URL, got \
             {state}"
        );
    }
}

/// RULE: retrying a stage runs its failed action again, and each ATTEMPT is an action
/// execution of its own — so the history holds one record per attempt, each under its own
/// `actionExecutionId`, and the `Latest` filter selects the newest of them.
///
/// The rows differ along what the rule generalises over: the `StageRetryMode` the retry
/// asks for and how many times the stage is retried.
#[tokio::test]
async fn each_retry_of_a_stage_appends_an_action_execution_for_its_attempt() {
    // (case, retryMode, how many times the stage is retried)
    let rows: Vec<(&str, &str, usize)> = vec![
        ("one retry of the failed action", "FAILED_ACTIONS", 1),
        ("two retries of the failed action", "FAILED_ACTIONS", 2),
        ("one retry of every action of the stage", "ALL_ACTIONS", 1),
    ];

    for (index, (case, retry_mode, retries)) in rows.into_iter().enumerate() {
        let emulator = Emulator::new();
        let pipeline = format!("retried-{index}");
        let artifacts = format!("{pipeline}-artifacts");
        let repository = format!("{pipeline}-repo");
        emulator.bucket(&artifacts).await;
        // An empty repository has no branch, so the source action cannot resolve the one
        // it names and fails — the state a retry is for.
        emulator.repository(&repository, "main", &[]).await;
        emulator
            .cp_ok(
                "CreatePipeline",
                &declaration(
                    &pipeline,
                    &artifacts,
                    vec![
                        code_commit_source(&repository, "main", "SourceArtifact"),
                        approval_gate(),
                    ],
                ),
            )
            .await;
        let settled = wait_for_terminal(&emulator, case, &pipeline).await;
        let id = settled["pipelineExecutionId"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let first = wait_for_attempts(&emulator, case, &pipeline, "Src", 1).await;
        let mut seen: Vec<String> = vec![first[0]["actionExecutionId"]
            .as_str()
            .unwrap_or_default()
            .to_string()];

        for attempt in 1..=retries {
            emulator
                .cp_ok(
                    "RetryStageExecution",
                    &json!({
                        "pipelineName": pipeline,
                        "stageName": "Source",
                        "pipelineExecutionId": id,
                        "retryMode": retry_mode,
                    }),
                )
                .await;
            wait_for_terminal(&emulator, case, &pipeline).await;
            let reported =
                wait_for_attempts(&emulator, case, &pipeline, "Src", attempt + 1).await;
            let ids: Vec<String> = reported
                .iter()
                .map(|detail| {
                    detail["actionExecutionId"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string()
                })
                .collect();
            for earlier in &seen {
                assert!(
                    ids.contains(earlier),
                    "case {case:?}: retry {attempt} must keep the earlier attempt \
                     {earlier}, got {ids:?}"
                );
            }
            let newest = ids
                .iter()
                .find(|id| !seen.contains(id))
                .unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: retry {attempt} runs the action again under a new \
                         id, got {ids:?}"
                    )
                })
                .clone();
            // Every attempt reports its own outcome, and this action fails the same way
            // each time: the configured branch is still not there.
            for detail in &reported {
                assert_eq!(
                    (
                        detail["status"].clone(),
                        detail["output"]["executionResult"]["errorDetails"]["code"].clone(),
                        detail["pipelineExecutionId"].clone()
                    ),
                    (json!("Failed"), json!(CONFIGURATION_ERROR), json!(id)),
                    "case {case:?}: each attempt of the retried action reports its own \
                     failure, got {detail}"
                );
            }
            seen.push(newest);
        }

        assert_eq!(
            seen.len(),
            retries + 1,
            "case {case:?}: one action execution per attempt, got {seen:?}"
        );
        // `LatestInPipelineExecutionFilter` with `Latest` selects the newest attempt of
        // each action, which is one record however many attempts there are.
        let latest = emulator
            .cp_ok(
                "ListActionExecutions",
                &json!({
                    "pipelineName": pipeline,
                    "filter": {"latestInPipelineExecution": {
                        "pipelineExecutionId": id,
                        "startTimeRange": "Latest",
                    }},
                }),
            )
            .await;
        let selected: Vec<&Value> = latest["actionExecutionDetails"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|detail| detail["actionName"] == json!("Src"))
            .collect();
        assert_eq!(
            selected.len(),
            1,
            "case {case:?}: the Latest filter keeps one attempt of the action, got {latest}"
        );
    }
}

/// RULE: an attempt exists only for a run that HAPPENED. An `ALL_ACTIONS` retry resets
/// every action of the failed stage, the ones the execution never reached included (an
/// action of a later `runOrder` than the one that failed), and those leave no attempt
/// behind — reporting one would report an action execution for work nothing performed.
#[tokio::test]
async fn an_all_actions_retry_records_no_attempt_for_an_action_that_never_ran() {
    let case = "two source actions, the second never reached";
    let emulator = Emulator::new();
    let pipeline = "unreached";
    let artifacts = "unreached-artifacts";
    let repository = "unreached-repo";
    emulator.bucket(artifacts).await;
    // An empty repository has no branch, so the action at the stage's lowest runOrder
    // fails and the one after it is never started.
    emulator.repository(repository, "main", &[]).await;
    let mut source = code_commit_source(repository, "main", "SourceArtifact");
    source["actions"][0]["runOrder"] = json!(1);
    source["actions"].as_array_mut().expect("the stage's actions").push(json!({
        "name": "Later",
        "actionTypeId": {
            "category": "Source", "owner": "AWS", "provider": "S3", "version": "1"
        },
        "configuration": {"S3Bucket": artifacts, "S3ObjectKey": "later.zip"},
        "outputArtifacts": [{"name": "LaterArtifact"}],
        "runOrder": 2,
    }));
    emulator
        .cp_ok(
            "CreatePipeline",
            &declaration(pipeline, artifacts, vec![source, approval_gate()]),
        )
        .await;
    let settled = wait_for_terminal(&emulator, case, pipeline).await;
    let id = settled["pipelineExecutionId"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    wait_for_attempts(&emulator, case, pipeline, "Src", 1).await;
    let unreached = |details: &[Value]| -> usize {
        details
            .iter()
            .filter(|detail| detail["actionName"] == json!("Later"))
            .count()
    };
    assert_eq!(
        unreached(&action_executions(&emulator, pipeline).await),
        0,
        "case {case:?}: the action the execution never started has no action execution"
    );

    emulator
        .cp_ok(
            "RetryStageExecution",
            &json!({
                "pipelineName": pipeline,
                "stageName": "Source",
                "pipelineExecutionId": id,
                "retryMode": "ALL_ACTIONS",
            }),
        )
        .await;
    wait_for_terminal(&emulator, case, pipeline).await;
    let attempts = wait_for_attempts(&emulator, case, pipeline, "Src", 2).await;
    assert_eq!(
        attempts.len(),
        2,
        "case {case:?}: the action that ran reports one attempt per run, got {attempts:?}"
    );
    let reported = action_executions(&emulator, pipeline).await;
    assert_eq!(
        unreached(&reported),
        0,
        "case {case:?}: the retry re-ran an action that had never started, so it still \
         has no action execution, got {reported:?}"
    );
}

/// RULE: the attempt history is a bounded WINDOW — a caller may retry a failed stage
/// again every time it fails again, so past the window the OLDEST attempt is dropped and
/// the store does not grow once per call
/// (`state::execution::MAX_ATTEMPTS_RETAINED_PER_STAGE`).
#[tokio::test]
async fn the_attempt_history_is_a_bounded_window() {
    /// The window the state keeps, plus the attempt in flight.
    const WINDOW: usize = 10;

    let emulator = Emulator::new();
    let pipeline = "windowed";
    let artifacts = "windowed-artifacts";
    let repository = "windowed-repo";
    emulator.bucket(artifacts).await;
    // An empty repository has no branch, so the source action fails every attempt.
    emulator.repository(repository, "main", &[]).await;
    emulator
        .cp_ok(
            "CreatePipeline",
            &declaration(
                pipeline,
                artifacts,
                vec![
                    code_commit_source(repository, "main", "SourceArtifact"),
                    approval_gate(),
                ],
            ),
        )
        .await;
    let settled = wait_for_terminal(&emulator, "the window", pipeline).await;
    let id = settled["pipelineExecutionId"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    // One retry past the window, so the oldest attempt has to fall out of it.
    let retries = WINDOW + 1;
    let mut dropped: Option<String> = None;
    for attempt in 1..=retries {
        let before = wait_for_attempts(&emulator, "the window", pipeline, "Src", {
            attempt.min(WINDOW + 1)
        })
        .await;
        if attempt == retries {
            // The attempt that will fall out of the window: the oldest reported one.
            dropped = before
                .last()
                .and_then(|oldest| oldest["actionExecutionId"].as_str())
                .map(str::to_string);
        }
        emulator
            .cp_ok(
                "RetryStageExecution",
                &json!({
                    "pipelineName": pipeline,
                    "stageName": "Source",
                    "pipelineExecutionId": id,
                    "retryMode": "FAILED_ACTIONS",
                }),
            )
            .await;
        wait_for_terminal(&emulator, "the window", pipeline).await;
    }

    let reported = wait_for_attempts(&emulator, "the window", pipeline, "Src", WINDOW + 1).await;
    let ids: Vec<&str> = reported
        .iter()
        .map(|detail| detail["actionExecutionId"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(
        ids.len(),
        WINDOW + 1,
        "case \"the window\": the history keeps the window and the attempt in flight, got \
         {ids:?}"
    );
    let dropped = dropped.unwrap_or_else(|| panic!("case \"the window\": an oldest attempt"));
    assert!(
        !ids.contains(&dropped.as_str()),
        "case \"the window\": the oldest attempt {dropped} falls out of the window, got \
         {ids:?}"
    );
}
