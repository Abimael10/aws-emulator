// SPDX-License-Identifier: CC0-1.0

//! The CLOCK a CodeBuild build reports: how long each of its phases took, and what happens
//! to a build still running when its execution timeout falls due.
//!
//! The rule under test: **a build's phase table is a partition of the build's own window.
//! Every phase that reports both a `startTime` and an `endTime` reports
//! `durationInSeconds` as the span between them ("how long, in seconds, between the starting
//! and ending times of the build's phase"), and that span is the time THAT phase's own
//! commands took — so a phase whose commands took N seconds reports at least N and a phase
//! whose commands took no time reports 0. The windows are in order and do not overlap,
//! every one of them lies inside the build's `startTime`..`endTime`, their durations sum to
//! no more than the build's own, and `COMPLETED` is the last phase and the only one with no
//! duration. The same clock decides the EXECUTION timeout: `timeoutInMinutes` (with
//! `StartBuild`'s `timeoutInMinutesOverride` applied) is "how long, in minutes, from 5 to
//! 2160 (36 hours), for CodeBuild to wait before timing out this build if it does not get
//! marked as completed", counted from the instant the build began executing, and a build
//! still running when it falls due is ended there: the phase that was running carries
//! `phaseStatus` `TIMED_OUT` with a context explaining it, the phases after it were never
//! attempted and are not reported, and the build is `FAILED` and `buildComplete`.**
//!
//! Two crates have to agree for the whole of it to hold, which is why the suite lives here:
//! CodeBuild runs the builds and reports their windows, and the `Duration` series it
//! publishes about each build that ended is answered by CloudWatch — so the window a client
//! reads off the build record and the one it reads off the metric are checked against each
//! other over the assembled router.
//!
//! Every phase, status and metric name below is written out as a literal taken from the
//! CodeBuild API model and metric reference, rather than from the service crate's constants,
//! so a build that agrees only with itself fails here.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The region every request here is signed for.
const REGION: &str = "eu-central-1";

/// CodeBuild's `X-Amz-Target` prefix and signing name.
const CODEBUILD_TARGET_PREFIX: &str = "CodeBuild_20161006";
/// See [`CODEBUILD_TARGET_PREFIX`].
const CODEBUILD_SIGNING_NAME: &str = "codebuild";

/// CloudWatch's `X-Amz-Target` prefix and signing name (`sigv4Name: monitoring`).
const CLOUDWATCH_TARGET_PREFIX: &str = "GraniteServiceVersion20100801";
/// See [`CLOUDWATCH_TARGET_PREFIX`].
const CLOUDWATCH_SIGNING_NAME: &str = "monitoring";

/// The `BuildPhaseType` values a build reports, in the order it reaches them
/// (`smithy_query(query="shape",shape="BuildPhaseType")`).
const PHASE_SEQUENCE: [&str; 11] = [
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

/// The four phases a buildspec declares commands for, which are therefore the only phases
/// whose commands can take any time (the buildspec reference's `phases` sequence).
const COMMAND_PHASES: [&str; 4] = ["INSTALL", "PRE_BUILD", "BUILD", "POST_BUILD"];

/// The terminal marker phase, which has started and not ended.
const COMPLETED_PHASE: &str = "COMPLETED";

/// The `StatusType` values under test (`smithy_query(query="shape",shape="StatusType")`:
/// SUCCEEDED, FAILED, FAULT, TIMED_OUT, IN_PROGRESS, STOPPED).
const SUCCEEDED: &str = "SUCCEEDED";
/// See [`SUCCEEDED`].
const FAILED: &str = "FAILED";
/// See [`SUCCEEDED`]: documented on `BuildPhase$phaseStatus` as "the build phase timed out".
const TIMED_OUT: &str = "TIMED_OUT";

/// Seconds in one minute, which `timeoutInMinutes` is read in.
const SECONDS_PER_MINUTE: i64 = 60;

/// The build timeout, in minutes, a project that states none is given: AWS documents it on
/// `CreateProject`'s `timeoutInMinutes` — "The default is 60 minutes."
const DEFAULT_TIMEOUT_MINUTES: i64 = 60;

/// How far two instants this suite compares may differ and still be the same instant: the
/// phase durations are whole seconds, so a window and its reported duration agree to within
/// one second (the rounding mode is undocumented), and an ordering comparison allows only
/// floating-point noise.
const ONE_SECOND: f64 = 1.0;
/// See [`ONE_SECOND`].
const NOISE: f64 = 0.001;

/// How long a row waits for the transition the clock owes a build, and how long one poll
/// pauses before asking again.
///
/// Generous on purpose: what is waited for is the CONDITION (the build reporting itself
/// complete), never an elapsed time, and the deadline is only here so a build that never
/// completes fails the row instead of hanging it.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);
/// See [`SETTLE_DEADLINE`].
const POLL_PAUSE: Duration = Duration::from_millis(20);

/// The emulator with the two services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them, with request authentication off so each call carries
/// a credential scope (naming the Region and the service) without a computed signature.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_codebuild_codebuild_20161006::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_cloudwatch_graniteserviceversion20100801::EmulatedService::new(
            engine,
        ),
    );
    router
}

