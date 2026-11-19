// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `StartCrawlerSchedule`, and the `GetCrawler`/`GetCrawlers` reads that
//! report what it wrote.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, so what is
//! asserted is HTTP behaviour and not a direct call into a handler.
//!
//! RULE under test: putting a crawler's schedule back in force sets that crawler's schedule
//! state to `SCHEDULED` and decides nothing else — the cron expression, the definition
//! `Version` and the run `State` stay where they were — and a crawler whose schedule is
//! already in force is served, because the state the call writes is the state it is in. A
//! crawler carrying NO cron expression has no schedule to start, so the call is the modelled
//! `NoScheduleException`; and a name no crawler in this account has is the modelled
//! `EntityNotFoundException`.
//!
//! The recorded workflow's own calls — one crawler created with no schedule, and one created
//! with a cron expression, suspended and started again — are ONE row each of the tables below;
//! the rows differ along the cron expression the crawler was scheduled by, how the schedule
//! got onto it, the run state the crawler is in and how many suspends and starts preceded the
//! call.

use std::sync::Arc;
use std::time::SystemTime;

use serde_json::{json, Value};
use service_com_amazonaws_glue_awsglue::state::{
    messages, ComAmazonawsGlueAwsglueState, CRAWLER_STATE_READY, CRAWLER_STATE_RUNNING,
    CRAWLER_STATE_STOPPING, SCHEDULE_STATE_NOT_SCHEDULED, SCHEDULE_STATE_SCHEDULED,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::wire::{call, emulator, emulator_on, error_code, error_message, ok};

/// The role every crawler here carries: a schedule write never touches it, so it is one of
/// the members each row reads back unchanged.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/service-role/AWSGlueServiceRole-resume";

/// Cron expressions the service accepts, as `Schedule$ScheduleExpression` documents the form
/// ("to run something every day at 12:15 UTC, you would specify: `cron(15 12 * * ? *)`"). The
/// first row is the documented example; the rest vary the fields the recorded call did not.
const CRON_EXPRESSIONS: &[(&str, &str)] = &[
    ("the documented example", "cron(15 12 * * ? *)"),
    ("a step in the minute field", "cron(0/20 * * * ? *)"),
    ("weekdays by name", "cron(30 8 ? * MON-FRI *)"),
    ("a list of months", "cron(0 6 1 FEB,MAY,AUG,NOV ? *)"),
];

/// A `CreateCrawler` body for `name`, with `schedule` when one is given.
fn crawler_body(name: &str, schedule: Option<&str>) -> Value {
    let mut body = json!({
        "Name": name,
        "Role": ROLE_ARN,
        "DatabaseName": "analytics",
        "Description": "a crawler whose schedule is put back in force",
        "Targets": { "S3Targets": [{ "Path": "s3://crawler-bucket/data/" }] },
        "TablePrefix": "resumed_",
    });
    if let Some(schedule) = schedule {
        body["Schedule"] = json!(schedule);
    }
    body
}

/// The `Crawler` structure `GetCrawler` reports for `name`.
async fn get_crawler(router: &MultiServiceRouter, name: &str) -> Value {
    let body = ok(router, "GetCrawler", &json!({ "Name": name })).await;
    body.get("Crawler")
        .cloned()
        .unwrap_or_else(|| panic!("GetCrawler must report a Crawler, got {body}"))
}

/// The same crawler as the paging read reports it, which is how a row checks the state is
/// held in the STORE rather than invented by one handler.
async fn get_crawler_through_the_listing(router: &MultiServiceRouter, name: &str) -> Value {
    let body = ok(router, "GetCrawlers", &json!({})).await;
    body.get("Crawlers")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("GetCrawlers must report a Crawlers list, got {body}"))
        .iter()
        .find(|crawler| crawler.get("Name").and_then(Value::as_str) == Some(name))
        .cloned()
        .unwrap_or_else(|| panic!("GetCrawlers must report {name}, got {body}"))
}

/// Create `name` scheduled by `expression` and then suspend that schedule, so the crawler is
/// a crawler with a schedule that is not in force — which is what a start puts back.
async fn suspended_crawler(router: &MultiServiceRouter, name: &str, expression: &str) -> Value {
    ok(router, "CreateCrawler", &crawler_body(name, Some(expression))).await;
    ok(router, "StopCrawlerSchedule", &json!({ "CrawlerName": name })).await;
    let crawler = get_crawler(router, name).await;
    assert_eq!(
        crawler.get("Schedule").cloned(),
        Some(json!({
            "ScheduleExpression": expression,
            "State": SCHEDULE_STATE_NOT_SCHEDULED,
        })),
        "case {expression}: the schedule is suspended before the start, got {crawler}"
    );
    crawler
}

