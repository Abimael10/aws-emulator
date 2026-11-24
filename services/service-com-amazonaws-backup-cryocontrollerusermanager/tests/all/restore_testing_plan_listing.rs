// SPDX-License-Identifier: CC0-1.0

//! `ListRestoreTestingPlans` driven as real HTTP through the router.
//!
//! The rules under test:
//!
//! * a listing REPORTS the stored restore testing plans: every plan
//!   `CreateRestoreTestingPlan` stored appears exactly once, in
//!   `RestoreTestingPlanName` order, carrying the ARN its create minted, the
//!   schedule it was given and the zone that schedule is read in, the start
//!   window in force, its creation instant and the instant it was last written
//!   — and a name the account holds no plan under appears not at all;
//! * a listed plan and the same plan read back through `GetRestoreTestingPlan`
//!   report the same metadata, because both project ONE stored record;
//! * the listing is cut into pages of at most `MaxResults` entries plus an
//!   opaque cursor that resumes exactly where the page stopped, and the pages
//!   concatenate to the whole result set in that one order;
//! * a `MaxResults` outside the model's `@range(1..1000)` is
//!   `InvalidParameterValueException`, and so is a `NextToken` that names no
//!   page of this result set;
//! * a listing is a REPORT: repeating it answers the same thing and changes
//!   nothing a later reader sees.
//!
//! The request harness (`router`, `call`, `error_code`, `string_at`) and the
//! plan helpers (`create_plan`, `create_body`, `get_plan`, `read_plan`,
//! `minimal_plan`, `plan_arn_prefix`) are the ones the sibling suites expose to
//! this test binary, so there is ONE wire path for this service's suites.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::restore_testing_plan::{
    create_body, create_plan, get_plan, minimal_plan, plan_arn_prefix, read_plan,
    DEFAULT_START_WINDOW_HOURS, DEFAULT_TIMEZONE, MAX_START_WINDOW_HOURS,
};
use crate::vault_and_backup_job::{call, error_code, router, string_at};

/// The ends of the modelled page-size range (`smithy_query` operation
/// `ListRestoreTestingPlans`: `MaxResults` targets
/// `ListRestoreTestingPlansInputMaxResultsInteger`, `@range` 1..1000).
const MAX_RESULTS_MIN: i64 = 1;
const MAX_RESULTS_MAX: i64 = 1000;

/// `GET /restore-testing/plans` — `ListRestoreTestingPlans`'s HTTP binding,
/// with whatever query string the case under test asks for.
async fn list_plans(router: &MultiServiceRouter, query: &str) -> (u16, Value) {
    call(router, "GET", &format!("/restore-testing/plans{query}"), None).await
}

/// The refusal sentence of an error body, whichever casing the envelope spells
/// the member with: a refusal the boundary validator words carries `message`,
/// one the handler returns carries the modelled `Message`.
fn refusal_message(body: &Value) -> String {
    for member in ["Message", "message"] {
        let text = string_at(body, member);
        if !text.is_empty() {
            return text;
        }
    }
    String::new()
}