/// Dispatch one RPC call at `signing_name` and answer its status and decoded body.
async fn rpc(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    content_type: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", host)
        .header("content-type", content_type)
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTEST/20260101/{REGION}/{signing_name}/\
                 aws4_request, SignedHeaders=host;x-amz-target, Signature=not-verified"
            ),
        )
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the test request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&payload).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One CodeBuild call, over its awsJson1_1 wire, required to succeed.
async fn codebuild(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = rpc(
        router,
        CODEBUILD_SIGNING_NAME,
        CODEBUILD_TARGET_PREFIX,
        "application/x-amz-json-1.1",
        operation,
        body,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CodeBuild {operation}, got {status} with {parsed}"
    );
    parsed
}

/// One CloudWatch call, over its awsJson wire, required to succeed.
async fn cloudwatch(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = rpc(
        router,
        CLOUDWATCH_SIGNING_NAME,
        CLOUDWATCH_TARGET_PREFIX,
        "application/x-amz-json-1.0",
        operation,
        body,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CloudWatch {operation}, got {status} with {parsed}"
    );
    parsed
}

/// A buildspec declaring `commands` for each phase named, in buildspec order.
fn buildspec(phases: &[(&str, &str)]) -> String {
    let mut spec = String::from("version: 0.2\nphases:\n");
    for (phase, command) in phases {
        spec.push_str(&format!("  {phase}:\n    commands:\n      - {command}\n"));
    }
    spec
}

/// Create a `NO_SOURCE` / `NO_ARTIFACTS` project carrying `spec` inline, with `timeout`
/// minutes of execution timeout when one is given and the service's documented default
/// otherwise.
async fn create_project(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    spec: &str,
    timeout: Option<i64>,
) {
    let mut body = json!({
        "name": name,
        "source": {"type": "NO_SOURCE", "buildspec": spec},
        "artifacts": {"type": "NO_ARTIFACTS"},
        "cache": {"type": "NO_CACHE"},
        "environment": {
            "type": "LINUX_CONTAINER",
            "image": "aws/codebuild/standard:7.0",
            "computeType": "BUILD_GENERAL1_SMALL",
        },
        "serviceRole": "arn:aws:iam::000000000000:role/service-role/codebuild-clock",
    });
    if let (Value::Object(target), Some(minutes)) = (&mut body, timeout) {
        target.insert("timeoutInMinutes".to_string(), json!(minutes));
    }
    let created = codebuild(router, case, "CreateProject", &body).await;
    assert_eq!(
        created["project"]["name"],
        json!(name),
        "case {case:?}: CreateProject must answer the project it created, got {created}"
    );
}

/// Start one build of `project` with `extra`'s members merged into the `StartBuild` request,
/// look again until it reports itself complete, and answer the record `BatchGetBuilds`
/// reports for it.
async fn settled_build(
    router: &MultiServiceRouter,
    case: &str,
    project: &str,
    extra: Value,
) -> Value {
    let mut body = json!({"projectName": project});
    if let (Value::Object(target), Value::Object(members)) = (&mut body, extra) {
        target.extend(members);
    }
    let started = codebuild(router, case, "StartBuild", &body).await;
    let id = started["build"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: StartBuild must answer a build id, got {started}"))
        .to_string();
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let read = codebuild(router, case, "BatchGetBuilds", &json!({"ids": [id.clone()]})).await;
        let build = read["builds"][0].clone();
        if build["buildComplete"] == json!(true) {
            return build;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: build {id} never completed, last read {build}"
        );
        tokio::time::sleep(POLL_PAUSE).await;
    }
}

