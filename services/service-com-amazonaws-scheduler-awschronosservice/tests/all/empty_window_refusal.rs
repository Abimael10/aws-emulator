// SPDX-License-Identifier: CC0-1.0

//! The ONE refusal an EMPTY invocation window draws, whichever boundary the
//! request states it with.
//!
//! `StartDate` is "the date, in UTC, after which the schedule can begin invoking
//! its target" and `EndDate` "the date, in UTC, before which the schedule can
//! invoke its target" (`CreateSchedule$StartDate` / `$EndDate`, carried unchanged
//! by `UpdateSchedule`). The pair is one window, and the window has one violation:
//! the instant it opens at — `StartDate` when the request states one, and the
//! instant of the request when it does not — does not come before the instant it
//! closes at. The model declares a single `ValidationException` (httpError 400)
//! for the whole member set, so that one violation has one sentence, with nothing
//! taken from the request or from state.
//!
//! The rows below differ along which boundary the request states (an explicitly
//! reversed pair, and an `EndDate` alone that the clock has already passed) and
//! along how far the window is empty by, and each is driven through BOTH writing
//! operations.

use smithy_server_runtime::MultiServiceRouter;

use crate::wire_harness::{call, error_code, error_message, now_epoch_seconds, scheduler_router};

/// The sentence an empty window is refused with — fixed text, the same for every
/// row.
const EMPTY_WINDOW: &str = "The StartDate you specify must come before the EndDate.";

/// A target every row shares: these rows are about the window, and this router
/// has no IAM to resolve a role against, so the role is only shaped like one.
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

/// The window members a row states, as the body members they travel as: a wire
/// timestamp of this protocol is epoch seconds with a fraction.
fn window(start_offset: Option<f64>, end_offset: Option<f64>) -> serde_json::Value {
    let now = now_epoch_seconds();
    let mut members = serde_json::Map::new();
    if let Some(offset) = start_offset {
        members.insert("StartDate".to_string(), (now + offset).into());
    }
    if let Some(offset) = end_offset {
        members.insert("EndDate".to_string(), (now + offset).into());
    }
    serde_json::Value::Object(members)
}

/// One row: the case, the `StartDate` offset from the clock in seconds (`None`
/// for a request stating none), and the `EndDate` offset.
type WindowRow = (&'static str, Option<f64>, Option<f64>);

/// Create the schedule the `UpdateSchedule` rows are applied to, and assert the
/// setup held.
async fn seed(router: &MultiServiceRouter, name: &str) {
    let (status, body) = call(
        router,
        "POST",
        &format!("/schedules/{name}"),
        Some(schedule_body(serde_json::json!({ "State": "ENABLED" }))),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateSchedule {name} must succeed, got {status} {body}"
    );
}

#[tokio::test]
async fn an_empty_window_is_refused_with_one_sentence_whichever_boundary_states_it() {
    let router = scheduler_router();
    seed(&router, "seeded").await;

    // An hour, a day and a decade: how far the window is empty by is no part of
    // the refusal, and neither is which boundary the request stated.
    let rows: [WindowRow; 8] = [
        ("an EndDate alone a minute behind the clock", None, Some(-60.0)),
        ("an EndDate alone an hour behind the clock", None, Some(-3_600.0)),
        ("an EndDate alone a day behind the clock", None, Some(-86_400.0)),
        (
            "an EndDate alone a decade behind the clock",
            None,
            Some(-315_360_000.0),
        ),
        ("a pair reversed by a day", Some(86_400.0), Some(0.0)),
        ("a pair reversed by a second", Some(3_601.0), Some(3_600.0)),
        (
            "a pair reversed by a millisecond",
            Some(3_600.002),
            Some(3_600.001),
        ),
        (
            "a pair reversed by a decade",
            Some(315_360_000.0),
            Some(0.0),
        ),
    ];

    for (case, start_offset, end_offset) in rows {
        let body = schedule_body(window(start_offset, end_offset));
        for (operation, method, uri) in [
            ("CreateSchedule", "POST", "/schedules/created"),
            ("UpdateSchedule", "PUT", "/schedules/seeded"),
        ] {
            let (status, answer) = call(&router, method, uri, Some(body.clone())).await;
            assert_eq!(
                (status, error_code(&answer).as_str(), error_message(&answer)),
                (400, "ValidationException", EMPTY_WINDOW.to_string()),
                "case {case:?} on {operation}: expected {EMPTY_WINDOW:?}, got {status} {answer}"
            );
        }
    }
}

#[tokio::test]
async fn a_window_that_holds_an_instant_is_accepted() {
    let router = scheduler_router();
    seed(&router, "seeded").await;

    // The refusal above is the empty window's: a window holding an instant is
    // accepted whichever boundaries state it, including a start the clock has
    // already passed and a pair naming one single instant.
    let rows: [WindowRow; 6] = [
        ("neither boundary", None, None),
        ("a StartDate alone, ahead of the clock", Some(3_600.0), None),
        ("a StartDate alone, behind the clock", Some(-3_600.0), None),
        ("an EndDate alone, ahead of the clock", None, Some(3_600.0)),
        ("an ordered pair", Some(3_600.0), Some(90_000.0)),
        (
            "a pair naming one instant",
            Some(3_600.0),
            Some(3_600.0),
        ),
    ];

    for (index, (case, start_offset, end_offset)) in rows.into_iter().enumerate() {
        let body = schedule_body(window(start_offset, end_offset));
        for (operation, method, uri) in [
            ("CreateSchedule", "POST", format!("/schedules/accepted-{index}")),
            ("UpdateSchedule", "PUT", "/schedules/seeded".to_string()),
        ] {
            let (status, answer) = call(&router, method, &uri, Some(body.clone())).await;
            assert_eq!(
                status, 200,
                "case {case:?} on {operation}: a window holding an instant must be \
                 accepted, got {status} {answer}"
            );
        }
    }
}

#[tokio::test]
async fn a_refused_window_leaves_the_schedule_as_it_stood() {
    let router = scheduler_router();
    seed(&router, "untouched").await;

    let (status, answer) = call(
        &router,
        "PUT",
        "/schedules/untouched",
        Some(schedule_body(window(None, Some(-60.0)))),
    )
    .await;
    assert_eq!(
        (status, error_code(&answer).as_str(), error_message(&answer)),
        (400, "ValidationException", EMPTY_WINDOW.to_string()),
        "the refused update must state {EMPTY_WINDOW:?}, got {status} {answer}"
    );

    let (status, read) = call(&router, "GET", "/schedules/untouched", None).await;
    assert_eq!(
        (
            status,
            read.get("EndDate").cloned(),
            read.get("State").and_then(serde_json::Value::as_str)
        ),
        (200, None, Some("ENABLED")),
        "the refused update must store no EndDate and leave the state alone, got \
         {status} {read}"
    );
}
