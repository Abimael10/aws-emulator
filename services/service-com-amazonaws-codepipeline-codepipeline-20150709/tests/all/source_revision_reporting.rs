// SPDX-License-Identifier: CC0-1.0

//! What the reads report about the revision a SOURCE action resolved.
//!
//! The rule under test: a source action's run records the revision it resolved — the
//! provider's revision id, what the provider names the change by, and the provider's own
//! summary of that revision — and every read projects what was recorded rather than
//! leaving it out. For an object in an Amazon S3 bucket the id is the object's version
//! (its entity tag where the bucket mints no versions), the change identifier is the
//! entity tag ("for artifacts stored in Amazon S3 buckets, the ETag value",
//! `ArtifactRevision$revisionChangeIdentifier`) and the summary is "the user-provided
//! content of a `codepipeline-artifact-revision-summary` key specified in the object
//! metadata" (`ArtifactRevision$revisionSummary`), or, where the object states none, the
//! words the provider names a version by.
//!
//! The reads it is asserted through: `GetPipelineExecution`'s
//! `artifactRevisions`, `ListPipelineExecutions`' `sourceRevisions` and
//! `GetPipelineState`'s `actionStates[].currentRevision` and
//! `actionStates[].latestExecution`.
//!
//! The rows span what the rule generalises over: whether the source object states a
//! revision summary of its own, whether the bucket mints version ids at all, and whether
//! the revision the run resolved is the object's current one or an older one a
//! `SourceRevisionOverride` pinned.
//!
//! The harness comes from `tests/all/execution_engine.rs`, so the bytes really move
//! through the emulated S3 service and every assertion goes through the router.

use serde_json::{json, Value};

use crate::execution_engine::{
    artifact_bucket, creation_run, declaration, deploy_stage, source_stage, wait_for_terminal,
    Emulator,
};

/// The bucket the source objects live in.
const SOURCE_BUCKET: &str = "revision-source-bucket";

/// The bucket the deploy action delivers the artifact to.
const DEPLOY_BUCKET: &str = "revision-deploy-bucket";

/// The source object's key.
const SOURCE_KEY: &str = "app.zip";

/// The artifact the source action produces and the deploy action delivers.
const ARTIFACT: &str = "SourceArtifact";

/// The key the deploy action delivers the artifact under.
const DELIVERED: &str = "delivered";

/// The object metadata key whose content IS a source object's revision summary
/// (`ArtifactRevision$revisionSummary`: "for Amazon S3 buckets or actions, the
/// user-provided content of a `codepipeline-artifact-revision-summary` key specified in
/// the object metadata").
const SUMMARY_KEY: &str = "codepipeline-artifact-revision-summary";

/// The fixed words the Amazon S3 source provider names a version by, before the version
/// id, when the object states no summary of its own.
const VERSION_PREFIX: &str = "Amazon S3 version id: ";

/// The bytes of the two writes of the source object: the older ones, then the current.
const OLD_BYTES: &[u8] = b"the older source";
/// See [`OLD_BYTES`].
const NEW_BYTES: &[u8] = b"the current source";

/// Which revision of the source object a row's execution resolves.
#[derive(Debug, Clone, Copy)]
enum Resolves {
    /// The object's current version, with nothing pinning one.
    Current,
    /// The version the FIRST write made, pinned by an `S3_OBJECT_VERSION_ID` override.
    PinnedFirst,
}

/// What one row writes and expects.
#[derive(Debug)]
struct Row {
    /// What this case is, named in every assertion of it.
    case: &'static str,
    /// Whether the source bucket mints version ids.
    versioned: bool,
    /// The content of the source object's revision-summary metadata key, when the row's
    /// object states one.
    stated_summary: Option<&'static str>,
    /// Which revision the execution resolves.
    resolves: Resolves,
}

