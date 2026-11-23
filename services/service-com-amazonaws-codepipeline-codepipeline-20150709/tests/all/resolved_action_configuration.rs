// SPDX-License-Identifier: CC0-1.0

//! What an action execution reports about the configuration it was RUN with, for an
//! action of any CATEGORY — end to end over the wire each service speaks.
//!
//! The rule under test:
//!
//! * `ActionExecutionInput$resolvedConfiguration` is "configuration data for an action
//!   execution with all variable references replaced with their real values for the
//!   execution", so `ListActionExecutions` reports it for EVERY action whose declared
//!   configuration this execution can resolve — in whatever category, through whatever
//!   provider, and whatever the run then did — beside the `configuration` the
//!   declaration states, which stays un-substituted. The member is the configuration the
//!   work ran with: the project a build action's build really ran for is the project the
//!   member names.
//!
//! The rows differ along what the rule generalises over: the action's CATEGORY — `Build`,
//! `Test`, `Deploy`, `Invoke` and `Approval` of the categories the model declares
//! (`smithy_query query="shape" shape="ActionCategory"` -> `Source`, `Build`, `Deploy`,
//! `Test`, `Invoke`, `Approval`, `Compute`) — the configuration MEMBER the reference is
//! declared in, the form of the reference (one on its own, one inside surrounding text),
//! and what the run then did with it: work that ran, work no mounted service could run, and
//! an action still waiting for a decision.
//!
//! The harness comes from `tests/all/image_and_build_actions.rs`: it is the one place in
//! this crate's tests where CodeBuild and Amazon ECR are mounted beside CodePipeline and
//! S3, which is what lets a Build action's work really run here.

use std::time::Instant;

use serde_json::{json, Value};

use crate::image_and_build_actions::{
    action_state, codebuild_build, declaration, ecr_source, manifest_of, s3_deploy,
    wait_for_terminal, Emulator, WAIT,
};

/// The bucket every pipeline of this suite keeps its artifacts in.
const ARTIFACT_BUCKET: &str = "resolved-config-artifacts";

/// The bucket the deploy row delivers into.
const DEPLOY_BUCKET: &str = "resolved-config-deploy";

/// The Amazon ECR repository the source action of every pipeline here reads.
const REPOSITORY: &str = "resolved-config-image";

/// The tag it reads the image at.
const TAG: &str = "latest";

/// The artifact the source action produces.
const ARTIFACT: &str = "ImageArt";

/// The CodeBuild project the Build and Test rows resolve the name of, and that the Build
/// action's work therefore really runs a build of.
const PROJECT: &str = "resolved-config-project";

/// The value the pipeline variable `Which` carries, which is the last label of [`PROJECT`]:
/// a row declares a reference to it inside surrounding text and resolves to that project.
const WHICH: &str = "project";

/// How often a wait looks again. The steps of an execution fall due on the service's own
/// span, so a wait polls for the CONDITION it needs and reports what it last saw when the
/// generous [`WAIT`] ceiling passes.
const LOOK: std::time::Duration = std::time::Duration::from_millis(25);

/// The pipeline variables every declaration of this suite carries, which are what its
/// references resolve against.
///
/// Two of them, so a row can declare a reference on its own and another can declare one
/// inside surrounding text that resolves to the same project name.
fn variables() -> Value {
    json!([
        {"name": "Which", "defaultValue": WHICH},
        {"name": "Project", "defaultValue": PROJECT},
    ])
}

/// A V2 declaration whose first stage reads the image and whose second stage is `stage`.
///
/// `variables` is a V2 declaration's member, and `pipelineType` is stated rather than left
/// to the default so the declaration says what it is.
fn pipeline_with(name: &str, stage: Value) -> Value {
    let mut body = declaration(
        name,
        ARTIFACT_BUCKET,
        vec![ecr_source(REPOSITORY, Some(TAG), ARTIFACT), stage],
    );
    body["pipeline"]["pipelineType"] = json!("V2");
    body["pipeline"]["variables"] = variables();
    body
}

