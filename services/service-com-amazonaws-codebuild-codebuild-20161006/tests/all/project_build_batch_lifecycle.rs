// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for CodeBuild's project -> batch build slice, driven over the wire
//! the router serves: `POST /` with `X-Amz-Target: CodeBuild_20161006.<Operation>` on
//! awsJson1_1.
//!
//! Every assertion here is about state: what `StartBuildBatch` persisted is what
//! `BatchGetBuildBatches` reports, and nothing is recomputed per call. The same holds for
//! the call that runs a stored batch build again, `RetryBuildBatch`, whose cover is the
//! last three suites below.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use service_com_amazonaws_codebuild_codebuild_20161006::ops::types::{
    BuildBatch, RETRY_BUILD_BATCH_TYPE_VALUES,
};
use service_com_amazonaws_codebuild_codebuild_20161006::state::{
    self, ComAmazonawsCodebuildCodebuild20161006State as CodeBuildState, StartBuildToken,
    StoredBuildBatch,
};
use service_com_amazonaws_codebuild_codebuild_20161006::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

// This suite drives the wire through its own harness (its own region, access key and
// `call`), and reaches into the shared harness for the ONE fragment both need: the
// `buildBatchConfig` that makes a project batch-enabled.
use crate::project_build_lifecycle::{batch_enabled, merged};

/// The region every request below is signed for — deliberately not the emulator's
/// `us-east-1` default, so an ARN assertion proves the region came from the request.
const REGION: &str = "us-west-2";

/// The access key id every request below is signed with. `BuildBatch$initiator`
/// reports the calling identity, which is this.
const ACCESS_KEY_ID: &str = "AKIDCODEBUILDBATCH";

/// Long enough for a batch build's clock-owed transition out of `IN_PROGRESS` to have
/// fallen due (the service settles it 750 ms after StartBuildBatch).
const PAST_THE_SETTLE_SPAN: Duration = Duration::from_millis(1_100);

/// One router carrying only this service, over one shared engine — the same assembly
/// `core/router` builds, minus the other services.
fn emulator() -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::new(engine::InMemoryEngine::new())));
    router
}

/// Call one operation and return its HTTP status and decoded body.
async fn call(router: &MultiServiceRouter, operation: &str, body: Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("CodeBuild_20161006.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential={ACCESS_KEY_ID}/20260101/{REGION}/codebuild/\
                 aws4_request, SignedHeaders=host;x-amz-target, Signature=0f0f"
            ),
        )
        .body(bytes::Bytes::from(
            serde_json::to_vec(&body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let decoded = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, decoded)
}

/// A minimal valid `CreateProject` request for `name`, with `extra`'s members merged in.
fn create_project_body(name: &str, extra: Value) -> Value {
    let mut body = json!({
        "name": name,
        "source": {"type": "NO_SOURCE", "buildspec": "version: 0.2"},
        "artifacts": {"type": "NO_ARTIFACTS"},
        "environment": {
            "type": "LINUX_CONTAINER",
            "image": "aws/codebuild/standard:7.0",
            "computeType": "BUILD_GENERAL1_SMALL",
        },
        "serviceRole": "arn:aws:iam::000000000000:role/service-role/codebuild",
    });
    if let (Value::Object(target), Value::Object(members)) = (&mut body, extra) {
        target.extend(members);
    }
    body
}

/// Create `name`, asserting it succeeded, and hand back the stored project.
async fn create_project(router: &MultiServiceRouter, name: &str, extra: Value) -> Value {
    let (status, body) = call(router, "CreateProject", create_project_body(name, extra)).await;
    assert_eq!(
        status, 200,
        "CreateProject {name:?}: expected 200, got {status} with {body}"
    );
    body["project"].clone()
}

/// Start a batch build, asserting it succeeded, and hand back the batch build.
async fn start_build_batch(router: &MultiServiceRouter, body: Value) -> Value {
    let (status, response) = call(router, "StartBuildBatch", body.clone()).await;
    assert_eq!(
        status, 200,
        "StartBuildBatch {body}: expected 200, got {status} with {response}"
    );
    response["buildBatch"].clone()
}

/// Read one batch build back through `BatchGetBuildBatches`.
async fn get_build_batch(router: &MultiServiceRouter, id: &str) -> Value {
    let (status, body) = call(router, "BatchGetBuildBatches", json!({"ids": [id]})).await;
    assert_eq!(
        status, 200,
        "BatchGetBuildBatches {id:?}: expected 200, got {status} with {body}"
    );
    body["buildBatches"][0].clone()
}

/// The `__type` an error body carries, for asserting the error code a client branches on.
fn error_type(body: &Value) -> String {
    body["__type"].as_str().unwrap_or_default().to_string()
}

fn expected_arn(resource: &str) -> String {
    format!(
        "arn:aws:codebuild:{REGION}:{}:{resource}",
        engine::EMULATOR_ACCOUNT_ID
    )
}

/// The member `path` names, walked from `value`.
fn at<'a>(value: &'a Value, path: &[&str]) -> &'a Value {
    let mut cursor = value;
    for segment in path {
        cursor = &cursor[*segment];
    }
    cursor
}

