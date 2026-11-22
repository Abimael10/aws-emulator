// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for WHICH phases a build the caller STOPPED reports, driven over the
//! wire the router serves (`POST /` with `X-Amz-Target: CodeBuild_20161006.<Operation>` on
//! awsJson1_1) through the harness [`super::project_build_lifecycle`] owns.
//!
//! RULE under test: `StopBuild` ends a build WHERE IT STANDS. The phase that was in flight
//! when the stop arrived is the one phase carrying `phaseStatus` `STOPPED`, closed with an
//! `endTime` and the span it really ran for; the phases the build had already finished keep
//! the outcomes they reached; the phases it had not reached are work it never attempted, so
//! none of them is reported; and the build itself settles `buildStatus` `STOPPED`,
//! `currentPhase` `COMPLETED`, with an `endTime`.
//!
//! The rows span what the rule generalises over: WHICH phase the build is in when the stop
//! arrives — each of the phases a buildspec declares commands for, and the queue a build
//! sits in before it is ever placed on a machine. Each row's control is a TWIN build of the
//! same project that runs the same phases to completion, which is what says both which
//! phases a run of that buildspec reports at all and how long the interrupted phase would
//! have taken.
//!
//! Nothing here waits on the wall clock: every wait ends on the CONDITION it is waiting for,
//! and the row that needs a build no machine has taken yet moves the stored instant that
//! build may first be placed on a machine at — the service stores it and never sleeps for
//! it.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use service_com_amazonaws_codebuild_codebuild_20161006::state::ComAmazonawsCodebuildCodebuild20161006State as CodeBuildState;
use smithy_server_runtime::MultiServiceRouter;

use super::project_build_lifecycle::{
    call, create_project, emulator, emulator_on, phase_of, phase_pairs, start_build,
};

/// How long a wait below gives the condition it waits for. Generous on purpose: the wait
/// ends on the condition, and the deadline is only there so a condition that never holds
/// fails the test instead of hanging it.
const CONDITION_DEADLINE: Duration = Duration::from_secs(90);

/// How long a wait below leaves before looking again.
const LOOK_AGAIN_AFTER: Duration = Duration::from_millis(25);

/// The phases a buildspec declares commands for, as the `BuildPhaseType` each is reported by
/// paired with the buildspec key that declares it — the buildspec reference's `phases`
/// sequence, in the order CodeBuild runs them.
const COMMAND_PHASES: [(&str, &str); 4] = [
    ("INSTALL", "install"),
    ("PRE_BUILD", "pre_build"),
    ("BUILD", "build"),
    ("POST_BUILD", "post_build"),
];

/// How long the phase the stop interrupts states it takes. Its commands are performed in
/// process and spend none of the host's clock, so this is the time the phase STATES it took;
/// how soon the build's outcome may be READ is a shorter, compressed span, which is what
/// makes the phase a running build sits in observable at all.
const SUBJECT_SECONDS: u32 = 300;

/// How long the SAME phase of the control build states it takes — shorter, so the control
/// runs that phase to completion while the subject is still inside its own.
const CONTROL_SECONDS: u32 = 60;

/// How far out a row moves the instant a build may first be placed on a machine, to hold it
/// in the queue: far enough that nothing admits it while the row runs, and well inside the
/// wait the build is given in the queue, so the build is still waiting rather than ended.
const QUEUE_HOLD_MILLIS: i64 = 60_000;

/// A buildspec declaring every command phase, `holding`'s own commands stating they take
/// `seconds` and every other phase echoing.
fn buildspec(holding: &str, seconds: u32) -> String {
    let mut spec = String::from("version: 0.2\nphases:\n");
    for (phase_type, key) in COMMAND_PHASES {
        let command = if phase_type == holding {
            format!("sleep {seconds}")
        } else {
            "echo ran".to_string()
        };
        spec.push_str(&format!("  {key}:\n    commands:\n      - {command}\n"));
    }
    spec
}

/// `CreateProject` members whose source carries `spec` inline, with no input source code of
/// its own — the form that fixes exactly what a build runs.
fn source_with(spec: &str) -> Value {
    json!({"source": {"type": "NO_SOURCE", "buildspec": spec}})
}

/// Read one build back through `BatchGetBuilds`, asserting it is reported.
async fn read_build(router: &MultiServiceRouter, id: &str) -> Value {
    let (status, body) = call(router, "BatchGetBuilds", json!({"ids": [id]})).await;
    assert_eq!(
        status, 200,
        "BatchGetBuilds {id:?}: expected 200, got {status} with {body}"
    );
    assert!(
        body["builds"][0].is_object(),
        "BatchGetBuilds {id:?}: expected the build back, got {body}"
    );
    body["builds"][0].clone()
}

