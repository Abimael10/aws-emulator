// SPDX-License-Identifier: CC0-1.0

//! What the Build record a run REPORTS carries, start to finish.
//!
//! Three rules, each asserted over a table whose rows differ along what that rule
//! generalises over:
//!
//! * a run always STATES the artifacts destination it carries — `artifacts.type` in the
//!   token form the wire uses and `artifacts.location` as a stated value, the empty string
//!   for a destination that names none — so a client reading `artifacts` can tell a
//!   destination that holds nothing from an answer that said nothing;
//! * a build whose environment was provisioned goes through the WHOLE phase sequence and
//!   reports every phase it reached, the four command phases among them whether or not the
//!   buildspec declares commands for any of them, and every phase that ENDED carries a
//!   `contexts` list — the empty report when it had nothing to report;
//! * a settled build reports the environment it RAN IN and where its output went: the
//!   console link to the log stream it wrote, and the variables its buildspec exported.
//!
//! Every call goes over the wire through the harness `project_build_lifecycle.rs` keeps.

use serde_json::{json, Value};

use super::project_build_lifecycle::{
    call, create_project, emulator, merged, phase_of, settled_build, start_build,
};

/// A `CreateProject` source carrying `buildspec` inline, with no input source code of its
/// own — the form that fixes exactly what a build runs.
fn source_with(buildspec: &str) -> Value {
    json!({"source": {"type": "NO_SOURCE", "buildspec": buildspec}})
}

/// The whole phase sequence a build that got an environment goes through, in order.
///
/// The `BuildPhaseType` enum, as the model declares its values
/// (`smithy_query(query="shape",shape="BuildPhaseType")`): SUBMITTED, QUEUED,
/// PROVISIONING, DOWNLOAD_SOURCE, INSTALL, PRE_BUILD, BUILD, POST_BUILD,
/// UPLOAD_ARTIFACTS, FINALIZING, COMPLETED.
const WHOLE_SEQUENCE: [&str; 11] = [
    "SUBMITTED",
    "QUEUED",
    "PROVISIONING",
    "DOWNLOAD_SOURCE",
    "INSTALL",
    "PRE_BUILD",
    "BUILD",
    "POST_BUILD",
    "UPLOAD_ARTIFACTS",
    "FINALIZING",
    "COMPLETED",
];

/// The two phases that happen before the build environment exists: the build having been
/// submitted, and its wait in the queue. Neither is a phase of that environment, so
/// neither carries a context.
const PHASES_BEFORE_THE_ENVIRONMENT: [&str; 2] = ["SUBMITTED", "QUEUED"];