/// The whole slice, in the order a client drives it: `StartBuildBatch` mints a batch
/// build out of the stored project, and every later read reports that record.
#[tokio::test]
async fn the_project_to_build_batch_path_is_served_from_stored_state() {
    let router = emulator();
    let project = create_project(
        &router,
        "batch-proj",
        json!({
            "buildBatchConfig": {
                "serviceRole": "arn:aws:iam::000000000000:role/service-role/batch",
                "combineArtifacts": true,
                "timeoutInMins": 120,
                "restrictions": {"maximumBuildsAllowed": 4},
            },
        }),
    )
    .await;

    let batch = start_build_batch(&router, json!({"projectName": "batch-proj"})).await;

    let id = batch["id"]
        .as_str()
        .unwrap_or_else(|| panic!("StartBuildBatch: the batch build carries an id, got {batch}"))
        .to_string();
    let minted = id.strip_prefix("batch-proj:").unwrap_or_else(|| {
        panic!("StartBuildBatch: id must be <projectName>:<uuid>, got {id:?}")
    });
    assert_eq!(
        (
            minted.len(),
            minted.match_indices('-').map(|(at, _)| at).collect::<Vec<_>>(),
            minted.chars().all(|c| c.is_ascii_hexdigit() || c == '-'),
        ),
        (36, vec![8, 13, 18, 23], true),
        "StartBuildBatch: the minted id must be an 8-4-4-4-12 hex UUID, got {minted:?}"
    );
    assert_eq!(
        batch["arn"],
        expected_arn(&format!("build-batch/{id}")).as_str(),
        "StartBuildBatch: a batch build's ARN names the build-batch resource, got {batch}"
    );
    assert_eq!(
        batch["buildBatchNumber"], 1,
        "StartBuildBatch: a project's first batch build is number 1, got {batch}"
    );
    assert_eq!(
        batch["buildBatchStatus"], "IN_PROGRESS",
        "StartBuildBatch: a fresh batch build is IN_PROGRESS, got {batch}"
    );
    assert_eq!(
        batch["complete"], false,
        "StartBuildBatch: a fresh batch build is not complete, got {batch}"
    );
    assert_eq!(
        batch["currentPhase"], "DOWNLOAD_BATCHSPEC",
        "StartBuildBatch: a submitted batch build fetches its batch spec next, got {batch}"
    );
    assert_eq!(
        batch["initiator"], ACCESS_KEY_ID,
        "StartBuildBatch: initiator is the calling identity, got {batch}"
    );
    assert!(
        batch["startTime"].is_number(),
        "StartBuildBatch: startTime must be set, got {batch}"
    );
    assert!(
        batch["endTime"].is_null(),
        "StartBuildBatch: a running batch build has no endTime, got {batch}"
    );
    // Resolved from the stored project, not echoed from the request. A batch reports
    // the configuration it RUNS WITH, so the members AWS documents a default for are
    // resolved on the batch even though the project kept them unset: `source`'s
    // `insecureSsl` is off, the environment pulls with `CODEBUILD` credentials and
    // runs unprivileged, and the batch's cache and encryption key are the defaults.
    for (member, from_project) in [
        ("serviceRole", "serviceRole"),
        ("buildBatchConfig", "buildBatchConfig"),
    ] {
        assert_eq!(
            batch[member], project[from_project],
            "StartBuildBatch: {member} comes from the project, got {batch}"
        );
    }
    for member in ["type", "buildspec"] {
        assert_eq!(
            batch["source"][member], project["source"][member],
            "StartBuildBatch: source.{member} comes from the project, got {batch}"
        );
    }
    for member in ["type", "image", "computeType"] {
        assert_eq!(
            batch["environment"][member], project["environment"][member],
            "StartBuildBatch: environment.{member} comes from the project, got {batch}"
        );
    }
    // (member path, the effective value a run with nothing configured reports)
    let defaults: [(&[&str], Value); 5] = [
        (&["source", "insecureSsl"], json!(false)),
        (&["environment", "privilegedMode"], json!(false)),
        (
            &["environment", "imagePullCredentialsType"],
            json!("CODEBUILD"),
        ),
        (&["cache", "type"], json!("NO_CACHE")),
        (
            &["encryptionKey"],
            json!(format!(
                "arn:aws:kms:{REGION}:{}:alias/aws/s3",
                engine::EMULATOR_ACCOUNT_ID
            )),
        ),
    ];
    for (path, expected) in defaults {
        let mut reported = &batch;
        for segment in path {
            reported = &reported[*segment];
        }
        assert_eq!(
            reported, &expected,
            "StartBuildBatch: {} must report its documented default {expected}, got {batch}",
            path.join(".")
        );
    }
    // Collections the batch takes from the project are stated even when the project
    // names none.
    for member in ["secondarySources", "secondarySourceVersions"] {
        assert_eq!(
            batch[member],
            json!([]),
            "StartBuildBatch: {member} must be stated as [], got {batch}"
        );
    }
    assert_eq!(
        batch["buildTimeoutInMinutes"], project["timeoutInMinutes"],
        "StartBuildBatch: the batch build timeout comes from the project, got {batch}"
    );

    // The batch build is PERSISTED: the read path reports the record verbatim.
    let fetched = get_build_batch(&router, &id).await;
    assert_eq!(
        fetched, batch,
        "BatchGetBuildBatches must report the stored batch build verbatim, got {fetched}"
    );

    // An id this emulator holds nothing for is reported as not found.
    let bogus = "batch-proj:00000000-0000-0000-0000-000000000000";
    let (status, mixed) = call(
        &router,
        "BatchGetBuildBatches",
        json!({"ids": [id.clone(), bogus]}),
    )
    .await;
    assert_eq!(
        status, 200,
        "BatchGetBuildBatches: expected 200, got {status} with {mixed}"
    );
    assert_eq!(
        mixed["buildBatches"].as_array().map(Vec::len),
        Some(1),
        "BatchGetBuildBatches: only the known id resolves, got {mixed}"
    );
    assert_eq!(
        mixed["buildBatchesNotFound"],
        json!([state::build_batch_arn(REGION, bogus)]),
        "BatchGetBuildBatches: unknown ids belong in buildBatchesNotFound, by ARN, got {mixed}"
    );

    // A build id is NOT a batch build id: the two resources are held apart.
    let (_, started) = call(&router, "StartBuild", json!({"projectName": "batch-proj"})).await;
    let build_id = started["build"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("StartBuild: a build id, got {started}"))
        .to_string();
    let (_, as_batch) = call(
        &router,
        "BatchGetBuildBatches",
        json!({"ids": [build_id.clone()]}),
    )
    .await;
    assert_eq!(
        as_batch["buildBatchesNotFound"],
        json!([state::build_batch_arn(REGION, &build_id)]),
        "a build id must not resolve as a batch build, got {as_batch}"
    );

    // Each batch build of a project takes the next number, in its own sequence.
    let second = start_build_batch(&router, json!({"projectName": "batch-proj"})).await;
    assert_eq!(
        second["buildBatchNumber"], 2,
        "StartBuildBatch: buildBatchNumber increments by 1 per project, got {second}"
    );
    assert_ne!(
        second["id"], batch["id"],
        "StartBuildBatch: each batch build carries its own id, got {second}"
    );

    // Numbering is per project: another project starts at 1 again.
    create_project(&router, "other-proj", batch_enabled()).await;
    let elsewhere = start_build_batch(&router, json!({"projectName": "other-proj"})).await;
    assert_eq!(
        elsewhere["buildBatchNumber"], 1,
        "StartBuildBatch: each project numbers its own batch builds from 1, got {elsewhere}"
    );

    // An unknown project cannot be batch-built, and nothing is persisted for it.
    let (status, body) = call(
        &router,
        "StartBuildBatch",
        json!({"projectName": "never-created"}),
    )
    .await;
    assert_eq!(
        status, 400,
        "StartBuildBatch on an unknown project: expected 400, got {status} with {body}"
    );
    assert_eq!(
        error_type(&body),
        "ResourceNotFoundException",
        "StartBuildBatch on an unknown project: expected ResourceNotFoundException, got {body}"
    );

    // Starting a batch build leaves the project's own record alone: "The settings in
    // the project are unaltered."
    let (_, projects) = call(
        &router,
        "BatchGetProjects",
        json!({"names": ["batch-proj"]}),
    )
    .await;
    assert_eq!(
        projects["projects"][0], project,
        "StartBuildBatch must not alter the project, got {projects}"
    );

    // The transition the clock owes the batch build: still IN_PROGRESS when read
    // straight after the call, terminal once the span has elapsed. The failing phase
    // says why — nothing here fetches a source or runs a build container, so the
    // batch build specification is never downloaded.
    assert_eq!(
        get_build_batch(&router, &id).await["buildBatchStatus"],
        "IN_PROGRESS",
        "a batch build read straight after StartBuildBatch is still IN_PROGRESS"
    );
    tokio::time::sleep(PAST_THE_SETTLE_SPAN).await;
    let settled = get_build_batch(&router, &id).await;
    assert_eq!(
        settled["buildBatchStatus"], "FAULT",
        "past its due instant the batch build must report the terminal status, got {settled}"
    );
    assert_eq!(
        settled["complete"], true,
        "a terminal batch build is complete, got {settled}"
    );
    assert_eq!(
        settled["currentPhase"], "FAILED",
        "a faulted batch build sits in its FAILED phase, got {settled}"
    );
    assert!(
        settled["endTime"].is_number(),
        "a terminal batch build carries an endTime, got {settled}"
    );
    let phases = settled["phases"]
        .as_array()
        .unwrap_or_else(|| panic!("a settled batch build carries phases, got {settled}"))
        .clone();
    let download = phases
        .iter()
        .find(|phase| phase["phaseType"] == "DOWNLOAD_BATCHSPEC")
        .unwrap_or_else(|| panic!("DOWNLOAD_BATCHSPEC must be among the phases, got {settled}"));
    assert_eq!(
        download["phaseStatus"], "FAULT",
        "the batch spec download is where an emulated batch build stops, got {settled}"
    );
    assert!(
        at(download, &["contexts"])[0]["message"]
            .as_str()
            .is_some_and(|message| !message.is_empty()),
        "the failing phase must say why the batch build faulted, got {settled}"
    );
    // Reading a settled batch build again reports the same record: materialising the
    // owed transition is idempotent.
    assert_eq!(
        get_build_batch(&router, &id).await,
        settled,
        "a second read of a settled batch build must report the same record"
    );
    // And the batch build carries no build group: no batch spec was ever read, so no
    // build of the batch was started.
    assert!(
        settled["buildGroups"].is_null(),
        "nothing ran, so the batch build declares no build group, got {settled}"
    );
}

