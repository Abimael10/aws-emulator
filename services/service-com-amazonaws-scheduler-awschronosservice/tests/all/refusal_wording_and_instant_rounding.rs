// SPDX-License-Identifier: CC0-1.0

//! How this service WORDS the refusals a schedule write can draw, which resource a
//! read that resolves nothing names, and where a sub-second instant is held.
//!
//! Each suite is one rule over a TABLE of inputs, driven through the real
//! `restJson1` front door:
//!
//! * a write addressed into a schedule group whose deletion is under way is
//!   refused 400 `ValidationException` stating that the group named by the request
//!   is being deleted — the clause that distinguishes "the group is on its way out"
//!   from every other validation failure;
//! * a `ScheduleExpressionTimezone` naming no zone is refused in the TIMEZONE
//!   class, which names the submitted zone and nothing about the expression;
//! * a `StartDate`/`EndDate` pair in the wrong order is refused with the fixed
//!   sentence that rule has, carrying nothing from the request;
//! * a read addressed into a group that does not exist names the GROUP, and one
//!   into a group that does names the schedule;
//! * `StartDate`/`EndDate` are held at the NEAREST millisecond to the submitted
//!   instant, rounding up as readily as down and carrying into the next second.

use smithy_server_runtime::MultiServiceRouter;

use crate::wire_harness::{call, error_code, error_message, scheduler_router};

/// How long a suite keeps looking for a deleted group's removal: generous next to
/// the span the service holds a group in `DELETING` for, so the wait ends on the
/// condition rather than on how fast the host runs.
const REMOVAL_PATIENCE: std::time::Duration = std::time::Duration::from_secs(60);

/// How long one look for that removal waits before looking again.
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(25);

/// A whole second well ahead of any clock a test runs on (2099-05-30T22:40:00Z),
/// so a window built on it is never behind the clock. A JSON timestamp travels as
/// epoch seconds with a fraction, which is where the resolution shows.
const BASE_SECONDS: i64 = 4_082_400_000;

/// How far ahead of `StartDate` each row's `EndDate` stands, in seconds.
const WINDOW_WIDTH: i64 = 86_400;

/// A target every row shares: these suites are about refusal wording and
/// normalisation, and this router has no IAM to resolve a role against, so the
/// role is only shaped like one.
fn target() -> serde_json::Value {
    serde_json::json!({
        "Arn": "arn:aws:sqs:eu-west-2:000000000000:q",
        "RoleArn": "arn:aws:iam::000000000000:role/r"
    })
}

/// A `CreateSchedule`/`UpdateSchedule` body carrying `members` on top of the
/// required ones.
fn schedule_body(members: serde_json::Value) -> serde_json::Value {
    let mut body = serde_json::json!({
        "ScheduleExpression": "rate(5 minutes)",
        "FlexibleTimeWindow": { "Mode": "OFF" },
        "Target": target()
    });
    let object = body
        .as_object_mut()
        .expect("the body under construction is an object");
    for (member, value) in members
        .as_object()
        .expect("the members to set are an object")
    {
        object.insert(member.clone(), value.clone());
    }
    body
}

/// Create a schedule group and assert the setup held.
async fn create_group(router: &MultiServiceRouter, name: &str) {
    let (status, body) = call(router, "POST", &format!("/schedule-groups/{name}"), None).await;
    assert_eq!(
        status, 200,
        "setup: CreateScheduleGroup {name} must succeed, got {status} {body}"
    );
}

/// Create a schedule and assert the setup held.
async fn create_schedule(router: &MultiServiceRouter, name: &str, members: serde_json::Value) {
    let (status, body) = call(
        router,
        "POST",
        &format!("/schedules/{name}"),
        Some(schedule_body(members)),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateSchedule {name} must succeed, got {status} {body}"
    );
}

