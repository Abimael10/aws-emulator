// SPDX-License-Identifier: CC0-1.0

//! The ARTIFACTS a CodePipeline `CodeBuild` action and the build it starts exchange.
//!
//! RULE: a build action does not perform its work itself — it starts a build of the project
//! its `ProjectName` names — and such a project's own source states no location, because
//! "CodePipeline uses the settings in a pipeline's source action instead of this value". So
//! the pipeline hands the build the two ends of the action's artifacts: the build is built
//! FROM the action's INPUT artifact, which is the object an earlier action produced in the
//! pipeline's artifact store (the build reports it as its own `sourceVersion`, and its
//! commands run with the files that archive holds), and it PUTS what it produced at the
//! object the action's OUTPUT artifact occupies in that same store, which is then the
//! artifact the actions after it read. An action that declares no output artifact has
//! nowhere to put anything, so its build uploads nothing and the execution publishes no
//! artifact for it.
//!
//! Four crates have to agree for that to hold (CodePipeline drives the action, CodeBuild
//! runs the build, Amazon S3 holds both ends of the exchange and IAM holds the two roles),
//! so this suite lives here: a crate-local test can only see one side of it.
//!
//! The rows differ along what the rule generalises over: the files the input artifact holds
//! and where in its tree they are, which of them the build's commands read, and whether the
//! action declares an output artifact at all.
//!
//! Nothing here sleeps for a build or an execution to finish: it POLLS the reporting
//! operations until the CONDITION holds, exactly as a client does.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode};

/// The Region every request of this suite is signed for.
const REGION: &str = "us-west-2";

/// How long a poll waits for a transition the clock owes. A generous ceiling, far above the
/// spans the two services allot a step, not an expectation.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

/// How long one poll pauses before asking again.
const POLL_PAUSE: Duration = Duration::from_millis(20);

/// The `PipelineExecutionStatus` values an execution never leaves.
const TERMINAL: [&str; 5] = [
    "Succeeded",
    "Failed",
    "Stopped",
    "Superseded",
    "Cancelled",
];

/// The emulator this suite drives: CodePipeline, CodeBuild, Amazon S3 and IAM on ONE
/// engine, as the router mounts them, with request authentication off so each call carries
/// a credential scope without a computed signature.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_codepipeline_codepipeline_20150709::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_codebuild_codebuild_20161006::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// Dispatch one request addressed to `signing_name`, the way a signed SDK call is.
async fn send(
    router: &MultiServiceRouter,
    signing_name: &str,
    method: &str,
    path: &str,
    headers: &[(&str, String)],
    body: Bytes,
) -> (u16, Vec<u8>) {
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let mut request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTEST/20260101/{REGION}/{signing_name}/\
                 aws4_request, SignedHeaders=host, Signature=not-verified"
            ),
        );
    for (name, value) in headers {
        request = request.header(*name, value);
    }
    let response = router
        .dispatch(request.body(body).expect("a well-formed request"))
        .await;
    (response.status().as_u16(), response.into_body().to_vec())
}

