// SPDX-License-Identifier: CC0-1.0

//! WHICH phases a build reports once a phase failure has ENDED it.
//!
//! RULE: `on-failure` decides what a failing phase does to the phases after it, and its
//! default is `ABORT`: "the build stops" at a phase that PREPARES the build (`install`,
//! `pre_build`), so every phase that would have come after it — the later buildspec phases
//! AND the upload of the build's artifacts — is work the build never attempted, and
//! `BatchGetBuilds` reports none of them. Not reported as failed, not reported as
//! succeeded: absent. From the BUILD phase on, a failure stops none of that work: the
//! commands of that phase ran and whatever they wrote is in the build's working tree, so
//! CodeBuild still runs the phases it runs after the build phase and still packages what the
//! build produced. Either way the build's own finishing work ran, so `FINALIZING` and the
//! terminal `COMPLETED` marker are reported, the build is `buildComplete` and its
//! `buildStatus` is `FAILED`. `on-failure: CONTINUE` is the other arm of the first half: the
//! phases after a failure it continues past DID run, so each of them is reported with its
//! own outcome and the upload among them.
//!
//! The rows span what the rule generalises over: WHICH phase fails (each of the four a
//! buildspec declares commands for), the FORM its failing command takes, and whether the
//! project names an artifacts DESTINATION at all — what decides the upload is where the
//! failure fell rather than there being something to upload.
//!
//! Every call goes over the wire through the harness `project_build_lifecycle.rs` keeps.

use serde_json::{json, Value};

use super::project_build_lifecycle::{create_project, emulator, merged, phase_pairs, settled_build};

/// The phases a build goes through after its specification was read, in order: the four
/// phases a buildspec declares commands for and then the upload of what they produced. The
/// ones a build reports are decided by where its failure fell, so a row reads the phases
/// AFTER its own failing phase out of this sequence.
const PHASE_SEQUENCE: [&str; 5] = [
    "INSTALL",
    "PRE_BUILD",
    "BUILD",
    "POST_BUILD",
    "UPLOAD_ARTIFACTS",
];

/// The phases every build that ran its commands reports, however they answered.
const FINISHING_PHASES: [&str; 2] = ["FINALIZING", "COMPLETED"];

/// A `CreateProject` source carrying `buildspec` inline, with no input source code of its
/// own — the form that fixes exactly what a build runs.
fn source_with(buildspec: &str) -> Value {
    json!({"source": {"type": "NO_SOURCE", "buildspec": buildspec}})
}

/// A buildspec declaring `commands` for each phase named, in buildspec order.
fn buildspec(phases: &[(&str, &str)]) -> String {
    let mut spec = String::from("version: 0.2\nphases:\n");
    for (phase, command) in phases {
        spec.push_str(&format!(
            "  {phase}:\n    commands:\n      - {command}\n"
        ));
    }
    spec
}

