// SPDX-License-Identifier: CC0-1.0

//! End-to-end wire tests for stopping a SageMaker training session:
//! `StopTrainingSession` and the `DescribeTrainingSession` that reads the lifecycle it
//! starts.
//!
//! RULE: the operation a service models to STOP a resource moves that resource into the
//! modelled transient stopping status, and the clock then settles it in the terminal
//! stopped status and stamps the instant it ended; a read reports the status the clock put
//! the resource in and moves it neither earlier nor later. Here that is
//! `StopTrainingSession` ("Stops a running training session and sets the session status to
//! Stopping. After the session stops, the status changes to Stopped" —
//! `smithy_query(query="operation",operation="StopTrainingSession")`), whose
//! `TrainingSessionStatus` passes through `Stopping` to the terminal `Stopped` while
//! `EndTime` names the instant the session ended.
//!
//! Every case goes over the wire (`POST /` with `X-Amz-Target: SageMaker.<Op>`,
//! `awsJson1_1`) through the same `MultiServiceRouter` the production router uses.
//!
//! The rows span what the rule generalises over rather than one recorded call: the status
//! the session is stopped FROM (the transient one its create left it in and the running one
//! the clock moved it to), the Region the session was created in, the name's shape, how
//! many times the stop is repeated, and — for the refusal — why the named session resolves
//! to nothing.

use serde_json::{json, Value};

// The crate's ONE wire harness, plus the training-session helpers its own suite defines:
// the create request every row is built from, the describe, the status poll and the
// accounted members an existing session always reports.
use super::inference_deployment_lifecycle::{error_code, message, Emulator};
use super::training_session_lifecycle::{
    create_request, describe, field, poll_until_status, zero_accounting, POLL_INTERVAL,
    SETTLE_DEADLINE, STATUS_CREATED, STATUS_RUNNING,
};

/// The two `TrainingSessionStatus` members a stopped session passes through, from
/// `smithy_query(query="shape",shape="TrainingSessionStatus")` — the enum's transient
/// `Stopping` and its terminal `Stopped`.
const STATUS_STOPPING: &str = "Stopping";
/// See [`STATUS_STOPPING`].
const STATUS_STOPPED: &str = "Stopped";

/// Stop `name` in `region`, requiring a 200, and answer the body it sent back.
async fn stop(emulator: &Emulator, region: &str, name: &str) -> Value {
    let (status, parsed) = emulator
        .call_in(
            region,
            "StopTrainingSession",
            &json!({ "TrainingSessionName": name }),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {name}: expected HTTP 200 from the stop, got {status} with {parsed}"
    );
    parsed
}

/// A numeric member of a reported detail, which is how `awsJson1_1` carries a timestamp.
fn instant(reported: &Value, member: &str) -> f64 {
    field(reported, member)
        .as_f64()
        .unwrap_or_else(|| panic!("expected {member} to be reported as a timestamp in {reported}"))
}

