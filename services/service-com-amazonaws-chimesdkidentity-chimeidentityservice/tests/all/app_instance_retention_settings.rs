// SPDX-License-Identifier: CC0-1.0

//! `PutAppInstanceRetentionSettings` and `GetAppInstanceRetentionSettings`, end to end
//! over HTTP.
//!
//! The RULE pinned here: an app instance's data-retention settings are the ones last
//! written for it — a put REPLACES them and answers with the settings now in effect plus
//! the instant from which data outside the retention window is deleted, a get REPORTS
//! exactly that and changes nothing, and an instance nobody has written settings for
//! reports none.
//!
//! The tables span what the rule generalises over: the number of retention days (both
//! published bounds and values between them), settings that name no number of days at
//! all (how a caller turns retention off) and settings carrying no channel member at
//! all, how many puts in a row an instance has had, and which ARNs the two operations
//! are keyed by.

use serde_json::{json, Value};

use crate::app_instance_lifecycle::{uri_encode, Emulator};

/// The published bounds of `RetentionDays`.
///
/// Source: `smithy_query(query="shape",shape="RetentionDays")` — an integer with
/// `range { min: 1, max: 5475 }`, the same range the model repeats on
/// `ChannelRetentionSettings$RetentionDays`.
pub(crate) const MIN_RETENTION_DAYS: i64 = 1;
pub(crate) const MAX_RETENTION_DAYS: i64 = 5475;

/// `PUT` the retention-settings route of one app instance.
///
/// Source: `smithy_query(query="operation",operation="PutAppInstanceRetentionSettings")`
/// — `PUT /app-instances/{AppInstanceArn}/retention-settings`, answering 200, with the
/// `AppInstanceRetentionSettings` member required in the body.
pub(crate) async fn put_retention(
    emulator: &Emulator,
    instance_arn: &str,
    settings: Value,
) -> (u16, Value) {
    put_retention_body(
        emulator,
        instance_arn,
        json!({ "AppInstanceRetentionSettings": settings }),
    )
    .await
}

/// `PUT` the retention-settings route of one app instance with the given body VERBATIM,
/// so a suite can send a body that carries no `AppInstanceRetentionSettings` member at
/// all. The route itself is stated once, here.
pub(crate) async fn put_retention_body(
    emulator: &Emulator,
    instance_arn: &str,
    body: Value,
) -> (u16, Value) {
    emulator
        .call(
            "PUT",
            &format!(
                "/app-instances/{}/retention-settings",
                uri_encode(instance_arn)
            ),
            Some(body),
        )
        .await
}

/// `GET` the retention-settings route of one app instance.
///
/// Source: `smithy_query(query="operation",operation="GetAppInstanceRetentionSettings")`
/// — `GET /app-instances/{AppInstanceArn}/retention-settings`, answering 200.
pub(crate) async fn get_retention(emulator: &Emulator, instance_arn: &str) -> (u16, Value) {
    emulator
        .call(
            "GET",
            &format!(
                "/app-instances/{}/retention-settings",
                uri_encode(instance_arn)
            ),
            None,
        )
        .await
}

/// The `AppInstanceRetentionSettings` member of a response, or `Value::Null` when it
/// carries none.
pub(crate) fn reported_settings(body: &Value) -> Value {
    body.get("AppInstanceRetentionSettings")
        .cloned()
        .unwrap_or(Value::Null)
}

/// The `InitiateDeletionTimestamp` member of a response, or `None` when it carries none.
/// Stated once here so both retention suites name the member in one place.
pub(crate) fn reported_instant(body: &Value) -> Option<f64> {
    body.get("InitiateDeletionTimestamp").and_then(Value::as_f64)
}

/// `AppInstanceRetentionSettings` naming `days` days of channel retention.
pub(crate) fn settings(days: i64) -> Value {
    json!({ "ChannelRetentionSettings": { "RetentionDays": days } })
}