/// Epoch seconds of a wall-clock instant, for bounding a server-set timestamp.
fn epoch_seconds(at: SystemTime) -> f64 {
    at.duration_since(SystemTime::UNIX_EPOCH)
        .expect("the clock is past the epoch")
        .as_secs_f64()
}

/// The `LastUpdated` of a reported crawler, as epoch seconds.
fn last_updated(case: &str, crawler: &Value) -> f64 {
    crawler
        .get("LastUpdated")
        .and_then(Value::as_f64)
        .unwrap_or_else(|| panic!("case {case}: expected a LastUpdated, got {crawler}"))
}

/// Assert that `after` is `before` with its schedule back in force: the same cron expression
/// with the state `SCHEDULED`, a `LastUpdated` inside the window the start happened in, and
/// every other member — `Version` and the run `State` included — untouched.
fn assert_schedule_in_force(
    case: &str,
    before: &Value,
    after: &Value,
    started: SystemTime,
    finished: SystemTime,
) {
    let expression = before
        .pointer("/Schedule/ScheduleExpression")
        .cloned()
        .unwrap_or_else(|| panic!("case {case}: the crawler carried a schedule, got {before}"));
    assert_eq!(
        after.get("Schedule").cloned(),
        Some(json!({
            "ScheduleExpression": expression,
            "State": SCHEDULE_STATE_SCHEDULED,
        })),
        "case {case}: expected the expression kept and the state in force, got {after}"
    );

    let moved = last_updated(case, after);
    let floor = epoch_seconds(started).floor();
    let ceiling = epoch_seconds(finished).ceil();
    assert!(
        (floor..=ceiling).contains(&moved),
        "case {case}: expected a LastUpdated within [{floor}, {ceiling}], got {moved}"
    );

    let mut expected = before.clone();
    let members = expected.as_object_mut().expect("a Crawler is a structure");
    members.insert("Schedule".to_string(), after["Schedule"].clone());
    members.insert("LastUpdated".to_string(), after["LastUpdated"].clone());
    assert_eq!(
        after, &expected,
        "case {case}: expected {expected}, got {after}"
    );
}

// ---------------------------------------------------------------------------
// The start
// ---------------------------------------------------------------------------

/// Starting a suspended schedule sets the addressed crawler's schedule state to `SCHEDULED`
/// and leaves everything else about the crawler standing — and the pair of writes can be
/// repeated, because each decides only that one member.
#[tokio::test]
async fn a_start_puts_the_crawlers_schedule_back_in_force() {
    let router = emulator();

    for (index, (form, expression)) in CRON_EXPRESSIONS.iter().enumerate() {
        let name = format!("resume-{index}");
        let before = suspended_crawler(&router, &name, expression).await;

        let started = SystemTime::now();
        let answer = ok(
            &router,
            "StartCrawlerSchedule",
            &json!({ "CrawlerName": &name }),
        )
        .await;
        assert_eq!(
            answer,
            json!({}),
            "case {form}: StartCrawlerSchedule answers an empty structure, got {answer}"
        );

        let after = get_crawler(&router, &name).await;
        assert_schedule_in_force(form, &before, &after, started, SystemTime::now());

        // The state is the store's, not one reader's.
        let listed = get_crawler_through_the_listing(&router, &name).await;
        assert_eq!(
            listed, after,
            "case {form}: the paging read must report the same crawler, expected {after}, got \
             {listed}"
        );

        // Suspend and start again: the two writes are each other's inverse, so the pair can
        // be made as often as a scheduler is toggled.
        ok(
            &router,
            "StopCrawlerSchedule",
            &json!({ "CrawlerName": &name }),
        )
        .await;
        ok(
            &router,
            "StartCrawlerSchedule",
            &json!({ "CrawlerName": &name }),
        )
        .await;
        let again = get_crawler(&router, &name).await;
        assert_eq!(
            again.pointer("/Schedule/State").and_then(Value::as_str),
            Some(SCHEDULE_STATE_SCHEDULED),
            "case {form}: a second start puts the schedule back in force, got {again}"
        );
    }
}