#[tokio::test]
async fn a_build_a_preparing_phase_ended_reports_none_of_the_phases_it_never_attempted() {
    let router = emulator();

    // (case, the phase whose command fails, the whole buildspec, the artifacts destination)
    let destination = json!({"artifacts": {"type": "S3", "location": "artifact-destination"}});
    let rows: Vec<(&str, &str, String, Value)> = vec![
        (
            "the install phase ends the build",
            "INSTALL",
            buildspec(&[
                ("install", "false"),
                ("pre_build", "echo never"),
                ("build", "echo never"),
                ("post_build", "echo never"),
            ]),
            json!({}),
        ),
        (
            "the pre_build phase ends the build",
            "PRE_BUILD",
            buildspec(&[
                ("install", "echo installing"),
                ("pre_build", "false"),
                ("build", "echo never"),
                ("post_build", "echo never"),
            ]),
            json!({}),
        ),
        (
            "the pre_build phase ends a build whose artifacts name a destination",
            "PRE_BUILD",
            buildspec(&[("pre_build", "false"), ("build", "echo never")]),
            destination.clone(),
        ),
        (
            "an install phase ended by an explicit non-zero exit",
            "INSTALL",
            buildspec(&[("install", "exit 7"), ("build", "echo never")]),
            json!({}),
        ),
        (
            "a pre_build phase whose own finally block is what failed",
            "PRE_BUILD",
            "version: 0.2\nphases:\n  pre_build:\n    commands:\n      - echo preparing\n    \
             finally:\n      - false\n  build:\n    commands:\n      - echo never\n"
                .to_string(),
            destination.clone(),
        ),
    ];

    for (index, (case, failing, spec, artifacts)) in rows.iter().enumerate() {
        let project = format!("abort-phases-proj-{index}");
        create_project(
            &router,
            &project,
            merged(source_with(spec), artifacts.clone()),
        )
        .await;
        let build = settled_build(&router, &project, json!({})).await;
        let pairs = phase_pairs(&build);
        let reported: Vec<String> = pairs.iter().map(|(phase, _)| phase.clone()).collect();

        assert_eq!(
            build["buildStatus"],
            json!("FAILED"),
            "case {case:?}: expected the ended build to be FAILED, got {build}"
        );
        assert_eq!(
            build["buildComplete"],
            json!(true),
            "case {case:?}: a build a failure ended is complete, got {build}"
        );
        // The phase that failed is reported, and it is the only one that failed.
        let failures: Vec<&String> = pairs
            .iter()
            .filter(|(_, status)| status.as_deref() == Some("FAILED"))
            .map(|(phase, _)| phase)
            .collect();
        assert_eq!(
            failures,
            vec![failing],
            "case {case:?}: expected {failing} to be the one failing phase, got {pairs:?}"
        );
        // Every phase the build would have reached after it is absent — neither failed nor
        // succeeded, but never attempted. The phases BEFORE the failure are not in scope
        // here, so the list is cut at the failing phase.
        let after = PHASE_SEQUENCE
            .iter()
            .skip_while(|phase| *phase != failing)
            .skip(1);
        for phase in after {
            assert!(
                !reported.contains(&(*phase).to_string()),
                "case {case:?}: {phase} was never attempted, so it must not be reported, got \
                 {pairs:?}"
            );
        }
        // And the build's own finishing work is still reported.
        for phase in FINISHING_PHASES {
            assert!(
                reported.contains(&phase.to_string()),
                "case {case:?}: expected {phase} to be reported, got {pairs:?}"
            );
        }
    }
}

