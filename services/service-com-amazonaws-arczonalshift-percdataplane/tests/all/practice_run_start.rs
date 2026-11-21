// SPDX-License-Identifier: CC0-1.0

//! Starting an on-demand practice run, driven end to end over the wire.
//!
//! Every case here builds the production `MultiServiceRouter`, registers this service on
//! one shared engine and dispatches real `restJson1` requests, so what is pinned is the
//! HTTP behaviour rather than a handler called directly.
//!
//! The rules under test, each asserted over a TABLE of inputs rather than one request:
//!
//! * a practice run a caller starts is a zonal shift of the `PRACTICE_RUN` type ARC
//!   PERSISTS for the resource it names, so every later read — `GetManagedResource`,
//!   `ListZonalShifts` — reports it and the weights it moves, and its expiry time stands
//!   one minute out (`StartPracticeRunResponse$expiryTime`);
//! * one practice run at a time per resource, so a second while one is active is refused
//!   409 `ConflictException`; a customer-initiated zonal shift is a shift of ANOTHER
//!   type, so it runs beside a practice run and takes precedence over it;
//! * a practice run is run against the resource's practice run configuration, so a
//!   resource that has none is refused 409 `ConflictException`
//!   (`PracticeConfigurationDoesNotExist`);
//! * whether that configuration admits a run AT THE INSTANT of the call decides the call:
//!   a blocked date, a blocked window, or a time outside the allowed windows is each
//!   refused 409 with the `ConflictExceptionReason` the model names for it (the alarms it
//!   watches are `practice_run_alarm_blockers.rs`);
//! * the target is authorized before it is resolved, so a `resourceIdentifier` ARC holds
//!   nothing for is refused 403 `AccessDeniedException`, and `awayFrom` must name an
//!   Availability Zone the resource is deployed in (400 `ValidationException`).
//!
//! The rows differ along what those rules generalise over: WHICH resource the run is for
//! (each resource this store is seeded with, and one the service that owns it registered),
//! WHICH of its Availability Zones traffic moves away from, and — for the calendar rules —
//! which day and which part of the day the configuration names.

use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use service_com_amazonaws_arczonalshift_percdataplane::ops::types::{
    CONFLICT_EXCEPTION_REASON_VALUES, ZONAL_SHIFT_STATUS_VALUES,
};
use service_com_amazonaws_arczonalshift_percdataplane::state::{
    window_contains, ACTIVE_ZONAL_SHIFT_EXISTS_MESSAGE, INVALID_AVAILABILITY_ZONE_MESSAGE,
    PRACTICE_CONFIGURATION_MISSING_MESSAGE, PRACTICE_CONFIGURATION_MISSING_REASON,
    PRACTICE_IN_BLOCKED_DATES_MESSAGE, PRACTICE_IN_BLOCKED_DATES_REASON,
    PRACTICE_IN_BLOCKED_WINDOWS_MESSAGE, PRACTICE_IN_BLOCKED_WINDOWS_REASON,
    PRACTICE_OUTSIDE_ALLOWED_WINDOWS_MESSAGE, PRACTICE_OUTSIDE_ALLOWED_WINDOWS_REASON,
    PRACTICE_RUN_EXPIRY_WINDOW, TARGET_UNAVAILABLE_MESSAGE, UNSUPPORTED_AZ_REASON,
    ZONAL_SHIFT_ALREADY_EXISTS_REASON,
};
use smithy_server_runtime::MultiServiceRouter;

use super::practice_run_configuration::{alarm_arn, get_managed_resource};
use super::registered_managed_resources::register;
use super::zonal_shift_lifecycle::{call, emulator, encoded, error_code, seeded, start_body};

/// `StartPracticeRun` over the wire.
pub(crate) async fn start_practice_run(
    router: &MultiServiceRouter,
    resource: &str,
    away_from: &str,
    comment: &str,
) -> (u16, Value) {
    call(
        router,
        "POST",
        "/practiceruns",
        &json!({
            "resourceIdentifier": resource,
            "awayFrom": away_from,
            "comment": comment,
        })
        .to_string(),
    )
    .await
}