/// One phase of a build, as the members the rule is about.
#[derive(Debug, Clone)]
struct Phase {
    phase_type: String,
    status: Option<String>,
    start: Option<f64>,
    end: Option<f64>,
    duration: Option<i64>,
    contexts: Value,
}

/// The phases `build` reports, in the order it reached them.
fn phases_of(build: &Value) -> Vec<Phase> {
    build["phases"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|phase| Phase {
            phase_type: phase["phaseType"].as_str().unwrap_or_default().to_string(),
            status: phase["phaseStatus"].as_str().map(str::to_string),
            start: phase["startTime"].as_f64(),
            end: phase["endTime"].as_f64(),
            duration: phase["durationInSeconds"].as_i64(),
            contexts: phase["contexts"].clone(),
        })
        .collect()
}

/// The one phase of `phases` whose type is `phase_type`, when the build reports one.
fn phase_named<'a>(phases: &'a [Phase], phase_type: &str) -> Option<&'a Phase> {
    phases
        .iter()
        .find(|phase| phase.phase_type == phase_type)
}

/// The window `build` itself reports, as `(startTime, endTime)` in epoch seconds.
fn build_window(case: &str, build: &Value) -> (f64, f64) {
    let start = build["startTime"]
        .as_f64()
        .unwrap_or_else(|| panic!("case {case:?}: a build states when it started, got {build}"));
    let end = build["endTime"]
        .as_f64()
        .unwrap_or_else(|| panic!("case {case:?}: a complete build states when it ended, got {build}"));
    (start, end)
}

/// Every invariant the rule states about ONE build's phase table, whatever its buildspec did:
/// each ended phase's duration IS its own window, the windows are ordered and do not overlap,
/// they all lie inside the build's own window, their durations sum to no more than it, and the
/// terminal marker is last and the only phase without a duration.
fn assert_phase_table_partitions_the_build(case: &str, build: &Value) {
    let phases = phases_of(build);
    assert!(
        !phases.is_empty(),
        "case {case:?}: a complete build reports the phases it reached, got {build}"
    );
    let (build_start, build_end) = build_window(case, build);
    assert!(
        build_end + NOISE >= build_start,
        "case {case:?}: a build cannot end before it started, got {build_start}..{build_end}"
    );

    // The phases are reported in the order CodeBuild reaches them, and the terminal marker is
    // the last of them.
    let reported: Vec<&str> = phases
        .iter()
        .map(|phase| phase.phase_type.as_str())
        .collect();
    let expected_order: Vec<&str> = PHASE_SEQUENCE
        .iter()
        .copied()
        .filter(|phase| reported.contains(phase))
        .collect();
    assert_eq!(
        reported, expected_order,
        "case {case:?}: expected the phases in the order a build reaches them, got {reported:?}"
    );
    assert_eq!(
        reported.last().copied(),
        Some(COMPLETED_PHASE),
        "case {case:?}: {COMPLETED_PHASE} is the last phase a build reports, got {reported:?}"
    );

    let mut sum = 0_i64;
    let mut previous_end = build_start;
    for phase in &phases {
        let name = phase.phase_type.as_str();
        let start = phase.start.unwrap_or_else(|| {
            panic!("case {case:?}: phase {name} states when it started, got {phase:?}")
        });
        if name == COMPLETED_PHASE {
            // The terminal marker has started and has not ended, so it is the one phase with
            // no duration at all.
            assert!(
                phase.end.is_none() && phase.duration.is_none(),
                "case {case:?}: {COMPLETED_PHASE} has not ended, so it reports no duration, got \
                 {phase:?}"
            );
            assert!(
                start + NOISE >= previous_end && start <= build_end + NOISE,
                "case {case:?}: {COMPLETED_PHASE} begins where the build ended, got {start} \
                 against {previous_end}..{build_end}"
            );
            continue;
        }
        let end = phase.end.unwrap_or_else(|| {
            panic!("case {case:?}: phase {name} ended, so it states when, got {phase:?}")
        });
        let duration = phase.duration.unwrap_or_else(|| {
            panic!("case {case:?}: phase {name} ended, so it states how long it took, got {phase:?}")
        });
        // The documented definition: the duration IS the span between the two instants.
        let span = end - start;
        assert!(
            (span - duration as f64).abs() <= ONE_SECOND,
            "case {case:?}: phase {name} must report the span of its own window \
             ({span} s) as durationInSeconds, got {duration}"
        );
        assert!(
            duration >= 0,
            "case {case:?}: phase {name} cannot have taken negative time, got {duration}"
        );
        // Ordered, non-overlapping, and inside the build's own window.
        assert!(
            start + NOISE >= previous_end,
            "case {case:?}: phase {name} begins at {start}, before the phase before it ended \
             ({previous_end})"
        );
        assert!(
            end + NOISE >= start && end <= build_end + NOISE,
            "case {case:?}: phase {name}'s window {start}..{end} must lie inside the build's \
             {build_start}..{build_end}"
        );
        previous_end = end;
        sum += duration;
    }
    let build_seconds = (build_end - build_start) as i64;
    assert!(
        sum <= build_seconds,
        "case {case:?}: the phases took {sum} s in all, which cannot exceed the build's own \
         {build_seconds} s window"
    );
}