/// RULE: a stopped session reports the transient `Stopping` first and then settles in the
/// terminal `Stopped` with the instant it ended — whichever non-terminal status the stop
/// was asked from.
#[tokio::test]
async fn stopping_a_session_settles_it_in_the_terminal_stopped_status() {
    let emulator = Emulator::new();

    // (case, the Region the session is created in, its name, whether to let the clock move
    // it to `Running` before the stop). The name rows span a single character and a
    // hyphenated name, both inside `TrainingSessionName`'s
    // `^[a-zA-Z0-9](-*[a-zA-Z0-9]){0,62}$`.
    let rows: Vec<(&str, &str, &str, bool)> = vec![
        ("stopped-while-still-provisioning", "us-east-1", "p", false),
        (
            "stopped-after-the-clock-made-it-running",
            "eu-west-2",
            "running-session",
            true,
        ),
    ];

    for (case, region, name, let_it_run) in rows {
        let (status, created) = emulator
            .call_in(region, "CreateTrainingSession", &create_request(name, json!({})))
            .await;
        assert_eq!(
            status, 200,
            "case {case}: expected HTTP 200 from the create, got {status} with {created}"
        );
        let before = if let_it_run {
            poll_until_status(&emulator, name, STATUS_RUNNING).await
        } else {
            describe(&emulator, region, name).await
        };
        let expected_before = if let_it_run {
            STATUS_RUNNING
        } else {
            STATUS_CREATED
        };
        assert_eq!(
            field(&before, "TrainingSessionStatus"),
            json!(expected_before),
            "case {case}: expected the session to be stopped from {expected_before}, \
             got {before}"
        );
        assert_eq!(
            field(&before, "EndTime"),
            Value::Null,
            "case {case}: a session nothing has ended reports no EndTime, got {before}"
        );

        // The stop itself answers an empty output: the modelled response carries no
        // member, so the status it moved the session into is read back through the
        // describe.
        let answer = stop(&emulator, region, name).await;
        assert_eq!(
            answer.as_object().map(serde_json::Map::len).unwrap_or(0),
            0,
            "case {case}: StopTrainingSession models no output member, got {answer}"
        );

        // The passage is never zero, so a caller that describes the session straight
        // after stopping it still observes the transient status, and nothing has ended
        // yet.
        let stopping = describe(&emulator, region, name).await;
        assert_eq!(
            field(&stopping, "TrainingSessionStatus"),
            json!(STATUS_STOPPING),
            "case {case}: TrainingSessionStatus straight after the stop, got {stopping}"
        );
        assert_eq!(
            field(&stopping, "EndTime"),
            Value::Null,
            "case {case}: a session that has not finished stopping has not ended, \
             got {stopping}"
        );

        // The clock settles it in the terminal status, and the instant it ended is the
        // instant the transition was due — which is also when it was last modified.
        let stopped = poll_until_status_in(&emulator, region, name, STATUS_STOPPED).await;
        let created_at = instant(&stopped, "CreationTime");
        let ended_at = instant(&stopped, "EndTime");
        let modified_at = instant(&stopped, "LastModifiedTime");
        assert!(
            ended_at > created_at,
            "case {case}: expected EndTime after CreationTime, got {ended_at} and \
             {created_at}"
        );
        assert_eq!(
            modified_at, ended_at,
            "case {case}: expected LastModifiedTime to be the instant the session ended, \
             got {modified_at} and {ended_at}"
        );
        // Every instant of the record is reported at the whole second it fell in.
        for (member, at) in [
            ("CreationTime", created_at),
            ("LastModifiedTime", modified_at),
            ("EndTime", ended_at),
        ] {
            assert_eq!(
                at.fract(),
                0.0,
                "case {case}: {member} carries the whole second the instant fell in, \
                 got {at}"
            );
        }
        // The accounted members a session always reports are unaffected by the stop.
        for (member, want) in zero_accounting() {
            assert_eq!(
                field(&stopped, member),
                want,
                "case {case}: {member} must still be reported as {want}, got {stopped}"
            );
        }

        // A read does not change what a later caller sees: the terminal status and the
        // instant it was reached stay where the clock put them.
        let again = describe(&emulator, region, name).await;
        assert_eq!(
            again, stopped,
            "case {case}: a second describe must report the same session, got {again}"
        );
    }
}

/// RULE: a repeated stop is the same stop — the session keeps the lifecycle the first call
/// started, so neither the passage nor the instant it ended is restarted or moved.
#[tokio::test]
async fn stopping_a_session_again_changes_nothing_about_it() {
    let emulator = Emulator::new();
    emulator
        .ok(
            "CreateTrainingSession",
            &create_request("repeated", json!({})),
        )
        .await;

    stop(&emulator, "us-east-1", "repeated").await;

    // A second stop while the first one is still in progress: wherever the clock has got
    // to by the time the read lands, the session is on the course the FIRST stop set — the
    // transient status or the terminal one, never back to a status it had left — because a
    // repeated stop neither restarts the passage nor undoes it.
    stop(&emulator, "us-east-1", "repeated").await;
    let still_stopping = describe(&emulator, "us-east-1", "repeated").await;
    let status = field(&still_stopping, "TrainingSessionStatus");
    assert!(
        status == json!(STATUS_STOPPING) || status == json!(STATUS_STOPPED),
        "case stopped-again-while-stopping: expected {STATUS_STOPPING} or {STATUS_STOPPED}, \
         got {still_stopping}"
    );

    // Once the session has settled, the terminal record is fixed: further stops leave
    // every member of it — the status, the instant it ended and the instant it was last
    // modified — exactly as the clock left them.
    let stopped = poll_until_status(&emulator, "repeated", STATUS_STOPPED).await;
    for case in ["stopped-once-more", "stopped-twice-more"] {
        stop(&emulator, "us-east-1", "repeated").await;
        let reread = describe(&emulator, "us-east-1", "repeated").await;
        assert_eq!(
            reread, stopped,
            "case {case}: a stop of a session that has already stopped must leave it as \
             it is, got {reread}"
        );
    }
}