/// The practice run configuration members a case starts from: the one alarm the model
/// requires, plus whatever `extra` the case adds.
pub(crate) fn configuration_with(resource: &str, extra: &[(&str, Value)]) -> String {
    let mut body = serde_json::Map::new();
    body.insert("resourceIdentifier".to_string(), json!(resource));
    body.insert(
        "outcomeAlarms".to_string(),
        json!([{"type": "CLOUDWATCH", "alarmIdentifier": alarm_arn("practice-outcome")}]),
    );
    for (member, value) in extra {
        body.insert((*member).to_string(), value.clone());
    }
    Value::Object(body).to_string()
}

/// Hold a practice run configuration for `resource` through
/// `CreatePracticeRunConfiguration`, which is the state every practice run starts from.
pub(crate) async fn given_configuration(
    router: &MultiServiceRouter,
    resource: &str,
    extra: &[(&str, Value)],
) {
    let (status, created) = call(
        router,
        "POST",
        "/configuration",
        &configuration_with(resource, extra),
    )
    .await;
    assert_eq!(
        status, 201,
        "setup for {resource}: the practice run configuration is created first, got {status} \
         {created}"
    );
}

/// The instant this process reads the clock at, as the calendar date and time of day (UTC)
/// a `blockedDates` entry and a window are written in.
fn now_utc() -> chrono::DateTime<chrono::Utc> {
    let since_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the clock reads past the Unix epoch");
    chrono::DateTime::from_timestamp(
        i64::try_from(since_epoch.as_secs()).expect("the clock is in the calendar's range"),
        0,
    )
    .expect("the clock is in the calendar's range")
}

/// The calendar date (UTC) `days` days from now, in the `YYYY-MM-DD` format
/// `blockedDates` documents.
fn date_in(days: i64) -> String {
    (now_utc() + chrono::Duration::days(days))
        .format("%Y-%m-%d")
        .to_string()
}

/// The day token (`Mon`..`Sun`) a window entry names the day `days` days from now by, as
/// the member's `@pattern` writes it
/// (`smithy_query(query="shape", shape="BlockedWindows")`).
fn day_token(days: i64) -> String {
    (now_utc() + chrono::Duration::days(days))
        .format("%a")
        .to_string()
}

/// A window of a single day that COVERS the instant of the call: today's day token, from
/// an hour before now to an hour after, each clamped into the day.
///
/// `None` in the last two minutes of a UTC day: a window's end is a time of day of the
/// same day, so the final minute of a day (`23:59:00` onwards) falls in no window a caller
/// can write at all. The table over [`window_contains`] below covers that edge with
/// instants of its own rather than with the clock.
fn window_covering_now() -> Option<String> {
    use chrono::Timelike;

    let now = now_utc();
    let day = day_token(0);
    let minutes = i64::from(now.hour() * 60 + now.minute());
    if minutes >= 23 * 60 + 58 {
        return None;
    }
    let start = (minutes - 60).max(0);
    let end = (minutes + 60).min(23 * 60 + 59);
    Some(format!(
        "{day}:{:02}:{:02}-{day}:{:02}:{:02}",
        start / 60,
        start % 60,
        end / 60,
        end % 60
    ))
}

/// A window of a single day that covers NO instant of today: the whole of a day three days
/// out, which is neither today nor the same day of the week.
fn window_on_another_day() -> String {
    let day = day_token(3);
    format!("{day}:00:00-{day}:23:59")
}

/// The practice run a resource view reports, if any: the first `zonalShifts` entry whose
/// `shiftType` is `PRACTICE_RUN`.
fn reported_practice_run(resource_view: &Value) -> Option<Value> {
    resource_view["zonalShifts"]
        .as_array()?
        .iter()
        .find(|shift| shift["shiftType"] == json!("PRACTICE_RUN"))
        .cloned()
}

/// A resource the service that owns it registered with ARC, named for the case using it so
/// every case acts on a resource of its own.
fn registered(engine: &engine::InMemoryEngine, case: &str, zones: &[&str]) -> String {
    let arn = format!("arn:aws:eks:us-east-1:000000000000:cluster/practice-{case}");
    register(engine, &arn, &format!("practice-{case}"), zones);
    arn
}

/// One row of the persistence table: the resource the run is for, the zone traffic moves
/// away from, and the zones that resource is deployed in.
struct PersistedRow {
    resource: String,
    away_from: String,
    zones: Vec<String>,
}