/// RULE: every `*Override` member of `StartBuildBatch` replaces, for this batch build
/// only, the corresponding setting resolved from the stored project; a member the
/// request leaves out keeps the project's value. The table spans the override class —
/// scalars, enums, structures, the source members and the two timeouts at the bounds
/// the model publishes (`BuildTimeOut` `@range(min: 5, max: 2160)`, `TimeOut`
/// `@range(min: 5, max: 480)`).
#[tokio::test]
async fn each_override_replaces_the_project_setting_for_that_batch_build_only() {
    let router = emulator();
    let project = create_project(
        &router,
        "override-proj",
        json!({
            "timeoutInMinutes": 30,
            "queuedTimeoutInMinutes": 15,
            "encryptionKey": "alias/project-key",
            "cache": {"type": "NO_CACHE"},
            "logsConfig": {"cloudWatchLogs": {"status": "DISABLED"}},
            "buildBatchConfig": {"timeoutInMins": 60},
        }),
    )
    .await;

    // (case, the override members sent, the member path to read back, its value)
    let cases: Vec<(&str, Value, Vec<&str>, Value)> = vec![
        (
            "no override keeps the project's build timeout",
            json!({}),
            vec!["buildTimeoutInMinutes"],
            json!(30),
        ),
        (
            "build timeout at the published minimum",
            json!({"buildTimeoutInMinutesOverride": 5}),
            vec!["buildTimeoutInMinutes"],
            json!(5),
        ),
        (
            "build timeout at the published maximum",
            json!({"buildTimeoutInMinutesOverride": 2160}),
            vec!["buildTimeoutInMinutes"],
            json!(2160),
        ),
        (
            "queued timeout at the published minimum",
            json!({"queuedTimeoutInMinutesOverride": 5}),
            vec!["queuedTimeoutInMinutes"],
            json!(5),
        ),
        (
            "queued timeout at the published maximum",
            json!({"queuedTimeoutInMinutesOverride": 480}),
            vec!["queuedTimeoutInMinutes"],
            json!(480),
        ),
        (
            "service role",
            json!({"serviceRoleOverride": "arn:aws:iam::000000000000:role/other"}),
            vec!["serviceRole"],
            json!("arn:aws:iam::000000000000:role/other"),
        ),
        (
            "encryption key",
            json!({"encryptionKeyOverride": "alias/batch-key"}),
            vec!["encryptionKey"],
            json!("alias/batch-key"),
        ),
        (
            "compute type, an enum member the issue did not cite",
            json!({"computeTypeOverride": "BUILD_GENERAL1_2XLARGE"}),
            vec!["environment", "computeType"],
            json!("BUILD_GENERAL1_2XLARGE"),
        ),
        (
            "environment type, an enum member the issue did not cite",
            json!({"environmentTypeOverride": "ARM_CONTAINER"}),
            vec!["environment", "type"],
            json!("ARM_CONTAINER"),
        ),
        (
            "image",
            json!({"imageOverride": "aws/codebuild/standard:5.0"}),
            vec!["environment", "image"],
            json!("aws/codebuild/standard:5.0"),
        ),
        (
            "privileged mode, a boolean",
            json!({"privilegedModeOverride": true}),
            vec!["environment", "privilegedMode"],
            json!(true),
        ),
        (
            "certificate",
            json!({"certificateOverride": "bucket/cert.pem"}),
            vec!["environment", "certificate"],
            json!("bucket/cert.pem"),
        ),
        (
            "image pull credentials type",
            json!({"imagePullCredentialsTypeOverride": "SERVICE_ROLE"}),
            vec!["environment", "imagePullCredentialsType"],
            json!("SERVICE_ROLE"),
        ),
        (
            "cache, a structure",
            json!({"cacheOverride": {"type": "S3", "location": "bucket/cache"}}),
            vec!["cache"],
            json!({"type": "S3", "location": "bucket/cache"}),
        ),
        (
            "logs configuration, a structure reported as logConfig",
            json!({"logsConfigOverride": {"cloudWatchLogs": {"status": "ENABLED", "groupName": "g"}}}),
            vec!["logConfig", "cloudWatchLogs", "groupName"],
            json!("g"),
        ),
        (
            "batch configuration, a structure",
            json!({"buildBatchConfigOverride": {"combineArtifacts": true, "timeoutInMins": 90}}),
            vec!["buildBatchConfig", "timeoutInMins"],
            json!(90),
        ),
        (
            "buildspec, a source member",
            json!({"buildspecOverride": "version: 0.2\nbatch: {}"}),
            vec!["source", "buildspec"],
            json!("version: 0.2\nbatch: {}"),
        ),
        (
            "git clone depth, a source member at its published minimum",
            json!({"gitCloneDepthOverride": 0}),
            vec!["source", "gitCloneDepth"],
            json!(0),
        ),
        (
            "source type and location, source members",
            json!({
                "sourceTypeOverride": "GITHUB",
                "sourceLocationOverride": "https://github.com/example/repo.git",
            }),
            vec!["source", "location"],
            json!("https://github.com/example/repo.git"),
        ),
        (
            "insecure SSL, a source member",
            json!({"insecureSslOverride": true}),
            vec!["source", "insecureSsl"],
            json!(true),
        ),
        (
            "batch status reporting, reported on the source",
            json!({
                "sourceTypeOverride": "GITHUB",
                "reportBuildBatchStatusOverride": true,
            }),
            vec!["source", "reportBuildStatus"],
            json!(true),
        ),
        (
            "source version",
            json!({"sourceVersion": "refs/heads/release"}),
            vec!["sourceVersion"],
            json!("refs/heads/release"),
        ),
        (
            "artifacts, reported as the batch build's artifacts",
            json!({"artifactsOverride": {"type": "S3", "location": "bucket/out"}}),
            vec!["artifacts", "location"],
            json!("bucket/out"),
        ),
        (
            "debug session",
            json!({"debugSessionEnabled": true}),
            vec!["debugSessionEnabled"],
            json!(true),
        ),
    ];

    for (case, overrides, path, expected) in cases {
        let mut request = json!({"projectName": "override-proj"});
        if let (Value::Object(target), Value::Object(members)) = (&mut request, overrides) {
            target.extend(members);
        }
        let batch = start_build_batch(&router, request).await;
        let actual = at(&batch, &path);
        assert_eq!(
            actual,
            &expected,
            "case {case:?}: expected {} = {expected}, got {actual}",
            path.join(".")
        );

        // The override lives on the batch build record, not on the project.
        let id = batch["id"].as_str().expect("an id").to_string();
        let fetched = get_build_batch(&router, &id).await;
        assert_eq!(
            at(&fetched, &path),
            &expected,
            "case {case:?}: BatchGetBuildBatches must report the stored value, got {fetched}"
        );
        let (_, projects) = call(
            &router,
            "BatchGetProjects",
            json!({"names": ["override-proj"]}),
        )
        .await;
        assert_eq!(
            projects["projects"][0], project,
            "case {case:?}: an override must leave the project unaltered, got {projects}"
        );
    }

    // One case past each published bound is refused, with the modelled error.
    for (case, overrides) in [
        (
            "build timeout below the minimum",
            json!({"buildTimeoutInMinutesOverride": 4}),
        ),
        (
            "build timeout past the maximum",
            json!({"buildTimeoutInMinutesOverride": 2161}),
        ),
        (
            "queued timeout below the minimum",
            json!({"queuedTimeoutInMinutesOverride": 4}),
        ),
        (
            "queued timeout past the maximum",
            json!({"queuedTimeoutInMinutesOverride": 481}),
        ),
        (
            "git clone depth below the minimum",
            json!({"gitCloneDepthOverride": -1}),
        ),
        ("an empty image name", json!({"imageOverride": ""})),
    ] {
        let mut request = json!({"projectName": "override-proj"});
        if let (Value::Object(target), Value::Object(members)) = (&mut request, overrides) {
            target.extend(members);
        }
        let (status, body) = call(&router, "StartBuildBatch", request).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400, got {status} with {body}"
        );
        assert_eq!(
            error_type(&body),
            "InvalidInputException",
            "case {case:?}: expected InvalidInputException, got {body}"
        );
    }
}