/// One row of the served table: the settings a put writes, and whether those settings put
/// a retention period in effect (which is what decides whether there is a deletion
/// instant to report).
struct Case {
    label: &'static str,
    written: Value,
    retention_in_effect: bool,
}

fn served_cases() -> Vec<Case> {
    vec![
        Case {
            label: "the lower published bound",
            written: settings(MIN_RETENTION_DAYS),
            retention_in_effect: true,
        },
        Case {
            label: "one day inside the lower bound",
            written: settings(MIN_RETENTION_DAYS + 1),
            retention_in_effect: true,
        },
        Case {
            label: "a value between the bounds",
            written: settings(730),
            retention_in_effect: true,
        },
        Case {
            label: "one day inside the upper bound",
            written: settings(MAX_RETENTION_DAYS - 1),
            retention_in_effect: true,
        },
        Case {
            label: "the upper published bound",
            written: settings(MAX_RETENTION_DAYS),
            retention_in_effect: true,
        },
        Case {
            label: "channel settings naming no number of days turns retention off",
            written: json!({ "ChannelRetentionSettings": {} }),
            retention_in_effect: false,
        },
        Case {
            label: "settings carrying no channel member at all",
            written: json!({}),
            retention_in_effect: false,
        },
        Case {
            label: "retention set again after being turned off",
            written: settings(90),
            retention_in_effect: true,
        },
    ]
}

/// RULE: a put replaces the named instance's retention settings and answers with the
/// settings now in effect, a get reports exactly those settings, and the deletion instant
/// is reported precisely when the settings in effect name a number of days.
#[tokio::test]
async fn a_put_replaces_the_retention_settings_and_a_get_reports_them() {
    let emulator = Emulator::new();
    let instance = emulator
        .app_instance("retention-roundtrip", "tok-ret-roundtrip")
        .await;

    // Before any put the instance has no settings and no deletion instant to report.
    let (status, body) = get_retention(&emulator, &instance).await;
    assert_eq!(
        (status, reported_settings(&body)),
        (200, Value::Null),
        "expected a fresh instance to report no retention settings, got {status} {body:?}"
    );
    assert!(
        reported_instant(&body).is_none(),
        "expected a fresh instance to report no deletion instant, got {body:?}"
    );

    // Every row is written to the SAME instance in order, so each put also proves that a
    // put replaces whatever the previous one left behind.
    for case in served_cases() {
        let label = case.label;
        let (status, put_body) = put_retention(&emulator, &instance, case.written.clone()).await;
        assert_eq!(
            status, 200,
            "case {label:?}: expected the put to answer 200, got {status} {put_body:?}"
        );
        assert_eq!(
            reported_settings(&put_body),
            case.written,
            "case {label:?}: expected the put to answer with {:?}, got {put_body:?}",
            case.written
        );
        let put_instant = reported_instant(&put_body);
        assert_eq!(
            put_instant.is_some(),
            case.retention_in_effect,
            "case {label:?}: expected a deletion instant to be reported = {}, got \
             {put_instant:?}",
            case.retention_in_effect
        );

        let (status, get_body) = get_retention(&emulator, &instance).await;
        assert_eq!(
            (status, reported_settings(&get_body)),
            (200, case.written.clone()),
            "case {label:?}: expected a get to report {:?}, got {status} {get_body:?}",
            case.written
        );
        let get_instant = reported_instant(&get_body);
        assert_eq!(
            get_instant, put_instant,
            "case {label:?}: expected the get to report the put's deletion instant \
             {put_instant:?}, got {get_instant:?}"
        );
    }
}