/// Create the CodeBuild project a build action of this suite builds.
async fn create_project(emulator: &Emulator, name: &str) {
    let (status, created) = emulator
        .codebuild(
            "CreateProject",
            &json!({
                "name": name,
                "serviceRole": "arn:aws:iam::123456789012:role/service-role/cp",
                "source": {"type": "CODEPIPELINE", "buildspec": "version: 0.2"},
                "artifacts": {"type": "CODEPIPELINE"},
                "environment": {
                    "type": "LINUX_CONTAINER",
                    "image": "aws/codebuild/standard:7.0",
                    "computeType": "BUILD_GENERAL1_SMALL",
                },
            }),
        )
        .await;
    assert_eq!(
        status, 200,
        "the project {name:?} must be created, got {status} with {created}"
    );
}

/// The `ActionExecutionDetail` `ListActionExecutions` reports for `action`, waited for
/// until the execution has reached that action at all.
async fn reported_action(emulator: &Emulator, case: &str, pipeline: &str, action: &str) -> Value {
    let deadline = Instant::now() + WAIT;
    loop {
        let page = emulator
            .cp_ok("ListActionExecutions", &json!({"pipelineName": pipeline}))
            .await;
        let reported = page["actionExecutionDetails"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|detail| detail["actionName"] == json!(action))
            .cloned();
        if let Some(detail) = reported {
            return detail;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the run of {action} must be reported, last {page}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// One row of the category table: the stage the pipeline's second stage is, and the
/// configuration member whose declared reference the execution resolves.
struct CategoryRow {
    /// What this row is.
    case: &'static str,
    /// The stage declaration carrying the one action under test.
    stage: Value,
    /// That action's name.
    action: &'static str,
    /// The configuration member the reference is declared in.
    member: &'static str,
    /// The value the declaration states for it.
    declared: &'static str,
    /// What this execution resolves that value to, derived from the constants the
    /// declaration's references name rather than spelled out again.
    resolved: String,
}

/// RULE: the resolved configuration is reported for an action of ANY category — the
/// declared configuration with every reference the execution resolves replaced, beside
/// the declaration's own un-substituted text.
#[tokio::test]
async fn the_resolved_configuration_is_reported_for_an_action_of_every_category() {
    let emulator = Emulator::new();
    emulator.bucket(ARTIFACT_BUCKET).await;
    emulator.bucket(DEPLOY_BUCKET).await;
    emulator
        .ecr_ok("CreateRepository", &json!({"repositoryName": REPOSITORY}))
        .await;
    let digest = emulator
        .push(REPOSITORY, &manifest_of("resolved-config"), TAG)
        .await;
    assert!(
        digest.starts_with("sha256:"),
        "the image the source action reads must be stored, got {digest:?}"
    );
    create_project(&emulator, PROJECT).await;

    let rows = vec![
        CategoryRow {
            case: "a Build action's project name, from a reference inside surrounding text",
            stage: codebuild_build("resolved-config-#{variables.Which}", ARTIFACT, None),
            action: "Build",
            member: "ProjectName",
            declared: "resolved-config-#{variables.Which}",
            resolved: PROJECT.to_string(),
        },
        CategoryRow {
            case: "a Test action's project name, from a reference on its own",
            stage: json!({
                "name": "Verify",
                "actions": [{
                    "name": "Test",
                    "actionTypeId": {
                        "category": "Test",
                        "owner": "AWS",
                        "provider": "CodeBuild",
                        "version": "1",
                    },
                    "configuration": {"ProjectName": "#{variables.Project}"},
                    "inputArtifacts": [{"name": ARTIFACT}],
                }],
            }),
            action: "Test",
            member: "ProjectName",
            declared: "#{variables.Project}",
            resolved: PROJECT.to_string(),
        },
        CategoryRow {
            case: "a Deploy action's object key",
            stage: s3_deploy(ARTIFACT, DEPLOY_BUCKET, "out/#{variables.Which}.zip"),
            action: "Dep",
            member: "ObjectKey",
            declared: "out/#{variables.Which}.zip",
            resolved: format!("out/{WHICH}.zip"),
        },
        CategoryRow {
            case: "an Invoke action's function name, which no mounted service invokes",
            stage: json!({
                "name": "Invoke",
                "actions": [{
                    "name": "InvokeLambda",
                    "actionTypeId": {
                        "category": "Invoke",
                        "owner": "AWS",
                        "provider": "Lambda",
                        "version": "1",
                    },
                    "configuration": {
                        "FunctionName": "#{variables.Which}-function",
                        "UserParameters": "for #{variables.Project}",
                    },
                    "inputArtifacts": [{"name": ARTIFACT}],
                }],
            }),
            action: "InvokeLambda",
            member: "FunctionName",
            declared: "#{variables.Which}-function",
            resolved: format!("{WHICH}-function"),
        },
        CategoryRow {
            case: "an Approval action's custom data, while it waits for a decision",
            stage: json!({
                "name": "Gate",
                "actions": [{
                    "name": "Approve",
                    "actionTypeId": {
                        "category": "Approval",
                        "owner": "AWS",
                        "provider": "Manual",
                        "version": "1",
                    },
                    "configuration": {"CustomData": "approve #{variables.Project}"},
                }],
            }),
            action: "Approve",
            member: "CustomData",
            declared: "approve #{variables.Project}",
            resolved: format!("approve {PROJECT}"),
        },
    ];

    for (index, row) in rows.into_iter().enumerate() {
        let pipeline = format!("resolved-config-{index}");
        emulator
            .cp_ok("CreatePipeline", &pipeline_with(&pipeline, row.stage))
            .await;
        let detail = reported_action(&emulator, row.case, &pipeline, row.action).await;
        let member = row.member;
        assert_eq!(
            detail["input"]["configuration"][member],
            json!(row.declared),
            "case {:?}: the declared configuration is reported as declared, got {detail}",
            row.case
        );
        assert_eq!(
            detail["input"]["resolvedConfiguration"][member],
            json!(row.resolved),
            "case {:?}: expected the substituted {member}, got {detail}",
            row.case
        );
    }
}

/// RULE, the same member against the work it names: an action's resolved configuration is
/// what the action RAN with, so the project a build action's build really ran for is the
/// one the member reports — not the template the declaration carries.
#[tokio::test]
async fn a_build_actions_resolved_configuration_names_the_project_its_build_ran_for() {
    let case = "a build action whose project name is a reference";
    let emulator = Emulator::new();
    emulator.bucket(ARTIFACT_BUCKET).await;
    emulator
        .ecr_ok("CreateRepository", &json!({"repositoryName": REPOSITORY}))
        .await;
    emulator
        .push(REPOSITORY, &manifest_of("resolved-build"), TAG)
        .await;
    create_project(&emulator, PROJECT).await;

    let declared = "resolved-config-#{variables.Which}";
    let pipeline = "resolved-config-build";
    emulator
        .cp_ok(
            "CreatePipeline",
            &pipeline_with(pipeline, codebuild_build(declared, ARTIFACT, None)),
        )
        .await;
    wait_for_terminal(&emulator, pipeline).await;

    // The work really ran against the RESOLVED name: CodeBuild holds a build of that
    // project, and it is the build the action reports as its external execution.
    let (status, listed) = emulator
        .codebuild("ListBuildsForProject", &json!({"projectName": PROJECT}))
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: ListBuildsForProject must succeed, got {status} with {listed}"
    );
    let builds = listed["ids"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        builds.len(),
        1,
        "case {case:?}: the action must have started one build of {PROJECT}, got {listed}"
    );
    let build_id = builds[0].as_str().unwrap_or_default().to_string();
    let (status, read) = emulator
        .codebuild("BatchGetBuilds", &json!({"ids": [build_id.clone()]}))
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: BatchGetBuilds must succeed, got {status} with {read}"
    );
    assert_eq!(
        read["builds"][0]["projectName"],
        json!(PROJECT),
        "case {case:?}: the build belongs to the resolved project, got {read}"
    );

    let state = action_state(&emulator, pipeline, "Build", "Build").await;
    assert_eq!(
        state["latestExecution"]["externalExecutionId"],
        json!(build_id),
        "case {case:?}: the action reports the build it started, got {state}"
    );

    // And what the action execution reports it ran with is that same project, beside the
    // template the declaration still carries.
    let detail = reported_action(&emulator, case, pipeline, "Build").await;
    assert_eq!(
        detail["input"]["resolvedConfiguration"]["ProjectName"],
        json!(PROJECT),
        "case {case:?}: expected the project the build ran for, got {detail}"
    );
    assert_eq!(
        detail["input"]["configuration"]["ProjectName"],
        json!(declared),
        "case {case:?}: the declared configuration keeps its reference, got {detail}"
    );
}