/// RULE: a failure from the BUILD phase on ends none of the work CodeBuild does after that
/// phase — the commands of that phase ran and what they wrote is in the build's working
/// tree, so the later buildspec phases still run and the build still ATTEMPTS the upload of
/// its artifacts, which it reports as an `UPLOAD_ARTIFACTS` phase — while the failing phase
/// keeps its `FAILED` status and the build is `FAILED`.
///
/// The rows differ along what the rule generalises over: WHICH phase from the build phase on
/// fails, the FORM its failing command takes (a utility that answers failure, an explicit
/// non-zero `exit`, a command of the phase's own `finally` block), and whether the project
/// names an artifacts destination at all. WHERE the upload's bytes land, and the status that
/// upload then reports, is the object store's half of the run, covered where a store is
/// mounted (`core/router/tests/all/codebuild_s3_build_output.rs`).
#[tokio::test]
async fn a_failure_from_the_build_phase_on_still_reaches_the_upload() {
    let router = emulator();

    // (case, the phase whose command fails, the whole buildspec, the artifacts destination,
    //  the phases that must be reported SUCCEEDED after it)
    let destination = json!({"artifacts": {"type": "S3", "location": "artifact-destination"}});
    let rows: Vec<(&str, &str, String, Value, Vec<&str>)> = vec![
        (
            "the build phase fails with an explicit non-zero exit",
            "BUILD",
            buildspec(&[("build", "exit 7"), ("post_build", "echo after")]),
            json!({}),
            vec!["POST_BUILD"],
        ),
        (
            "the build phase fails with the utility that answers failure",
            "BUILD",
            buildspec(&[("build", "false"), ("post_build", "echo after")]),
            json!({}),
            vec!["POST_BUILD"],
        ),
        (
            "a build phase whose own finally block is what failed",
            "BUILD",
            "version: 0.2\nphases:\n  build:\n    commands:\n      - echo building\n    \
             finally:\n      - false\n  post_build:\n    commands:\n      - echo after\n"
                .to_string(),
            json!({}),
            vec!["POST_BUILD"],
        ),
        (
            "the post_build phase fails, and nothing of the buildspec is left to run",
            "POST_BUILD",
            buildspec(&[("build", "echo building"), ("post_build", "false")]),
            json!({}),
            vec![],
        ),
        (
            "the build phase fails on a project whose artifacts name a destination",
            "BUILD",
            buildspec(&[("build", "exit 3"), ("post_build", "echo after")]),
            destination.clone(),
            vec!["POST_BUILD"],
        ),
    ];

    for (index, (case, failing, spec, artifacts, succeeding)) in rows.iter().enumerate() {
        let project = format!("build-failure-proj-{index}");
        create_project(
            &router,
            &project,
            merged(source_with(spec), artifacts.clone()),
        )
        .await;
        let build = settled_build(&router, &project, json!({})).await;
        let pairs = phase_pairs(&build);
        let reported: Vec<String> = pairs.iter().map(|(phase, _)| phase.clone()).collect();

        assert_eq!(
            build["buildStatus"],
            json!("FAILED"),
            "case {case:?}: a phase that failed fails the build, got {build}"
        );
        assert_eq!(
            pairs
                .iter()
                .find(|(phase, _)| phase == failing)
                .and_then(|(_, status)| status.clone()),
            Some("FAILED".to_string()),
            "case {case:?}: expected {failing} to be reported FAILED, got {pairs:?}"
        );
        // The phases CodeBuild runs after it ran, and so did the build's own upload: the
        // phase is REPORTED, whatever that upload then answered.
        for phase in succeeding {
            assert_eq!(
                pairs
                    .iter()
                    .find(|(reported, _)| reported == phase)
                    .and_then(|(_, status)| status.clone()),
                Some("SUCCEEDED".to_string()),
                "case {case:?}: expected {phase} to have run after the failure, got {pairs:?}"
            );
        }
        for phase in ["UPLOAD_ARTIFACTS", "FINALIZING", "COMPLETED"] {
            assert!(
                reported.contains(&phase.to_string()),
                "case {case:?}: expected {phase} to be reported, got {pairs:?}"
            );
        }
    }
}

