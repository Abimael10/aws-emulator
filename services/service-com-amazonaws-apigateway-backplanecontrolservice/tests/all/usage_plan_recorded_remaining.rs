// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the USAGE DATA a grant records and the sentences a read of it is
//! refused with: `UpdateUsage` against `UpdateUsagePlan`, and `GetUsage`'s own refusals.
//!
//! Every case drives the real wire path — a `restJson1` HTTP request through the
//! multi-service router into the mounted service, and the `execute-api` data plane on the
//! api's own invoke host — so what is asserted is what a client sees, not what a handler
//! returns. The emulator, the metered api, the API keys, the metering plan, the grant and
//! the report reader are the harness the sibling suites over the usage report
//! (`usage_plan_usage_report`) and the grant (`usage_plan_quota_extension`) already stand up.
//!
//! The RULES under test, asserted over SETS of inputs rather than one recorded call:
//!
//! * a grant of remaining quota RECORDS what it states for that `(usage plan, API key)` pair
//!   — `UpdateUsage` "grants a temporary extension to the remaining quota of a usage plan
//!   associated with a specified API key" (`smithy_query(query="operation",
//!   operation="UpdateUsage")`) — so the daily `[used quota, remaining quota]` entry
//!   (`Usage$items`) of a day already recorded is a fact about that day: a later
//!   `UpdateUsagePlan` that replaces the plan's `QuotaSettings` is reflected in the plan
//!   immediately and rewrites none of it, and the next request from the key is admitted
//!   against the recorded remaining rather than against the plan's new limit — over a table
//!   spanning which quota member is patched, both metering periods either side of the patch,
//!   whether the new limit is above or below the grant, and whether the grant left the key
//!   anything at all;
//! * an interval wider than the window the usage data is available over is refused 400
//!   `BadRequestException` with the service's own fixed sentence for that condition, the same
//!   sentence however far past the bound the interval reaches and with no date of the request
//!   in it, while the interval AT the bound is served;
//! * a `keyId` that names no API key the plan meters is refused 404 `NotFoundException` with
//!   the sentence a READ of the usage data words that condition with, which is not the
//!   sentence the GRANT of a remaining quota words the same condition with.

use serde_json::{json, Value};
use service_com_amazonaws_apigateway_backplanecontrolservice::state::{
    MAX_USAGE_INTERVAL_DAYS, NO_SUCH_METERED_API_KEY, NO_SUCH_REPORTED_API_KEY,
};

use crate::rest_api_lifecycle::{assert_failure, string_member, Answer, Gateway};
use crate::usage_plan_lifecycle::get_usage_plan;
use crate::usage_plan_quota_extension::{remaining_patch, update_usage};
use crate::usage_plan_usage_report::{
    api_key, date, get_usage, invoke, log_of, metered_api, metering_plan, plus, send_one, today,
    LIMIT_EXCEEDED,
};

/// `UpdateUsagePlan` for `plan`, carrying one `replace` of each `(path, value)` pair.
async fn repoint_quota(gateway: &Gateway, plan: &str, patches: &[(&str, &str)]) -> Answer {
    let operations: Vec<Value> = patches
        .iter()
        .map(|(path, value)| json!({ "op": "replace", "path": path, "value": value }))
        .collect();
    gateway
        .call(
            "PATCH",
            &format!("/usageplans/{plan}"),
            Some(json!({ "patchOperations": operations })),
        )
        .await
}

/// Assert `body` reports the plan's quota as `patches` just repointed it: every patched
/// member, read back from the body the plan answers with.
fn assert_quota_reported(body: &Value, patches: &[(&str, &str)], case: &str) {
    for (path, value) in patches {
        let member = path
            .strip_prefix("/quota/")
            .unwrap_or_else(|| panic!("case {case}: {path} names no quota member"));
        let reported = &body["quota"][member];
        let matched = match value.parse::<i64>() {
            Ok(count) => reported.as_i64() == Some(count),
            Err(_) => reported.as_str() == Some(*value),
        };
        assert!(
            matched,
            "case {case}: expected the plan to report {member} as {value:?}, got {reported:?}"
        );
    }
}