/// One awsJson1_1 call to the service `signing_name` publishes under `prefix`.
async fn json_rpc(
    router: &MultiServiceRouter,
    signing_name: &str,
    prefix: &str,
    operation: &str,
    body: Value,
) -> (u16, Value) {
    let (status, bytes) = send(
        router,
        signing_name,
        "POST",
        "/",
        &[
            ("content-type", "application/x-amz-json-1.1".to_string()),
            ("x-amz-target", format!("{prefix}.{operation}")),
        ],
        Bytes::from(serde_json::to_vec(&body).expect("the test body serializes")),
    )
    .await;
    let parsed = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One CodeBuild operation that has to succeed.
async fn codebuild(router: &MultiServiceRouter, case: &str, operation: &str, body: Value) -> Value {
    let (status, answer) = json_rpc(router, "codebuild", "CodeBuild_20161006", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: codebuild {operation} must answer 200, got {status} with {answer}"
    );
    answer
}

/// One CodePipeline operation that has to succeed.
async fn codepipeline(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: Value,
) -> Value {
    let (status, answer) =
        json_rpc(router, "codepipeline", "CodePipeline_20150709", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: codepipeline {operation} must answer 200, got {status} with {answer}"
    );
    answer
}

/// Create a bucket in the Region this suite is signed for.
async fn create_bucket(router: &MultiServiceRouter, bucket: &str) {
    let (status, body) = send(
        router,
        "s3",
        "PUT",
        &format!("/{bucket}"),
        &[("content-type", "application/xml".to_string())],
        Bytes::from(format!(
            "<CreateBucketConfiguration xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
             <LocationConstraint>{REGION}</LocationConstraint></CreateBucketConfiguration>"
        )),
    )
    .await;
    assert_eq!(
        status,
        200,
        "setup: CreateBucket({bucket:?}) must succeed, got {status} / {}",
        String::from_utf8_lossy(&body)
    );
}

/// The bytes the object at `bucket`/`key` holds — `None` when the store holds none there.
async fn object_bytes(router: &MultiServiceRouter, bucket: &str, key: &str) -> Option<Vec<u8>> {
    let (status, body) = send(
        router,
        "s3",
        "GET",
        &format!("/{bucket}/{key}"),
        &[],
        Bytes::new(),
    )
    .await;
    (status == 200).then_some(body)
}

/// Create an IAM role named `name` that `service` may assume, and answer its ARN.
async fn role(router: &MultiServiceRouter, name: &str, service: &str) -> String {
    let document = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": service },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let form = format!(
        "Action=CreateRole&Version=2010-05-08&RoleName={}&AssumeRolePolicyDocument={}",
        form_encode(name),
        form_encode(&document)
    );
    let (status, body) = send(
        router,
        "iam",
        "POST",
        "/",
        &[(
            "content-type",
            "application/x-www-form-urlencoded".to_string(),
        )],
        Bytes::from(form),
    )
    .await;
    let body = String::from_utf8_lossy(&body).to_string();
    assert_eq!(
        status, 200,
        "setup: CreateRole({name:?}) must succeed, got {status} / {body}"
    );
    element(&body, "Arn")
}

/// Poll `BatchGetBuilds` for `id` until the build reports itself complete.
async fn settled_build(router: &MultiServiceRouter, case: &str, id: &str) -> Value {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    let mut last = Value::Null;
    while Instant::now() < deadline {
        let read = codebuild(router, case, "BatchGetBuilds", json!({"ids": [id]})).await;
        last = read["builds"][0].clone();
        if last["buildComplete"] == json!(true) {
            return last;
        }
        tokio::time::sleep(POLL_PAUSE).await;
    }
    panic!("case {case:?}: the build {id} never reported itself complete; last read {last}")
}