/// RULE: `environmentVariablesOverride` members "override, or add to, the environment
/// variables defined in the batch build project" — one of the same name replaces the
/// project's in place, and the rest are appended in the order the request listed them.
#[tokio::test]
async fn environment_variable_overrides_are_merged_by_name_for_a_batch_build() {
    let router = emulator();
    create_project(
        &router,
        "batch-env-proj",
        json!({
            "buildBatchConfig": {"timeoutInMins": 60},
            "environment": {
                "type": "LINUX_CONTAINER",
                "image": "aws/codebuild/standard:7.0",
                "computeType": "BUILD_GENERAL1_SMALL",
                "environmentVariables": [
                    {"name": "KEEP", "value": "project", "type": "PLAINTEXT"},
                    {"name": "REPLACE", "value": "project", "type": "PLAINTEXT"},
                ],
            },
        }),
    )
    .await;

    // (case, the override list, the resolved list the batch build must carry)
    let cases: Vec<(&str, Value, Value)> = vec![
        (
            "no override keeps the project's variables",
            json!(null),
            json!([
                {"name": "KEEP", "value": "project", "type": "PLAINTEXT"},
                {"name": "REPLACE", "value": "project", "type": "PLAINTEXT"},
            ]),
        ),
        (
            "an override replaces the project variable of the same name, in place",
            json!([{"name": "REPLACE", "value": "batch", "type": "PLAINTEXT"}]),
            json!([
                {"name": "KEEP", "value": "project", "type": "PLAINTEXT"},
                {"name": "REPLACE", "value": "batch", "type": "PLAINTEXT"},
            ]),
        ),
        (
            "a new name is appended after the project's variables",
            json!([{"name": "EXTRA", "value": "batch", "type": "PLAINTEXT"}]),
            json!([
                {"name": "KEEP", "value": "project", "type": "PLAINTEXT"},
                {"name": "REPLACE", "value": "project", "type": "PLAINTEXT"},
                {"name": "EXTRA", "value": "batch", "type": "PLAINTEXT"},
            ]),
        ),
        (
            "several overrides keep the order each came from",
            json!([
                {"name": "EXTRA", "value": "one", "type": "PLAINTEXT"},
                {"name": "REPLACE", "value": "two", "type": "PLAINTEXT"},
                {"name": "ANOTHER", "value": "three", "type": "PLAINTEXT"},
            ]),
            json!([
                {"name": "KEEP", "value": "project", "type": "PLAINTEXT"},
                {"name": "REPLACE", "value": "two", "type": "PLAINTEXT"},
                {"name": "EXTRA", "value": "one", "type": "PLAINTEXT"},
                {"name": "ANOTHER", "value": "three", "type": "PLAINTEXT"},
            ]),
        ),
        (
            "a repeated name in the override list resolves to the last one",
            json!([
                {"name": "REPLACE", "value": "first", "type": "PLAINTEXT"},
                {"name": "REPLACE", "value": "last", "type": "PLAINTEXT"},
            ]),
            json!([
                {"name": "KEEP", "value": "project", "type": "PLAINTEXT"},
                {"name": "REPLACE", "value": "last", "type": "PLAINTEXT"},
            ]),
        ),
    ];

    for (case, overrides, expected) in cases {
        let mut request = json!({"projectName": "batch-env-proj"});
        if !overrides.is_null() {
            request["environmentVariablesOverride"] = overrides;
        }
        let batch = start_build_batch(&router, request).await;
        let actual = at(&batch, &["environment", "environmentVariables"]);
        assert_eq!(
            actual, &expected,
            "case {case:?}: expected {expected}, got {actual}"
        );

        // And the merge is what was persisted, not something recomputed per read.
        let id = batch["id"].as_str().expect("an id").to_string();
        let fetched = get_build_batch(&router, &id).await;
        assert_eq!(
            at(&fetched, &["environment", "environmentVariables"]),
            &expected,
            "case {case:?}: BatchGetBuildBatches must report the merged list, got {fetched}"
        );
    }
}