/// The `phaseType`s a build reports, in the order it reached them.
fn phase_types(build: &Value) -> Vec<String> {
    build["phases"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|phase| phase["phaseType"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// RULE: a run reports the artifacts destination it RUNS WITH — the destination's type in
/// the token form the service puts on the wire (the configured value lowercased) and its
/// location as a stated value, which is the empty string for a destination that names none
/// — and the record it persists reports the same thing when it is read back.
///
/// The rows differ along what the rule generalises over: which destination type is
/// configured (each value `ArtifactsType` declares, from
/// `smithy_query(query="shape",shape="ArtifactsType")`: CODEPIPELINE, S3, NO_ARTIFACTS),
/// whether the destination names a location at all, and whether the destination comes from
/// the project or from `StartBuild`'s own override.
#[tokio::test]
async fn a_run_states_the_artifacts_destination_it_carries() {
    let router = emulator();
    // (case, the project's `artifacts`, the `StartBuild` override, the type, the location)
    let rows: Vec<(&str, Value, Value, &str, &str)> = vec![
        (
            "a project that produces no artifacts",
            json!({"artifacts": {"type": "NO_ARTIFACTS"}}),
            json!({}),
            "no_artifacts",
            "",
        ),
        (
            "a destination in the object store",
            json!({"artifacts": {"type": "S3", "location": "artifact-destination"}}),
            json!({}),
            "s3",
            "artifact-destination",
        ),
        (
            // The pipeline's own destination names no location: the pipeline decides it.
            "a destination the pipeline owns",
            json!({"artifacts": {"type": "CODEPIPELINE"}}),
            json!({}),
            "codepipeline",
            "",
        ),
        (
            "a destination the request overrides",
            json!({"artifacts": {"type": "NO_ARTIFACTS"}}),
            json!({"artifactsOverride": {"type": "S3", "location": "override-destination"}}),
            "s3",
            "override-destination",
        ),
    ];

    for (index, (case, artifacts, override_members, expected_type, expected_location)) in
        rows.iter().enumerate()
    {
        let project = format!("artifacts-record-proj-{index}");
        create_project(&router, &project, artifacts.clone()).await;
        let started = start_build(
            &router,
            merged(json!({"projectName": project}), override_members.clone()),
        )
        .await;
        // The start and the read-back report the same destination: it is the build's own
        // record rather than something one operation computes.
        assert_eq!(
            started["artifacts"]["type"],
            json!(expected_type),
            "case {case:?}: StartBuild expected artifacts.type {expected_type:?}, got {}",
            started["artifacts"]
        );
        assert_eq!(
            started["artifacts"]["location"],
            json!(expected_location),
            "case {case:?}: StartBuild expected artifacts.location {expected_location:?}, \
             got {}",
            started["artifacts"]
        );
        let id = started["id"].as_str().unwrap_or_default().to_string();
        let (status, read) = call(&router, "BatchGetBuilds", json!({"ids": [id.clone()]})).await;
        assert_eq!(
            status, 200,
            "case {case:?}: BatchGetBuilds expected 200, got {status} with {read}"
        );
        let stored = read["builds"][0].clone();
        assert_eq!(
            stored["artifacts"]["type"],
            json!(expected_type),
            "case {case:?}: BatchGetBuilds expected artifacts.type {expected_type:?}, got {}",
            stored["artifacts"]
        );
        // A destination nothing was uploaded to still states where it would have gone; the
        // location a build RESOLVED is the upload's own subject (`build_artifacts_destination`).
        assert!(
            stored["artifacts"]["location"].is_string(),
            "case {case:?}: BatchGetBuilds must state artifacts.location, got {}",
            stored["artifacts"]
        );
    }
}

/// RULE: a build whose environment was provisioned goes through the whole phase sequence
/// and reports every phase it reached — the four phases a buildspec declares commands for
/// among them, whether or not it declares any for them, since a phase that declares
/// nothing runs no command and so succeeds. Every phase that ENDED carries a `contexts`
/// list: what it reported, or one context whose `statusCode` and `message` are both empty
/// when it had nothing to report. The two phases before the environment exists carry none,
/// and neither does the terminal marker, which has not ended.
///
/// The rows differ along what the rule generalises over: how many of the four phases the
/// buildspec declares, which ones, and whether it declares a `phases` key at all.
#[tokio::test]
async fn a_build_reports_every_phase_it_went_through() {
    let router = emulator();
    // (case, the buildspec)
    let rows: Vec<(&str, &str)> = vec![
        ("a buildspec declaring no phases key at all", "version: 0.2"),
        (
            "a buildspec declaring an empty phases mapping",
            "version: 0.2\nphases:\n  build:\n    commands: []\n",
        ),
        (
            "a buildspec declaring one middle phase",
            "version: 0.2\nphases:\n  build:\n    commands:\n      - echo building\n",
        ),
        (
            "a buildspec declaring only the last phase",
            "version: 0.2\nphases:\n  post_build:\n    commands:\n      - echo after\n",
        ),
        (
            "a buildspec declaring every phase",
            "version: 0.2\nphases:\n  install:\n    commands:\n      - echo installing\n  \
             pre_build:\n    commands:\n      - echo before\n  build:\n    commands:\n      \
             - echo building\n  post_build:\n    commands:\n      - echo after\n",
        ),
    ];

    for (index, (case, buildspec)) in rows.iter().enumerate() {
        let project = format!("phase-sequence-proj-{index}");
        create_project(&router, &project, source_with(buildspec)).await;
        let build = settled_build(&router, &project, json!({})).await;
        assert_eq!(
            build["buildStatus"],
            json!("SUCCEEDED"),
            "case {case:?}: a build whose commands all succeeded is SUCCEEDED, got {build}"
        );
        assert_eq!(
            phase_types(&build),
            WHOLE_SEQUENCE
                .iter()
                .map(|phase| (*phase).to_string())
                .collect::<Vec<_>>(),
            "case {case:?}: expected the whole phase sequence, got {build}"
        );
        for phase_type in WHOLE_SEQUENCE {
            let phase = phase_of(&build, phase_type)
                .unwrap_or_else(|| panic!("case {case:?}: a {phase_type} phase, got {build}"));
            if phase_type == "COMPLETED" {
                // The terminal marker has started and has not ended, so it carries no
                // outcome and nothing to report about one.
                assert!(
                    phase["phaseStatus"].is_null()
                        && phase["endTime"].is_null()
                        && phase["durationInSeconds"].is_null()
                        && phase["contexts"].is_null(),
                    "case {case:?}: the terminal marker carries no outcome, got {phase}"
                );
                continue;
            }
            assert_eq!(
                phase["phaseStatus"],
                json!("SUCCEEDED"),
                "case {case:?}: expected {phase_type} to have SUCCEEDED, got {phase}"
            );
            assert!(
                phase["endTime"].is_number() && phase["durationInSeconds"].is_number(),
                "case {case:?}: an ended {phase_type} states when it ended and how long it \
                 took, got {phase}"
            );
            let expected = if PHASES_BEFORE_THE_ENVIRONMENT.contains(&phase_type) {
                Value::Null
            } else {
                json!([{"statusCode": "", "message": ""}])
            };
            assert_eq!(
                phase["contexts"], expected,
                "case {case:?}: expected {phase_type} contexts {expected}, got {phase}"
            );
        }
    }
}

/// One row of the table below: the case, the buildspec it runs, and the name/value pairs
/// its `env/exported-variables` must report.
type ExportRow = (&'static str, String, Vec<(&'static str, &'static str)>);

/// RULE: a settled build reports what its own RUN produced: the environment it ran in
/// (`environment.customEntrypoint`, which a build that has not been provisioned yet does
/// not report), the console link to the log stream its output went to, and every variable
/// its buildspec's `env/exported-variables` declared, with the value its commands computed.
///
/// The rows differ along what the rule generalises over: how the exported value is
/// computed (literal text, shell arithmetic, a command substitution), how many variables
/// are exported, and whether the phase that sets them is the first or a later one.
#[tokio::test]
async fn a_settled_build_reports_what_its_run_produced() {
    let router = emulator();
    // (case, the buildspec, the exported name/value pairs it must report)
    let rows: Vec<ExportRow> = vec![
        (
            "a literal value exported from the install phase",
            "version: 0.2\nenv:\n  exported-variables:\n    - PLAIN\nphases:\n  install:\n    \
             commands:\n      - export PLAIN=literal\n"
                .to_string(),
            vec![("PLAIN", "literal")],
        ),
        (
            "a value the build COMPUTES with shell arithmetic",
            "version: 0.2\nenv:\n  exported-variables:\n    - ANSWER\nphases:\n  build:\n    \
             commands:\n      - export ANSWER=$((19 + 23))\n"
                .to_string(),
            vec![("ANSWER", "42")],
        ),
        (
            "two values, one of them from a command substitution",
            "version: 0.2\nenv:\n  exported-variables:\n    - GREETING\n    - COUNT\nphases:\n  \
             pre_build:\n    commands:\n      - export GREETING=\"$(echo hello)\"\n  \
             post_build:\n    commands:\n      - export COUNT=\"$(( 2 * 3 ))\"\n"
                .to_string(),
            vec![("GREETING", "hello"), ("COUNT", "6")],
        ),
    ];

    for (index, (case, buildspec, exported)) in rows.iter().enumerate() {
        let project = format!("run-record-proj-{index}");
        create_project(&router, &project, source_with(buildspec)).await;

        // Before the run: the build has been submitted and no environment has been
        // provisioned for it, so it reports none of what running it produces.
        let started = start_build(&router, json!({"projectName": project})).await;
        assert!(
            started["environment"]["customEntrypoint"].is_null(),
            "case {case:?}: a build with no environment yet reports no custom entrypoint, \
             got {}",
            started["environment"]
        );

        let build = settled_build(&router, &project, json!({})).await;
        assert_eq!(
            build["buildStatus"],
            json!("SUCCEEDED"),
            "case {case:?}: every command of this buildspec succeeds, got {build}"
        );
        assert_eq!(
            build["environment"]["customEntrypoint"],
            json!(true),
            "case {case:?}: a provisioned environment is reported, got {}",
            build["environment"]
        );
        // Where its output went: the group and stream the build names, and the console
        // address of those same two names.
        let logs = build["logs"].clone();
        let group = logs["groupName"].as_str().unwrap_or_default().to_string();
        let stream = logs["streamName"].as_str().unwrap_or_default().to_string();
        assert!(
            !group.is_empty() && !stream.is_empty(),
            "case {case:?}: a delivered build names its group and stream, got {logs}"
        );
        let expected_link = format!("#logEvent:group={group};stream={stream}");
        assert!(
            logs["deepLink"]
                .as_str()
                .is_some_and(|link| link.ends_with(&expected_link)),
            "case {case:?}: expected a deep link ending {expected_link:?}, got {logs}"
        );
        assert!(
            logs["cloudWatchLogsArn"]
                .as_str()
                .is_some_and(|arn| arn.ends_with(&format!(
                    ":log-group:{group}:log-stream:{stream}"
                ))),
            "case {case:?}: expected the stream's own ARN, got {logs}"
        );
        // And what the buildspec declared it exports, with the values the commands computed.
        let reported: Vec<(String, String)> = build["exportedEnvironmentVariables"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|variable| {
                (
                    variable["name"].as_str().unwrap_or_default().to_string(),
                    variable["value"].as_str().unwrap_or_default().to_string(),
                )
            })
            .collect();
        for (name, value) in exported {
            assert!(
                reported
                    .iter()
                    .any(|(reported, held)| reported == name && held == value),
                "case {case:?}: expected the exported {name}={value}, got {reported:?}"
            );
        }
    }
}
