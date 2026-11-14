// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for what a PIPELINE of text utilities computes in a build, driven over the
//! wire the router serves: `POST /` with `X-Amz-Target: CodeBuild_20161006.<Operation>` on
//! awsJson1_1.
//!
//! RULE under test: a build's commands run in the build environment's shell, where a pipeline
//! connects its stages — what one stage writes to standard output is the standard input of the
//! next — and the environment provides the text utilities a build image carries, each computing
//! out of that one input. So `Build$exportedEnvironmentVariables` carries what the build's own
//! shell assigned to each name the buildspec's `env/exported-variables` listed, which for a
//! value computed through a pipeline is the answer of its LAST stage over the bytes the stage
//! before it produced — never the bytes that went in, and never an empty value.
//!
//! Each row below differs along what that rule generalises over: which utility reads the stage
//! before it, how many stages the pipeline has, how the utility's option is written, and the
//! form of the input (a plain word, text whose byte length differs from its character count,
//! several lines, delimited fields).

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::project_build_lifecycle::{create_project, emulator, merged, settled_build};

/// A `CreateProject` source carrying `buildspec` inline, with no input source code of its own.
fn source_with(buildspec: &str) -> Value {
    json!({"source": {"type": "NO_SOURCE", "buildspec": buildspec}})
}

/// The name of the variable each row's project puts its input under, as a `PLAINTEXT`
/// environment variable of the project: what the pipeline computes over.
const INPUT_NAME: &str = "PIPELINE_INPUT";

/// The name each row's buildspec exports its answer under.
const ANSWER_NAME: &str = "PIPELINE_ANSWER";

/// A buildspec that exports the value `command` assigns to [`ANSWER_NAME`].
///
/// The command is written as a single-quoted YAML scalar, with its own single quotes doubled as
/// YAML spells one inside: a command line carries `: ` and `#`, which a plain scalar may not.
fn buildspec_computing(command: &str) -> String {
    let quoted = format!("'{}'", command.replace('\'', "''"));
    format!(
        "version: 0.2\nenv:\n  exported-variables:\n    - {ANSWER_NAME}\nphases:\n  build:\n    \
         commands:\n      - {quoted}\n"
    )
}

/// The project members that put `input` in the build's environment under [`INPUT_NAME`].
fn environment_holding(input: &str) -> Value {
    json!({"environment": {
        "type": "LINUX_CONTAINER",
        "image": "aws/codebuild/standard:7.0",
        "computeType": "BUILD_GENERAL1_SMALL",
        "environmentVariables": [{"name": INPUT_NAME, "value": input, "type": "PLAINTEXT"}],
    }})
}