/// A crawl in progress does not stand in the way of a start, and the start does not touch it:
/// the call is served in every `CrawlerState` and leaves that state where it was.
///
/// The crawler is put into each state directly, because the operations that move it there are
/// not implemented yet; the rule under test is that the start decides nothing about the run.
#[tokio::test]
async fn a_start_does_not_touch_the_crawlers_run_state() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_on(engine.clone());

    /// Put the stored crawler into `state`.
    fn set_state(engine: &Arc<engine::InMemoryEngine>, name: &str, state: &str) {
        let store = engine.state::<ComAmazonawsGlueAwsglueState>();
        let mut store = store.lock();
        let entry = store
            .crawlers
            .get_mut(name)
            .unwrap_or_else(|| panic!("the suite created {name}"));
        entry.state = state.to_string();
    }

    for (round, run_state) in [
        CRAWLER_STATE_RUNNING,
        CRAWLER_STATE_STOPPING,
        CRAWLER_STATE_READY,
    ]
    .iter()
    .enumerate()
    {
        let name = format!("resume-busy-{round}");
        suspended_crawler(&router, &name, "cron(15 12 * * ? *)").await;
        set_state(&engine, &name, run_state);
        let before = get_crawler(&router, &name).await;

        let started = SystemTime::now();
        ok(
            &router,
            "StartCrawlerSchedule",
            &json!({ "CrawlerName": &name }),
        )
        .await;

        let after = get_crawler(&router, &name).await;
        let case = format!("run state {run_state}");
        assert_schedule_in_force(&case, &before, &after, started, SystemTime::now());
        assert_eq!(
            after.get("State").and_then(Value::as_str),
            Some(*run_state),
            "case {case}: a start does not move the crawler's run state, got {after}"
        );
    }
}

// ---------------------------------------------------------------------------
// Refusals
// ---------------------------------------------------------------------------

/// One row of the no-schedule table: how a crawler comes to carry no cron expression.
struct NoScheduleCase {
    /// What this row is, named in every assertion it makes.
    case: &'static str,
    /// The cron expression the create carried, if any.
    created_with: Option<&'static str>,
    /// The schedule a write states after the create, when a write is made at all:
    /// `Some(None)` states NO expression, which is how a write takes a schedule off.
    restated: Option<Option<&'static str>>,
}

/// A crawler with no cron expression has no schedule to start, so the call is the modelled
/// no-schedule refusal and the crawler is left exactly as it was.
///
/// The rows span the ways a crawler comes to carry none: created without one, and left without
/// one by a schedule write — including after a suspend, which takes a schedule out of force
/// but not off the crawler.
#[tokio::test]
async fn a_start_of_a_crawler_with_no_schedule_is_the_modelled_refusal() {
    let router = emulator();

    let cases = &[
        NoScheduleCase {
            case: "created without a schedule",
            created_with: None,
            restated: None,
        },
        NoScheduleCase {
            case: "a schedule write took the schedule off",
            created_with: Some("cron(15 12 * * ? *)"),
            restated: Some(None),
        },
        NoScheduleCase {
            case: "a schedule write stated none on a crawler that had none",
            created_with: None,
            restated: Some(None),
        },
    ];

    for (index, row) in cases.iter().enumerate() {
        let NoScheduleCase {
            case,
            created_with,
            restated,
        } = row;
        let name = format!("no-schedule-{index}");
        ok(&router, "CreateCrawler", &crawler_body(&name, *created_with)).await;
        if let Some(restated) = restated {
            let mut request = json!({ "CrawlerName": &name });
            if let Some(expression) = restated {
                request["Schedule"] = json!(expression);
            }
            ok(&router, "UpdateCrawlerSchedule", &request).await;
        }

        let before = get_crawler(&router, &name).await;
        // Repeated refusals: the second and third call answer exactly as the first.
        for attempt in 0..3 {
            let (status, body) = call(
                &router,
                "StartCrawlerSchedule",
                &json!({ "CrawlerName": &name }),
            )
            .await;
            assert_eq!(
                status, 400,
                "case {case} attempt {attempt}: expected 400, got {status} with {body}"
            );
            assert!(
                error_code(&body).contains("NoScheduleException"),
                "case {case} attempt {attempt}: expected NoScheduleException, got {body}"
            );
            assert_eq!(
                error_message(&body),
                messages::NO_SCHEDULE_FOUND,
                "case {case} attempt {attempt}: expected the no-schedule refusal, got {body}"
            );

            let after = get_crawler(&router, &name).await;
            assert_eq!(
                after, before,
                "case {case} attempt {attempt}: a refused start must change nothing, expected \
                 {before}, got {after}"
            );
            assert_eq!(
                after.get("Schedule"),
                None,
                "case {case} attempt {attempt}: the crawler still carries no schedule, got \
                 {after}"
            );
        }
    }
}