/// Start a build of `project`, with `extra`'s members merged into the request, and hand back
/// its id.
async fn started_build_id(router: &MultiServiceRouter, project: &str, extra: Value) -> String {
    let mut body = json!({"projectName": project});
    if let (Value::Object(target), Value::Object(members)) = (&mut body, extra) {
        target.extend(members);
    }
    let build = start_build(router, body).await;
    build["id"].as_str().expect("a build id").to_string()
}

/// Read `id` back until `holds` accepts the record, and hand that record back. `case` and
/// `want` only name the condition in the failure message.
async fn poll_build_until(
    router: &MultiServiceRouter,
    id: &str,
    case: &str,
    want: &str,
    holds: impl Fn(&Value) -> bool,
) -> Value {
    let deadline = tokio::time::Instant::now() + CONDITION_DEADLINE;
    loop {
        let record = read_build(router, id).await;
        if holds(&record) {
            return record;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: expected {want}, last read {record}"
        );
        tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    }
}

/// Stop `id`, asserting the call succeeded, and hand back the build it answered with.
async fn stop_build(router: &MultiServiceRouter, case: &str, id: &str) -> Value {
    let (status, body) = call(router, "StopBuild", json!({"id": id})).await;
    assert_eq!(
        status, 200,
        "case {case:?}: StopBuild {id}: expected 200, got {status} with {body}"
    );
    body["build"].clone()
}

/// The `durationInSeconds` a phase reports.
fn duration_of(phase: &Value) -> Option<i64> {
    phase["durationInSeconds"].as_i64()
}

/// RULE: the stop lands on the phase that was IN FLIGHT — that phase, and only that phase,
/// carries `STOPPED` — the phases already finished keep their outcomes, the phases after it
/// are not reported at all, and the interrupted phase reports a shorter duration than the
/// same phase of a twin build left to run it to completion.
#[tokio::test]
async fn a_stopped_build_stops_exactly_the_phase_that_was_in_flight() {
    for (index, (phase, _)) in COMMAND_PHASES.iter().enumerate() {
        let case = format!("the stop arrives while the build is in {phase}");
        // A fresh emulator per row: a row's state must not depend on the one before.
        let router = emulator();
        let project = format!("stop-phase-proj-{index}");
        // The project's own buildspec is the CONTROL's: the same phases, with the one this
        // row stops the subject in short enough to run to completion.
        create_project(
            &router,
            &project,
            source_with(&buildspec(phase, CONTROL_SECONDS)),
        )
        .await;

        let control_id = started_build_id(&router, &project, json!({})).await;
        let control = poll_build_until(&router, &control_id, &case, "the control to finish", |build| {
            build["buildComplete"] == json!(true)
        })
        .await;
        let control_pairs = phase_pairs(&control);
        assert_eq!(
            control["buildStatus"],
            json!("SUCCEEDED"),
            "case {case:?}: the control must run its phases to completion, got {control}"
        );
        assert!(
            control_pairs
                .iter()
                .all(|(_, status)| status.as_deref() != Some("STOPPED")),
            "case {case:?}: the control was never stopped, got {control_pairs:?}"
        );

        // The subject: the same phases, but the one this row stops it in states it takes
        // long enough that the stop falls inside it.
        let subject_id = started_build_id(
            &router,
            &project,
            json!({"buildspecOverride": buildspec(phase, SUBJECT_SECONDS)}),
        )
        .await;
        let in_flight = poll_build_until(
            &router,
            &subject_id,
            &case,
            &format!("the subject to report currentPhase {phase}"),
            |build| build["currentPhase"] == json!(phase),
        )
        .await;
        assert_eq!(
            in_flight["buildComplete"],
            json!(false),
            "case {case:?}: the subject must still be running when it is stopped, got \
             {in_flight}"
        );

        let answered = stop_build(&router, &case, &subject_id).await;
        // What the stop answered is what every later read reports: the record is the store's.
        let settled = read_build(&router, &subject_id).await;
        assert_eq!(
            phase_pairs(&answered),
            phase_pairs(&settled),
            "case {case:?}: StopBuild's answer and the later read must report the same \
             phases, got {answered} then {settled}"
        );

        // The phases the subject reports: the control's own sequence, cut at the phase that
        // was in flight — which carries STOPPED — and then the terminal marker.
        let cut = control_pairs
            .iter()
            .position(|(reported, _)| reported == phase)
            .unwrap_or_else(|| {
                panic!("case {case:?}: the control must report {phase}, got {control_pairs:?}")
            });
        let mut expected: Vec<(String, Option<String>)> = control_pairs[..cut].to_vec();
        expected.push(((*phase).to_string(), Some("STOPPED".to_string())));
        expected.push(("COMPLETED".to_string(), None));
        assert_eq!(
            phase_pairs(&settled),
            expected,
            "case {case:?}: expected the phases already run to keep their outcomes, {phase} \
             to carry STOPPED and nothing after it to be reported, got {settled}"
        );

        // And the build itself: stopped, complete, with an endTime.
        assert_eq!(
            settled["buildStatus"],
            json!("STOPPED"),
            "case {case:?}: expected buildStatus STOPPED, got {settled}"
        );
        assert_eq!(
            settled["buildComplete"],
            json!(true),
            "case {case:?}: a stopped build is complete, got {settled}"
        );
        assert_eq!(
            settled["currentPhase"],
            json!("COMPLETED"),
            "case {case:?}: expected currentPhase COMPLETED, got {settled}"
        );
        assert!(
            settled["endTime"].is_number(),
            "case {case:?}: a stopped build reports an endTime, got {settled}"
        );

        // The interrupted phase ran for less than the control's same phase did, because the
        // stop cut it short.
        let stopped_phase = phase_of(&settled, phase)
            .unwrap_or_else(|| panic!("case {case:?}: expected a {phase} phase in {settled}"));
        let control_phase = phase_of(&control, phase)
            .unwrap_or_else(|| panic!("case {case:?}: expected a {phase} phase in {control}"));
        assert!(
            stopped_phase["endTime"].is_number(),
            "case {case:?}: the stopped phase is closed with an endTime, got {stopped_phase}"
        );
        let stopped_duration = duration_of(&stopped_phase);
        let control_duration = duration_of(&control_phase);
        assert!(
            stopped_duration.is_some_and(|stopped| control_duration
                .is_some_and(|complete| stopped < complete)),
            "case {case:?}: expected the stopped {phase} to report a shorter duration than \
             the control's {control_duration:?}, got {stopped_duration:?}"
        );
    }
}