/// RULE: a practice run a caller starts is persisted as a zonal shift of the
/// `PRACTICE_RUN` type for the resource it names, so the start's own answer and every
/// later read report it, and its expiry time stands one minute past its start.
///
/// Rows differ along which resource the run is for — each resource this store is seeded
/// with, and one the service that owns it registered — and which of that resource's
/// Availability Zones traffic moves away from (the first and the last).
#[tokio::test]
async fn a_practice_run_a_caller_starts_is_persisted_and_read_back() {
    let (router, engine) = emulator();
    let mut rows: Vec<PersistedRow> = Vec::new();
    for (index, (resource, zones)) in seeded(&engine).into_iter().enumerate() {
        // The first zone for the first resource, the last for the others, so the rows do
        // not all move traffic away from one position in the list.
        let away_from = if index == 0 {
            zones[0].clone()
        } else {
            zones[zones.len() - 1].clone()
        };
        rows.push(PersistedRow {
            resource,
            away_from,
            zones,
        });
    }
    let registered_zones = ["use1-az1", "use1-az2", "use1-az3"];
    rows.push(PersistedRow {
        resource: registered(&engine, "persisted", &registered_zones),
        away_from: "use1-az2".to_string(),
        zones: registered_zones.iter().map(|z| (*z).to_string()).collect(),
    });

    for row in &rows {
        let (resource, away_from, zones) = (&row.resource, &row.away_from, &row.zones);
        let case = format!("{resource} away from {away_from}");
        given_configuration(&router, resource, &[]).await;
        let (status, started) =
            start_practice_run(&router, resource, away_from, "a practice run").await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the practice run to be started, got {status} {started}"
        );
        assert_eq!(
            (
                started["resourceIdentifier"].as_str(),
                started["awayFrom"].as_str(),
                started["comment"].as_str(),
                started["status"].as_str(),
            ),
            (
                Some(resource.as_str()),
                Some(away_from.as_str()),
                Some("a practice run"),
                Some("ACTIVE"),
            ),
            "case {case:?}: expected the practice run as asked for, ACTIVE, got {started}"
        );
        assert!(
            ZONAL_SHIFT_STATUS_VALUES.contains(&"ACTIVE"),
            "case {case:?}: ACTIVE is not a ZonalShiftStatus the model declares"
        );
        let zonal_shift_id = started["zonalShiftId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: zonalShiftId is required, got {started}"));
        assert!(
            (6..=36).contains(&zonal_shift_id.len())
                && zonal_shift_id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-'),
            "case {case:?}: expected a ZonalShiftId of 6..36 characters of [A-Za-z0-9-], got \
             {zonal_shift_id:?}"
        );
        // "the expiryTime field for practice run zonal shifts always has a value of 1
        // minute" (`StartPracticeRunResponse$expiryTime`).
        let (start_time, expiry_time) = (
            started["startTime"].as_f64().unwrap_or(f64::NAN),
            started["expiryTime"].as_f64().unwrap_or(f64::NAN),
        );
        assert_eq!(
            expiry_time - start_time,
            PRACTICE_RUN_EXPIRY_WINDOW.as_secs_f64(),
            "case {case:?}: expected the expiry time one minute past the start, got \
             {start_time} then {expiry_time}"
        );

        // Read back through the resource's own view: the run is in progress, it is the
        // shift in effect, and it moves the weight of the zone it shifts away from.
        let (status, resource_view) = get_managed_resource(&router, resource).await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetManagedResource must report the resource, got {status} \
             {resource_view}"
        );
        let reported = reported_practice_run(&resource_view).unwrap_or_else(|| {
            panic!("case {case:?}: expected the practice run reported, got {resource_view}")
        });
        assert_eq!(
            (
                reported["zonalShiftId"].as_str(),
                reported["appliedStatus"].as_str(),
                reported["awayFrom"].as_str(),
            ),
            (Some(zonal_shift_id), Some("APPLIED"), Some(away_from.as_str())),
            "case {case:?}: expected the practice run in effect for the resource, got {reported}"
        );
        for zone in zones {
            let expected = if zone == away_from { 0.0 } else { 1.0 };
            assert_eq!(
                resource_view["appliedWeights"][zone].as_f64(),
                Some(expected),
                "case {case:?}: expected weight {expected} for {zone}, got {resource_view}"
            );
        }

        // And through the list of shifts, where a practice run is reported as one.
        let (status, listed) = call(&router, "GET", "/zonalshifts?status=ACTIVE", "").await;
        assert_eq!(
            status, 200,
            "case {case:?}: ListZonalShifts must answer, got {status} {listed}"
        );
        let listed_run = listed["items"]
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .find(|item| item["zonalShiftId"] == json!(zonal_shift_id))
            .cloned()
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected the practice run listed, got {listed}")
            });
        assert_eq!(
            (
                listed_run["shiftType"].as_str(),
                listed_run["status"].as_str(),
                listed_run["resourceIdentifier"].as_str(),
            ),
            (
                Some("PRACTICE_RUN"),
                Some("ACTIVE"),
                Some(resource.as_str()),
            ),
            "case {case:?}: expected a PRACTICE_RUN shift in progress, got {listed_run}"
        );
    }
}