/// RULE: a get is a report — repeating it, with no write in between, answers exactly the
/// same thing, so a read never changes what a later caller sees.
#[tokio::test]
async fn repeating_a_get_answers_the_same_thing() {
    let emulator = Emulator::new();
    let instance = emulator
        .app_instance("retention-stable", "tok-ret-stable")
        .await;

    // Rows: before any put, and after each of a few puts spanning the value space.
    let rows: Vec<(&str, Option<Value>)> = vec![
        ("before any put", None),
        ("after the lower bound", Some(settings(MIN_RETENTION_DAYS))),
        ("after the upper bound", Some(settings(MAX_RETENTION_DAYS))),
        (
            "after retention was turned off",
            Some(json!({ "ChannelRetentionSettings": {} })),
        ),
    ];

    for (label, written) in rows {
        if let Some(written) = written {
            let (status, body) = put_retention(&emulator, &instance, written).await;
            assert_eq!(
                status, 200,
                "case {label:?}: expected the put to answer 200, got {status} {body:?}"
            );
        }
        let (first_status, first) = get_retention(&emulator, &instance).await;
        let (second_status, second) = get_retention(&emulator, &instance).await;
        assert_eq!(
            (first_status, &first),
            (second_status, &second),
            "case {label:?}: expected two gets to answer the same, got {first_status} \
             {first:?} then {second_status} {second:?}"
        );
    }
}

/// RULE: a number of retention days outside the published range is refused with a 4xx and
/// leaves the settings in effect exactly as they were.
#[tokio::test]
async fn a_number_of_days_outside_the_published_range_is_refused() {
    let emulator = Emulator::new();
    let instance = emulator
        .app_instance("retention-range", "tok-ret-range")
        .await;

    // The settings the instance is left holding: nothing refused below may change them.
    let surviving = settings(365);
    let (status, body) = put_retention(&emulator, &instance, surviving.clone()).await;
    assert_eq!(status, 200, "the surviving put answered: {status} {body:?}");
    let surviving_instant = reported_instant(&body)
        .unwrap_or_else(|| panic!("expected the put to report a deletion instant: {body:?}"));

    // One step past each published bound, a negative count, and zero.
    let refused: Vec<(String, Value)> = vec![
        (
            "one day below the lower bound".to_string(),
            settings(MIN_RETENTION_DAYS - 1),
        ),
        ("a negative number of days".to_string(), settings(-7)),
        (
            "one day past the upper bound".to_string(),
            settings(MAX_RETENTION_DAYS + 1),
        ),
        (
            "far past the upper bound".to_string(),
            settings(MAX_RETENTION_DAYS * 2),
        ),
    ];

    for (label, written) in refused {
        let (status, body) = put_retention(&emulator, &instance, written.clone()).await;
        assert_eq!(
            status, 400,
            "case {label:?}: expected {written:?} to be refused with 400, got {status} \
             {body:?}"
        );
        let (status, body) = get_retention(&emulator, &instance).await;
        assert_eq!(
            (status, reported_settings(&body)),
            (200, surviving.clone()),
            "case {label:?}: a refused put must leave {surviving:?} in place, got \
             {status} {body:?}"
        );
        assert_eq!(
            reported_instant(&body),
            Some(surviving_instant),
            "case {label:?}: a refused put must leave the deletion instant \
             {surviving_instant:?} in place, got {body:?}"
        );
    }
}

