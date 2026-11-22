// SPDX-License-Identifier: CC0-1.0

//! What an action reports when its PROVIDER classified the failure itself, rather than
//! relaying an answer to a call the engine made.
//!
//! The rule under test: an Amazon S3 source action whose configured `S3Bucket` does not
//! exist fails as a `ConfigurationError` whose text is the S3 source provider's own
//! sentence about that misconfiguration — the fixed words
//! `[No bucket with the name ` … ` was found]` with the configured bucket name as the one
//! variable part. That sentence is the provider's summary for the action execution
//! (`ActionExecutionResult$externalExecutionSummary` is "the action provider's summary for
//! the action execution", `smithy_query query="shape" shape="ActionExecutionResult"`) and
//! the text the action's state carries (`ActionState$latestExecution` ->
//! `ActionExecution$errorDetails$message` and `$summary`). Because the provider published
//! that one sentence, the action-execution history carries it once — as the provider's
//! summary — with `errorDetails` there stating the provider's code alone.
//!
//! The rows differ along what the rule generalises over: the form and length of the
//! configured bucket name, the shape of the object key, and whether the execution pinned
//! an object version — none of which is part of the sentence beyond the bucket name.
//!
//! The harness comes from `tests/all/execution_engine.rs`, so these pipelines run their
//! real actions through the emulated S3 service and every assertion goes through the
//! router.

use serde_json::{json, Value};

use crate::execution_engine::{
    action_detail, artifact_bucket, declaration, deploy_stage, latest_execution, source_stage,
    wait_for_terminal, Emulator,
};

/// The `FailureType` a source the declaration cannot read from at all is reported under
/// (`smithy_query query="shape" shape="FailureType"` -> `JobFailed`,
/// `ConfigurationError`, `PermissionError`, `RevisionOutOfSync`, `RevisionUnavailable`,
/// `SystemUnavailable`).
const CONFIGURATION_ERROR: &str = "ConfigurationError";

/// The bucket the deploy stage of each pipeline here would deliver into.
const DEPLOY_BUCKET: &str = "classified-deploy";

/// The artifact the source action would produce.
const ARTIFACT: &str = "SourceArtifact";

/// The modelled length of `ErrorDetails$message` -> `Message`, `@length(min: 1, max:
/// 5000)` (`smithy_query query="shape" shape="ErrorDetails"`).
const MAX_ERROR_MESSAGE: usize = 5_000;

/// The modelled length of `ActionExecution$summary` -> `ExecutionSummary`,
/// `@length(min: 1, max: 2048)` (`smithy_query query="shape" shape="ActionExecution"`).
const MAX_ACTION_SUMMARY: usize = 2_048;

/// The internal representation of the client error object an answer arrives in, which is
/// no fact about a pipeline: the Rust type and field names of the error, the raw response
/// document it carries and the per-call ids inside that document.
const CLIENT_INTERNALS: [&str; 7] = [
    "ServiceError",
    "ErrorMetadata",
    "SdkBody",
    "StatusCode",
    "RequestId",
    "HostId",
    "Response {",
];

/// The error code Amazon S3 answers a read of a bucket it does not hold with. The
/// provider CLASSIFIED this failure instead of relaying that answer, so its sentence
/// states neither the code nor S3's own words for it.
const S3_ANSWER: [&str; 2] = ["NoSuchBucket", "The specified bucket does not exist"];

/// The sentence the Amazon S3 source provider publishes for a bucket that does not exist:
/// fixed text, with the bucket the action's `S3Bucket` configuration named as its one
/// variable part.
fn provider_sentence(bucket: &str) -> String {
    format!("[No bucket with the name {bucket} was found]")
}

