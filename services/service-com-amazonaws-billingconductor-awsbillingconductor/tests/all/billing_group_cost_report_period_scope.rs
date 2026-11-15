// SPDX-License-Identifier: CC0-1.0

//! WHICH BILLING PERIODS the single-group margin summary is served for, end to end over
//! the service's real `restJson1` wire through [`MultiServiceRouter::dispatch`] — the same
//! path the production router serves.
//!
//! RULE: a billing group bills a consolidated family FOR a billing period, so a margin
//! summary report is of a group that was BILLING in the periods the request's
//! `BillingPeriodRange` covers — from the period the group was created in through the last
//! period before a delete took it — and a range that covers none of those periods
//! references a resource that doesn't exist (HTTP 404 `ResourceNotFoundException`). ONE
//! rule, so the answer does not turn on whether the store still holds the group, and the
//! periods `GetBillingGroupCostReport` is served for are exactly the periods the
//! period-scoped listings report the group in (`ListBillingGroups`,
//! `ListBillingGroupCostReports`).
//!
//! The rows differ along what the rule generalises over: how long the group's life was
//! (still live with no end at all, one period, two, thirteen crossing a year, and none at
//! all for a group created and deleted in one period), where that life falls relative to
//! the current billing period (starting in it, a month back, a year back, two years back),
//! the range the read asks about (a year before the creation, the month before it, a span
//! ending at the creation, a span straddling it, the creation month itself, the twelve
//! published months from it on, the month of the delete, a span straddling the delete, a
//! month a decade on, and no range at all), and how long that range is (one month, two,
//! the published maximum of twelve).
//!
//! `tests/all/billing_group_cost_report.rs` owns what the summary of a served period
//! CONTAINS and `tests/all/deleted_billing_group_period_reads.rs` what the period-scoped
//! reads answer once a group is deleted; this suite owns which periods the single-group
//! report answers for at all, for a live group as much as for a deleted one.
//!
//! The wire can neither create nor delete a group in a PAST billing period — a write
//! happens now — so a row writes through the real wire and then backdates the stored
//! instants, the one thing the test cannot ask the service for. The harness, the
//! backdating and the month arithmetic are the ones this crate's period-scoped suites
//! share.

use serde_json::{json, Value};

use crate::billing_group_billing_periods::{current_period, primary_account_id, Emulator};
use crate::pricing_rule_association_periods::shift_period;

/// The single-group margin summary under test, and the period-scoped listing of the same
/// summary that has to agree with it.
const GET_REPORT_PATH: &str = "/get-billing-group-cost-report";
const LIST_REPORTS_PATH: &str = "/list-billing-group-cost-reports";