/// RULE: both retention-settings operations are keyed by an `AppInstanceArn` this service
/// holds, and an ARN it resolves to no live instance earns one of TWO refusals — an
/// instance that has been DELETED is a request the caller may no longer make (403
/// `Forbidden`), while an ARN that never named an instance is absent (400 `NotFound`) —
/// and the put writes nothing either way.
#[tokio::test]
async fn an_arn_that_names_no_app_instance_is_refused() {
    let emulator = Emulator::new();
    let instance = emulator
        .app_instance("retention-absent", "tok-ret-absent")
        .await;
    let deleted = emulator
        .app_instance("retention-deleted", "tok-ret-deleted")
        .await;
    let (status, body) = emulator
        .call("DELETE", &format!("/app-instances/{}", uri_encode(&deleted)), None)
        .await;
    assert_eq!(status, 204, "DeleteAppInstance answered: {status} {body:?}");

    // Rows: an ARN of the right shape naming nothing, an instance that has been deleted,
    // and the ARN of a user — a resource of another kind this service mints, which is no
    // `AppInstance`. The DELETED instance is its own class: the instance that held the
    // settings is gone, so the request is one this caller may no longer make, which is
    // the answer every operation of this service keyed by a deleted instance's ARN gives.
    let user_arn = format!("{instance}/user/not-an-app-instance");
    let rows: Vec<(&str, String, u16, &str)> = vec![
        (
            "an instance that never existed",
            format!("{instance}x"),
            400,
            "NotFound",
        ),
        (
            "an instance that has been deleted",
            deleted.clone(),
            403,
            "Forbidden",
        ),
        (
            "a user ARN in the app-instance space",
            user_arn,
            400,
            "NotFound",
        ),
    ];

    for (label, arn, expected_status, expected_code) in rows {
        for (operation, (status, body)) in [
            ("get", get_retention(&emulator, &arn).await),
            ("put", put_retention(&emulator, &arn, settings(30)).await),
        ] {
            assert_eq!(
                (status, body["Code"].as_str()),
                (expected_status, Some(expected_code)),
                "case {label:?} / {operation}: expected {expected_status} \
                 {expected_code:?} for {arn:?}, got {status} {body:?}"
            );
        }
    }

    // The live instance the refusals never named still carries no settings at all.
    let (status, body) = get_retention(&emulator, &instance).await;
    assert_eq!(
        (status, reported_settings(&body)),
        (200, Value::Null),
        "expected the refusals to write nothing, got {status} {body:?}"
    );
}

/// `GET` one app instance and return the created and last-updated instants the
/// `AppInstance` it wraps reports.
///
/// Source: `smithy_query(query="operation",operation="DescribeAppInstance")` —
/// `GET /app-instances/{AppInstanceArn}`, answering 200 with an `AppInstance` whose
/// optional `LastUpdatedTimestamp` is the time the instance was last updated.
async fn described_instants(emulator: &Emulator, arn: &str) -> (Option<f64>, Option<f64>) {
    let (status, body) = emulator
        .call("GET", &format!("/app-instances/{}", uri_encode(arn)), None)
        .await;
    assert_eq!(
        status, 200,
        "DescribeAppInstance answered: {status} {body:?}"
    );
    let instance = &body["AppInstance"];
    (
        instance["CreatedTimestamp"].as_f64(),
        instance["LastUpdatedTimestamp"].as_f64(),
    )
}