/// RULE: a bucket an S3 source action's configuration names and the service does not hold
/// is a source the declaration cannot read from, which the S3 source provider classifies
/// itself: the action fails `ConfigurationError` with the provider's own sentence naming
/// that bucket, as the state's message and summary and as the history's provider summary,
/// and the history's `errorDetails` states the code alone beside it.
#[tokio::test]
async fn a_missing_source_bucket_fails_with_the_providers_own_sentence() {
    // (case, the bucket the source action is configured with, the object key it names,
    //  the object version the execution pins)
    let rows: Vec<(&str, String, &str, Option<&str>)> = vec![
        (
            "a plain bucket name",
            "classified-absent".to_string(),
            "source.zip",
            None,
        ),
        (
            "a dotted name and a nested key",
            "team.builds.absent".to_string(),
            "app/releases/v2/source.zip",
            None,
        ),
        (
            "a pinned object version of an object in a bucket that is not there",
            "absent-versioned-source".to_string(),
            "source.zip",
            Some("00000000000000000000000000000001"),
        ),
        (
            // The longest name S3 holds a bucket under ("bucket names must be between 3
            // (min) and 63 (max) characters long"), so the one variable part of the
            // sentence is at its longest.
            "the longest bucket name S3 allows",
            format!("absent-{}", "x".repeat(56)),
            "source.zip",
            None,
        ),
    ];

    for (index, (case, bucket, key, pinned)) in rows.into_iter().enumerate() {
        let emulator = Emulator::new();
        let pipeline = format!("classified-{index}");
        emulator.bucket(&artifact_bucket(&pipeline)).await;
        emulator.bucket(DEPLOY_BUCKET).await;
        // The source bucket is the one thing that is NOT created.
        emulator
            .ok(
                "CreatePipeline",
                &declaration(
                    &pipeline,
                    json!([
                        source_stage(&bucket, key, ARTIFACT),
                        deploy_stage("Deploy", ARTIFACT, DEPLOY_BUCKET, "delivered.zip", false),
                    ]),
                    None,
                ),
            )
            .await;
        let mut start = json!({"name": pipeline});
        if let Some(version) = pinned {
            start["sourceRevisions"] = json!([{
                "actionName": "Src",
                "revisionType": "S3_OBJECT_VERSION_ID",
                "revisionValue": version,
            }]);
        }
        let started = emulator.ok("StartPipelineExecution", &start).await;
        let execution = started["pipelineExecutionId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: a start reports its execution, got {started}"))
            .to_string();
        let answer = wait_for_terminal(&emulator, &pipeline, &execution).await;
        assert_eq!(
            answer["pipelineExecution"]["status"],
            json!("Failed"),
            "case {case:?}: a source that cannot be read fails the execution, got {answer}"
        );

        let expected = provider_sentence(&bucket);
        // What the action's STATE says: the provider's classification and its sentence,
        // as both members carrying the failure.
        let latest = latest_execution(&emulator, &pipeline, "Src").await;
        assert_eq!(
            (
                latest["errorDetails"]["code"].clone(),
                latest["errorDetails"]["message"].clone(),
                latest["summary"].clone(),
            ),
            (
                json!(CONFIGURATION_ERROR),
                json!(expected),
                json!(expected),
            ),
            "case {case:?}: expected the provider's sentence {expected:?}, got {latest}"
        );

        // What the HISTORY says: the same sentence as the provider's summary, with the
        // provider's code alone in the error details beside it.
        let detail = action_detail(&emulator, &pipeline, &execution, "Src").await;
        let result = detail["output"]["executionResult"].clone();
        assert_eq!(
            (
                result["externalExecutionSummary"].clone(),
                result["errorDetails"]["code"].clone(),
                result["errorDetails"]["message"].clone(),
            ),
            (json!(expected), json!(CONFIGURATION_ERROR), Value::Null),
            "case {case:?}: expected the provider's summary and its code alone, got \
             {detail}"
        );

        // The sentence names the BUCKET and nothing else of the request: not the object
        // key, and no part of the emulator's own call to S3.
        for (member, text, limit) in [
            (
                "errorDetails.message",
                latest["errorDetails"]["message"].as_str().unwrap_or_default(),
                MAX_ERROR_MESSAGE,
            ),
            (
                "externalExecutionSummary",
                result["externalExecutionSummary"].as_str().unwrap_or_default(),
                MAX_ACTION_SUMMARY,
            ),
        ] {
            assert!(
                !text.contains(key),
                "case {case:?}: {member} is about the bucket, not the object {key:?}, got \
                 {text:?}"
            );
            for internal in CLIENT_INTERNALS {
                assert!(
                    !text.contains(internal),
                    "case {case:?}: {member} carries the client object's {internal:?}, got \
                     {text:?}"
                );
            }
            for relayed in S3_ANSWER {
                assert!(
                    !text.contains(relayed),
                    "case {case:?}: {member} relays S3's {relayed:?} instead of the \
                     provider's own sentence, got {text:?}"
                );
            }
            let measured = text.chars().count();
            assert!(
                (1..=limit).contains(&measured),
                "case {case:?}: {member} is modelled 1..={limit} characters, got {measured}"
            );
        }

        // And nothing was delivered downstream from a source that was never read.
        let (status, _) = emulator.get_object(DEPLOY_BUCKET, "delivered.zip").await;
        assert_eq!(
            status, 404,
            "case {case:?}: nothing is delivered from a source that is not there, got \
             {status}"
        );
    }
}