#[tokio::test]
async fn a_source_action_reports_the_revision_it_resolved_and_the_providers_summary_of_it() {
    let rows = vec![
        Row {
            case: "a versioned object stating no summary of its own",
            versioned: true,
            stated_summary: None,
            resolves: Resolves::Current,
        },
        Row {
            case: "a versioned object whose metadata states the revision summary",
            versioned: true,
            stated_summary: Some("release 4.2: the quarterly cut"),
            resolves: Resolves::Current,
        },
        Row {
            case: "a versioned object whose stated summary carries no characters",
            versioned: true,
            stated_summary: Some(""),
            resolves: Resolves::Current,
        },
        Row {
            case: "an older version pinned by a source revision override",
            versioned: true,
            stated_summary: None,
            resolves: Resolves::PinnedFirst,
        },
        Row {
            case: "a bucket that mints no version ids, the object stating the summary",
            versioned: false,
            stated_summary: Some("built from tag v1.0.0"),
            resolves: Resolves::Current,
        },
        Row {
            case: "a bucket that mints no version ids and an object stating no summary",
            versioned: false,
            stated_summary: None,
            resolves: Resolves::Current,
        },
    ];

    for (index, row) in rows.into_iter().enumerate() {
        let Row {
            case,
            versioned,
            stated_summary,
            resolves,
        } = row;
        // One pipeline and one emulator per row, so what a row reads back is what its own
        // execution resolved.
        let emulator = Emulator::new();
        let pipeline = format!("revision-{index}");
        emulator.bucket(&artifact_bucket(&pipeline)).await;
        emulator.bucket(DEPLOY_BUCKET).await;
        match versioned {
            true => emulator.bucket(SOURCE_BUCKET).await,
            false => emulator.unversioned_bucket(SOURCE_BUCKET).await,
        }

        let metadata: Vec<(&str, &str)> = stated_summary
            .map(|summary| vec![(SUMMARY_KEY, summary)])
            .unwrap_or_default();
        let (first_version, first_etag) = emulator
            .put_object_with_metadata(SOURCE_BUCKET, SOURCE_KEY, &metadata, OLD_BYTES)
            .await;
        let (current_version, current_etag) = emulator
            .put_object_with_metadata(SOURCE_BUCKET, SOURCE_KEY, &metadata, NEW_BYTES)
            .await;
        assert_eq!(
            versioned,
            current_version.is_some(),
            "case {case:?}: a versioned bucket mints a version id and an unversioned one \
             does not, got {current_version:?}"
        );

        // What the run must report: the revision it resolved, the change the provider
        // names it by, and the summary of it.
        let (version, etag, bytes) = match resolves {
            Resolves::Current => (current_version.clone(), current_etag.clone(), NEW_BYTES),
            Resolves::PinnedFirst => (first_version.clone(), first_etag.clone(), OLD_BYTES),
        };
        let expected_revision = version
            .clone()
            .or_else(|| etag.clone())
            .unwrap_or_else(|| panic!("case {case:?}: a write reports a version id or an etag"));
        // `RevisionSummary` is modelled `@length(min: 1, max: 2048)`, so a stated summary
        // carrying no characters is no summary at all and the provider's own words for
        // the version stand.
        let expected_summary = stated_summary
            .filter(|summary| !summary.is_empty())
            .map(str::to_string)
            .or_else(|| {
                version
                    .as_ref()
                    .map(|version| format!("{VERSION_PREFIX}{version}"))
            });

        emulator
            .ok(
                "CreatePipeline",
                &declaration(
                    &pipeline,
                    json!([
                        source_stage(SOURCE_BUCKET, SOURCE_KEY, ARTIFACT),
                        deploy_stage("Deploy", ARTIFACT, DEPLOY_BUCKET, DELIVERED, false),
                    ]),
                    Some("PARALLEL"),
                ),
            )
            .await;
        // Creating a pipeline runs it once. That run is let finish before this row's own
        // one starts, so the revision the pipeline's action is left carrying is the one
        // THIS row's execution resolved.
        let created_run = creation_run(&emulator, &pipeline).await;
        wait_for_terminal(&emulator, &pipeline, &created_run).await;

        let mut request = json!({"name": pipeline});
        if let Resolves::PinnedFirst = resolves {
            request["sourceRevisions"] = json!([{
                // `source_stage` names the source action `Src`.
                "actionName": "Src",
                "revisionType": "S3_OBJECT_VERSION_ID",
                "revisionValue": version
                    .clone()
                    .unwrap_or_else(|| panic!("case {case:?}: a pinned row needs a version id")),
            }]);
        }
        let started = emulator.ok("StartPipelineExecution", &request).await;
        let execution = started["pipelineExecutionId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: the start reports an execution id, got {started}"))
            .to_string();

        let settled = wait_for_terminal(&emulator, &pipeline, &execution).await;
        assert_eq!(
            settled["pipelineExecution"]["status"].as_str(),
            Some("Succeeded"),
            "case {case:?}: the source the row resolves can be read, so the execution must \
             finish, got {settled}"
        );
        // The bytes prove WHICH revision ran, so the reported revision is about the one
        // the action really consumed.
        let (status, delivered) = emulator.get_object(DEPLOY_BUCKET, DELIVERED).await;
        assert_eq!(
            (status, delivered.as_slice()),
            (200, bytes),
            "case {case:?}: the resolved revision's bytes are what was delivered, got {status}"
        );

        // GetPipelineExecution: the artifact's revision and the summary of it.
        let revisions = settled["pipelineExecution"]["artifactRevisions"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let artifact = revisions
            .iter()
            .find(|revision| revision["name"] == json!(ARTIFACT))
            .unwrap_or_else(|| panic!("case {case:?}: the artifact's revision is reported, got {settled}"));
        assert_eq!(
            artifact["revisionId"],
            json!(expected_revision),
            "case {case:?}: expected the resolved revision {expected_revision:?}, got {artifact}"
        );
        assert_eq!(
            artifact["revisionSummary"],
            summary_value(&expected_summary),
            "case {case:?}: expected the summary {expected_summary:?}, got {artifact}"
        );

        // ListPipelineExecutions: the same two facts, for the same execution.
        let listed = emulator
            .ok("ListPipelineExecutions", &json!({"pipelineName": pipeline}))
            .await;
        let summary = listed["pipelineExecutionSummaries"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|entry| entry["pipelineExecutionId"] == json!(execution))
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: the execution is listed, got {listed}"));
        let revisions = summary["sourceRevisions"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            revisions.len(),
            1,
            "case {case:?}: the one source action's revision is reported, got {summary}"
        );
        let source_revision = &revisions[0];
        assert_eq!(
            (
                source_revision["actionName"].clone(),
                source_revision["revisionId"].clone(),
                source_revision["revisionSummary"].clone(),
            ),
            (
                json!("Src"),
                json!(expected_revision),
                summary_value(&expected_summary),
            ),
            "case {case:?}: expected the revision {expected_revision:?} summarised as \
             {expected_summary:?}, got {source_revision}"
        );

        // GetPipelineState: the revision the action is sitting on, and its run.
        let state = emulator.ok("GetPipelineState", &json!({"name": pipeline})).await;
        let action = state["stageStates"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|stage| stage["stageName"] == json!("Source"))
            .and_then(|stage| stage["actionStates"].as_array())
            .and_then(|actions| actions.first())
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: the source action has a state, got {state}"));
        assert_eq!(
            action["currentRevision"]["revisionId"],
            json!(expected_revision),
            "case {case:?}: expected currentRevision.revisionId {expected_revision:?}, got {action}"
        );
        // `ActionRevision$revisionChangeId` is "the unique identifier of the change that
        // set the state to this revision": for an object in an S3 bucket, its entity tag.
        let expected_change = etag.clone().unwrap_or_else(|| expected_revision.clone());
        assert_eq!(
            action["currentRevision"]["revisionChangeId"],
            json!(expected_change),
            "case {case:?}: expected currentRevision.revisionChangeId {expected_change:?}, got {action}"
        );
        assert!(
            action["currentRevision"]["created"].is_number(),
            "case {case:?}: a revision carries the instant it was recorded, got {action}"
        );
        assert_eq!(
            action["latestExecution"]["externalExecutionId"],
            json!(expected_revision),
            "case {case:?}: the run's external execution is the revision it delivered, got {action}"
        );
        if let Some(expected_summary) = &expected_summary {
            assert_eq!(
                action["latestExecution"]["summary"],
                json!(expected_summary),
                "case {case:?}: the run's summary is the provider's summary of the revision, \
                 got {action}"
            );
        }
    }
}

