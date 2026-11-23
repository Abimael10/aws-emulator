// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the TIMELINE `StopBuild` leaves behind, driven over the wire the
//! router serves: `POST /` with `X-Amz-Target: CodeBuild_20161006.StopBuild` on
//! awsJson1_1.
//!
//! The rule under test: a stop ends the build WHERE IT STOOD. Exactly one phase takes
//! `phaseStatus` `STOPPED` — the one that was in flight when the call arrived — the phases
//! that had already finished keep the status, window and duration they reported, the phases
//! the build never reached are not reported at all (it never ran them), and the terminal
//! `COMPLETED` marker is appended. `BuildPhase$phaseStatus` takes its values from
//! `StatusType` (SUCCEEDED, FAILED, FAULT, TIMED_OUT, IN_PROGRESS, STOPPED) and
//! `phaseType` from `BuildPhaseType` (SUBMITTED, QUEUED, PROVISIONING, DOWNLOAD_SOURCE,
//! INSTALL, PRE_BUILD, BUILD, POST_BUILD, UPLOAD_ARTIFACTS, FINALIZING, COMPLETED) —
//! `smithy_query(query="shape",shape="StatusType,BuildPhaseType")`.
//!
//! The builds below are SEEDED with the phases a running build was reporting, so which
//! phase the stop lands on is read off stored state rather than raced for: this suite pins
//! the rule over a timeline it states outright, including the two phases a build cannot be
//! driven to on demand (the queue it waits in, and PROVISIONING). The same rule driven over
//! real builds — each command phase, with a twin control that runs the phase to completion —
//! is [`super::stop_build_phases`]'s, and the wire path that stops a queued build is
//! [`super::project_build_lifecycle`]'s; the stop transition itself lives in
//! `state::StoredBuild::stop`.
//!
//! Each suite in `tests/all/` drives the wire itself, so the harness below is this suite's
//! own.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_codebuild_codebuild_20161006::ops::types::{Build, BuildPhase};
use service_com_amazonaws_codebuild_codebuild_20161006::state::{
    self, ComAmazonawsCodebuildCodebuild20161006State as CodeBuildState, StoredBuild,
};
use service_com_amazonaws_codebuild_codebuild_20161006::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region every request below is signed for.
const REGION: &str = "us-west-2";

/// The access key id every request below is signed with.
const ACCESS_KEY_ID: &str = "AKIDCODEBUILDSTOPPHASES";

/// One router carrying only this service, over one shared engine.
fn emulator_on(engine: Arc<engine::InMemoryEngine>) -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(engine));
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

/// A phase that finished, as a running build reports it.
fn finished(phase_type: &str, start_millis: i64, end_millis: i64) -> BuildPhase {
    BuildPhase {
        phase_type: Some(phase_type.to_string()),
        phase_status: Some("SUCCEEDED".to_string()),
        start_time: Some(state::datetime(start_millis)),
        end_time: Some(state::datetime(end_millis)),
        duration_in_seconds: Some((end_millis - start_millis) / 1000),
        contexts: None,
    }
}

/// The phase a running build is IN: it has started and carries no outcome yet.
fn in_flight(phase_type: &str, start_millis: i64) -> BuildPhase {
    BuildPhase {
        phase_type: Some(phase_type.to_string()),
        phase_status: None,
        start_time: Some(state::datetime(start_millis)),
        end_time: None,
        duration_in_seconds: None,
        contexts: None,
    }
}

/// Seed a running build of `project` reporting `phases`, and hand back its id.
fn seed_running_build(
    engine: &engine::InMemoryEngine,
    project: &str,
    id: &str,
    start_millis: i64,
    phases: Vec<BuildPhase>,
) -> String {
    let current_phase = phases
        .last()
        .and_then(|phase| phase.phase_type.clone())
        .unwrap_or_default();
    engine.state::<CodeBuildState>().lock().insert_build(
        id.to_string(),
        StoredBuild {
            build: Build {
                id: Some(id.to_string()),
                arn: Some(state::build_arn(REGION, id)),
                project_name: Some(project.to_string()),
                start_time: Some(state::datetime(start_millis)),
                build_status: Some("IN_PROGRESS".to_string()),
                current_phase: Some(current_phase),
                build_complete: false,
                phases: Some(phases),
                ..Build::default()
            },
            start_millis,
            settle_due_millis: start_millis,
            artifacts: None,
            secondary_artifacts: Vec::new(),
            run_claimed: true,
            on_fleet_machine: false,
            executing: None,
        },
    );
    id.to_string()
}

/// The `(phaseType, phaseStatus)` pairs a build reports, in order.
fn reported_phases(build: &Value) -> Vec<(String, Option<String>)> {
    build["phases"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|phase| {
            (
                phase["phaseType"].as_str().unwrap_or_default().to_string(),
                phase["phaseStatus"].as_str().map(str::to_string),
            )
        })
        .collect()
}