/// The wire error code of a failure response (`__type`), without any namespace.
fn error_code(body: &Value) -> String {
    body["__type"]
        .as_str()
        .unwrap_or_default()
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// One row of the table: a billing group created `created` months from the current billing
/// period and, when `deleted` names a month, deleted in that one.
struct Row {
    case: &'static str,
    /// Names differ along the characters the modelled `BillingGroupName` pattern
    /// `^[a-zA-Z0-9_\+=\.\-@]+$` allows
    /// (`smithy_query(query="shape",shape="BillingGroupName")`).
    name: &'static str,
    created: i64,
    /// `None` for a group the store still holds, which has no last billing period.
    deleted: Option<i64>,
}

/// The rows differ along how long the group billed and where that life falls relative to
/// the CURRENT billing period: a live group created in it, one created a month back, one
/// created a year back, and deleted groups whose life ran for one period, for two, for
/// thirteen crossing a year, and for no period at all.
const ROWS: &[Row] = &[
    Row {
        case: "a live group created in the current billing period",
        name: "Live_Now",
        created: 0,
        deleted: None,
    },
    Row {
        case: "a live group created in the previous billing period",
        name: "Live@Prev",
        created: -1,
        deleted: None,
    },
    Row {
        case: "a live group created a year back",
        name: "Live-Year",
        created: -12,
        deleted: None,
    },
    Row {
        case: "a deleted group that billed for one period",
        name: "Gone+One",
        created: -1,
        deleted: Some(0),
    },
    Row {
        case: "a deleted group that billed for two periods, ending a month back",
        name: "Gone.Two",
        created: -3,
        deleted: Some(-1),
    },
    Row {
        case: "a deleted group that billed for thirteen periods, crossing a year",
        name: "Gone=Year",
        created: -13,
        deleted: Some(0),
    },
    Row {
        case: "a group created and deleted in one billing period, so it billed in none",
        name: "Gone_Never",
        created: -2,
        deleted: Some(-2),
    },
];

/// Was this row's group BILLING in the period `offset` months from the current one? From
/// the period it was created in through the last period before the delete, and with no end
/// at all while the store still holds it.
fn billing(row: &Row, offset: i64) -> bool {
    offset >= row.created && row.deleted.is_none_or(|deleted| offset < deleted)
}

/// The ranges one row is read over: `(case, the first month as an offset from the current
/// billing period, how many months the half-open range covers)`.
///
/// Every span is named relative to the row's own life rather than by a fixed month, so each
/// case means the same thing whatever month the suite runs in, and none is longer than the
/// twelve months the model publishes ("You can specify up to 12 months",
/// `smithy_query(query="doc",shape="GetBillingGroupCostReportInput",member="BillingPeriodRange")`).
fn probes(row: &Row) -> Vec<(String, i64, i64)> {
    let mut probes = vec![
        ("the twelve months before the creation".to_string(), row.created - 12, 12),
        ("the month a year before the creation".to_string(), row.created - 13, 1),
        ("the month before the creation".to_string(), row.created - 1, 1),
        ("the two months ending at the creation".to_string(), row.created - 2, 2),
        ("the month before the creation and the creation month".to_string(), row.created - 1, 2),
        ("the creation month alone".to_string(), row.created, 1),
        ("the twelve months from the creation on".to_string(), row.created, 12),
        ("the month after the creation".to_string(), row.created + 1, 1),
        ("a month a decade on".to_string(), row.created + 120, 1),
    ];
    if let Some(deleted) = row.deleted {
        probes.push(("the month of the delete".to_string(), deleted, 1));
        probes.push((
            "the month before the delete and the month of it".to_string(),
            deleted - 1,
            2,
        ));
        probes.push(("the twelve months from the delete on".to_string(), deleted, 12));
    }
    probes
}

/// Does `GetBillingGroupCostReport` serve the range `[start, end)` of `group`, and does
/// everything it answers hold? Answers the status so the caller can say what it expected.
async fn report_over(em: &Emulator, case: &str, group: &str, range: Option<(&str, &str)>) -> u16 {
    let mut body = json!({"Arn": group});
    if let Some((start, end)) = range {
        body["BillingPeriodRange"] = json!({
            "InclusiveStartBillingPeriod": start,
            "ExclusiveEndBillingPeriod": end,
        });
    }
    let (status, answer) = em.post(GET_REPORT_PATH, &body).await;
    if status == 200 {
        let rows = answer["BillingGroupCostReportResults"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            rows.len(),
            1,
            "case {case}: a report asking for no breakdown is one row, got {answer:?}"
        );
        assert_eq!(
            rows[0]["Arn"].as_str(),
            Some(group),
            "case {case}: expected the report to be of the group asked about, got {answer:?}"
        );
    } else {
        assert_eq!(
            status, 404,
            "case {case}: a report is either served or refused as a resource that doesn't \
             exist, got {status} with {answer:?}"
        );
        assert_eq!(
            error_code(&answer),
            "ResourceNotFoundException",
            "case {case}: expected the modelled ResourceNotFoundException, got {answer:?}"
        );
        assert_eq!(
            answer["ResourceType"].as_str(),
            Some("billinggroup"),
            "case {case}: expected the refusal to name the resource type, got {answer:?}"
        );
        assert_eq!(
            answer["ResourceId"].as_str(),
            Some(group),
            "case {case}: expected the refusal to name the group asked about, got {answer:?}"
        );
    }
    status
}

/// Does `ListBillingGroupCostReports` report an element for `group` in `period` — the
/// sibling report of the same charges, scoped to one period?
async fn listed_in(em: &Emulator, case: &str, group: &str, period: &str) -> bool {
    let (status, body) = em
        .post(
            LIST_REPORTS_PATH,
            &json!({"BillingPeriod": period, "Filters": {"BillingGroupArns": [group]}}),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case}: ListBillingGroupCostReports expected 200, got {status} with {body:?}"
    );
    body["BillingGroupCostReports"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .any(|element| element["Arn"].as_str() == Some(group))
}

#[tokio::test]
async fn a_cost_report_is_served_for_the_periods_the_group_was_billing_in() {
    let em = Emulator::new();
    let plan = em.create_plan("PeriodScopePlan").await;
    let now = current_period();

    for (index, row) in ROWS.iter().enumerate() {
        let account = primary_account_id(index);
        let group = em
            .create_group(row.name, &plan, &account, &json!({}))
            .await;
        em.created_in(&group, &shift_period(&now, row.created));
        if let Some(deleted) = row.deleted {
            em.delete_group(row.case, &group).await;
            em.deleted_in(&group, &shift_period(&now, deleted));
        }

        for (probe, first, months) in probes(row) {
            let case = format!("{} read over {probe}", row.case);
            let start = shift_period(&now, first);
            let end = shift_period(&now, first + months);
            // The range is served when it covers ANY period the group was billing in.
            let served = (first..first + months).any(|offset| billing(row, offset));
            let status = report_over(&em, &case, &group, Some((&start, &end))).await;
            assert_eq!(
                status,
                if served { 200 } else { 404 },
                "case {case}: the range {start}..{end} covers {} period the group was \
                 billing in, so HTTP {} is due; got {status}",
                if served { "a" } else { "no" },
                if served { 200 } else { 404 }
            );

            // One rule: for a range that covers exactly one period, the period-scoped
            // listings report the group for that period exactly when the single-group
            // report is served for it.
            if months == 1 {
                let listed = listed_in(&em, &case, &group, &start).await;
                assert_eq!(
                    listed, served,
                    "case {case}: ListBillingGroupCostReports reports the group for {start} = \
                     {listed}, but the single-group report answers served = {served}"
                );
                let element = em.element_in(&case, &group, Some(&start)).await;
                assert_eq!(
                    element.is_some(),
                    served,
                    "case {case}: ListBillingGroups reports the group for {start} = {:?}, but \
                     the single-group report answers served = {served}",
                    element.is_some()
                );
            }
        }

        // A request that names no range at all asks about the billing period the call is
        // made in, so it is answered exactly as the range covering that one period is.
        let case = format!("{} read over no range at all", row.case);
        let served = billing(row, 0);
        let status = report_over(&em, &case, &group, None).await;
        assert_eq!(
            status,
            if served { 200 } else { 404 },
            "case {case}: the current billing period {now} is {}a period the group was \
             billing in, so HTTP {} is due; got {status}",
            if served { "" } else { "not " },
            if served { 200 } else { 404 }
        );
        let element = em.element_in(&case, &group, None).await;
        assert_eq!(
            element.is_some(),
            served,
            "case {case}: ListBillingGroups reports the group for the current period = {:?}, \
             but the single-group report answers served = {served}",
            element.is_some()
        );
    }
}

/// RULE, the reading side: the refusal is still a READ — a report refused for a period the
/// group never billed in writes nothing, so the periods it DID bill in answer exactly as
/// they did before.
#[tokio::test]
async fn a_report_refused_for_a_period_the_group_never_billed_in_changes_nothing() {
    let em = Emulator::new();
    let plan = em.create_plan("RefusalIsAReadPlan").await;
    let now = current_period();
    let group = em
        .create_group("Refusal_Is_A_Read", &plan, &primary_account_id(0), &json!({}))
        .await;
    em.created_in(&group, &shift_period(&now, -1));

    // The periods this group really billed in, read before and after the refusals.
    let billing_months = [-1, 0];
    let mut before = Vec::new();
    for offset in billing_months {
        let period = shift_period(&now, offset);
        let case = format!("the report of {period} before any refusal");
        let status = report_over(
            &em,
            &case,
            &group,
            Some((&period, &shift_period(&now, offset + 1))),
        )
        .await;
        assert_eq!(status, 200, "case {case}: expected HTTP 200, got {status}");
        before.push(listed_in(&em, &case, &group, &period).await);
    }

    // The ranges differ along how far before the creation they fall and how many months
    // they cover, so no one refusal is the only one the rule is read through.
    for (probe, first, months) in [
        ("the month before the creation", -2, 1),
        ("the two months before the creation", -3, 2),
        ("the twelve months before the creation", -13, 12),
    ] {
        let case = format!("the refused report over {probe}");
        let status = report_over(
            &em,
            &case,
            &group,
            Some((
                &shift_period(&now, first),
                &shift_period(&now, first + months),
            )),
        )
        .await;
        assert_eq!(status, 404, "case {case}: expected HTTP 404, got {status}");
    }

    for (offset, listed_before) in billing_months.into_iter().zip(before) {
        let period = shift_period(&now, offset);
        let case = format!("the report of {period} after the refusals");
        let status = report_over(
            &em,
            &case,
            &group,
            Some((&period, &shift_period(&now, offset + 1))),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case}: a refused read changes nothing a later caller reads, so HTTP 200 is \
             still due; got {status}"
        );
        let listed_after = listed_in(&em, &case, &group, &period).await;
        assert_eq!(
            listed_after, listed_before,
            "case {case}: expected the listing of {period} to answer {listed_before} as it did \
             before the refusals, got {listed_after}"
        );
    }
}