/// One row of the duration table: the case, the buildspec the build runs, and the minimum
/// `durationInSeconds` each named phase must report. Every command phase the row does not
/// name ran commands that took no time, so it must report 0.
type DurationRow = (&'static str, String, Vec<(&'static str, i64)>);

/// RULE: every phase of a build reports, as `durationInSeconds`, the span between its own
/// `startTime` and `endTime`, and that span is the time that phase's own commands took.
///
/// The rows differ along what the rule generalises over: WHICH phase takes the time (each of
/// the four a buildspec declares commands for), HOW MUCH time it takes, the FORM the command
/// states it in (plain seconds, a fractional operand, a minutes suffix), how MANY phases take
/// time at once, and the degenerate case of a build no command of which takes any time.
#[tokio::test]
async fn a_builds_phase_durations_are_the_windows_its_own_commands_took() {
    let router = emulator();
    let rows: Vec<DurationRow> = vec![
        (
            "the build phase takes twenty-four seconds",
            buildspec(&[
                ("install", "echo installing"),
                ("pre_build", "echo before"),
                ("build", "sleep 24"),
                ("post_build", "echo after"),
            ]),
            vec![("BUILD", 24)],
        ),
        (
            "the install phase is the slow one",
            buildspec(&[("install", "sleep 7"), ("build", "echo building")]),
            vec![("INSTALL", 7)],
        ),
        (
            "the post_build phase is the slow one",
            buildspec(&[("build", "echo building"), ("post_build", "sleep 3")]),
            vec![("POST_BUILD", 3)],
        ),
        (
            "a wait stated in minutes",
            buildspec(&[("build", "sleep 2m")]),
            vec![("BUILD", 2 * SECONDS_PER_MINUTE)],
        ),
        (
            "a fractional wait, which is still a whole second of duration",
            buildspec(&[("build", "sleep 1.5")]),
            vec![("BUILD", 1)],
        ),
        (
            "two phases each take their own time",
            buildspec(&[
                ("pre_build", "sleep 4"),
                ("build", "sleep 9"),
                ("post_build", "echo after"),
            ]),
            vec![("PRE_BUILD", 4), ("BUILD", 9)],
        ),
        (
            "no command of the build takes any time",
            buildspec(&[
                ("install", "echo installing"),
                ("build", "echo building"),
            ]),
            vec![],
        ),
    ];

    for (index, (case, spec, expected)) in rows.iter().enumerate() {
        let project = format!("phase-clock-{index}");
        create_project(&router, case, &project, spec, None).await;
        let build = settled_build(&router, case, &project, json!({})).await;
        assert_eq!(
            build["buildStatus"],
            json!(SUCCEEDED),
            "case {case:?}: a build whose commands all succeeded is {SUCCEEDED}, got {build}"
        );
        assert_phase_table_partitions_the_build(case, &build);

        let phases = phases_of(&build);
        for (phase_type, at_least) in expected {
            let phase = phase_named(&phases, phase_type).unwrap_or_else(|| {
                panic!("case {case:?}: expected a {phase_type} phase, got {build}")
            });
            assert!(
                phase.duration.is_some_and(|seconds| seconds >= *at_least),
                "case {case:?}: {phase_type} ran commands that took {at_least} s, so it must \
                 report at least that, got {phase:?}"
            );
        }
        // A phase whose commands took no time took no time: that is what makes the slow phase
        // of a build the one a client can identify.
        for phase_type in COMMAND_PHASES {
            if expected.iter().any(|(named, _)| *named == phase_type) {
                continue;
            }
            let phase = phase_named(&phases, phase_type).unwrap_or_else(|| {
                panic!("case {case:?}: expected a {phase_type} phase, got {build}")
            });
            assert_eq!(
                phase.duration,
                Some(0),
                "case {case:?}: {phase_type} ran no command that took time, so it took 0 s, got \
                 {phase:?}"
            );
            for (slow, _) in expected {
                let slower = phase_named(&phases, slow).and_then(|phase| phase.duration);
                assert!(
                    slower > phase.duration,
                    "case {case:?}: expected {slow} to have taken strictly longer than \
                     {phase_type}, got {slower:?} against {:?}",
                    phase.duration
                );
            }
        }
    }
}