/// The `RestoreTestingPlans` member of a successful listing.
fn entries(body: &Value) -> Vec<Value> {
    body.get("RestoreTestingPlans")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// The `RestoreTestingPlanName` of every entry of a page, in the order the page
/// lists them.
fn names(body: &Value) -> Vec<String> {
    entries(body)
        .iter()
        .map(|entry| string_at(entry, "RestoreTestingPlanName"))
        .collect()
}

/// A plan body: the smallest one the model admits, with the schedule this row
/// asks for and, where the row states them, a timezone and a start window.
fn plan_request(
    name: &str,
    schedule: &str,
    timezone: Option<&str>,
    start_window_hours: Option<i64>,
) -> Value {
    let mut plan = minimal_plan(name);
    plan["ScheduleExpression"] = json!(schedule);
    if let Some(timezone) = timezone {
        plan["ScheduleExpressionTimezone"] = json!(timezone);
    }
    if let Some(hours) = start_window_hours {
        plan["StartWindowHours"] = json!(hours);
    }
    plan
}

/// Create every plan a row asks for, returning the create answers keyed by name.
async fn seed(router: &MultiServiceRouter, requests: &[Value]) -> Vec<(String, Value)> {
    let mut created = Vec::new();
    for request in requests {
        let (status, answer) = create_plan(router, &create_body(request.clone())).await;
        assert_eq!(
            status, 201,
            "setup: expected CreateRestoreTestingPlan 201 for {request:?}, got {status} {answer:?}"
        );
        created.push((string_at(request, "RestoreTestingPlanName"), answer));
    }
    created
}

/// The rows the "report what is stored" table walks: they differ in HOW MANY
/// plans the account holds, in the spelling of the schedule, in whether a
/// timezone and a start window were stated, and in the order the creates ran
/// relative to the name order a listing must report.
fn stored_set_cases() -> Vec<(&'static str, Vec<Value>)> {
    vec![
        ("an account holding no restore testing plan at all", vec![]),
        (
            "one plan, no timezone and no stated window",
            vec![plan_request("only_plan", "cron(0 5 ? * * *)", None, None)],
        ),
        (
            "one plan, a bare cron, a stated timezone and the ceiling window",
            vec![plan_request(
                "zoned_plan",
                "0 12 * * ? *",
                Some("Europe/Dublin"),
                Some(MAX_START_WINDOW_HOURS),
            )],
        ),
        (
            "three plans created in reverse name order",
            vec![
                plan_request("zulu_plan", "cron(0 1 ? * SUN *)", None, Some(72)),
                plan_request("mike_plan", "cron(15 * ? * * *)", Some("Etc/UTC"), None),
                plan_request("alfa_plan", "cron(0 5 ? * * *)", None, Some(1)),
            ],
        ),
        (
            "names that differ only in case and in digits",
            vec![
                plan_request("Plan_2", "cron(0 5 ? * * *)", None, None),
                plan_request("Plan_10", "cron(0 6 ? * * *)", None, Some(2)),
                plan_request("plan_2", "cron(0 7 ? * * *)", Some("Asia/Tokyo"), None),
            ],
        ),
        (
            "six plans, windows and timezones walking the rows",
            (0..6)
                .map(|index| {
                    plan_request(
                        &format!("walked_{index}"),
                        &format!("cron({index} 5 ? * * *)"),
                        (index % 3 == 0).then_some("America/New_York"),
                        (index % 2 == 0).then_some(i64::from(index) + 1),
                    )
                })
                .collect(),
        ),
    ]
}

/// Every stored plan is listed exactly once, in name order, reporting the
/// members the stored record holds; nothing else is listed, and the listing
/// agrees with the read.
#[tokio::test]
async fn a_listing_reports_every_stored_plan_once_in_name_order() {
    for (case, requests) in stored_set_cases() {
        // One router per row: the row's plans are the only ones the account
        // holds, so a row cannot see what another row stored.
        let router = router();
        let created = seed(&router, &requests).await;

        let (status, listed) = list_plans(&router, "").await;
        assert_eq!(
            (status, error_code(&listed).as_str()),
            (200, "<none>"),
            "case {case:?}: expected ListRestoreTestingPlans 200, got {status} {listed:?}"
        );

        let mut expected: Vec<String> = created.iter().map(|(name, _)| name.clone()).collect();
        expected.sort();
        assert_eq!(
            names(&listed),
            expected,
            "case {case:?}: expected every stored plan once in name order, got {listed:?}"
        );
        assert!(
            listed.get("NextToken").is_none(),
            "case {case:?}: one page holds the whole set, so there is no cursor, got {listed:?}"
        );

        for entry in entries(&listed) {
            let name = string_at(&entry, "RestoreTestingPlanName");
            let request = requests
                .iter()
                .find(|request| string_at(request, "RestoreTestingPlanName") == name)
                .unwrap_or_else(|| panic!("case {case:?}: {name:?} was never created"));
            let (_, create_answer) = created
                .iter()
                .find(|(created_name, _)| *created_name == name)
                .unwrap_or_else(|| panic!("case {case:?}: {name:?} was never created"));

            let prefix = plan_arn_prefix(&name);
            assert!(
                string_at(&entry, "RestoreTestingPlanArn").starts_with(&prefix),
                "case {case:?}: {name:?} expected an ARN starting {prefix:?}, got {entry:?}"
            );
            assert_eq!(
                entry.get("RestoreTestingPlanArn"),
                create_answer.get("RestoreTestingPlanArn"),
                "case {case:?}: {name:?} must be listed under the ARN its create minted, got \
                 {entry:?}"
            );
            assert_eq!(
                (
                    string_at(&entry, "ScheduleExpression"),
                    string_at(&entry, "ScheduleExpressionTimezone"),
                ),
                (
                    string_at(request, "ScheduleExpression"),
                    request
                        .get("ScheduleExpressionTimezone")
                        .and_then(Value::as_str)
                        .unwrap_or(DEFAULT_TIMEZONE)
                        .to_string(),
                ),
                "case {case:?}: {name:?} must report the schedule its create carried and the zone \
                 it is read in, got {entry:?}"
            );
            assert_eq!(
                entry.get("StartWindowHours").and_then(Value::as_i64),
                Some(
                    request
                        .get("StartWindowHours")
                        .and_then(Value::as_i64)
                        .unwrap_or(DEFAULT_START_WINDOW_HOURS)
                ),
                "case {case:?}: {name:?} must report the window in force, got {entry:?}"
            );
            assert_eq!(
                (
                    entry.get("CreationTime").cloned(),
                    entry.get("LastUpdateTime").cloned(),
                    entry.get("LastExecutionTime").cloned(),
                ),
                (
                    create_answer.get("CreationTime").cloned(),
                    create_answer.get("CreationTime").cloned(),
                    None,
                ),
                "case {case:?}: {name:?} was last written by its create, and no restore test has \
                 run with it, got {entry:?}"
            );

            // The listing and the read project ONE stored record, so they
            // cannot disagree about the plan.
            let (status, read) = get_plan(&router, &name).await;
            let stored = read_plan(&read);
            assert_eq!(
                (
                    status,
                    stored.get("RestoreTestingPlanArn").cloned(),
                    stored.get("ScheduleExpression").cloned(),
                    stored.get("ScheduleExpressionTimezone").cloned(),
                    stored.get("StartWindowHours").cloned(),
                    stored.get("CreationTime").cloned(),
                    stored.get("LastUpdateTime").cloned(),
                ),
                (
                    200,
                    entry.get("RestoreTestingPlanArn").cloned(),
                    entry.get("ScheduleExpression").cloned(),
                    entry.get("ScheduleExpressionTimezone").cloned(),
                    entry.get("StartWindowHours").cloned(),
                    entry.get("CreationTime").cloned(),
                    entry.get("LastUpdateTime").cloned(),
                ),
                "case {case:?}: {name:?} listed and read must agree, got {status} {read:?}"
            );
        }
    }
}

/// The listing is paged: a page holds at most `MaxResults` entries plus a
/// cursor, and walking the cursors yields the whole result set exactly once in
/// the order one unpaginated call reports.
#[tokio::test]
async fn a_listing_pages_by_max_results_with_a_resumable_cursor() {
    let router = router();
    let requests: Vec<Value> = (0..7)
        .map(|index| {
            plan_request(
                &format!("paged_{index}"),
                "cron(0 5 ? * * *)",
                None,
                Some(i64::from(index) + 1),
            )
        })
        .collect();
    seed(&router, &requests).await;

    let (status, whole) = list_plans(&router, "").await;
    assert_eq!(
        status, 200,
        "expected the unpaginated listing 200, got {status} {whole:?}"
    );
    let expected = names(&whole);
    assert_eq!(
        expected.len(),
        requests.len(),
        "setup: expected every plan in one page, got {whole:?}"
    );

    // Page sizes below, at and above the number of plans held, plus the
    // published minimum and maximum.
    for page_size in [1_usize, 2, 3, 7, 8, 1000] {
        let mut walked: Vec<String> = Vec::new();
        let mut query = format!("?MaxResults={page_size}");
        let mut pages = 0;
        loop {
            let (status, page) = list_plans(&router, &query).await;
            assert_eq!(
                status, 200,
                "case page_size={page_size}: expected 200, got {status} {page:?}"
            );
            let page_names = names(&page);
            assert!(
                page_names.len() <= page_size,
                "case page_size={page_size}: a page must not exceed MaxResults, got {page:?}"
            );
            walked.extend(page_names);
            pages += 1;
            assert!(
                pages <= requests.len() + 1,
                "case page_size={page_size}: the walk must terminate, got {walked:?}"
            );
            match page.get("NextToken").and_then(Value::as_str) {
                Some(token) => query = format!("?MaxResults={page_size}&NextToken={token}"),
                None => break,
            }
        }
        assert_eq!(
            walked, expected,
            "case page_size={page_size}: the pages must concatenate to the whole set in order, \
             got {walked:?}"
        );
    }
}

/// A `MaxResults` the model's `@range` admits is served; one outside it is this
/// service's `InvalidParameterValueException`.
#[tokio::test]
async fn a_max_results_outside_the_published_range_is_refused() {
    let router = router();
    seed(
        &router,
        &[plan_request("bounded_plan", "cron(0 5 ? * * *)", None, None)],
    )
    .await;

    let cases: [(&str, i64, u16); 7] = [
        ("at the published minimum", MAX_RESULTS_MIN, 200),
        ("at the published maximum", MAX_RESULTS_MAX, 200),
        ("inside the range", 42, 200),
        ("one below the published minimum", MAX_RESULTS_MIN - 1, 400),
        ("negative", -1, 400),
        ("one above the published maximum", MAX_RESULTS_MAX + 1, 400),
        ("far above", 100_000, 400),
    ];
    for (case, value, expected) in cases {
        let (status, body) = list_plans(&router, &format!("?MaxResults={value}")).await;
        assert_eq!(
            status, expected,
            "case {case:?} (MaxResults={value}): expected {expected}, got {status} {body:?}"
        );
        if expected == 400 {
            assert_eq!(
                error_code(&body).as_str(),
                "InvalidParameterValueException",
                "case {case:?} (MaxResults={value}): expected the modelled refusal, got {body:?}"
            );
            assert!(
                !refusal_message(&body).is_empty(),
                "case {case:?} (MaxResults={value}): the refusal states the condition, got \
                 {body:?}"
            );
        } else {
            assert_eq!(
                names(&body),
                vec!["bounded_plan".to_string()],
                "case {case:?} (MaxResults={value}): expected the stored plan, got {body:?}"
            );
        }
    }
}

/// A `NextToken` this result set never handed out names no page of it, and is
/// refused rather than silently answering the first page.
#[tokio::test]
async fn a_next_token_outside_this_result_set_is_refused() {
    let router = router();
    seed(
        &router,
        &[plan_request("cursored_one", "cron(0 5 ? * * *)", None, None)],
    )
    .await;

    let cases: [(&str, &str); 3] = [
        ("a bare word", "not-a-token"),
        // The hex encoding of `nevercreated` — a cursor of this service's own
        // form pointing at a plan the account does not hold.
        (
            "this service's form naming an unheld plan",
            "6e6576657263726561746564",
        ),
        ("odd-length hex", "abc"),
    ];
    for (case, token) in cases {
        let (status, body) = list_plans(&router, &format!("?NextToken={token}")).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "InvalidParameterValueException"),
            "case {case:?} ({token:?}): expected the modelled refusal, got {status} {body:?}"
        );
    }

    // A cursor the listing DID hand out resolves to the next plan.
    seed(
        &router,
        &[plan_request("cursored_two", "cron(0 6 ? * * *)", None, None)],
    )
    .await;
    let (status, first) = list_plans(&router, "?MaxResults=1").await;
    let token = string_at(&first, "NextToken");
    assert_eq!(
        (status, token.is_empty()),
        (200, false),
        "expected a cursor from the first page, got {status} {first:?}"
    );
    let (status, second) = list_plans(&router, &format!("?MaxResults=1&NextToken={token}")).await;
    assert_eq!(
        (status, names(&second)),
        (200, vec!["cursored_two".to_string()]),
        "the cursor must resume at the next plan, got {status} {second:?}"
    );
}