/// RULE: a phase whose `on-failure` is `CONTINUE` runs the phases after it however the
/// failing command came to fail — each of them reported with its own outcome and the upload
/// among them — while the failed phase keeps its `FAILED` status and the build is `FAILED`
/// either way.
///
/// The rows differ along what the rule generalises over: WHICH phase fails and continues, and
/// the FORM its failing command takes — a command that exits non-zero of its own accord, and
/// one that ends its shell with an explicit `exit`, which ends that command's own program and
/// not the build's phase sequence.
#[tokio::test]
async fn a_phase_that_continues_past_its_failure_runs_the_later_phases() {
    let router = emulator();

    /// One row: the case, the buildspec, and the `(phaseType, phaseStatus)` pairs the build
    /// must report for the phases the rule is about.
    type Row = (&'static str, String, Vec<(&'static str, &'static str)>);

    /// A buildspec declaring all four command phases, `phase`'s own command being `command`
    /// and its `on-failure` `CONTINUE`, with every other phase echoing.
    fn continuing(phase: &str, command: &str) -> String {
        let mut spec = String::from("version: 0.2\nphases:\n");
        for declared in ["install", "pre_build", "build", "post_build"] {
            spec.push_str(&format!("  {declared}:\n"));
            if declared == phase {
                spec.push_str("    on-failure: CONTINUE\n");
            }
            let line = if declared == phase { command } else { "echo ran" };
            spec.push_str(&format!("    commands:\n      - {line}\n"));
        }
        spec
    }

    let rows: Vec<Row> = vec![
        (
            "pre_build fails with a command that exits non-zero",
            continuing("pre_build", "false"),
            vec![
                ("INSTALL", "SUCCEEDED"),
                ("PRE_BUILD", "FAILED"),
                ("BUILD", "SUCCEEDED"),
                ("POST_BUILD", "SUCCEEDED"),
                ("UPLOAD_ARTIFACTS", "SUCCEEDED"),
                ("FINALIZING", "SUCCEEDED"),
            ],
        ),
        (
            "pre_build fails with an explicit non-zero exit",
            continuing("pre_build", "exit 7"),
            vec![
                ("INSTALL", "SUCCEEDED"),
                ("PRE_BUILD", "FAILED"),
                ("BUILD", "SUCCEEDED"),
                ("POST_BUILD", "SUCCEEDED"),
                ("UPLOAD_ARTIFACTS", "SUCCEEDED"),
                ("FINALIZING", "SUCCEEDED"),
            ],
        ),
        (
            "install fails with an explicit non-zero exit",
            continuing("install", "exit 1"),
            vec![
                ("INSTALL", "FAILED"),
                ("PRE_BUILD", "SUCCEEDED"),
                ("BUILD", "SUCCEEDED"),
                ("POST_BUILD", "SUCCEEDED"),
                ("UPLOAD_ARTIFACTS", "SUCCEEDED"),
                ("FINALIZING", "SUCCEEDED"),
            ],
        ),
        (
            "build fails with an explicit non-zero exit",
            continuing("build", "exit 2"),
            vec![
                ("INSTALL", "SUCCEEDED"),
                ("PRE_BUILD", "SUCCEEDED"),
                ("BUILD", "FAILED"),
                ("POST_BUILD", "SUCCEEDED"),
                ("UPLOAD_ARTIFACTS", "SUCCEEDED"),
                ("FINALIZING", "SUCCEEDED"),
            ],
        ),
    ];

    for (index, (case, spec, expected)) in rows.iter().enumerate() {
        let project = format!("continue-form-proj-{index}");
        create_project(&router, &project, source_with(spec)).await;
        let build = settled_build(&router, &project, json!({})).await;
        let pairs = phase_pairs(&build);

        assert_eq!(
            build["buildStatus"],
            json!("FAILED"),
            "case {case:?}: a phase that failed fails the build, got {build}"
        );
        for (phase, status) in expected {
            let reported = pairs
                .iter()
                .find(|(reported, _)| reported == phase)
                .and_then(|(_, status)| status.clone());
            assert_eq!(
                reported,
                Some((*status).to_string()),
                "case {case:?}: expected {phase} to be reported {status}, got {pairs:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_failure_the_phase_continues_past_still_reports_the_upload() {
    let router = emulator();

    // The control for the rule above: with `on-failure: CONTINUE` the build DID attempt the
    // phases after the failure, so each of them is reported — the upload among them — while
    // the build still ends FAILED.
    let spec = "version: 0.2\nphases:\n  pre_build:\n    on-failure: CONTINUE\n    \
                commands:\n      - false\n  build:\n    commands:\n      - echo ran\n  \
                post_build:\n    commands:\n      - echo also ran\n";
    create_project(&router, "continue-phases-proj", source_with(spec)).await;
    let build = settled_build(&router, "continue-phases-proj", json!({})).await;
    let pairs = phase_pairs(&build);

    assert_eq!(
        build["buildStatus"],
        json!("FAILED"),
        "expected the build to still be FAILED, got {build}"
    );
    for (phase, expected) in [
        ("PRE_BUILD", "FAILED"),
        ("BUILD", "SUCCEEDED"),
        ("POST_BUILD", "SUCCEEDED"),
        ("UPLOAD_ARTIFACTS", "SUCCEEDED"),
        ("FINALIZING", "SUCCEEDED"),
    ] {
        let reported = pairs.iter().find(|(reported, _)| reported == phase);
        assert_eq!(
            reported.and_then(|(_, status)| status.clone()),
            Some(expected.to_string()),
            "expected {phase} to be reported {expected}, got {pairs:?}"
        );
    }
}
