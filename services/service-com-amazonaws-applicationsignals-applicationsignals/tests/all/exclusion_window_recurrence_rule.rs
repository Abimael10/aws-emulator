// SPDX-License-Identifier: CC0-1.0

//! The `RecurrenceRule` every exclusion window a read reports carries.
//!
//! The rule under test: an exclusion window is reported with its recurrence rule built from
//! the window's stored recurrence, so the member is on EVERY window
//! `ListServiceLevelObjectiveExclusionWindows` reports — carrying exactly the cron or rate
//! expression the window was added with, and carrying no expression for a window that recurs
//! on no schedule. A window named back in the form the read reported it in is the window the
//! read reported, so what a read hands a client is what a `RemoveExclusionWindows` can take
//! away.
//!
//! Every test drives the real `restJson1` surface through the router that production mounts,
//! and the rule is asserted over a TABLE of recurrences: no rule at all, a cron expression, a
//! rate expression and an expression at each published bound of the member's own
//! `@length 1..1024` (`smithy_query(shape="RecurrenceRule",member="Expression")`).
//!
//! The request helpers are the ones the `slo_lifecycle` and `exclusion_windows` suites
//! already drive this service's surface with, so every suite speaks to one harness.

use serde_json::json;

use crate::exclusion_windows::{batch, list, window, windows_of};
use crate::slo_lifecycle::{create, router, sli_config};

/// One recurrence a row adds its window with, and the `Expression` the read must report for
/// it.
#[derive(Debug)]
struct Row {
    label: &'static str,
    /// The `RecurrenceRule.Expression` the addition states, or `None` for an addition that
    /// names no recurrence rule at all.
    added: Option<String>,
}

/// The table: the recurrence forms the member documents ("supports both cron and rate
/// expressions"), the absence of one, and the expression length at each published bound.
fn rows() -> Vec<Row> {
    vec![
        Row {
            label: "no recurrence rule at all",
            added: None,
        },
        Row {
            label: "a cron expression",
            added: Some("cron(0 2 ? * SUN *)".to_string()),
        },
        Row {
            label: "a rate expression",
            added: Some("rate(7 days)".to_string()),
        },
        Row {
            label: "an expression at the modelled minimum length of one",
            added: Some("*".to_string()),
        },
        Row {
            label: "an expression at the modelled maximum length of 1024",
            added: Some(format!("cron({})", "a".repeat(1024 - "cron()".len()))),
        },
    ]
}

/// Create one SLO and answer the name it is addressed by.
async fn slo(router: &smithy_server_runtime::MultiServiceRouter, name: &str) -> String {
    let (status, answer) = create(
        router,
        json!({ "Name": name, "SliConfig": sli_config("checkout", "GET /orders", 200.0) }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {name:?}: expected the SLO create to answer 200, got {status} with {answer}"
    );
    name.to_string()
}

/// Every window a read reports carries the `RecurrenceRule` member, and its `Expression` is
/// the one the window was added with — absent for a window added without a recurrence.
#[tokio::test]
async fn every_listed_window_carries_its_recurrence_rule() {
    for row in rows() {
        let router = router();
        let name = slo(&router, "checkout-latency").await;
        let added = window(
            "HOUR",
            2,
            None,
            row.added.as_deref(),
            Some("scheduled maintenance"),
        );
        let (status, answer) =
            batch(&router, std::slice::from_ref(&name), std::slice::from_ref(&added), &[]).await;
        assert_eq!(
            status, 200,
            "case {:?}: expected the batch update to answer 200, got {status} with {answer}",
            row.label
        );

        let (status, page) = list(&router, &name, "").await;
        assert_eq!(
            status, 200,
            "case {:?}: expected the listing to answer 200, got {status} with {page}",
            row.label
        );
        let reported = windows_of(&page);
        assert_eq!(
            reported.len(),
            1,
            "case {:?}: expected one stored window, got {page}",
            row.label
        );
        let listed = &reported[0];
        assert!(
            listed.get("RecurrenceRule").is_some(),
            "case {:?}: expected the window to carry a RecurrenceRule member, got {listed}",
            row.label
        );
        assert_eq!(
            listed["RecurrenceRule"]["Expression"].as_str(),
            row.added.as_deref(),
            "case {:?}: expected the expression the window was added with, got {:?}",
            row.label,
            listed["RecurrenceRule"]
        );
        // The rest of the configuration is untouched by the recurrence the row states.
        assert_eq!(
            listed["Window"],
            json!({ "DurationUnit": "HOUR", "Duration": 2 }),
            "case {:?}: expected the window sent, got {listed}",
            row.label
        );
        assert_eq!(
            listed["Reason"].as_str(),
            Some("scheduled maintenance"),
            "case {:?}: expected the reason sent, got {listed}",
            row.label
        );
    }
}

/// The window a read reported is the window a removal names, whether the removal echoes the
/// `RecurrenceRule` the read carried or states the recurrence the way the addition did.
///
/// The rows are the same recurrence table: for each, the window is removed twice over — once
/// by the configuration the read reported, once by the configuration the addition stated —
/// and both name the one stored window.
#[tokio::test]
async fn a_removal_names_the_window_a_read_reported() {
    for row in rows() {
        for echoed in [true, false] {
            let case = format!(
                "{} removed by the {} configuration",
                row.label,
                if echoed { "reported" } else { "added" }
            );
            let router = router();
            let name = slo(&router, "checkout-latency").await;
            let added = window(
                "HOUR",
                2,
                None,
                row.added.as_deref(),
                Some("scheduled maintenance"),
            );
            let (status, answer) =
                batch(&router, std::slice::from_ref(&name), std::slice::from_ref(&added), &[]).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected the batch update to answer 200, got {status} with {answer}"
            );

            let (_, page) = list(&router, &name, "").await;
            let reported = windows_of(&page);
            assert_eq!(
                reported.len(),
                1,
                "case {case:?}: expected one stored window, got {page}"
            );
            // The configuration the removal names: the one the read reported (which carries
            // the RecurrenceRule structure) or the one the addition stated, with the
            // StartTime the read reported for it either way, since that is the member the
            // service settled.
            let named = if echoed {
                reported[0].clone()
            } else {
                let mut named = added.clone();
                named["StartTime"] = reported[0]["StartTime"].clone();
                named
            };

            let (status, answer) =
                batch(&router, std::slice::from_ref(&name), &[], std::slice::from_ref(&named))
                    .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected the removal to answer 200, got {status} with {answer}"
            );
            let (_, page) = list(&router, &name, "").await;
            assert!(
                windows_of(&page).is_empty(),
                "case {case:?}: expected the removal of {named} to take the window away, got \
                 {page}"
            );
        }
    }
}