/// RULE: writing an instance's retention settings writes the instance, so the instant it
/// reports as its last update moves to that write — at or after its created instant,
/// which never moves, and never backwards — while an instance nothing has written to
/// reports no last-updated instant at all and a refused put leaves both instants alone.
#[tokio::test]
async fn a_served_put_stamps_the_instance_and_a_refused_put_does_not() {
    let emulator = Emulator::new();
    let instance = emulator
        .app_instance("retention-stamped", "tok-ret-stamped")
        .await;

    let (created, before_any_put) = described_instants(&emulator, &instance).await;
    assert!(
        created.is_some(),
        "expected the created instance to report a created instant, got {created:?}"
    );
    assert!(
        before_any_put.is_none(),
        "expected an instance nothing has written to report no last-updated instant, \
         got {before_any_put:?}"
    );

    // Rows spanning what the rule generalises over: both published bounds, a value
    // between them, that same value AGAIN (a repeat is still a write), settings naming no
    // number of days and settings carrying no channel member at all (both turn retention
    // off, and so report no deletion instant), and a number of days set once more. Every
    // row is a write, so every row must move the stamp.
    let served: Vec<(&str, Value)> = vec![
        ("the lower published bound", settings(MIN_RETENTION_DAYS)),
        ("a value between the bounds", settings(180)),
        ("that same value again", settings(180)),
        ("the upper published bound", settings(MAX_RETENTION_DAYS)),
        (
            "channel settings naming no number of days",
            json!({ "ChannelRetentionSettings": {} }),
        ),
        ("settings carrying no channel member at all", json!({})),
        ("a number of days again", settings(14)),
    ];

    let mut stamped = before_any_put;
    for (label, written) in served {
        let (status, put_body) = put_retention(&emulator, &instance, written.clone()).await;
        assert_eq!(
            status, 200,
            "case {label:?}: expected the put to answer 200, got {status} {put_body:?}"
        );

        let (now_created, now_stamped) = described_instants(&emulator, &instance).await;
        assert_eq!(
            now_created, created,
            "case {label:?}: a put must not move the created instant {created:?}, got \
             {now_created:?}"
        );
        assert!(
            now_stamped.is_some_and(|at| created.is_some_and(|born| at >= born)),
            "case {label:?}: expected a last-updated instant at or after the created \
             instant {created:?}, got {now_stamped:?}"
        );
        assert!(
            stamped.is_none_or(|previous| now_stamped.is_some_and(|at| at >= previous)),
            "case {label:?}: expected the last-updated instant to never move backwards \
             from {stamped:?}, got {now_stamped:?}"
        );
        // One put is one write, so the instant the settings take effect from and the
        // instant the instance reports as its last update are the same instant.
        if let Some(deletion_instant) = reported_instant(&put_body) {
            assert_eq!(
                now_stamped,
                Some(deletion_instant),
                "case {label:?}: expected the instance to be stamped at the instant the \
                 settings took effect, {deletion_instant}, got {now_stamped:?}"
            );
        }
        stamped = now_stamped;
    }

    // Rows that are refused rather than served: a number of days outside the published
    // range, and a body carrying none of the required member. Neither writes the
    // instance, so neither stamps it.
    let refused: Vec<(&str, Value)> = vec![
        (
            "one day below the lower bound",
            json!({ "AppInstanceRetentionSettings": settings(MIN_RETENTION_DAYS - 1) }),
        ),
        (
            "one day past the upper bound",
            json!({ "AppInstanceRetentionSettings": settings(MAX_RETENTION_DAYS + 1) }),
        ),
        ("a body carrying no settings member", json!({})),
    ];

    for (label, body) in refused {
        let (status, response) = put_retention_body(&emulator, &instance, body.clone()).await;
        assert_eq!(
            status, 400,
            "case {label:?}: expected {body:?} to be refused with 400, got {status} \
             {response:?}"
        );
        let (now_created, now_stamped) = described_instants(&emulator, &instance).await;
        assert_eq!(
            (now_created, now_stamped),
            (created, stamped),
            "case {label:?}: a refused put must move neither instant, got {now_created:?} \
             / {now_stamped:?}"
        );
    }
}

/// RULE: retention settings belong to the instance they were written for, so each
/// instance reports its own and a write to one is invisible to the others.
#[tokio::test]
async fn retention_settings_are_per_app_instance() {
    let emulator = Emulator::new();

    // Rows: one instance per distinct settings value, plus one left without any.
    let written = [
        Some(settings(MIN_RETENTION_DAYS)),
        Some(settings(45)),
        Some(settings(MAX_RETENTION_DAYS)),
        Some(json!({ "ChannelRetentionSettings": {} })),
        None,
    ];

    let mut instances = Vec::new();
    for (index, settings) in written.iter().enumerate() {
        let arn = emulator
            .app_instance(&format!("retention-per-{index}"), &format!("tok-ret-per-{index}"))
            .await;
        if let Some(settings) = settings {
            let (status, body) = put_retention(&emulator, &arn, settings.clone()).await;
            assert_eq!(
                status, 200,
                "case instance {index}: expected the put to answer 200, got {status} \
                 {body:?}"
            );
        }
        instances.push(arn);
    }

    for (index, arn) in instances.iter().enumerate() {
        let expected = written[index].clone().unwrap_or(Value::Null);
        let (status, body) = get_retention(&emulator, arn).await;
        assert_eq!(
            (status, reported_settings(&body)),
            (200, expected.clone()),
            "case instance {index}: expected {expected:?}, got {status} {body:?}"
        );
    }
}