/// RULE: stopping one session is a change to that session alone — every other session the
/// account holds reads back exactly as it did.
#[tokio::test]
async fn stopping_one_session_leaves_the_others_running() {
    let emulator = Emulator::new();
    for name in ["stopme", "untouched-one", "untouched-two"] {
        emulator
            .ok("CreateTrainingSession", &create_request(name, json!({})))
            .await;
    }
    // Every session has to be past its own provisioning transition first, so what the
    // stop changes is told from what the clock was going to change anyway.
    let mut before = Vec::new();
    for name in ["untouched-one", "untouched-two"] {
        before.push((name, poll_until_status(&emulator, name, STATUS_RUNNING).await));
    }
    poll_until_status(&emulator, "stopme", STATUS_RUNNING).await;

    stop(&emulator, "us-east-1", "stopme").await;
    poll_until_status(&emulator, "stopme", STATUS_STOPPED).await;

    for (name, reported) in before {
        let now = describe(&emulator, "us-east-1", name).await;
        assert_eq!(
            now, reported,
            "case {name}: a stop of another session must leave this one as it was, \
             got {now}"
        );
    }
}

/// RULE: the stop refuses a name no create stored, with the modelled `ResourceNotFound` at
/// HTTP 400 naming the identifier the request carried, and the refusal does not bring the
/// session into being.
#[tokio::test]
async fn stopping_an_unknown_training_session_is_refused_with_resource_not_found() {
    let emulator = Emulator::new();
    emulator
        .ok("CreateTrainingSession", &create_request("present", json!({})))
        .await;

    // Rows: a name never created, one differing from a stored name only in case, one
    // differing by a single character, and the longest legal name.
    let longest = format!("z{}", "y".repeat(62));
    let rows: Vec<(&str, String)> = vec![
        ("never-created", "absent".to_string()),
        ("case-differs-from-a-stored-name", "Present".to_string()),
        ("one-character-off", "presen".to_string()),
        ("longest-legal-name", longest),
    ];

    for (case, name) in &rows {
        let (status, parsed) = emulator
            .call(
                "StopTrainingSession",
                &json!({ "TrainingSessionName": name }),
            )
            .await;
        assert_eq!(
            status, 400,
            "case {case}: expected HTTP 400, got {status} with {parsed}"
        );
        assert_eq!(
            error_code(&parsed),
            "ResourceNotFound",
            "case {case}: expected the modelled ResourceNotFound, got {parsed}"
        );
        assert_eq!(
            message(&parsed),
            format!("Could not find training session \"{name}\"."),
            "case {case}: the refusal must quote the name the request carried, got {parsed}"
        );
        // A refused stop writes nothing: the name still resolves to no session.
        let (status, parsed) = emulator
            .call(
                "DescribeTrainingSession",
                &json!({ "TrainingSessionName": name }),
            )
            .await;
        assert_eq!(
            (status, error_code(&parsed).as_str()),
            (400, "ResourceNotFound"),
            "case {case}: a refused stop must write nothing, got {status} with {parsed}"
        );
    }

    // The session the account does hold is untouched by every refusal above.
    let described = describe(&emulator, "us-east-1", "present").await;
    assert_ne!(
        field(&described, "TrainingSessionStatus"),
        json!(STATUS_STOPPED),
        "a refused stop must not stop another session, got {described}"
    );
}

/// RULE: a request outside the reader's modelled constraints is refused as a constraint
/// violation — never as a not-implemented operation — and reaches no store.
#[tokio::test]
async fn stop_constraint_violations_are_refused_as_validation_errors() {
    let emulator = Emulator::new();

    // `TrainingSessionName` is `len 1..63, pattern ^[a-zA-Z0-9](-*[a-zA-Z0-9]){0,62}$`, so
    // the rows carry a name one character past the published maximum, the empty name one
    // short of the minimum, a name whose characters the pattern does not admit, and a
    // request missing the required member altogether.
    let too_long = "a".repeat(64);
    let rows: Vec<(&str, Value)> = vec![
        ("one-past-the-published-maximum-length", json!({ "TrainingSessionName": too_long })),
        ("one-short-of-the-published-minimum-length", json!({ "TrainingSessionName": "" })),
        (
            "characters-outside-the-pattern",
            json!({ "TrainingSessionName": "not a session name" }),
        ),
        ("missing-the-required-name", json!({})),
    ];

    for (case, body) in &rows {
        let (status, parsed) = emulator.call("StopTrainingSession", body).await;
        assert_eq!(
            status, 400,
            "case {case}: the stop must be refused, got {status} with {parsed}"
        );
        assert_eq!(
            error_code(&parsed),
            "ValidationException",
            "case {case}: the stop must be refused as a constraint violation, got {parsed}"
        );
    }
}

/// [`poll_until_status`] in a chosen Region, for a session created outside `us-east-1`.
async fn poll_until_status_in(emulator: &Emulator, region: &str, name: &str, want: &str) -> Value {
    let started = std::time::Instant::now();
    loop {
        let reported = describe(emulator, region, name).await;
        if field(&reported, "TrainingSessionStatus") == json!(want) {
            return reported;
        }
        assert!(
            started.elapsed() < SETTLE_DEADLINE,
            "case {name}: expected TrainingSessionStatus {want} before the deadline, \
             got {reported}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}