/// RULE: a `StartBuildBatch` idempotency token in force resolves to the batch build the
/// first request with it started (AWS: "valid for five minutes"); the same token with a
/// changed parameter is a parameter mismatch, not a second batch build. A token is
/// scoped to the operation that issued it, so one in force for a build does not resolve
/// a batch build.
#[tokio::test]
async fn an_idempotency_token_resolves_to_the_batch_build_it_started() {
    let router = emulator();
    create_project(&router, "idem-batch-proj", batch_enabled()).await;

    let request = json!({"projectName": "idem-batch-proj", "idempotencyToken": "token-1"});
    let first = start_build_batch(&router, request.clone()).await;
    let replay = start_build_batch(&router, request).await;
    assert_eq!(
        replay["id"], first["id"],
        "a replayed token must resolve to the same batch build, got {replay}"
    );
    assert_eq!(
        replay["buildBatchNumber"], first["buildBatchNumber"],
        "a replayed token must not consume a batch build number, got {replay}"
    );

    // A different token is a different batch build.
    let other = start_build_batch(
        &router,
        json!({"projectName": "idem-batch-proj", "idempotencyToken": "token-2"}),
    )
    .await;
    assert_eq!(
        other["buildBatchNumber"], 2,
        "a new token starts the next batch build, got {other}"
    );

    // The same token with a changed parameter is refused, and starts nothing.
    for (case, changed) in [
        (
            "a changed buildspec",
            json!({
                "projectName": "idem-batch-proj",
                "idempotencyToken": "token-1",
                "buildspecOverride": "version: 0.2\nbatch: {}",
            }),
        ),
        (
            "a changed compute type",
            json!({
                "projectName": "idem-batch-proj",
                "idempotencyToken": "token-1",
                "computeTypeOverride": "BUILD_GENERAL1_LARGE",
            }),
        ),
    ] {
        let (status, body) = call(&router, "StartBuildBatch", changed).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400, got {status} with {body}"
        );
        assert_eq!(
            error_type(&body),
            "InvalidInputException",
            "case {case:?}: expected InvalidInputException, got {body}"
        );
    }

    // The next batch build is number 3: the replay and the refusals started none.
    let third = start_build_batch(&router, json!({"projectName": "idem-batch-proj"})).await;
    assert_eq!(
        third["buildBatchNumber"], 3,
        "only the accepted requests may have started a batch build, got {third}"
    );

    // A token is per operation: `StartBuild`'s token-1 resolves its own build, and
    // leaves the batch build this test started under the same value alone.
    let (status, build) = call(
        &router,
        "StartBuild",
        json!({"projectName": "idem-batch-proj", "idempotencyToken": "token-1"}),
    )
    .await;
    assert_eq!(
        status, 200,
        "StartBuild with a token a batch build used: expected 200, got {status} with {build}"
    );
    assert_ne!(
        build["build"]["id"], first["id"],
        "a build and a batch build are different resources, got {build}"
    );
    assert_eq!(
        get_build_batch(&router, first["id"].as_str().expect("an id")).await["id"],
        first["id"],
        "the batch build must still be the one its own token resolves to"
    );
}

/// RULE: every store `StartBuildBatch` feeds is bounded at its growth site — the batch
/// build store by eviction (the oldest COMPLETED batch build first, so one a caller may
/// still be waiting on survives a round), and the batch idempotency store by the
/// lifetime AWS documents for the token.
#[test]
fn the_batch_build_stores_are_bounded_at_their_growth_site() {
    let ceiling = i64::try_from(state::MAX_BUILD_BATCHES).expect("the ceiling fits in i64");
    let engine = engine::InMemoryEngine::new();
    let handle = engine.state::<CodeBuildState>();
    let mut guard = handle.lock();

    for index in 0..=ceiling {
        let id = format!("bounded:{index:08}");
        guard.insert_build_batch(
            id.clone(),
            StoredBuildBatch {
                build_batch: BuildBatch {
                    id: Some(id),
                    project_name: Some("bounded".to_string()),
                    complete: true,
                    ..BuildBatch::default()
                },
                start_millis: index,
                settle_due_millis: index,
            },
        );
    }
    assert_eq!(
        guard.build_batches.len(),
        state::MAX_BUILD_BATCHES,
        "the batch build store must stay at its ceiling, got {} entries",
        guard.build_batches.len()
    );
    assert!(
        !guard.build_batches.contains_key("bounded:00000000"),
        "the oldest completed batch build must be the one evicted"
    );
    assert!(
        guard
            .build_batches
            .contains_key(&format!("bounded:{ceiling:08}")),
        "the newest batch build must survive the eviction round"
    );
    assert_eq!(
        guard.build_batches_by_project["bounded"].len(),
        state::MAX_BUILD_BATCHES,
        "evicting a batch build must also drop its id from the project's list, got {} entries",
        guard.build_batches_by_project["bounded"].len()
    );

    // An entry past its documented lifetime is retired, not merely ignored.
    let now = state::now_millis();
    let stale = now - state::IDEMPOTENCY_TOKEN_TTL_MILLIS - 1;
    guard.record_start_build_batch_token(
        ("bounded".to_string(), "stale".to_string()),
        StartBuildToken {
            build_id: "bounded:00000001".to_string(),
            request_fingerprint: state::request_fingerprint("stale"),
            issued_millis: stale,
        },
        stale,
    );
    guard.record_start_build_batch_token(
        ("bounded".to_string(), "fresh".to_string()),
        StartBuildToken {
            build_id: "bounded:00000002".to_string(),
            request_fingerprint: state::request_fingerprint("fresh"),
            issued_millis: now,
        },
        now,
    );
    assert_eq!(
        guard
            .start_build_batch_tokens
            .keys()
            .map(|(_, token)| token.as_str())
            .collect::<Vec<_>>(),
        vec!["fresh"],
        "an expired batch idempotency entry must be dropped, not merely ignored"
    );
    assert!(
        guard.start_build_tokens.is_empty(),
        "a batch token must not be recorded in the build token store"
    );
}

// --- RetryBuildBatch ---------------------------------------------------------------
//
// `RetryBuildBatch` "restarts a failed batch build. Only batch builds that have failed can
// be retried", and it restarts that batch IN PLACE: the record keeps the identity it
// already had and runs its build groups again, which is why `BuildGroup` carries a
// `priorBuildSummaryList` of "summaries of previous build groups".

/// A `retryType` outside the modelled `RetryBuildBatchType` value set — a modelled value
/// in the wrong case, which is the form a typo takes.
const UNMODELLED_RETRY_TYPE: &str = "retry_failed_builds";

/// How long the retry suites wait for the clock-owed transitions of a batch build and the
/// builds it fanned out into, looking again until the batch reports itself complete.
/// Generous on purpose: the wait ends on the CONDITION, and the deadline is only there so
/// a batch that never settles fails the test instead of hanging it.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);

/// How long [`settled_build_batch`] waits before looking again.
const LOOK_AGAIN_AFTER: Duration = Duration::from_millis(25);

/// Look again until the batch build `id` reports itself complete, and hand back that
/// record as `BatchGetBuildBatches` reports it.
async fn settled_build_batch(router: &MultiServiceRouter, id: &str) -> Value {
    let deadline = tokio::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let batch = get_build_batch(router, id).await;
        if batch["complete"] == json!(true) {
            return batch;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "batch build {id}: expected it to complete, last read {batch}"
        );
        tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    }
}

/// A buildspec whose `batch` section declares one `build-list` entry per pair, each entry
/// running its own buildspec — so `command` is what decides whether that group's build
/// succeeds (`false` fails it, the way `buildspec_execution` drives a failing phase).
fn batch_list_spec(entries: &[(&str, &str)]) -> String {
    let mut spec = String::from("version: 0.2\nbatch:\n  build-list:\n");
    for (identifier, command) in entries {
        spec.push_str(&format!(
            "    - identifier: {identifier}\n      \
               buildspec: |\n        \
                 version: 0.2\n        \
                 phases:\n          \
                   build:\n            \
                     commands:\n              \
                       - {command}\n"
        ));
    }
    spec.push_str("phases:\n  build:\n    commands:\n      - echo base\n");
    spec
}

/// A `CreateProject` request body fragment for a batch-enabled project whose inline
/// buildspec is `buildspec`.
fn batch_project_with(buildspec: &str) -> Value {
    merged(
        json!({"source": {"type": "NO_SOURCE", "buildspec": buildspec}}),
        batch_enabled(),
    )
}