/// One row of the repoint table: the quota the plan is created with, what the key had sent
/// and been granted before the plan was edited, and the edit itself.
struct Row {
    case: &'static str,
    /// The metering period of the plan's quota, from the modelled `QuotaPeriodType`
    /// (`smithy_query(query="shape", shape="QuotaPeriodType")`: DAY, WEEK, MONTH).
    period: &'static str,
    /// The plan's `quota` limit and `offset` at creation.
    limit: i64,
    offset: i64,
    /// How many requests the key sends before the grant; every one of them is admitted.
    sent: i64,
    /// The remaining quota the grant records for the day it is made on.
    granted: i64,
    /// The `replace` operations one `UpdateUsagePlan` then applies to the plan's quota.
    patches: &'static [(&'static str, &'static str)],
}

/// The rows differ along which quota member the plan edit repoints, the metering period
/// either side of the edit, whether the new limit is above or below the recorded grant, and
/// whether the grant left the key anything at all.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the period repointed from a day to a week and the limit raised",
            period: "DAY",
            limit: 1_000,
            offset: 0,
            sent: 0,
            granted: 200,
            patches: &[("/quota/limit", "5000"), ("/quota/period", "WEEK")],
        },
        Row {
            case: "the limit alone raised",
            period: "DAY",
            limit: 1_000,
            offset: 0,
            sent: 2,
            granted: 5,
            patches: &[("/quota/limit", "5000")],
        },
        Row {
            case: "the limit alone lowered below the recorded grant",
            period: "WEEK",
            limit: 100,
            offset: 0,
            sent: 1,
            granted: 50,
            patches: &[("/quota/limit", "7")],
        },
        Row {
            case: "the period alone repointed from a week to a month",
            period: "WEEK",
            limit: 100,
            offset: 6,
            sent: 3,
            granted: 4,
            patches: &[("/quota/period", "MONTH")],
        },
        Row {
            case: "the period repointed from a month to a day and the limit lowered",
            period: "MONTH",
            limit: 1_000,
            offset: 0,
            sent: 1,
            granted: 2,
            patches: &[("/quota/limit", "3"), ("/quota/period", "DAY")],
        },
        Row {
            case: "a grant of nothing, with the limit raised and an offset added",
            period: "WEEK",
            limit: 50,
            offset: 0,
            sent: 1,
            granted: 0,
            patches: &[("/quota/limit", "500"), ("/quota/offset", "6")],
        },
    ]
}

#[tokio::test]
async fn a_recorded_remaining_survives_an_edit_of_the_plans_quota() {
    let gateway = Gateway::new();
    let api = metered_api(&gateway, "usage-recorded-remaining").await;
    let day = date(today());

    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let (key_id, key_value) = api_key(&gateway, case, 400 + index).await;
        let plan = metering_plan(
            &gateway,
            case,
            Some(json!({
                "limit": row.limit,
                "offset": row.offset,
                "period": row.period,
            })),
            &api,
            ::std::slice::from_ref(&key_id),
        )
        .await;
        for _ in 0..row.sent {
            send_one(&gateway, &api, &key_value, case).await;
        }

        // The grant: what the metering period has left for this key from now on.
        let granted = update_usage(
            &gateway,
            &plan,
            &key_id,
            remaining_patch("replace", row.granted),
        )
        .await;
        assert_eq!(
            granted.status, 200,
            "case {case}: expected the grant to be served, got {} with {:?}",
            granted.status, granted.body
        );
        let recorded = vec![vec![row.sent, row.granted]];
        let before = get_usage(&gateway, &plan, &day, &day, "").await;
        assert_eq!(
            log_of(&before.body, &key_id, case),
            recorded,
            "case {case}: expected the grant to be recorded as {recorded:?}, got {:?}",
            before.body["values"]
        );

        // The plan edit: reflected in the plan at once…
        let repointed = repoint_quota(&gateway, &plan, row.patches).await;
        assert_eq!(
            repointed.status, 200,
            "case {case}: expected the plan edit to be served, got {} with {:?}",
            repointed.status, repointed.body
        );
        assert_quota_reported(&repointed.body, row.patches, case);
        let read = get_usage_plan(&gateway, &plan).await;
        assert_eq!(
            read.status, 200,
            "case {case}: expected the plan to be read back, got {} with {:?}",
            read.status, read.body
        );
        assert_quota_reported(&read.body, row.patches, case);
        assert_eq!(
            string_member(&read.body, "id", case),
            plan,
            "case {case}: expected the plan the edit was made on, got {:?}",
            read.body["id"]
        );

        // … and rewriting nothing of the day already recorded.
        let after = get_usage(&gateway, &plan, &day, &day, "").await;
        assert_eq!(
            log_of(&after.body, &key_id, case),
            recorded,
            "case {case}: expected the recorded entry to survive the plan edit as \
             {recorded:?}, got {:?}",
            after.body["values"]
        );

        // The number the next request is admitted against is that same recorded remaining,
        // not the plan's new limit: a grant that left the key nothing admits nothing.
        let sent = invoke(&gateway, &api, &key_value).await;
        if row.granted > 0 {
            assert_eq!(
                sent.status, 200,
                "case {case}: expected the recorded remaining to admit one more request, got \
                 {} with {:?}",
                sent.status, sent.body
            );
        } else {
            assert_eq!(
                sent.status, 429,
                "case {case}: expected a recorded remaining of nothing to admit no request, \
                 got {} with {:?}",
                sent.status, sent.body
            );
            assert_eq!(
                sent.message(),
                LIMIT_EXCEEDED,
                "case {case}: expected {LIMIT_EXCEEDED:?} past the recorded remaining, got {:?}",
                sent.message()
            );
        }
        let spent = get_usage(&gateway, &plan, &day, &day, "").await;
        let counted = if row.granted > 0 {
            vec![vec![row.sent + 1, row.granted - 1]]
        } else {
            recorded.clone()
        };
        assert_eq!(
            log_of(&spent.body, &key_id, case),
            counted,
            "case {case}: expected the request to be counted down from the recorded \
             remaining as {counted:?}, got {:?}",
            spent.body["values"]
        );
    }
}