/// RULE: the stop lands on the phase that was in flight and leaves every phase before it
/// as it stood.
///
/// The rows differ along HOW FAR the build had got when the stop arrived — still waiting
/// in the queue, provisioning, part way through its command phases, and in the last of
/// them — because that is what the rule generalises over.
#[tokio::test]
async fn a_stop_lands_on_the_phase_that_was_in_flight() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_on(Arc::clone(&engine));
    let started = state::now_millis() - 10_000;

    // (case, the phases the running build reports, the phases already finished, the phase
    // the stop must land on)
    let cases: Vec<(&str, Vec<BuildPhase>, Vec<&str>, &str)> = vec![
        (
            "a build still waiting in the queue",
            vec![
                finished("SUBMITTED", started, started),
                in_flight("QUEUED", started),
            ],
            vec!["SUBMITTED"],
            "QUEUED",
        ),
        (
            "a build whose environment is being provisioned",
            vec![
                finished("SUBMITTED", started, started),
                finished("QUEUED", started, started + 1_000),
                in_flight("PROVISIONING", started + 1_000),
            ],
            vec!["SUBMITTED", "QUEUED"],
            "PROVISIONING",
        ),
        (
            "a build part way through its command phases",
            vec![
                finished("SUBMITTED", started, started),
                finished("QUEUED", started, started + 1_000),
                finished("PROVISIONING", started + 1_000, started + 2_000),
                finished("DOWNLOAD_SOURCE", started + 2_000, started + 3_000),
                finished("INSTALL", started + 3_000, started + 4_000),
                in_flight("PRE_BUILD", started + 4_000),
            ],
            vec![
                "SUBMITTED",
                "QUEUED",
                "PROVISIONING",
                "DOWNLOAD_SOURCE",
                "INSTALL",
            ],
            "PRE_BUILD",
        ),
        (
            "a build in the last of its command phases",
            vec![
                finished("SUBMITTED", started, started),
                finished("QUEUED", started, started + 1_000),
                finished("PROVISIONING", started + 1_000, started + 2_000),
                finished("DOWNLOAD_SOURCE", started + 2_000, started + 3_000),
                finished("INSTALL", started + 3_000, started + 4_000),
                finished("PRE_BUILD", started + 4_000, started + 5_000),
                finished("BUILD", started + 5_000, started + 6_000),
                in_flight("POST_BUILD", started + 6_000),
            ],
            vec![
                "SUBMITTED",
                "QUEUED",
                "PROVISIONING",
                "DOWNLOAD_SOURCE",
                "INSTALL",
                "PRE_BUILD",
                "BUILD",
            ],
            "POST_BUILD",
        ),
    ];

    for (index, (case, phases, already_finished, in_flight_phase)) in
        cases.into_iter().enumerate()
    {
        let project = format!("stop-timeline-{index}");
        let id = seed_running_build(
            &engine,
            &project,
            &format!("{project}:0000000{index}-1111-1111-1111-111111111111"),
            started,
            phases,
        );
        // What the running build was reporting, read over the same wire, so the comparison
        // below is of two answers of this service rather than of a hand-built record.
        let (_, before) = call(&router, "BatchGetBuilds", json!({"ids": [id.clone()]})).await;
        let before_phases = before["builds"][0]["phases"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let (status, stopped) = call(&router, "StopBuild", json!({"id": id.clone()})).await;
        assert_eq!(
            status, 200,
            "case {case:?}: StopBuild expected 200, got {status} with {stopped}"
        );
        let build = stopped["build"].clone();
        assert_eq!(
            build["buildStatus"],
            json!("STOPPED"),
            "case {case:?}: a stopped build reports STOPPED, got {build}"
        );
        assert_eq!(
            build["currentPhase"],
            json!("COMPLETED"),
            "case {case:?}: a stopped build has ended, got {build}"
        );
        assert!(
            build["buildComplete"] == json!(true) && build["endTime"].is_number(),
            "case {case:?}: a stopped build is complete and reports when, got {build}"
        );

        let reported = reported_phases(&build);
        let stopped_types: Vec<&str> = reported
            .iter()
            .filter(|(_, status)| status.as_deref() == Some("STOPPED"))
            .map(|(phase_type, _)| phase_type.as_str())
            .collect();
        assert_eq!(
            stopped_types,
            vec![in_flight_phase],
            "case {case:?}: exactly the phase in flight must carry STOPPED, got {reported:?}"
        );

        // The phases that had already finished are reported exactly as they were.
        let kept: Vec<Value> = build["phases"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .take(already_finished.len())
            .collect();
        assert_eq!(
            kept,
            before_phases[..already_finished.len()].to_vec(),
            "case {case:?}: the phases already run must be reported as they stood, got \
             {reported:?}"
        );
        assert!(
            kept.iter().all(|phase| phase["phaseStatus"] == json!("SUCCEEDED")),
            "case {case:?}: a phase that ran keeps its own outcome, got {kept:?}"
        );

        // Nothing the build never reached is reported, and the terminal marker is last.
        assert_eq!(
            reported.len(),
            already_finished.len() + 2,
            "case {case:?}: only the phases reached, the stopped one and COMPLETED are \
             reported, got {reported:?}"
        );
        assert_eq!(
            reported.last().map(|(phase_type, status)| (
                phase_type.as_str(),
                status.clone()
            )),
            Some(("COMPLETED", None)),
            "case {case:?}: the COMPLETED marker comes last and carries no outcome, got \
             {reported:?}"
        );

        // The stopped phase closed where the build ended, so it has a window of its own.
        let stop = build["phases"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .find(|phase| phase["phaseStatus"] == json!("STOPPED"))
            .unwrap_or(Value::Null);
        assert!(
            stop["endTime"].is_number() && stop["durationInSeconds"].is_number(),
            "case {case:?}: the stopped phase is closed out, got {stop}"
        );
        assert!(
            stop["endTime"].as_f64() <= build["endTime"].as_f64(),
            "case {case:?}: the stopped phase cannot end after the build did, got {build}"
        );

        // A later read reports the same record: the stop was written, not fabricated.
        let (_, reread) = call(&router, "BatchGetBuilds", json!({"ids": [id]})).await;
        assert_eq!(
            reread["builds"][0], build,
            "case {case:?}: a stopped build must read back as it was recorded, got {reread}"
        );
    }
}