/// Put a ZIP archive holding `files` at `bucket`/`key`, by running a build whose artifacts
/// are packaged as one.
///
/// A build's `ZIP` artifacts are "a ZIP file that contains the build output" put at the
/// destination as one object, which is exactly the form a pipeline's source object takes —
/// so the suite stages its input without writing an archive by hand.
async fn staged_archive(
    router: &MultiServiceRouter,
    case: &str,
    role_arn: &str,
    bucket: &str,
    key: &str,
    files: &[(&str, &str)],
) {
    let project = format!("stage-{key}").replace(['/', '.'], "-");
    let mut commands = String::new();
    for (path, contents) in files {
        if let Some((directory, _)) = path.rsplit_once('/') {
            commands.push_str(&format!("      - \"mkdir -p {directory}\"\n"));
        }
        commands.push_str(&format!(
            "      - \"printf '%s' '{contents}' > {path}\"\n"
        ));
    }
    let buildspec = format!(
        "version: 0.2\nphases:\n  build:\n    commands:\n{commands}artifacts:\n  files:\n    \
         - '**/*'\n"
    );
    codebuild(
        router,
        case,
        "CreateProject",
        json!({
            "name": project,
            "serviceRole": role_arn,
            "source": {"type": "NO_SOURCE", "buildspec": buildspec},
            "artifacts": {
                "type": "S3",
                "location": bucket,
                "name": key,
                "packaging": "ZIP",
            },
            "environment": {
                "type": "LINUX_CONTAINER",
                "image": "aws/codebuild/standard:7.0",
                "computeType": "BUILD_GENERAL1_SMALL",
            },
        }),
    )
    .await;
    let started = codebuild(
        router,
        case,
        "StartBuild",
        json!({"projectName": project}),
    )
    .await;
    let id = started["build"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: StartBuild must mint a build, got {started}"))
        .to_string();
    let staged = settled_build(router, case, &id).await;
    assert_eq!(
        staged["buildStatus"],
        json!("SUCCEEDED"),
        "setup: the build that stages {key:?} must succeed, got {staged}"
    );
}

/// A pipeline whose Source stage reads `source_key` out of `bucket` and whose Build stage
/// invokes `project`, declaring an output artifact only when `output` names one.
fn declaration(
    name: &str,
    role_arn: &str,
    bucket: &str,
    source_key: &str,
    project: &str,
    output: Option<&str>,
) -> Value {
    let mut build = json!({
        "name": "Build",
        "actionTypeId": {
            "category": "Build",
            "owner": "AWS",
            "provider": "CodeBuild",
            "version": "1",
        },
        "configuration": { "ProjectName": project },
        "inputArtifacts": [{ "name": "Src" }],
        "runOrder": 1,
    });
    if let Some(output) = output {
        build["outputArtifacts"] = json!([{ "name": output }]);
    }
    json!({
        "name": name,
        "roleArn": role_arn,
        "artifactStore": { "type": "S3", "location": bucket },
        "stages": [
            {
                "name": "Source",
                "actions": [{
                    "name": "Src",
                    "actionTypeId": {
                        "category": "Source",
                        "owner": "AWS",
                        "provider": "S3",
                        "version": "1",
                    },
                    "configuration": {
                        "S3Bucket": bucket,
                        "S3ObjectKey": source_key,
                        "PollForSourceChanges": "false",
                    },
                    "outputArtifacts": [{ "name": "Src" }],
                    "runOrder": 1,
                }],
            },
            { "name": "Build", "actions": [build] },
        ],
    })
}

/// Wait until every execution of `pipeline` has reached a terminal status.
async fn drained(router: &MultiServiceRouter, case: &str, pipeline: &str) {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    let mut last = Value::Null;
    while Instant::now() < deadline {
        let answer = codepipeline(
            router,
            case,
            "ListPipelineExecutions",
            json!({"pipelineName": pipeline}),
        )
        .await;
        let pending: Vec<Value> = answer["pipelineExecutionSummaries"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|summary| !TERMINAL.contains(&summary["status"].as_str().unwrap_or_default()))
            .collect();
        if pending.is_empty() {
            return;
        }
        last = json!(pending);
        tokio::time::sleep(POLL_PAUSE).await;
    }
    panic!("case {case:?}: executions of {pipeline} never settled; last pending {last}")
}

/// Start an execution of `pipeline`, wait for it to settle, and answer its id and status.
async fn ran(router: &MultiServiceRouter, case: &str, pipeline: &str) -> (String, String) {
    let started = codepipeline(
        router,
        case,
        "StartPipelineExecution",
        json!({"name": pipeline}),
    )
    .await;
    let id = started["pipelineExecutionId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the start must report an id, got {started}"))
        .to_string();
    let deadline = Instant::now() + SETTLE_DEADLINE;
    let mut last = String::new();
    while Instant::now() < deadline {
        let answer = codepipeline(
            router,
            case,
            "GetPipelineExecution",
            json!({"pipelineName": pipeline, "pipelineExecutionId": &id}),
        )
        .await;
        last = answer["pipelineExecution"]["status"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if TERMINAL.contains(&last.as_str()) {
            return (id, last);
        }
        tokio::time::sleep(POLL_PAUSE).await;
    }
    panic!("case {case:?}: the execution {id} never settled; last status {last:?}")
}

/// The action execution `ListActionExecutions` reports for `action` of `execution`.
async fn action_execution(
    router: &MultiServiceRouter,
    case: &str,
    pipeline: &str,
    execution: &str,
    action: &str,
) -> Value {
    let answer = codepipeline(
        router,
        case,
        "ListActionExecutions",
        json!({
            "pipelineName": pipeline,
            "filter": {"pipelineExecutionId": execution},
        }),
    )
    .await;
    answer["actionExecutionDetails"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .find(|detail| detail["actionName"].as_str() == Some(action))
        .unwrap_or_else(|| panic!("case {case:?}: no action execution for {action}, got {answer}"))
}

/// The value a settled build reports for the exported variable `name`, or an empty string.
fn exported(build: &Value, name: &str) -> String {
    build["exportedEnvironmentVariables"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .find(|variable| variable["name"] == json!(name))
        .and_then(|variable| variable["value"].as_str().map(str::to_string))
        .unwrap_or_default()
}

/// The buildspec of the project a row's Build action invokes: it reads one file out of the
/// tree the pipeline's input artifact was unpacked into, publishes the address the build
/// was built from, and declares every file of the tree as its artifact.
fn buildspec(read_path: &str) -> String {
    format!(
        "version: 0.2\nenv:\n  exported-variables:\n    - PAYLOAD\n    - SOURCE\n\
         phases:\n  build:\n    commands:\n      \
         - \"export PAYLOAD=\\\"$(cat {read_path})\\\"\"\n      \
         - \"export SOURCE=\\\"$CODEBUILD_SOURCE_VERSION\\\"\"\n\
         artifacts:\n  files:\n    - '**/*'\n"
    )
}

/// RULE: the build a Build action starts is built FROM the action's input artifact — the
/// object an earlier action produced in the pipeline's artifact store, which the build
/// reports as its `sourceVersion` and whose files its commands run on — and PUTS what it
/// produced at the object the action's output artifact occupies there, which is what the
/// execution then publishes for that artifact. An action that declares no output artifact
/// has its build upload nothing.
#[tokio::test]
async fn a_build_action_hands_the_build_its_input_artifact_and_publishes_what_it_produced() {
    /// One row: the case, the files the input artifact holds, the one the build's commands
    /// read, the payload it must read out of it, and the output artifact the action
    /// declares, if any.
    struct Row {
        case: &'static str,
        staged: Vec<(&'static str, &'static str)>,
        read_path: &'static str,
        payload: &'static str,
        output: Option<&'static str>,
    }
    let rows = [
        Row {
            case: "one file at the root of the input artifact",
            staged: vec![("app.txt", "sim2real-root-payload")],
            read_path: "app.txt",
            payload: "sim2real-root-payload",
            output: Some("BuildOut"),
        },
        Row {
            case: "several files, the one read under a directory",
            staged: vec![
                ("app.txt", "sim2real-root-payload"),
                ("dist/app.txt", "sim2real-nested-payload"),
            ],
            read_path: "dist/app.txt",
            payload: "sim2real-nested-payload",
            output: Some("Packaged"),
        },
        Row {
            case: "an action that declares no output artifact",
            staged: vec![("app.txt", "sim2real-no-output-payload")],
            read_path: "app.txt",
            payload: "sim2real-no-output-payload",
            output: None,
        },
    ];

    let router = emulator();
    let pipeline_role = role(&router, "cp-artifacts-pipeline-role", "codepipeline.amazonaws.com")
        .await;
    let project_role = role(&router, "cp-artifacts-project-role", "codebuild.amazonaws.com").await;
    let bucket = "cp-build-action-artifacts";
    create_bucket(&router, bucket).await;

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let source_key = format!("source-{index}.zip");
        let project = format!("cp-artifacts-project-{index}");
        let pipeline = format!("cp-artifacts-pipeline-{index}");
        staged_archive(
            &router,
            case,
            &project_role,
            bucket,
            &source_key,
            &row.staged,
        )
        .await;
        codebuild(
            &router,
            case,
            "CreateProject",
            json!({
                "name": project,
                "serviceRole": project_role,
                "source": {"type": "CODEPIPELINE", "buildspec": buildspec(row.read_path)},
                "artifacts": {"type": "CODEPIPELINE"},
                "environment": {
                    "type": "LINUX_CONTAINER",
                    "image": "aws/codebuild/standard:7.0",
                    "computeType": "BUILD_GENERAL1_SMALL",
                },
            }),
        )
        .await;
        codepipeline(
            &router,
            case,
            "CreatePipeline",
            json!({
                "pipeline": declaration(
                    &pipeline,
                    &pipeline_role,
                    bucket,
                    &source_key,
                    &project,
                    row.output,
                ),
            }),
        )
        .await;
        drained(&router, case, &pipeline).await;

        let (execution, status) = ran(&router, case, &pipeline).await;
        assert_eq!(
            status, "Succeeded",
            "case {case:?}: the execution must succeed, got {status:?}"
        );

        // The build the action followed, as CodeBuild holds it.
        let detail = action_execution(&router, case, &pipeline, &execution, "Build").await;
        let build_id = detail["output"]["executionResult"]["externalExecutionId"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {case:?}: the action must name the build it ran, got {detail}")
            })
            .to_string();
        let build = settled_build(&router, case, &build_id).await;

        // The input artifact the earlier action produced is what the build was built FROM:
        // the action reports where that artifact is, and the build reports that same object
        // as its source version.
        let input = detail["input"]["inputArtifacts"][0]["s3location"].clone();
        let input_key = input["key"].as_str().unwrap_or_default().to_string();
        assert_eq!(
            input["bucket"].as_str(),
            Some(bucket),
            "case {case:?}: the input artifact must live in the artifact store, got {input}"
        );
        let addressed = format!("arn:aws:s3:::{bucket}/{input_key}");
        assert_eq!(
            build["sourceVersion"].as_str(),
            Some(addressed.as_str()),
            "case {case:?}: expected the build to be built from {addressed:?}, got {build}"
        );
        assert!(
            object_bytes(&router, bucket, &input_key).await.is_some(),
            "case {case:?}: the store must hold the artifact the build was built from, \
             {input_key:?}"
        );
        assert_eq!(
            exported(&build, "SOURCE"),
            addressed,
            "case {case:?}: the build environment must carry that same address, got {build}"
        );

        // And the build really RAN on the files that artifact holds.
        assert_eq!(
            exported(&build, "PAYLOAD"),
            row.payload,
            "case {case:?}: expected the build's commands to read {:?} out of the input \
             artifact, got {build}",
            row.payload
        );

        // What the build produced is at the object the action's output artifact occupies —
        // and an action that declares none has nothing put anywhere.
        let published = detail["output"]["outputArtifacts"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        match row.output {
            Some(name) => {
                let location = published
                    .iter()
                    .find(|artifact| artifact["name"].as_str() == Some(name))
                    .map(|artifact| artifact["s3location"].clone())
                    .unwrap_or_else(|| {
                        panic!(
                            "case {case:?}: the action must publish the artifact {name}, got \
                             {published:?}"
                        )
                    });
                let key = location["key"].as_str().unwrap_or_default().to_string();
                assert_eq!(
                    location["bucket"].as_str(),
                    Some(bucket),
                    "case {case:?}: expected the artifact store, got {location}"
                );
                assert_eq!(
                    build["artifacts"]["location"].as_str(),
                    Some(format!("arn:aws:s3:::{bucket}/{key}").as_str()),
                    "case {case:?}: the build must report that it put its output there, got \
                     {build}"
                );
                let bytes = object_bytes(&router, bucket, &key).await.unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: the store must hold the artifact the build produced, \
                         {key:?}"
                    )
                });
                assert_eq!(
                    bytes.get(..2),
                    Some(b"PK".as_slice()),
                    "case {case:?}: an artifact of a pipeline is an archive, got {} bytes \
                     starting {:?}",
                    bytes.len(),
                    String::from_utf8_lossy(bytes.get(..4).unwrap_or_default())
                );
            }
            None => {
                assert!(
                    published.is_empty(),
                    "case {case:?}: an action declaring no output artifact publishes none, got \
                     {published:?}"
                );
                let location = build["artifacts"]["location"].as_str().unwrap_or_default();
                assert!(
                    location.is_empty(),
                    "case {case:?}: and its build names no object it put anywhere, got \
                     {location:?}"
                );
            }
        }
    }
}