/// The value a settled build reports for the exported variable `name`.
fn exported_value(build: &Value, name: &str) -> Option<String> {
    build["exportedEnvironmentVariables"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .find(|variable| variable["name"] == json!(name))
        .and_then(|variable| variable["value"].as_str().map(str::to_string))
}

/// The lowercase hexadecimal SHA-256 of `text`, which is what `sha256sum` writes for it.
fn digest_of(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    hex::encode(hasher.finalize())
}

/// RULE: the exported value is what the LAST stage of the pipeline computed over the bytes the
/// stage before it wrote.
#[tokio::test]
async fn an_exported_variable_carries_what_the_pipeline_computed() {
    let router = emulator();
    // (case, the input the environment holds, the command the build phase runs, the value the
    // build must export)
    let rows: Vec<(&str, &str, String, String)> = vec![
        (
            "a digest over the variable's value",
            "a-plaintext-value",
            format!(
                "export {ANSWER_NAME}=\"$(printf '%s' \"${INPUT_NAME}\" | sha256sum \
                 | cut -c1-64)\""
            ),
            digest_of("a-plaintext-value"),
        ),
        (
            // The published SHA-256 vector for `abc`, so one row is pinned to a digest nothing
            // in this emulator computed.
            "a digest of a value whose digest is published with the algorithm",
            "abc",
            format!(
                "export {ANSWER_NAME}=\"$(printf '%s' \"${INPUT_NAME}\" | sha256sum \
                 | cut -c1-64)\""
            ),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad".to_string(),
        ),
        (
            "a byte count over the variable's value",
            "a-plaintext-value",
            format!("export {ANSWER_NAME}=\"$(printf '%s' \"${INPUT_NAME}\" | wc -c)\""),
            "17".to_string(),
        ),
        (
            // The bytes and the characters differ here, and `wc -c` is the byte count.
            "a byte count over a value carrying multi-byte characters",
            "clé-£",
            format!("export {ANSWER_NAME}=\"$(printf '%s' \"${INPUT_NAME}\" | wc -c)\""),
            "clé-£".len().to_string(),
        ),
        (
            "the character count of the same value",
            "clé-£",
            format!("export {ANSWER_NAME}=\"$(printf '%s' \"${INPUT_NAME}\" | wc -m)\""),
            "clé-£".chars().count().to_string(),
        ),
        (
            "a field selected out of a delimited value",
            "left:middle:right",
            format!("export {ANSWER_NAME}=\"$(printf '%s' \"${INPUT_NAME}\" | cut -d: -f2)\""),
            "middle".to_string(),
        ),
        (
            "the characters a glued range names",
            "0123456789",
            format!("export {ANSWER_NAME}=\"$(printf '%s' \"${INPUT_NAME}\" | cut -c3-5)\""),
            "234".to_string(),
        ),
        (
            "the first lines of a value written over several",
            "first\nsecond\nthird\n",
            format!("export {ANSWER_NAME}=\"$(printf '%s' \"${INPUT_NAME}\" | head -n 1)\""),
            // The value the environment holds carries the line breaks, so what the pipeline
            // reads is three lines and the first of them is the answer.
            "first".to_string(),
        ),
        (
            "a count of those lines",
            "first\nsecond\nthird\n",
            format!("export {ANSWER_NAME}=\"$(printf '%s' \"${INPUT_NAME}\" | wc -l)\""),
            "3".to_string(),
        ),
        (
            "four stages, each over what the one before it produced",
            "a-plaintext-value",
            format!(
                "export {ANSWER_NAME}=\"$(printf '%s' \"${INPUT_NAME}\" | sha256sum \
                 | cut -c1-64 | wc -c)\""
            ),
            // The digest is 64 characters and the selection ends with the newline the utility
            // writes for each line it selected, so the count over it is 65.
            "65".to_string(),
        ),
    ];

    for (index, (case, input, command, expected)) in rows.iter().enumerate() {
        let project = format!("pipeline-proj-{index}");
        create_project(
            &router,
            &project,
            merged(
                source_with(&buildspec_computing(command)),
                environment_holding(input),
            ),
        )
        .await;
        let build = settled_build(&router, &project, json!({})).await;
        assert_eq!(
            build["buildStatus"],
            json!("SUCCEEDED"),
            "case {case:?}: the computing build must succeed, got {build}"
        );
        let answered = exported_value(&build, ANSWER_NAME);
        assert_eq!(
            answered.as_deref(),
            Some(expected.as_str()),
            "case {case:?}: expected {expected:?}, got {answered:?} in {build}"
        );
    }
}

/// RULE: only the LAST stage's output is the pipeline's, so what an earlier stage wrote is not
/// part of the value — and a stage that reads nothing at all is a command that did not run,
/// which fails the phase that declared it.
#[tokio::test]
async fn an_earlier_stages_output_is_not_the_pipelines() {
    let router = emulator();
    // (case, the command, the build's status, the value it exports)
    let rows: Vec<(&str, String, &str, Option<String>)> = vec![
        (
            // The stage before the last one wrote fifteen bytes into the pipe; the value is the
            // COUNT the last stage answered, not the bytes it counted.
            "the stage before the last one is not what the value carries",
            format!("export {ANSWER_NAME}=\"$(printf '%s' pipeline-input | wc -c)\""),
            "SUCCEEDED",
            Some("14".to_string()),
        ),
        (
            "a filter with no input at all could not run, and a command that could not run \
             fails its phase",
            "wc -c".to_string(),
            "FAILED",
            None,
        ),
    ];
    for (index, (case, command, status, expected)) in rows.iter().enumerate() {
        let project = format!("pipeline-stage-proj-{index}");
        create_project(
            &router,
            &project,
            merged(
                source_with(&buildspec_computing(command)),
                environment_holding("unread"),
            ),
        )
        .await;
        let build = settled_build(&router, &project, json!({})).await;
        assert_eq!(
            build["buildStatus"],
            json!(status),
            "case {case:?}: expected buildStatus {status}, got {build}"
        );
        let answered = exported_value(&build, ANSWER_NAME);
        assert_eq!(
            answered, *expected,
            "case {case:?}: expected {expected:?}, got {answered:?} in {build}"
        );
    }
}
