// SPDX-License-Identifier: CC0-1.0

//! WHICH phases a build reports once a phase failure has ABORTED it.
//!
//! RULE: `on-failure` decides what a failing phase does to the build, and its default is
//! `ABORT`: "the build stops" at the phase that failed, so every phase that would have come
//! after it — the later buildspec phases AND the upload of the build's artifacts — is work
//! the build never attempted, and `BatchGetBuilds` reports none of them. Not reported as
//! failed, not reported as succeeded: absent. The build's own finishing work still ran, so
//! `FINALIZING` and the terminal `COMPLETED` marker are reported, the build is
//! `buildComplete` and its `buildStatus` is `FAILED`. `on-failure: CONTINUE` is the other
//! arm: the phases after the failure DID run, so each of them is reported with its own
//! outcome and the upload among them.
//!
//! The rows span what the rule generalises over: WHICH phase aborts the build (each of the
//! four a buildspec declares commands for), and whether the project names an artifacts
//! DESTINATION at all — the upload phase is absent either way, since the abort is what
//! decides it rather than there being nothing to upload.
//!
//! Every call goes over the wire through the harness `project_build_lifecycle.rs` keeps.

use serde_json::{json, Value};

use super::project_build_lifecycle::{create_project, emulator, merged, phase_pairs, settled_build};

/// The phases a build reports ONLY when it got past its buildspec's phases: the later
/// phases of the buildspec, and the upload of what they produced.
const PHASES_AFTER_A_FAILURE: [&str; 4] = ["PRE_BUILD", "BUILD", "POST_BUILD", "UPLOAD_ARTIFACTS"];

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
async fn an_aborted_build_reports_none_of_the_phases_it_never_attempted() {
    let router = emulator();

    // (case, the phase whose command fails, the whole buildspec, the artifacts destination)
    let destination = json!({"artifacts": {"type": "S3", "location": "artifact-destination"}});
    let rows: Vec<(&str, &str, String, Value)> = vec![
        (
            "the install phase aborts the build",
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
            "the pre_build phase aborts the build",
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
            "the pre_build phase aborts a build whose artifacts name a destination",
            "PRE_BUILD",
            buildspec(&[("pre_build", "false"), ("build", "echo never")]),
            destination.clone(),
        ),
        (
            "the build phase aborts the build",
            "BUILD",
            buildspec(&[("build", "exit 7"), ("post_build", "echo never")]),
            json!({}),
        ),
        (
            "the post_build phase aborts the build",
            "POST_BUILD",
            buildspec(&[("build", "echo building"), ("post_build", "false")]),
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
            "case {case:?}: expected the aborted build to be FAILED, got {build}"
        );
        assert_eq!(
            build["buildComplete"],
            json!(true),
            "case {case:?}: an aborted build is complete, got {build}"
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
        let after = PHASES_AFTER_A_FAILURE
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

/// RULE: whether the build runs the phase AFTER one that failed is decided by that phase's
/// own `on-failure` and by nothing else — `CONTINUE` runs the later phases, each reported
/// with its own outcome and the upload among them, however the failing command came to fail
/// — while the failed phase keeps its `FAILED` status and the build is `FAILED` either way.
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