/// RULE: the object a Build action publishes for its output artifact is the one the actions
/// AFTER it read — so a Deploy action consuming it delivers the very files the build
/// produced.
#[tokio::test]
async fn the_artifact_a_build_action_publishes_is_what_the_next_action_reads() {
    let case = "a Source, Build and Deploy pipeline";
    let router = emulator();
    let pipeline_role = role(&router, "cp-chain-pipeline-role", "codepipeline.amazonaws.com").await;
    let project_role = role(&router, "cp-chain-project-role", "codebuild.amazonaws.com").await;
    let bucket = "cp-build-action-chain";
    create_bucket(&router, bucket).await;
    staged_archive(
        &router,
        case,
        &project_role,
        bucket,
        "chain-source.zip",
        &[("app.txt", "sim2real-chained-payload")],
    )
    .await;

    // The build's own commands produce a file of their own out of the input artifact's, so
    // what the Deploy action delivers can only have come through the build.
    let project = "cp-chain-project";
    codebuild(
        &router,
        case,
        "CreateProject",
        json!({
            "name": project,
            "serviceRole": project_role,
            "source": {
                "type": "CODEPIPELINE",
                "buildspec": "version: 0.2\nphases:\n  build:\n    commands:\n      \
                    - \"printf 'built:%s' \\\"$(cat app.txt)\\\" > built.txt\"\n\
                    artifacts:\n  files:\n    - built.txt\n",
            },
            "artifacts": {"type": "CODEPIPELINE"},
            "environment": {
                "type": "LINUX_CONTAINER",
                "image": "aws/codebuild/standard:7.0",
                "computeType": "BUILD_GENERAL1_SMALL",
            },
        }),
    )
    .await;

    let pipeline = "cp-chain-pipeline";
    let mut declared = declaration(
        pipeline,
        &pipeline_role,
        bucket,
        "chain-source.zip",
        project,
        Some("BuildOut"),
    );
    declared["stages"].as_array_mut().expect("the stages").push(json!({
        "name": "Deploy",
        "actions": [{
            "name": "Deploy",
            "actionTypeId": {
                "category": "Deploy",
                "owner": "AWS",
                "provider": "S3",
                "version": "1",
            },
            "configuration": {
                "BucketName": bucket,
                "ObjectKey": "deployed",
                "Extract": "true",
            },
            "inputArtifacts": [{ "name": "BuildOut" }],
            "runOrder": 1,
        }],
    }));
    codepipeline(&router, case, "CreatePipeline", json!({"pipeline": declared})).await;
    drained(&router, case, pipeline).await;

    let (_, status) = ran(&router, case, pipeline).await;
    assert_eq!(
        status, "Succeeded",
        "case {case:?}: the execution must succeed, got {status:?}"
    );
    let delivered = object_bytes(&router, bucket, "deployed/built.txt")
        .await
        .unwrap_or_else(|| {
            panic!("case {case:?}: the Deploy action must deliver the file the build produced")
        });
    assert_eq!(
        String::from_utf8_lossy(&delivered),
        "built:sim2real-chained-payload",
        "case {case:?}: expected the bytes the build wrote out of its input artifact, got {:?}",
        String::from_utf8_lossy(&delivered)
    );
}