/// RULE: the same rule for a build no machine has taken yet — the phase in flight is the
/// `QUEUED` phase it is waiting in, so that is the one carrying `STOPPED`, and the phases a
/// build only reaches on a machine are never reported.
#[tokio::test]
async fn a_build_stopped_before_it_was_placed_on_a_machine_stops_its_queued_phase() {
    let case = "the stop arrives while the build is still waiting for a machine";
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_on(Arc::clone(&engine));
    create_project(&router, "stop-queued-proj", json!({})).await;

    let id = started_build_id(&router, "stop-queued-proj", json!({})).await;
    // Hold the build in the queue: the service stores the instant a build may first be
    // placed on a machine and never sleeps for it, so moving that instant out is the same
    // thing as a build nothing has taken yet — and it makes this row's state the build's own
    // rather than a race against the span every build spends in the queue.
    {
        let handle = engine.state::<CodeBuildState>();
        let mut guard = handle.lock();
        let stored = guard
            .builds
            .get_mut(&id)
            .unwrap_or_else(|| panic!("the store must hold build {id}"));
        stored.settle_due_millis += QUEUE_HOLD_MILLIS;
    }

    let waiting = read_build(&router, &id).await;
    assert_eq!(
        waiting["currentPhase"],
        json!("QUEUED"),
        "case {case:?}: the build must still be in its QUEUED phase, got {waiting}"
    );
    assert_eq!(
        waiting["buildComplete"],
        json!(false),
        "case {case:?}: the build must still be running, got {waiting}"
    );

    let answered = stop_build(&router, case, &id).await;
    let settled = read_build(&router, &id).await;
    let expected: Vec<(String, Option<String>)> = vec![
        ("SUBMITTED".to_string(), Some("SUCCEEDED".to_string())),
        ("QUEUED".to_string(), Some("STOPPED".to_string())),
        ("COMPLETED".to_string(), None),
    ];
    assert_eq!(
        phase_pairs(&answered),
        expected,
        "case {case:?}: expected the QUEUED phase to carry the stop, got {answered}"
    );
    assert_eq!(
        phase_pairs(&settled),
        expected,
        "case {case:?}: every later read reports the same phases, got {settled}"
    );
    assert_eq!(
        settled["buildStatus"],
        json!("STOPPED"),
        "case {case:?}: expected buildStatus STOPPED, got {settled}"
    );
    assert!(
        settled["endTime"].is_number(),
        "case {case:?}: a stopped build reports an endTime, got {settled}"
    );
    // The build never got a machine, so nothing a machine does for it is reported.
    for never_reached in ["PROVISIONING", "DOWNLOAD_SOURCE", "INSTALL"] {
        assert!(
            phase_of(&settled, never_reached).is_none(),
            "case {case:?}: {never_reached} was never attempted, so it must not be reported, \
             got {settled}"
        );
    }
}