/// What a row of the timeout table expects of the build it runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expected {
    /// The build ran past its execution timeout and was ended in this phase.
    TimedOutIn(&'static str),
    /// The build stayed inside its execution timeout and ran to success.
    RanToSuccess,
}

/// One row of the timeout table.
struct TimeoutRow {
    case: &'static str,
    /// The project's own `timeoutInMinutes` — `None` leaves it to the documented default.
    project_timeout: Option<i64>,
    /// `StartBuild`'s `timeoutInMinutesOverride`, when the row sends one.
    override_timeout: Option<i64>,
    /// The buildspec the build runs.
    spec: String,
    /// What the build must then report.
    expected: Expected,
}

/// RULE: a build still running when its execution timeout falls due is ended there — the
/// phase that was running carries `phaseStatus` `TIMED_OUT` with a context explaining it, the
/// phases after it were never attempted and are not reported, and the build is `FAILED` and
/// complete. The effective wait is the project's `timeoutInMinutes` with `StartBuild`'s
/// `timeoutInMinutesOverride` applied, or the documented default when neither states one.
///
/// The rows differ along what the rule generalises over: WHICH phase is running when the wait
/// runs out, WHERE the effective wait comes from (the project, an override that shortens it,
/// an override that lengthens it, the default), and how far past or short of the wait the
/// build's commands run — including the builds that stay inside it, which must not be touched.
#[tokio::test]
async fn a_build_that_outruns_its_timeout_is_ended_at_the_phase_that_was_running() {
    let router = emulator();
    let rows: Vec<TimeoutRow> = vec![
        TimeoutRow {
            case: "the build phase runs an hour under a five-minute timeout",
            project_timeout: Some(5),
            override_timeout: None,
            spec: buildspec(&[
                ("install", "echo installing"),
                ("build", "sleep 3600"),
                ("post_build", "echo never"),
            ]),
            expected: Expected::TimedOutIn("BUILD"),
        },
        TimeoutRow {
            case: "an earlier phase is the one running when the wait runs out",
            project_timeout: Some(5),
            override_timeout: None,
            spec: buildspec(&[("install", "sleep 1200"), ("build", "echo never")]),
            expected: Expected::TimedOutIn("INSTALL"),
        },
        TimeoutRow {
            case: "StartBuild's override shortens the wait the project states",
            project_timeout: Some(60),
            override_timeout: Some(5),
            spec: buildspec(&[("build", "sleep 3600")]),
            expected: Expected::TimedOutIn("BUILD"),
        },
        TimeoutRow {
            case: "StartBuild's override lengthens the wait the project states",
            project_timeout: Some(5),
            override_timeout: Some(60),
            spec: buildspec(&[("build", "sleep 600")]),
            expected: Expected::RanToSuccess,
        },
        TimeoutRow {
            case: "the default wait a project that states none is given",
            project_timeout: None,
            override_timeout: None,
            spec: buildspec(&[("build", "sleep 4000")]),
            expected: Expected::TimedOutIn("BUILD"),
        },
        TimeoutRow {
            case: "a build whose commands finish well inside the wait",
            project_timeout: Some(5),
            override_timeout: None,
            spec: buildspec(&[("build", "sleep 30"), ("post_build", "echo after")]),
            expected: Expected::RanToSuccess,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let project = format!("build-timeout-{index}");
        create_project(&router, case, &project, &row.spec, row.project_timeout).await;
        let extra = match row.override_timeout {
            Some(minutes) => json!({"timeoutInMinutesOverride": minutes}),
            None => json!({}),
        };
        let build = settled_build(&router, case, &project, extra).await;
        assert_phase_table_partitions_the_build(case, &build);
        let phases = phases_of(&build);
        let effective = row
            .override_timeout
            .or(row.project_timeout)
            .unwrap_or(DEFAULT_TIMEOUT_MINUTES);

        let Expected::TimedOutIn(timed_out) = row.expected else {
            assert_eq!(
                build["buildStatus"],
                json!(SUCCEEDED),
                "case {case:?}: a build inside its timeout runs to success, got {build}"
            );
            let touched: Vec<&Phase> = phases
                .iter()
                .filter(|phase| phase.status.as_deref() == Some(TIMED_OUT))
                .collect();
            assert!(
                touched.is_empty(),
                "case {case:?}: no phase of a build inside its timeout is {TIMED_OUT}, got \
                 {touched:?}"
            );
            continue;
        };

        assert_eq!(
            build["buildStatus"],
            json!(FAILED),
            "case {case:?}: a build its execution timeout ended is {FAILED} (the scalar \
             {TIMED_OUT} is the outcome of a build that never left the queue), got {build}"
        );
        assert_eq!(
            build["buildComplete"],
            json!(true),
            "case {case:?}: a build its timeout ended is complete, got {build}"
        );
        let phase = phase_named(&phases, timed_out).unwrap_or_else(|| {
            panic!("case {case:?}: expected a {timed_out} phase, got {build}")
        });
        assert_eq!(
            phase.status.as_deref(),
            Some(TIMED_OUT),
            "case {case:?}: {timed_out} was running when the wait ran out, so it is \
             {TIMED_OUT}, got {phase:?}"
        );
        let explained = phase.contexts.as_array().is_some_and(|contexts| {
            contexts.iter().any(|context| {
                context["message"]
                    .as_str()
                    .is_some_and(|message| !message.trim().is_empty())
            })
        });
        assert!(
            explained,
            "case {case:?}: the phase that timed out carries the explanation, got {phase:?}"
        );
        // The build was ended AT its deadline, so neither that phase nor the build itself
        // reports more time than the wait allowed.
        let allowed = effective * SECONDS_PER_MINUTE;
        assert!(
            phase.duration.is_some_and(|seconds| seconds <= allowed),
            "case {case:?}: the phase the timeout ended cannot have run past the {effective} \
             minute wait ({allowed} s), got {phase:?}"
        );
        let (start, end) = build_window(case, &build);
        let ran = (end - start) as i64;
        assert!(
            ran <= allowed + 1,
            "case {case:?}: a build ended by a {effective} minute wait cannot report a longer \
             window, got {ran} s"
        );
        // Every phase the build would have reached after the one that timed out is absent:
        // never attempted, so neither failed nor succeeded.
        let after = PHASE_SEQUENCE
            .iter()
            .skip_while(|phase| **phase != timed_out)
            .skip(1)
            .filter(|phase| **phase != "FINALIZING" && **phase != COMPLETED_PHASE);
        for phase_type in after {
            assert!(
                phase_named(&phases, phase_type).is_none(),
                "case {case:?}: {phase_type} was never attempted, so it must not be reported, \
                 got {build}"
            );
        }
        // The build's own finishing work still ran, and it still ends in the terminal marker.
        for phase_type in ["FINALIZING", COMPLETED_PHASE] {
            assert!(
                phase_named(&phases, phase_type).is_some(),
                "case {case:?}: expected {phase_type} to be reported, got {build}"
            );
        }
    }
}

/// How far back and forward of now the metric read's window reaches, in seconds: a generous
/// window around the datapoints a build publishes, inside the two hours ahead of now
/// CloudWatch documents for an ingested timestamp.
const WINDOW_BACK_SECS: f64 = 2_400.0;
/// See [`WINDOW_BACK_SECS`].
const WINDOW_FORWARD_SECS: f64 = 7_200.0;

/// The `Period`, in seconds, the metric read asks for.
const PERIOD_SECS: i64 = 300;

/// Epoch seconds, the unit a CloudWatch timestamp is read in.
fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the clock is after the Unix epoch")
        .as_secs_f64()
}