/// The sentence API Gateway refuses an OVER-WIDE usage date range with, as the recorded
/// refusal words it: wholly fixed text naming the window the usage data is available over,
/// with nothing of the request in it.
const OVER_WIDE_RANGE_SENTENCE: &str = "Usage date range cannot exceed 90 days";

#[tokio::test]
async fn an_over_wide_usage_date_range_is_refused_with_the_services_sentence() {
    let gateway = Gateway::new();
    let api = metered_api(&gateway, "usage-over-wide-range").await;
    let case = "the plan every row reads";
    let (key_id, key_value) = api_key(&gateway, case, 500).await;
    let plan = metering_plan(
        &gateway,
        case,
        Some(json!({ "limit": 1_000, "offset": 0, "period": "MONTH" })),
        &api,
        ::std::slice::from_ref(&key_id),
    )
    .await;
    send_one(&gateway, &api, &key_value, case).await;

    // Rows differ along HOW FAR past the bound the interval reaches and where it sits in the
    // calendar: one day past it, ten, the span a runbook asks a year back over, a decade, a
    // whole leap year, and a pair of fixed calendar dates owing nothing to the day the test
    // runs.
    let today = today();
    let bound = MAX_USAGE_INTERVAL_DAYS;
    let rows: Vec<(&str, String, String)> = vec![
        (
            "one day past the bound",
            date(today),
            date(plus(today, bound)),
        ),
        (
            "ten days past the bound",
            date(today),
            date(plus(today, bound + 9)),
        ),
        (
            "more than a year",
            date(today),
            date(plus(today, 400)),
        ),
        ("a decade", date(today), date(plus(today, 3_653))),
        (
            "a whole leap year of fixed calendar dates",
            "2028-01-01".to_string(),
            "2028-12-31".to_string(),
        ),
        (
            "a fixed calendar quarter and a day",
            "2026-01-01".to_string(),
            "2026-04-01".to_string(),
        ),
    ];
    let mut sentences: Vec<String> = Vec::new();
    for (case, start, end) in &rows {
        let refused = get_usage(&gateway, &plan, start, end, "").await;
        assert_failure(&refused, 400, "BadRequestException", case);
        let message = refused.body["message"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case}: expected a message, got {:?}", refused.body));
        assert_eq!(
            message, OVER_WIDE_RANGE_SENTENCE,
            "case {case}: expected {OVER_WIDE_RANGE_SENTENCE:?}, got {message:?}"
        );
        // Fixed text: neither endpoint of the rejected interval is quoted back.
        assert!(
            !message.contains(start.as_str()) && !message.contains(end.as_str()),
            "case {case}: expected no date of the request in the sentence, got {message:?}"
        );
        sentences.push(message.to_string());
    }
    sentences.dedup();
    assert_eq!(
        sentences.len(),
        1,
        "case every over-wide range: expected one sentence for the condition, got {sentences:?}"
    );

    // The CONTROLS: the interval AT the bound is served, and so is the narrowest one, so the
    // rows above are refused for the width and not for the read.
    for (case, through) in [
        ("the interval at the bound", plus(today, bound - 1)),
        ("an interval of one day", today),
    ] {
        let served = get_usage(&gateway, &plan, &date(today), &date(through), "").await;
        assert_eq!(
            served.status, 200,
            "case {case}: expected 200, got {} with {:?}",
            served.status, served.body
        );
        assert!(
            !log_of(&served.body, &key_id, case).is_empty(),
            "case {case}: expected the key's daily log, got {:?}",
            served.body["values"]
        );
    }
}