/// A listing is a REPORT: repeating it answers the same thing, and a plan the
/// account holds no record of is listed nowhere.
#[tokio::test]
async fn a_listing_repeats_its_answer_and_reports_only_stored_plans() {
    let router = router();
    seed(
        &router,
        &[
            plan_request("reported_one", "cron(0 5 ? * * *)", None, None),
            plan_request(
                "reported_two",
                "cron(0 6 ? * * *)",
                Some("Etc/UTC"),
                Some(12),
            ),
        ],
    )
    .await;

    let (status, first) = list_plans(&router, "").await;
    assert_eq!(
        status, 200,
        "expected ListRestoreTestingPlans 200, got {status} {first:?}"
    );
    let (status, again) = list_plans(&router, "").await;
    assert_eq!(
        (status, &again),
        (200, &first),
        "two listings of an unchanged store must agree, got {status} {again:?}"
    );

    // The names the account holds no plan under are absent from the listing,
    // and the read of one of them is still the not-found refusal, so the
    // listing invented nothing.
    for absent in ["reported_three", "Reported_One", "REPORTED_TWO"] {
        assert!(
            !names(&first).contains(&absent.to_string()),
            "case {absent:?}: a name nothing created must not be listed, got {first:?}"
        );
        let (status, refused) = get_plan(&router, absent).await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (400, "ResourceNotFoundException"),
            "case {absent:?}: a name holding no plan is not found, got {status} {refused:?}"
        );
    }
}