/// RULE: a resource has at most one practice run in progress, so a second while one is
/// active is refused 409 `ConflictException` naming the run it already has; a
/// customer-initiated zonal shift is a shift of another type, so it runs beside a practice
/// run and takes precedence over it.
#[tokio::test]
async fn one_practice_run_at_a_time_per_resource_and_a_zonal_shift_beside_it() {
    let (router, engine) = emulator();
    let (resource, zones) = seeded(&engine)[0].clone();
    given_configuration(&router, &resource, &[]).await;
    let (status, first) = start_practice_run(&router, &resource, &zones[0], "the first run").await;
    assert_eq!(
        status, 200,
        "setup: the first practice run must start, got {status} {first}"
    );
    let held = first["zonalShiftId"].as_str().unwrap_or_default().to_string();

    // Rows differ along the zone the second run names: the same one the run in progress
    // moves traffic away from, and another zone of the same resource.
    for away_from in [&zones[0], &zones[1]] {
        let case = format!("a second practice run away from {away_from}");
        let (status, refused) =
            start_practice_run(&router, &resource, away_from, "the second run").await;
        assert_eq!(
            status, 409,
            "case {case:?}: expected the conflict, got {status} {refused}"
        );
        assert_eq!(
            (
                error_code(&refused).as_str(),
                refused["reason"].as_str(),
                refused["zonalShiftId"].as_str(),
                refused["message"].as_str(),
            ),
            (
                "ConflictException",
                Some(ZONAL_SHIFT_ALREADY_EXISTS_REASON),
                Some(held.as_str()),
                Some(ACTIVE_ZONAL_SHIFT_EXISTS_MESSAGE),
            ),
            "case {case:?}: expected the conflict naming the run already held, got {refused}"
        );
    }

    // A customer-initiated zonal shift is admitted beside the practice run, and takes
    // precedence over it: "customer-initiated zonal shifts > autoshifts > practice run
    // zonal shifts" (`ZonalShiftInResource$appliedStatus`).
    let (status, shift) = call(
        &router,
        "POST",
        "/zonalshifts",
        &start_body(&resource, &zones[1], "30m", "a customer shift"),
    )
    .await;
    assert_eq!(
        status, 201,
        "case a zonal shift beside a practice run: expected it started, got {status} {shift}"
    );
    let (status, resource_view) = get_managed_resource(&router, &resource).await;
    assert_eq!(status, 200, "GetManagedResource must answer, got {status}");
    let applied: Vec<(String, String)> = resource_view["zonalShifts"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .map(|entry| {
            (
                entry["shiftType"].as_str().unwrap_or_default().to_string(),
                entry["appliedStatus"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect();
    assert!(
        applied.contains(&("ZONAL_SHIFT".to_string(), "APPLIED".to_string()))
            && applied.contains(&("PRACTICE_RUN".to_string(), "NOT_APPLIED".to_string())),
        "case a zonal shift beside a practice run: expected the customer shift applied and \
         the practice run not applied, got {applied:?}"
    );
}

/// RULE: a practice run is run against the practice run configuration ARC holds for the
/// resource, so a resource that has none is refused 409 `ConflictException` with the
/// reason the model names for a missing configuration.
///
/// Rows differ along how the resource came to have none: a resource this store was seeded
/// with that was never configured, one the owning service registered that was never
/// configured, and one whose configuration was deleted again.
#[tokio::test]
async fn a_resource_with_no_practice_run_configuration_cannot_be_practiced() {
    let (router, engine) = emulator();
    let (seeded_resource, seeded_zones) = seeded(&engine)[1].clone();
    let registered_zones = ["use1-az1", "use1-az5"];
    let registered_arn = registered(&engine, "unconfigured", &registered_zones);
    let (deleted_resource, deleted_zones) = seeded(&engine)[2].clone();
    given_configuration(&router, &deleted_resource, &[]).await;
    let (status, deleted) = call(
        &router,
        "DELETE",
        &format!("/configuration/{}", encoded(&deleted_resource)),
        "",
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: the configuration under test is deleted first, got {status} {deleted}"
    );

    let rows: &[(&str, &str, &str)] = &[
        (
            "a seeded resource never configured",
            &seeded_resource,
            &seeded_zones[0],
        ),
        (
            "a registered resource never configured",
            &registered_arn,
            registered_zones[0],
        ),
        (
            "a resource whose configuration was deleted",
            &deleted_resource,
            &deleted_zones[0],
        ),
    ];
    assert!(
        CONFLICT_EXCEPTION_REASON_VALUES.contains(&PRACTICE_CONFIGURATION_MISSING_REASON),
        "{PRACTICE_CONFIGURATION_MISSING_REASON} is not a ConflictExceptionReason the model \
         declares"
    );
    for (case, resource, away_from) in rows {
        let (status, refused) = start_practice_run(&router, resource, away_from, "no config").await;
        assert_eq!(
            status, 409,
            "case {case:?}: expected the conflict, got {status} {refused}"
        );
        assert_eq!(
            (
                error_code(&refused).as_str(),
                refused["reason"].as_str(),
                refused["message"].as_str(),
            ),
            (
                "ConflictException",
                Some(PRACTICE_CONFIGURATION_MISSING_REASON),
                Some(PRACTICE_CONFIGURATION_MISSING_MESSAGE),
            ),
            "case {case:?}: expected the missing-configuration conflict, got {refused}"
        );
    }
}

/// RULE: the target of the call is authorized before it is resolved, so a
/// `resourceIdentifier` ARC holds no resource for is refused 403 `AccessDeniedException`
/// stating that the requested resource is unavailable — the one refusal every zonal-shift
/// mutation keyed by a resource answers, whatever form the identifier took — and `awayFrom`
/// has to name an Availability Zone the resource is deployed in (400 `ValidationException`,
/// `UnsupportedAz`).
///
/// Rows differ along the FORM of the identifier and of the zone: an ARN of a resource
/// nothing registered, an identifier that is not an ARN at all, a zone the resource is not
/// deployed in, a zone of another Region, and the resource's own zone in another case.
#[tokio::test]
async fn an_unheld_target_or_an_undeployed_zone_is_refused() {
    let (router, engine) = emulator();
    let (resource, zones) = seeded(&engine)[0].clone();
    given_configuration(&router, &resource, &[]).await;
    // A zone the resource is NOT deployed in, taken from the Availability Zone IDs of the
    // Region the seeded resources live in (`use1-az1`..`use1-az6`).
    let other_zone = (1..=6)
        .map(|index| format!("use1-az{index}"))
        .find(|zone| !zones.contains(zone))
        .expect("a seeded resource spans fewer than six zones");

    // (case, resourceIdentifier, awayFrom, expected status, expected message)
    let rows: &[(&str, &str, &str, u16, &str)] = &[
        (
            "an ARN no service registered",
            "arn:aws:eks:us-east-1:000000000000:cluster/nothing-registered-this",
            &zones[0],
            403,
            TARGET_UNAVAILABLE_MESSAGE,
        ),
        (
            "an identifier that is not a resource ARN",
            "not-an-arn-at-all",
            &zones[0],
            403,
            TARGET_UNAVAILABLE_MESSAGE,
        ),
        (
            "a zone the resource is not deployed in",
            &resource,
            &other_zone,
            400,
            INVALID_AVAILABILITY_ZONE_MESSAGE,
        ),
        (
            "a zone of another Region",
            &resource,
            "usw2-az1",
            400,
            INVALID_AVAILABILITY_ZONE_MESSAGE,
        ),
        (
            "the resource's own zone in upper case",
            &resource,
            "USE1-AZ1",
            400,
            INVALID_AVAILABILITY_ZONE_MESSAGE,
        ),
    ];
    for (case, identifier, away_from, expected_status, expected_message) in rows {
        let (status, refused) = start_practice_run(&router, identifier, away_from, "refused").await;
        assert_eq!(
            status, *expected_status,
            "case {case:?}: expected {expected_status}, got {status} {refused}"
        );
        // A denial is the authorization layer's answer and states its text under
        // `Message`; the zone refusal is the operation's own `ValidationException` and
        // states it under the modelled `message` (`src/service/denial_envelope.rs`).
        let member = if *expected_status == 403 {
            "Message"
        } else {
            "message"
        };
        assert_eq!(
            refused[member].as_str(),
            Some(*expected_message),
            "case {case:?}: expected the sentence for the class at {member}, got {refused}"
        );
        if *expected_status == 400 {
            assert_eq!(
                refused["reason"].as_str(),
                Some(UNSUPPORTED_AZ_REASON),
                "case {case:?}: expected the reason the model names for an undeployed zone, \
                 got {refused}"
            );
        }
    }
}

/// One row of the calendar table: the configuration members it writes, and the refusal the
/// start is answered with, or `None` when the calendar admits the run.
struct CalendarRow {
    case: String,
    members: Vec<(&'static str, Value)>,
    refusal: Option<(&'static str, &'static str)>,
}

/// RULE: the calendar the practice run configuration keeps decides whether a run may start
/// at the instant of the call — a blocked date it falls on, a blocked window it falls in,
/// or allowed windows it falls outside of each refuse it 409 `ConflictException` with the
/// reason the model names for that blocker — and a configuration whose calendar does not
/// cover that instant admits the run.
///
/// Rows differ along which member the configuration names and whether it covers the
/// instant of the call: a blocked date list holding today, one holding only later days, a
/// blocked window covering now, one on another day of the week, allowed windows covering
/// now, and allowed windows only on another day.
#[tokio::test]
async fn the_configuration_calendar_decides_whether_a_run_may_start_now() {
    let (router, engine) = emulator();
    let covering_now = window_covering_now();
    let elsewhere = window_on_another_day();

    let mut rows: Vec<CalendarRow> = vec![
        CalendarRow {
            case: "blocked dates holding today and tomorrow".to_string(),
            members: vec![("blockedDates", json!([date_in(0), date_in(1)]))],
            refusal: Some((
                PRACTICE_IN_BLOCKED_DATES_REASON,
                PRACTICE_IN_BLOCKED_DATES_MESSAGE,
            )),
        },
        CalendarRow {
            case: "blocked dates holding only later days".to_string(),
            members: vec![("blockedDates", json!([date_in(2), date_in(3), date_in(4)]))],
            refusal: None,
        },
        CalendarRow {
            case: "a blocked window on another day of the week".to_string(),
            members: vec![("blockedWindows", json!([elsewhere.clone()]))],
            refusal: None,
        },
        CalendarRow {
            case: "allowed windows only on another day of the week".to_string(),
            members: vec![("allowedWindows", json!([elsewhere.clone()]))],
            refusal: Some((
                PRACTICE_OUTSIDE_ALLOWED_WINDOWS_REASON,
                PRACTICE_OUTSIDE_ALLOWED_WINDOWS_MESSAGE,
            )),
        },
        CalendarRow {
            case: "no calendar at all".to_string(),
            members: vec![],
            refusal: None,
        },
    ];
    // The two rows that need a window covering the instant of the call; in the last minute
    // of a UTC day no window a caller can write covers it (a window ends at a time of day
    // of the same day), so they are asserted whenever one can be written.
    if let Some(window) = covering_now {
        rows.push(CalendarRow {
            case: "a blocked window covering now".to_string(),
            members: vec![("blockedWindows", json!([window.clone()]))],
            refusal: Some((
                PRACTICE_IN_BLOCKED_WINDOWS_REASON,
                PRACTICE_IN_BLOCKED_WINDOWS_MESSAGE,
            )),
        });
        rows.push(CalendarRow {
            case: "allowed windows covering now, beside a day that does not".to_string(),
            members: vec![("allowedWindows", json!([elsewhere.clone(), window]))],
            refusal: None,
        });
    }

    for (index, row) in rows.iter().enumerate() {
        let (case, members, refusal) = (&row.case, &row.members, &row.refusal);
        // One resource per row: a resource holds one configuration and runs one practice
        // run at a time.
        let zones = ["use1-az1", "use1-az2"];
        let resource = registered(&engine, &format!("calendar-{index}"), &zones);
        given_configuration(&router, &resource, members).await;
        let (status, answered) = start_practice_run(&router, &resource, zones[0], case).await;
        match refusal {
            Some((reason, message)) => {
                assert_eq!(
                    status, 409,
                    "case {case:?}: expected the blocker to refuse the run, got {status} \
                     {answered}"
                );
                assert_eq!(
                    (
                        error_code(&answered).as_str(),
                        answered["reason"].as_str(),
                        answered["message"].as_str(),
                    ),
                    ("ConflictException", Some(*reason), Some(*message)),
                    "case {case:?}: expected the conflict the model names for the blocker, \
                     got {answered}"
                );
                assert!(
                    CONFLICT_EXCEPTION_REASON_VALUES.contains(reason),
                    "case {case:?}: {reason} is not a ConflictExceptionReason the model \
                     declares"
                );
                // A refused start persisted nothing: the resource still runs no practice
                // run.
                let (_, resource_view) = get_managed_resource(&router, &resource).await;
                assert!(
                    reported_practice_run(&resource_view).is_none(),
                    "case {case:?}: expected a refused start to persist nothing, got \
                     {resource_view}"
                );
            }
            None => assert_eq!(
                status, 200,
                "case {case:?}: expected the run admitted, got {status} {answered}"
            ),
        }
    }
}

/// RULE: a window covers an instant when it names that instant's day of the week (UTC) and
/// the time of day is at or after its start and before its end — the reading both the
/// write of a configuration and the start of a practice run go through.
///
/// A table over INSTANTS rather than the clock, so the day boundaries the clock reaches
/// only once a day are covered: the two ends of a window, each side of them, another day
/// of the week, and the final minute of a day, which no window can cover.
#[test]
fn a_window_covers_the_instants_of_its_own_day_between_its_ends() {
    // 2026-03-02 is a Monday, so `Mon` is the day token of these instants
    // (`seconds since the epoch` of 2026-03-02T00:00:00Z).
    let monday_midnight: u64 = 1_772_409_600;
    let at = |hours: u64, minutes: u64, seconds: u64| {
        UNIX_EPOCH
            + std::time::Duration::from_secs(
                monday_midnight + hours * 3_600 + minutes * 60 + seconds,
            )
    };
    // (case, the window, the instant, whether it covers it)
    let rows: &[(&str, &str, SystemTime, bool)] = &[
        (
            "the instant a window starts at",
            "Mon:09:00-Mon:17:00",
            at(9, 0, 0),
            true,
        ),
        (
            "a second before a window starts",
            "Mon:09:00-Mon:17:00",
            at(8, 59, 59),
            false,
        ),
        (
            "the middle of a window",
            "Mon:09:00-Mon:17:00",
            at(13, 30, 0),
            true,
        ),
        (
            "the instant a window ends at",
            "Mon:09:00-Mon:17:00",
            at(17, 0, 0),
            false,
        ),
        (
            "a second before a window ends",
            "Mon:09:00-Mon:17:00",
            at(16, 59, 59),
            true,
        ),
        (
            "the same time of day on another day of the week",
            "Tue:09:00-Tue:17:00",
            at(13, 30, 0),
            false,
        ),
        (
            "the first instant of a whole-day window",
            "Mon:00:00-Mon:23:59",
            at(0, 0, 0),
            true,
        ),
        (
            "the final minute of a day, which no window covers",
            "Mon:00:00-Mon:23:59",
            at(23, 59, 30),
            false,
        ),
        (
            "a window naming no day of the week at all",
            "Xyz:00:00-Xyz:23:59",
            at(13, 30, 0),
            false,
        ),
    ];
    for (case, window, instant, expected) in rows {
        let covered = window_contains(window, *instant);
        assert_eq!(
            covered, *expected,
            "case {case:?}: expected {window} to cover the instant: {expected}, got {covered}"
        );
    }
}