#[tokio::test]
async fn an_unknown_key_id_is_refused_with_the_reports_own_sentence() {
    let gateway = Gateway::new();
    let api = metered_api(&gateway, "usage-unknown-key").await;
    let day = date(today());
    let case = "the plan every row reads";

    let (key_id, _) = api_key(&gateway, case, 600).await;
    let (unmetered, _) = api_key(&gateway, case, 601).await;
    let (deleted, _) = api_key(&gateway, case, 602).await;
    let plan = metering_plan(
        &gateway,
        case,
        Some(json!({ "limit": 5, "offset": 0, "period": "DAY" })),
        &api,
        &[key_id.clone(), deleted.clone()],
    )
    .await;
    let dropped = gateway
        .call("DELETE", &format!("/apikeys/{deleted}"), None)
        .await;
    assert_eq!(
        dropped.status, 202,
        "case {case}: expected the key to be deleted, got {} with {:?}",
        dropped.status, dropped.body
    );

    // A grant on the real key, so the plan HAS usage data to report: the rows below are
    // refused for the key they name and not for the report being empty.
    let granted = update_usage(&gateway, &plan, &key_id, remaining_patch("replace", 3)).await;
    assert_eq!(
        granted.status, 200,
        "case {case}: expected the grant to be served, got {} with {:?}",
        granted.status, granted.body
    );

    // Rows differ in the FORM the unknown id takes: a value no key ever had, one shaped like
    // a minted id, another resource's id, a key the account no longer holds, and a stored key
    // this plan does not meter.
    for (case, unknown) in [
        ("a value no API key ever had", "no-such-key".to_string()),
        ("a value shaped like a minted key id", "zzzzzzzzzz".to_string()),
        ("the plan's own id as the key", plan.clone()),
        ("a key the account no longer holds", deleted.clone()),
        ("a stored key the plan does not meter", unmetered.clone()),
    ] {
        let refused = get_usage(&gateway, &plan, &day, &day, &format!("&keyId={unknown}")).await;
        assert_failure(&refused, 404, "NotFoundException", case);
        assert_eq!(
            refused.body["message"].as_str(),
            Some(NO_SUCH_REPORTED_API_KEY),
            "case {case}: expected {NO_SUCH_REPORTED_API_KEY:?}, got {:?}",
            refused.body["message"]
        );

        // The GRANT on the same id is refused the same status and code with its OWN sentence:
        // the two operations word the condition apart, so neither may be read off the other.
        let grant = update_usage(&gateway, &plan, &unknown, remaining_patch("replace", 1)).await;
        assert_failure(&grant, 404, "NotFoundException", case);
        assert_eq!(
            grant.body["message"].as_str(),
            Some(NO_SUCH_METERED_API_KEY),
            "case {case}: expected {NO_SUCH_METERED_API_KEY:?} from the grant, got {:?}",
            grant.body["message"]
        );
    }
    assert_ne!(
        NO_SUCH_REPORTED_API_KEY, NO_SUCH_METERED_API_KEY,
        "the report and the grant must word the unknown key apart, got \
         {NO_SUCH_REPORTED_API_KEY:?} for both"
    );

    // The CONTROL: the real metered key still reports what the grant recorded.
    let served = get_usage(&gateway, &plan, &day, &day, &format!("&keyId={key_id}")).await;
    assert_eq!(
        served.status, 200,
        "case a keyId the plan meters: expected 200, got {} with {:?}",
        served.status, served.body
    );
    assert_eq!(
        log_of(&served.body, &key_id, "a keyId the plan meters"),
        vec![vec![0, 3]],
        "case a keyId the plan meters: expected the recorded remaining, got {:?}",
        served.body["values"]
    );
}