/// `DeleteScheduleGroup`, then look again for the CONDITION the clock owes — the
/// group is gone — until a deadline far longer than the span that removal is due
/// after has passed.
async fn delete_group_and_await_removal(router: &MultiServiceRouter, name: &str) {
    let (status, body) = call(router, "DELETE", &format!("/schedule-groups/{name}"), None).await;
    assert_eq!(
        status, 200,
        "setup: DeleteScheduleGroup {name} must succeed, got {status} {body}"
    );
    let deadline = std::time::Instant::now() + REMOVAL_PATIENCE;
    loop {
        let (status, _) = call(router, "GET", &format!("/schedule-groups/{name}"), None).await;
        if status == 404 {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "setup: GetScheduleGroup still answers {status} for the deleted group {name}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

#[tokio::test]
async fn a_write_into_a_group_being_deleted_states_that_the_group_is_being_deleted() {
    let router = scheduler_router();
    // The group name is the only part of the refusal taken from the request, so
    // each row names a different one and expects its own name back.
    for (case, group) in [
        ("an empty group", "doomed.group-1"),
        ("a group holding a schedule", "doomed_group_2"),
        ("a group named like a date", "2026-11-08"),
    ] {
        create_group(&router, group).await;
        if case == "a group holding a schedule" {
            create_schedule(
                &router,
                "held",
                serde_json::json!({ "GroupName": group }),
            )
            .await;
        }
        let (status, body) = call(&router, "DELETE", &format!("/schedule-groups/{group}"), None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DeleteScheduleGroup must succeed, got {status} {body}"
        );

        // The group is still DELETING here: nothing has waited for its removal.
        let (status, body) = call(
            &router,
            "GET",
            &format!("/schedule-groups/{group}"),
            None,
        )
        .await;
        assert_eq!(
            (
                status,
                body.get("State").and_then(serde_json::Value::as_str)
            ),
            (200, Some("DELETING")),
            "case {case:?}: the group must still be DELETING, got {status} {body}"
        );

        let (status, body) = call(
            &router,
            "POST",
            "/schedules/late",
            Some(schedule_body(serde_json::json!({ "GroupName": group }))),
        )
        .await;
        let expected = format!(
            "You cannot perform this operation because schedule group {group} is being deleted."
        );
        assert_eq!(
            (status, error_code(&body).as_str(), error_message(&body)),
            (400, "ValidationException", expected.clone()),
            "case {case:?}: expected {expected:?}, got {status} {body}"
        );
    }
}

#[tokio::test]
async fn an_unnamed_timezone_is_refused_in_the_timezone_class() {
    let router = scheduler_router();
    create_schedule(&router, "seeded", serde_json::json!({})).await;

    // Zones no IANA entry answers to. The submitted value is the only variable
    // part of the refusal, so each row expects its own value back.
    for (case, zone) in [
        ("a region and a city that do not exist", "Not/ARealZone"),
        ("a region with no city", "Europe"),
        ("an offset rather than a zone", "+01:00"),
        ("prose", "here"),
    ] {
        let body = schedule_body(serde_json::json!({ "ScheduleExpressionTimezone": zone }));
        let expected = format!("Invalid timezone {zone}.");
        for (operation, method, uri) in [
            ("CreateSchedule", "POST", "/schedules/created"),
            ("UpdateSchedule", "PUT", "/schedules/seeded"),
        ] {
            let (status, answer) = call(&router, method, uri, Some(body.clone())).await;
            assert_eq!(
                (status, error_code(&answer).as_str(), error_message(&answer)),
                (400, "ValidationException", expected.clone()),
                "case {case:?} on {operation}: expected {expected:?}, got {status} {answer}"
            );
        }
    }

    // A zone the database DOES carry is accepted, so the refusal is the unknown
    // zone's and not every zone's.
    for (case, zone) in [
        ("UTC", "UTC"),
        ("a European zone", "Europe/London"),
        ("an American zone", "America/New_York"),
    ] {
        let (status, answer) = call(
            &router,
            "PUT",
            "/schedules/seeded",
            Some(schedule_body(
                serde_json::json!({ "ScheduleExpressionTimezone": zone }),
            )),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: a named zone must be accepted, got {status} {answer}"
        );
    }
}

#[tokio::test]
async fn a_reversed_window_is_refused_with_the_sentence_that_rule_has() {
    let router = scheduler_router();
    create_schedule(&router, "seeded", serde_json::json!({})).await;

    // The refusal carries nothing from the request, so every reversed pair —
    // however far apart, and whatever sub-second remainder each boundary states —
    // draws the SAME sentence.
    let expected = "The StartDate you specify must come before the EndDate.";
    let rows: [(&str, f64, f64); 4] = [
        ("a day the wrong way round", 86_400.0, 0.0),
        ("a second the wrong way round", 1.0, 0.0),
        ("a millisecond the wrong way round", 0.002, 0.001),
        ("a decade the wrong way round", 315_360_000.0, 0.0),
    ];
    for (case, start_offset, end_offset) in rows {
        let base = BASE_SECONDS as f64;
        let body = schedule_body(serde_json::json!({
            "StartDate": base + start_offset,
            "EndDate": base + end_offset,
        }));
        for (operation, method, uri) in [
            ("CreateSchedule", "POST", "/schedules/created"),
            ("UpdateSchedule", "PUT", "/schedules/seeded"),
        ] {
            let (status, answer) = call(&router, method, uri, Some(body.clone())).await;
            assert_eq!(
                (status, error_code(&answer).as_str(), error_message(&answer)),
                (400, "ValidationException", expected.to_string()),
                "case {case:?} on {operation}: expected {expected:?}, got {status} {answer}"
            );
        }
    }

    // An ordered pair is accepted, so the refusal is the reversed pair's.
    let (status, answer) = call(
        &router,
        "PUT",
        "/schedules/seeded",
        Some(schedule_body(serde_json::json!({
            "StartDate": BASE_SECONDS as f64,
            "EndDate": (BASE_SECONDS + WINDOW_WIDTH) as f64,
        }))),
    )
    .await;
    assert_eq!(
        status, 200,
        "an ordered pair must be accepted, got {status} {answer}"
    );
}

#[tokio::test]
async fn a_read_names_the_group_when_the_group_is_what_is_missing() {
    let router = scheduler_router();
    create_group(&router, "present-group").await;
    create_schedule(
        &router,
        "present",
        serde_json::json!({ "GroupName": "present-group" }),
    )
    .await;

    // A group that was created, held a schedule, and was deleted: the cascade
    // takes the group AND the schedules it held, so a read addressed into it has
    // no group to resolve.
    create_group(&router, "cascaded-group").await;
    create_schedule(
        &router,
        "cascaded",
        serde_json::json!({ "GroupName": "cascaded-group" }),
    )
    .await;
    delete_group_and_await_removal(&router, "cascaded-group").await;

    // (case, the schedule the read names, the group it names, the resource the
    // refusal must report)
    let rows: [(&str, &str, Option<&str>, String); 5] = [
        (
            "a group this account never held",
            "any-schedule",
            Some("never-created-group"),
            "Schedule group never-created-group does not exist.".to_string(),
        ),
        (
            "a group a cascade delete took away, naming a schedule it held",
            "cascaded",
            Some("cascaded-group"),
            "Schedule group cascaded-group does not exist.".to_string(),
        ),
        (
            "a group a cascade delete took away, naming a schedule it never held",
            "never-created",
            Some("cascaded-group"),
            "Schedule group cascaded-group does not exist.".to_string(),
        ),
        (
            "a group that exists, naming a schedule it does not hold",
            "absent",
            Some("present-group"),
            "Schedule absent does not exist.".to_string(),
        ),
        (
            "the implicit default group, which exists, naming a schedule it does not hold",
            "absent",
            None,
            "Schedule absent does not exist.".to_string(),
        ),
    ];

    for (case, name, group, expected) in rows {
        let uri = match group {
            Some(group) => format!("/schedules/{name}?groupName={group}"),
            None => format!("/schedules/{name}"),
        };
        let (status, body) = call(&router, "GET", &uri, None).await;
        assert_eq!(
            (status, error_code(&body).as_str(), error_message(&body)),
            (404, "ResourceNotFoundException", expected.clone()),
            "case {case:?}: expected {expected:?}, got {status} {body}"
        );
    }

    // The schedule that IS there still reads back, so the refusals above are
    // about what is missing and not about the read itself.
    let (status, body) = call(
        &router,
        "GET",
        "/schedules/present?groupName=present-group",
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "an existing schedule must still read back, got {status} {body}"
    );
}

/// One wire timestamp as the whole number of milliseconds since the epoch it
/// reports, which is exact in an `f64` for every instant this suite states.
fn reported_milliseconds(body: &serde_json::Value, member: &str) -> Option<i64> {
    body.get(member)
        .and_then(serde_json::Value::as_f64)
        .map(|seconds| (seconds * 1_000.0).round() as i64)
}

#[tokio::test]
async fn an_instant_is_held_at_the_nearest_millisecond() {
    let router = scheduler_router();
    // (case, the sub-second remainder submitted, the milliseconds past
    // `BASE_SECONDS` the service must report). A remainder in the lower half of
    // its millisecond keeps it, one in the upper half lands on the millisecond
    // above, and one past the last millisecond of its second carries into the
    // next second — which is what 1000 states.
    let rows: [(&str, f64, i64); 10] = [
        ("a whole second", 0.0, 0),
        ("whole milliseconds", 0.123, 123),
        ("half a second", 0.5, 500),
        ("microseconds in the lower half", 0.123_456, 123),
        ("nanoseconds in the lower half", 0.123_456_7, 123),
        ("under half a millisecond", 0.000_4, 0),
        ("microseconds in the upper half", 0.777_777, 778),
        ("a tenth of a millisecond over", 0.613_6, 614),
        ("just under a whole millisecond", 0.000_999, 1),
        ("a remainder that rounds into the next second", 0.999_8, 1_000),
    ];

    for (index, (case, remainder, held)) in rows.into_iter().enumerate() {
        let name = format!("rounded-{index}");
        let start = BASE_SECONDS as f64 + remainder;
        let body = schedule_body(serde_json::json!({
            "StartDate": start,
            "EndDate": start + WINDOW_WIDTH as f64,
        }));
        let expected_start = BASE_SECONDS * 1_000 + held;
        let expected_end = expected_start + WINDOW_WIDTH * 1_000;

        let (status, answer) = call(
            &router,
            "POST",
            &format!("/schedules/{name}"),
            Some(body.clone()),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateSchedule must succeed, got {status} {answer}"
        );
        let (status, read) = call(&router, "GET", &format!("/schedules/{name}"), None).await;
        assert_eq!(
            (status, reported_milliseconds(&read, "StartDate")),
            (200, Some(expected_start)),
            "case {case:?}: expected StartDate at {expected_start}ms, got {status} {read}"
        );
        assert_eq!(
            reported_milliseconds(&read, "EndDate"),
            Some(expected_end),
            "case {case:?}: expected EndDate at {expected_end}ms, got {read}"
        );

        // `UpdateSchedule` carries the same members, so it is held to the same
        // resolution.
        let (status, answer) = call(
            &router,
            "PUT",
            &format!("/schedules/{name}"),
            Some(body.clone()),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: UpdateSchedule must succeed, got {status} {answer}"
        );
        let (status, read) = call(&router, "GET", &format!("/schedules/{name}"), None).await;
        assert_eq!(
            (
                status,
                reported_milliseconds(&read, "StartDate"),
                reported_milliseconds(&read, "EndDate")
            ),
            (200, Some(expected_start), Some(expected_end)),
            "case {case:?}: an updated instant is held the same way, got {status} {read}"
        );
    }
}