/// The build each of a batch's groups is running, and the earlier attempts that group
/// retains, keyed by the group's identifier.
fn group_attempts(batch: &Value) -> BTreeMap<String, (String, Vec<String>)> {
    let arn_of = |summary: &Value| summary["arn"].as_str().unwrap_or_default().to_string();
    batch["buildGroups"]
        .as_array()
        .map(|groups| {
            groups
                .iter()
                .map(|group| {
                    (
                        group["identifier"].as_str().unwrap_or_default().to_string(),
                        (
                            arn_of(&group["currentBuildSummary"]),
                            group["priorBuildSummaryList"]
                                .as_array()
                                .map(|prior| prior.iter().map(arn_of).collect())
                                .unwrap_or_default(),
                        ),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The build ids `ListBuildsForProject` reports for `project`.
async fn listed_build_ids(router: &MultiServiceRouter, project: &str) -> Vec<String> {
    let (status, body) = call(
        router,
        "ListBuildsForProject",
        json!({"projectName": project}),
    )
    .await;
    assert_eq!(
        status, 200,
        "ListBuildsForProject {project:?}: expected 200, got {status} with {body}"
    );
    body["ids"]
        .as_array()
        .map(|ids| {
            ids.iter()
                .map(|id| id.as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// RULE: `RetryBuildBatch` restarts a FAILED batch build IN PLACE — the record keeps its
/// id, its ARN and its `buildBatchNumber`, reports the run this call started
/// (`IN_PROGRESS`, not complete, no end instant) and runs its build groups again: every
/// group when the request asks for `RETRY_ALL_BUILDS` or names no `retryType`, and only
/// the groups whose build did not succeed when it asks for `RETRY_FAILED_BUILDS`. A group
/// that is run again carries a NEW build of the project and moves its earlier attempt into
/// `priorBuildSummaryList`; one that is not keeps the build it already had.
///
/// The rows span the class: a batchspec that declares no build at all, a `build-list` whose
/// builds disagree on their outcome, and a `build-graph` whose groups depend on each other,
/// each with the `retryType` the request carries (absent, and both values of
/// `RetryBuildBatchType`).
#[tokio::test]
async fn retry_build_batch_restarts_a_failed_batch_build_in_place() {
    // The `retryType` values the rows below cover are the model's whole value set.
    // Source: `RetryBuildBatchType` —
    // smithy_query(query="shape",shape="RetryBuildBatchType",model_path="services/service-com-amazonaws-codebuild-codebuild-20161006/smithy-model/com-amazonaws-codebuild-codebuild-20161006.json")
    // — as the generated value list carries it.
    assert_eq!(
        RETRY_BUILD_BATCH_TYPE_VALUES,
        ["RETRY_ALL_BUILDS", "RETRY_FAILED_BUILDS"],
        "the rows below cover every modelled retryType, got {RETRY_BUILD_BATCH_TYPE_VALUES:?}"
    );
    let failing_list = batch_list_spec(&[("good", "echo fine"), ("bad", "false")]);
    // A graph whose leaf waits for a base that succeeds and then fails itself.
    let failing_graph = "version: 0.2\nbatch:\n  \
         build-graph:\n    \
           - identifier: base\n      \
             buildspec: |\n        \
               version: 0.2\n        \
               phases:\n          \
                 build:\n            \
                   commands:\n              \
                     - echo base\n    \
           - identifier: leaf\n      \
             depends-on:\n        \
               - base\n      \
             buildspec: |\n        \
               version: 0.2\n        \
               phases:\n          \
                 build:\n            \
                   commands:\n              \
                     - false\n"
        .to_string();
    // (case, the project's buildspec, the request's retryType, the identifiers whose build
    // the retry must REPLACE)
    let rows: Vec<(&str, String, Option<&str>, Vec<&str>)> = vec![
        (
            "a batchspec that declared no build at all",
            "version: 0.2\nphases:\n  build:\n    commands:\n      - echo plain\n".to_string(),
            None,
            vec![],
        ),
        (
            "a build-list, with no retryType",
            failing_list.clone(),
            None,
            vec![state::SOURCE_DOWNLOAD_IDENTIFIER, "good", "bad"],
        ),
        (
            "a build-list, RETRY_ALL_BUILDS",
            failing_list.clone(),
            Some("RETRY_ALL_BUILDS"),
            vec![state::SOURCE_DOWNLOAD_IDENTIFIER, "good", "bad"],
        ),
        (
            "a build-list, RETRY_FAILED_BUILDS",
            failing_list,
            Some("RETRY_FAILED_BUILDS"),
            vec!["bad"],
        ),
        (
            "a build-graph, RETRY_FAILED_BUILDS",
            failing_graph.clone(),
            Some("RETRY_FAILED_BUILDS"),
            vec!["leaf"],
        ),
        (
            "a build-graph, RETRY_ALL_BUILDS",
            failing_graph,
            Some("RETRY_ALL_BUILDS"),
            vec![state::SOURCE_DOWNLOAD_IDENTIFIER, "base", "leaf"],
        ),
    ];

    for (index, (case, buildspec, retry_type, replaced)) in rows.into_iter().enumerate() {
        // A fresh emulator per row: a row's state must not depend on the one before.
        let router = emulator();
        let project = format!("retry-batch-{index}");
        create_project(&router, &project, batch_project_with(&buildspec)).await;
        let started = start_build_batch(&router, json!({"projectName": project})).await;
        let id = started["id"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: the batch build carries an id, got {started}"))
            .to_string();

        // Only a batch build that has failed can be retried, so every row waits for the
        // outcome the clock owes this one before it asks.
        let failed = settled_build_batch(&router, &id).await;
        assert_ne!(
            failed["buildBatchStatus"],
            json!("SUCCEEDED"),
            "case {case:?}: the batch must have failed to be retryable, got {failed}"
        );
        let before = group_attempts(&failed);
        let builds_before = listed_build_ids(&router, &project).await;

        let mut request = json!({"id": id.clone()});
        if let (Value::Object(members), Some(retry_type)) = (&mut request, retry_type) {
            members.insert("retryType".to_string(), json!(retry_type));
        }
        let (status, body) = call(&router, "RetryBuildBatch", request.clone()).await;
        assert_eq!(
            status, 200,
            "case {case:?}: {request}: expected 200, got {status} with {body}"
        );
        let retried = body["buildBatch"].clone();

        // The identity the batch already had: a retry restarts THAT batch build.
        assert_eq!(
            retried["id"],
            json!(id),
            "case {case:?}: the retry restarts the same batch build, got {retried}"
        );
        assert_eq!(
            retried["arn"], failed["arn"],
            "case {case:?}: the restarted batch keeps its ARN, got {retried}"
        );
        assert_eq!(
            retried["buildBatchNumber"], failed["buildBatchNumber"],
            "case {case:?}: the restarted batch keeps its buildBatchNumber, got {retried}"
        );
        assert_eq!(
            retried["projectName"], failed["projectName"],
            "case {case:?}: the restarted batch is still a batch of its project, got {retried}"
        );
        // And the run this call started.
        assert_eq!(
            retried["buildBatchStatus"],
            json!("IN_PROGRESS"),
            "case {case:?}: a restarted batch build is IN_PROGRESS, got {retried}"
        );
        assert_eq!(
            retried["complete"],
            json!(false),
            "case {case:?}: a restarted batch build is not complete, got {retried}"
        );
        assert!(
            retried["endTime"].is_null(),
            "case {case:?}: a restarted batch build has not ended, got {retried}"
        );
        assert_eq!(
            retried["currentPhase"],
            json!("DOWNLOAD_BATCHSPEC"),
            "case {case:?}: a restarted batch fetches its batchspec first, got {retried}"
        );

        // A retry restarts the batch rather than minting a second one.
        let (_, listed) = call(
            &router,
            "ListBuildBatchesForProject",
            json!({"projectName": project}),
        )
        .await;
        assert_eq!(
            listed["ids"].as_array().map(Vec::as_slice),
            Some([json!(id.clone())].as_slice()),
            "case {case:?}: the project must still hold exactly that one batch build, \
             got {listed}"
        );

        let after = group_attempts(&retried);
        assert_eq!(
            after.keys().collect::<Vec<_>>(),
            before.keys().collect::<Vec<_>>(),
            "case {case:?}: the retry runs the groups the batch already had, got {retried}"
        );
        if before.is_empty() {
            assert!(
                retried["buildGroups"].is_null(),
                "case {case:?}: a batch that declared no build carries no group, got {retried}"
            );
        }
        for (identifier, (arn_before, prior_before)) in &before {
            let (arn_after, prior_after) = after
                .get(identifier)
                .unwrap_or_else(|| panic!("case {case:?}: group {identifier:?} is reported"));
            if replaced.contains(&identifier.as_str()) {
                assert_ne!(
                    arn_after, arn_before,
                    "case {case:?}: group {identifier:?} must run a NEW build, got {arn_after}"
                );
                let expected_prior: Vec<String> = prior_before
                    .iter()
                    .cloned()
                    .chain([arn_before.clone()])
                    .collect();
                assert_eq!(
                    prior_after, &expected_prior,
                    "case {case:?}: group {identifier:?} retains its earlier attempt, \
                     got {prior_after:?}"
                );
                // The ARN the group now names resolves to a real build of this project,
                // which belongs to this batch.
                let (_, builds) = call(&router, "BatchGetBuilds", json!({"ids": [arn_after]})).await;
                let build = builds["builds"][0].clone();
                assert_eq!(
                    build["arn"],
                    json!(arn_after),
                    "case {case:?}: group {identifier:?}'s new build must be retrievable, \
                     got {builds}"
                );
                assert_eq!(
                    build["projectName"],
                    json!(project),
                    "case {case:?}: group {identifier:?}'s new build is a build of the \
                     project, got {build}"
                );
                assert_eq!(
                    build["buildBatchArn"], retried["arn"],
                    "case {case:?}: group {identifier:?}'s new build names the batch it \
                     belongs to, got {build}"
                );
                assert!(
                    build["buildNumber"].is_number(),
                    "case {case:?}: group {identifier:?}'s new build takes a build number \
                     of its project, got {build}"
                );
            } else {
                assert_eq!(
                    arn_after, arn_before,
                    "case {case:?}: group {identifier:?} was not retried, so it keeps the \
                     build it had, got {arn_after}"
                );
                assert_eq!(
                    prior_after, prior_before,
                    "case {case:?}: group {identifier:?} was not retried, so its earlier \
                     attempts are unchanged, got {prior_after:?}"
                );
            }
        }

        // One new build per group the retry ran, and every build of the earlier attempt is
        // still retained — the summaries that name them must resolve.
        let builds_after = listed_build_ids(&router, &project).await;
        assert_eq!(
            builds_after.len(),
            builds_before.len() + replaced.len(),
            "case {case:?}: expected one new build per retried group, got {builds_after:?}"
        );
        assert!(
            builds_before
                .iter()
                .all(|held| builds_after.contains(held)),
            "case {case:?}: the builds of the earlier attempt are retained, got {builds_after:?}"
        );

        // What a later read reports is the record the retry wrote, not a per-call answer:
        // the groups it published are the ones the store now holds.
        let read = get_build_batch(&router, &id).await;
        assert_eq!(
            read["id"],
            json!(id),
            "case {case:?}: the stored batch build is the one the retry restarted, got {read}"
        );
        assert_eq!(
            group_attempts(&read)
                .iter()
                .map(|(identifier, (arn, _))| (identifier.clone(), arn.clone()))
                .collect::<Vec<_>>(),
            after
                .iter()
                .map(|(identifier, (arn, _))| (identifier.clone(), arn.clone()))
                .collect::<Vec<_>>(),
            "case {case:?}: the stored batch runs the builds the retry started, got {read}"
        );

        // And the restarted batch reaches an outcome of its own, like any other run.
        let settled = settled_build_batch(&router, &id).await;
        assert_eq!(
            settled["complete"],
            json!(true),
            "case {case:?}: the restarted batch build completes, got {settled}"
        );
    }
}

/// RULE: "only batch builds that have failed can be retried" — a batch build that is still
/// running and one that SUCCEEDED are both refused with the modelled
/// `InvalidInputException`, an identifier naming no batch build of this account with the
/// modelled `ResourceNotFoundException`, and a request naming no batch build at all with
/// the modelled `InvalidInputException`. A refused retry changes nothing.
#[tokio::test]
async fn retry_build_batch_refuses_a_batch_build_it_cannot_restart() {
    /// What the retry names, and so what it must answer.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Target {
        /// A batch build whose builds have not finished: it has not failed yet.
        StillRunning,
        /// A batch build every build of which succeeded.
        Succeeded,
        /// A well-formed batch build id nothing started.
        Unknown,
        /// A batch build that was deleted after it failed.
        Deleted,
        /// A failed batch build whose project has since been deleted.
        ProjectGone,
        /// A request carrying no `id` at all.
        Unnamed,
        /// A request whose `retryType` is outside the modelled value set.
        UnmodelledRetryType,
    }

    // A chain of groups, each waiting for the one before it: the batch is paced one span
    // per level, so a batch of this shape is still running when the next call arrives with
    // generous room to spare.
    let chained: Vec<(String, String)> = (0..4_usize)
        .map(|step| (format!("step{step}"), format!("step{}", step.saturating_sub(1))))
        .collect();
    let mut chain_spec = String::from("version: 0.2\nbatch:\n  build-graph:\n");
    for (index, (identifier, parent)) in chained.iter().enumerate() {
        chain_spec.push_str(&format!("    - identifier: {identifier}\n"));
        if index > 0 {
            chain_spec.push_str(&format!("      depends-on:\n        - {parent}\n"));
        }
    }
    chain_spec.push_str("phases:\n  build:\n    commands:\n      - false\n");

    let targets = [
        Target::StillRunning,
        Target::Succeeded,
        Target::Unknown,
        Target::Deleted,
        Target::ProjectGone,
        Target::Unnamed,
        Target::UnmodelledRetryType,
    ];
    for (index, target) in targets.into_iter().enumerate() {
        let router = emulator();
        let project = format!("retry-refuse-{index}");
        let buildspec = match target {
            // Every group succeeds, so the batch succeeds.
            Target::Succeeded => batch_list_spec(&[("good", "echo fine")]),
            Target::StillRunning => chain_spec.clone(),
            _ => batch_list_spec(&[("bad", "false")]),
        };
        create_project(&router, &project, batch_project_with(&buildspec)).await;
        let started = start_build_batch(&router, json!({"projectName": project})).await;
        let id = started["id"].as_str().expect("a batch id").to_string();
        let arn = started["arn"].as_str().expect("a batch ARN").to_string();

        if target != Target::StillRunning {
            let settled = settled_build_batch(&router, &id).await;
            let succeeded = settled["buildBatchStatus"] == json!("SUCCEEDED");
            assert_eq!(
                succeeded,
                target == Target::Succeeded,
                "case {target:?}: the row needs the other outcome, got {settled}"
            );
        }
        if target == Target::Deleted {
            let (status, body) = call(&router, "DeleteBuildBatch", json!({"id": id.clone()})).await;
            assert_eq!(
                status, 200,
                "case {target:?}: DeleteBuildBatch expected 200, got {status} with {body}"
            );
        }
        if target == Target::ProjectGone {
            let (status, body) = call(&router, "DeleteProject", json!({"name": project})).await;
            assert_eq!(
                status, 200,
                "case {target:?}: DeleteProject expected 200, got {status} with {body}"
            );
        }

        let requested_id = match target {
            Target::Unknown => format!("{project}:00000000-0000-8000-8000-000000000001"),
            _ => id.clone(),
        };
        let (expected_error, expected_message) = match target {
            Target::StillRunning => (
                "InvalidInputException",
                format!("Cannot retry BuildBatch {arn}, as it is still in progress"),
            ),
            Target::Succeeded => (
                "InvalidInputException",
                format!("Cannot retry BuildBatch {arn}, as it did not fail"),
            ),
            Target::Unknown | Target::Deleted => (
                "ResourceNotFoundException",
                format!("BuildBatch cannot be found: {requested_id}"),
            ),
            Target::ProjectGone => (
                "ResourceNotFoundException",
                format!("Project cannot be found: {}", expected_arn(&format!("project/{project}"))),
            ),
            Target::Unnamed => (
                "InvalidInputException",
                "Value at 'id' failed to satisfy constraint: Member must not be null".to_string(),
            ),
            // `retryType` is the modelled enum `RetryBuildBatchType`, so a value outside
            // its set is refused by the shared constraint envelope before the handler
            // runs — the retry is never performed for one.
            Target::UnmodelledRetryType => (
                "InvalidInputException",
                format!(
                    "1 validation error detected: Value '{UNMODELLED_RETRY_TYPE}' at \
                     'retryType' failed to satisfy constraint: Member must satisfy enum \
                     value set: [{}]",
                    RETRY_BUILD_BATCH_TYPE_VALUES.join(", ")
                ),
            ),
        };
        let request = match target {
            Target::Unnamed => json!({}),
            Target::UnmodelledRetryType => {
                json!({"id": requested_id.clone(), "retryType": UNMODELLED_RETRY_TYPE})
            }
            _ => json!({"id": requested_id.clone()}),
        };

        let (status, body) = call(&router, "RetryBuildBatch", request.clone()).await;
        assert_eq!(
            status, 400,
            "case {target:?}: {request}: expected 400, got {status} with {body}"
        );
        assert_eq!(
            error_type(&body),
            expected_error,
            "case {target:?}: expected {expected_error}, got {body}"
        );
        assert_eq!(
            body["message"],
            json!(expected_message),
            "case {target:?}: expected the refusal to state why, got {body}"
        );

        // EFFECT ABSENT: a refused retry starts no build and leaves the batch build it
        // named exactly as it was. The project-deleted row lists nothing, because the
        // project whose builds would be listed is gone.
        if target != Target::ProjectGone {
            let builds = listed_build_ids(&router, &project).await;
            let expected_builds = match target {
                // Deleting a batch build deletes the builds it is made of.
                Target::Deleted => 0,
                Target::StillRunning => chained.len() + 1,
                _ => 2,
            };
            assert_eq!(
                builds.len(),
                expected_builds,
                "case {target:?}: a refused retry starts no build, got {builds:?}"
            );
        }
        if matches!(
            target,
            Target::StillRunning | Target::Succeeded | Target::UnmodelledRetryType
        ) {
            let read = get_build_batch(&router, &id).await;
            assert_eq!(
                read["buildBatchNumber"], started["buildBatchNumber"],
                "case {target:?}: the batch build the retry refused is unchanged, got {read}"
            );
            assert_eq!(
                group_attempts(&read)
                    .values()
                    .flat_map(|(_, prior)| prior.clone())
                    .count(),
                0,
                "case {target:?}: a refused retry retains no earlier attempt, got {read}"
            );
        }
    }
}

/// RULE: a `RetryBuildBatch` idempotency token is in force for the window AWS documents —
/// a repeat of the SAME request inside it resolves to the batch build the first call
/// restarted and starts no further build, while the same token carrying a CHANGED request
/// is the parameter mismatch AWS documents rather than a second restart.
#[tokio::test]
async fn a_retry_build_batch_idempotency_token_restarts_the_batch_once() {
    let router = emulator();
    let project = "retry-batch-token";
    create_project(
        &router,
        project,
        batch_project_with(&batch_list_spec(&[("good", "echo fine"), ("bad", "false")])),
    )
    .await;
    let started = start_build_batch(&router, json!({"projectName": project})).await;
    let id = started["id"].as_str().expect("a batch id").to_string();
    settled_build_batch(&router, &id).await;

    let request = json!({
        "id": id.clone(),
        "retryType": "RETRY_FAILED_BUILDS",
        "idempotencyToken": "retry-batch-token-1",
    });
    let (status, first) = call(&router, "RetryBuildBatch", request.clone()).await;
    assert_eq!(
        status, 200,
        "the first retry: expected 200, got {status} with {first}"
    );
    let groups = group_attempts(&first["buildBatch"]);
    let builds_after_first = listed_build_ids(&router, project).await;

    let (status, repeated) = call(&router, "RetryBuildBatch", request).await;
    assert_eq!(
        status, 200,
        "the repeat: expected 200, got {status} with {repeated}"
    );
    assert_eq!(
        repeated["buildBatch"]["id"],
        json!(id),
        "the repeat resolves to the batch build the first call restarted, got {repeated}"
    );
    assert_eq!(
        group_attempts(&repeated["buildBatch"])
            .iter()
            .map(|(identifier, (arn, _))| (identifier.clone(), arn.clone()))
            .collect::<Vec<_>>(),
        groups
            .iter()
            .map(|(identifier, (arn, _))| (identifier.clone(), arn.clone()))
            .collect::<Vec<_>>(),
        "the repeat must not restart the batch a second time, got {repeated}"
    );
    assert_eq!(
        listed_build_ids(&router, project).await,
        builds_after_first,
        "the repeat must start no further build"
    );

    // The same token with a changed parameter is a refusal, not a second restart.
    let (status, mismatched) = call(
        &router,
        "RetryBuildBatch",
        json!({
            "id": id.clone(),
            "retryType": "RETRY_ALL_BUILDS",
            "idempotencyToken": "retry-batch-token-1",
        }),
    )
    .await;
    assert_eq!(
        status, 400,
        "the changed repeat: expected 400, got {status} with {mismatched}"
    );
    assert_eq!(
        error_type(&mismatched),
        "InvalidInputException",
        "the changed repeat: expected InvalidInputException, got {mismatched}"
    );
    assert_eq!(
        mismatched["message"],
        json!(
            "Parameter mismatch: idempotencyToken was already used for a RetryBuildBatch \
             request with different parameters"
        ),
        "the changed repeat: expected the parameter-mismatch sentence, got {mismatched}"
    );
    assert_eq!(
        listed_build_ids(&router, project).await,
        builds_after_first,
        "a refused repeat must start no further build"
    );
}