/// RULE: the sentence is the PROVIDER's classification of the declaration, so it is not
/// what a failure of the WORK reports: a bucket that is there but holds no such object is
/// a revision that is not available, and its text is this service's own account of the
/// read, naming the object.
///
/// One row each side of the boundary the rule draws, so the fix cannot answer every S3
/// source failure with the provider's sentence.
#[tokio::test]
async fn only_the_missing_bucket_is_the_providers_classification() {
    // (case, the bucket exists, the FailureType expected, the sentence is the provider's)
    let rows: Vec<(&str, bool, &str, bool)> = vec![
        ("the bucket is not there at all", false, CONFIGURATION_ERROR, true),
        (
            "the bucket is there and holds no such object",
            true,
            "RevisionUnavailable",
            false,
        ),
    ];

    for (index, (case, bucket_exists, expected_code, classified)) in rows.into_iter().enumerate() {
        let emulator = Emulator::new();
        let pipeline = format!("boundary-{index}");
        let bucket = format!("boundary-source-{index}");
        let key = "missing-object.zip";
        emulator.bucket(&artifact_bucket(&pipeline)).await;
        emulator.bucket(DEPLOY_BUCKET).await;
        if bucket_exists {
            emulator.bucket(&bucket).await;
        }
        emulator
            .ok(
                "CreatePipeline",
                &declaration(
                    &pipeline,
                    json!([
                        source_stage(&bucket, key, ARTIFACT),
                        deploy_stage("Deploy", ARTIFACT, DEPLOY_BUCKET, "delivered.zip", false),
                    ]),
                    None,
                ),
            )
            .await;
        let started = emulator
            .ok("StartPipelineExecution", &json!({"name": pipeline}))
            .await;
        let execution = started["pipelineExecutionId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: a start reports its execution, got {started}"))
            .to_string();
        wait_for_terminal(&emulator, &pipeline, &execution).await;

        let latest = latest_execution(&emulator, &pipeline, "Src").await;
        assert_eq!(
            latest["errorDetails"]["code"],
            json!(expected_code),
            "case {case:?}: expected the failure type {expected_code}, got {latest}"
        );
        let message = latest["errorDetails"]["message"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert_eq!(
            message == provider_sentence(&bucket),
            classified,
            "case {case:?}: the provider's sentence is for the bucket that is not there, \
             got {message:?}"
        );
        let detail = action_detail(&emulator, &pipeline, &execution, "Src").await;
        let reported = detail["output"]["executionResult"]["errorDetails"]["message"].clone();
        // A failure of the WORK is explained by a message of its own, which the history
        // reports beside the code; the provider's classification is reported once, as its
        // summary.
        assert_eq!(
            reported.is_null(),
            classified,
            "case {case:?}: the history states the message of a failure the provider did \
             not classify, got {detail}"
        );
        if !classified {
            assert!(
                message.contains(key),
                "case {case:?}: the read that failed names the object, got {message:?}"
            );
        }
    }
}