/// The JSON a reported revision summary is: the text, or JSON null for a revision whose
/// provider states none (the member is then absent, which is what indexing answers).
fn summary_value(summary: &Option<String>) -> Value {
    summary
        .as_ref()
        .map(|summary| json!(summary))
        .unwrap_or(Value::Null)
}

/// RULE: the revision a read reports is the one the RUN resolved, so a second execution
/// that resolves a newer revision moves what the pipeline's action carries, and each
/// execution goes on reporting its own.
#[tokio::test]
async fn each_execution_reports_the_revision_it_ran_while_the_action_carries_the_latest() {
    let emulator = Emulator::new();
    let pipeline = "revision-sequence";
    emulator.bucket(&artifact_bucket(pipeline)).await;
    emulator.bucket(SOURCE_BUCKET).await;
    emulator.bucket(DEPLOY_BUCKET).await;

    let (first_version, _) = emulator
        .put_object_with_metadata(SOURCE_BUCKET, SOURCE_KEY, &[(SUMMARY_KEY, "the first cut")], OLD_BYTES)
        .await;
    let first_version =
        first_version.expect("a versioned write reports a version id");

    emulator
        .ok(
            "CreatePipeline",
            &declaration(
                pipeline,
                json!([
                    source_stage(SOURCE_BUCKET, SOURCE_KEY, ARTIFACT),
                    deploy_stage("Deploy", ARTIFACT, DEPLOY_BUCKET, DELIVERED, false),
                ]),
                Some("PARALLEL"),
            ),
        )
        .await;
    let first_run = creation_run(&emulator, pipeline).await;
    wait_for_terminal(&emulator, pipeline, &first_run).await;

    // A second revision of the source object, summarised differently, and a run of it.
    let (second_version, _) = emulator
        .put_object_with_metadata(
            SOURCE_BUCKET,
            SOURCE_KEY,
            &[(SUMMARY_KEY, "the second cut")],
            NEW_BYTES,
        )
        .await;
    let second_version = second_version.expect("a versioned write reports a version id");
    let started = emulator
        .ok("StartPipelineExecution", &json!({"name": pipeline}))
        .await;
    let second_run = started["pipelineExecutionId"]
        .as_str()
        .expect("the start reports an execution id")
        .to_string();
    wait_for_terminal(&emulator, pipeline, &second_run).await;

    // Each execution still reports the revision IT ran.
    for (case, execution, revision, summary) in [
        ("the first run", &first_run, &first_version, "the first cut"),
        ("the second run", &second_run, &second_version, "the second cut"),
    ] {
        let read = emulator
            .ok(
                "GetPipelineExecution",
                &json!({"pipelineName": pipeline, "pipelineExecutionId": execution}),
            )
            .await;
        assert_eq!(
            read["pipelineExecution"]["artifactRevisions"],
            json!([{
                "name": ARTIFACT,
                "revisionId": revision,
                "revisionSummary": summary,
            }]),
            "case {case:?}: an execution reports the revision it ran, got {read}"
        );
    }

    // And the action carries the latest revision resolved for it.
    let state = emulator.ok("GetPipelineState", &json!({"name": pipeline})).await;
    let action = state["stageStates"][0]["actionStates"][0].clone();
    assert_eq!(
        action["currentRevision"]["revisionId"],
        json!(second_version),
        "the action carries the revision its latest run resolved, got {action}"
    );
}