/// RULE (the same window, read where a client watches build health): the `Duration` series
/// CodeBuild publishes for a build that ended reports, in `Seconds`, the span the build's own
/// record reports — so the time a phase occupied is the time the metric accounts for, rather
/// than two unrelated numbers.
///
/// The rows differ along how long the build's commands take and in how many phases, which is
/// what the duration generalises over.
#[tokio::test]
async fn a_builds_duration_series_reports_the_window_the_build_reports() {
    let router = emulator();
    // (case, the buildspec, the seconds its commands take in all)
    let rows: Vec<(&str, String, f64)> = vec![
        (
            "one phase that takes thirty seconds",
            buildspec(&[("build", "sleep 30")]),
            30.0,
        ),
        (
            "three phases that take time between them",
            buildspec(&[
                ("install", "sleep 2"),
                ("build", "sleep 5"),
                ("post_build", "sleep 3"),
            ]),
            10.0,
        ),
        (
            "a build no command of which takes any time",
            buildspec(&[("build", "echo building")]),
            0.0,
        ),
    ];

    for (index, (case, spec, at_least)) in rows.iter().enumerate() {
        let project = format!("duration-metric-{index}");
        create_project(&router, case, &project, spec, None).await;
        let build = settled_build(&router, case, &project, json!({})).await;
        assert_eq!(
            build["buildStatus"],
            json!(SUCCEEDED),
            "case {case:?}: expected the build to succeed, got {build}"
        );
        let (start, end) = build_window(case, &build);
        let reported = end - start;
        assert!(
            reported >= *at_least,
            "case {case:?}: a build whose commands took {at_least} s reports at least that long \
             a window, got {reported} s"
        );

        let now = now_secs();
        let statistics = cloudwatch(
            &router,
            case,
            "GetMetricStatistics",
            &json!({
                "Namespace": "AWS/CodeBuild",
                "MetricName": "Duration",
                "Dimensions": [{"Name": "ProjectName", "Value": project}],
                "StartTime": now - WINDOW_BACK_SECS,
                "EndTime": now + WINDOW_FORWARD_SECS,
                "Period": PERIOD_SECS,
                "Statistics": ["Maximum", "SampleCount"],
            }),
        )
        .await;
        let points = statistics["Datapoints"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let samples: f64 = points
            .iter()
            .map(|point| point["SampleCount"].as_f64().unwrap_or_default())
            .sum();
        assert_eq!(
            samples, 1.0,
            "case {case:?}: one build that ended publishes one Duration observation, got \
             {points:?}"
        );
        let published = points
            .iter()
            .map(|point| point["Maximum"].as_f64().unwrap_or_default())
            .fold(0.0_f64, f64::max);
        assert!(
            (published - reported).abs() <= ONE_SECOND,
            "case {case:?}: expected the Duration series to report the {reported} s window the \
             build reports, got {published} s over {points:?}"
        );
        let units: Vec<&str> = points
            .iter()
            .map(|point| point["Unit"].as_str().unwrap_or_default())
            .collect();
        assert_eq!(
            units,
            vec!["Seconds"],
            "case {case:?}: a span of time is published in Seconds, got {points:?}"
        );
    }
}