/// A crawler whose schedule is already in force is SERVED: the state the call writes is the
/// state it is in, so the write is idempotent and nothing about the crawler moves but the
/// instant of the write.
///
/// The rows span the ways a schedule comes to be in force: stated by the create, stated by a
/// schedule write, and put back in force by this very operation.
#[tokio::test]
async fn a_start_of_a_schedule_already_in_force_is_served() {
    let router = emulator();

    for (index, (case, created_with, restated, started_first)) in [
        (
            "scheduled at create",
            Some("cron(15 12 * * ? *)"),
            None,
            false,
        ),
        (
            "scheduled by a schedule write",
            None,
            Some("cron(0/20 * * * ? *)"),
            false,
        ),
        (
            "put back in force by a start",
            Some("cron(30 8 ? * MON-FRI *)"),
            None,
            true,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let name = format!("already-running-{index}");
        ok(&router, "CreateCrawler", &crawler_body(&name, created_with)).await;
        if let Some(expression) = restated {
            ok(
                &router,
                "UpdateCrawlerSchedule",
                &json!({ "CrawlerName": &name, "Schedule": expression }),
            )
            .await;
        }
        if started_first {
            ok(
                &router,
                "StopCrawlerSchedule",
                &json!({ "CrawlerName": &name }),
            )
            .await;
            ok(
                &router,
                "StartCrawlerSchedule",
                &json!({ "CrawlerName": &name }),
            )
            .await;
        }

        let before = get_crawler(&router, &name).await;
        let started = SystemTime::now();
        // Repeated starts: each is served and each leaves the schedule in force.
        for attempt in 0..3 {
            let answer = ok(
                &router,
                "StartCrawlerSchedule",
                &json!({ "CrawlerName": &name }),
            )
            .await;
            assert_eq!(
                answer,
                json!({}),
                "case {case} attempt {attempt}: expected the empty answer, got {answer}"
            );
        }

        let after = get_crawler(&router, &name).await;
        assert_schedule_in_force(case, &before, &after, started, SystemTime::now());
    }
}

/// Starting the schedule of a crawler this account does not hold is the modelled miss, and it
/// creates nothing: the rows span a name never created and names that differ from a stored
/// crawler's only in case, because the model documents no case fold for a crawler name.
#[tokio::test]
async fn a_start_against_an_absent_crawler_is_the_modelled_miss() {
    let router = emulator();
    suspended_crawler(&router, "present-crawler", "cron(15 12 * * ? *)").await;

    for (case, name) in [
        ("never created", "absent-crawler"),
        ("differs only in case", "PRESENT-CRAWLER"),
        ("differs only in one letter's case", "present-Crawler"),
        ("a name with a space", "present crawler"),
    ] {
        let (status, body) = call(
            &router,
            "StartCrawlerSchedule",
            &json!({ "CrawlerName": name }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case}: expected 400, got {status} with {body}"
        );
        assert!(
            error_code(&body).contains("EntityNotFoundException"),
            "case {case}: expected EntityNotFoundException, got {body}"
        );
        assert_eq!(
            error_message(&body),
            messages::schedule_crawler_not_found(name),
            "case {case}: expected the crawler miss for {name}, got {body}"
        );

        // The refused call created nothing under that name, and left the crawler it nearly
        // addressed suspended.
        let (status, body) = call(&router, "GetCrawler", &json!({ "Name": name })).await;
        assert_eq!(
            status, 400,
            "case {case}: a refused start must not create a crawler, got {status} with {body}"
        );
        let present = get_crawler(&router, "present-crawler").await;
        assert_eq!(
            present.pointer("/Schedule/State").and_then(Value::as_str),
            Some(SCHEDULE_STATE_NOT_SCHEDULED),
            "case {case}: the stored crawler's schedule must still be suspended, got {present}"
        );
    }
}
