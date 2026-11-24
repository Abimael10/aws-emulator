// SPDX-License-Identifier: CC0-1.0

//! Every report definition a read serves states the delivery recorded against it, and no
//! more.
//!
//! The rule: the modelled `ReportStatus` is "a two element dictionary with a
//! `lastDelivery` and `lastStatus` key whose values describe the date and status of the
//! last delivered report for a particular report definition"
//! (`smithy_query(shape="ReportStatus")`), so the read path states that dictionary, with
//! both of its keys, for every report definition a delivery HAS been recorded against —
//! the instant of that delivery and the outcome it ended with — and states no
//! `ReportStatus` at all for one none has: the member's modelled constraints admit only
//! the values a delivery has, so there is nothing a report nothing delivered can fill it
//! with.
//!
//! The rows span what the rule generalises over: how many definitions the account holds
//! (one, several, more than one page's worth), which position in the page a definition
//! sits at, which of them have a delivery recorded, how that delivery ended (every value
//! of the modelled `LastStatus` catalog), the instant it was delivered, the page size the
//! request asks for, and which call the read follows (a create, each update, a refused
//! duplicate create, a tagging, a delete).

use serde_json::{json, Value};

use service_com_amazonaws_costandusagereportservice_awsorigamiservicegatewayservice as cur;

use cur::state::{ReportDelivery, StoredReport};

use super::report_definition_lifecycle::{
    assert_delivery_status, at_epoch_seconds, minimal_definition, recorded_deliveries,
    stored_report_at, Cur,
};

/// A stored report called `report_name` carrying `last_delivery`, created at one fixed
/// instant: this suite is about the delivery status, not about the creation date.
///
/// Built through the one seeded-report builder the suites of this target share
/// ([`super::report_definition_lifecycle::stored_report_at`]).
fn stored_with(report_name: &str, last_delivery: Option<ReportDelivery>) -> StoredReport {
    stored_report_at(report_name, at_epoch_seconds(1_767_225_600), last_delivery)
}

/// The deliveries a seeded store is built from: the histories every suite of this target
/// shares ([`super::report_definition_lifecycle::recorded_deliveries`]), with the labels
/// dropped — no delivery, and one per value of the modelled `LastStatus` catalog, each at
/// a different instant.
fn deliveries() -> Vec<Option<ReportDelivery>> {
    recorded_deliveries()
        .into_iter()
        .map(|(_, delivery)| delivery)
        .collect()
}

/// The rows of [`deliveries`] a delivery WAS recorded for, in the order the shared table
/// states them.
fn recorded() -> Vec<ReportDelivery> {
    deliveries().into_iter().flatten().collect()
}

/// How many definitions one page carries when a request names no `MaxResults`: the
/// modelled `MaxResults` is pinned to 5 (its range is 5..5), so a store holding more than
/// this is a store the read pages over.
const PAGE_SIZE: usize = 5;

#[tokio::test]
async fn every_definition_a_read_serves_states_the_delivery_recorded_against_it() {
    // Rows differ in how many definitions the account holds and in which of them have a
    // delivery recorded: a single undelivered report (the recorded case), a single
    // delivered one, a mixed account, every report delivered, and an account holding more
    // definitions than one page carries — so the rule is asserted on every element of
    // every page, not on the first element of the first one. Every row is built from the
    // shared delivery table rather than from fixed positions in it, so a change to the
    // modelled catalog widens the rows instead of breaking them.
    let recorded = recorded();
    assert!(
        !recorded.is_empty(),
        "expected the modelled LastStatus catalog to name at least one delivery outcome"
    );
    // An undelivered report beside each delivered one, so the two kinds sit next to each
    // other whatever the catalog names.
    let mixed: Vec<Option<ReportDelivery>> = recorded
        .iter()
        .flat_map(|delivery| [None, Some(delivery.clone())])
        .collect();
    let every: Vec<Option<ReportDelivery>> = recorded.iter().cloned().map(Some).collect();
    let mut across_pages = mixed.clone();
    across_pages.push(None);
    while across_pages.len() <= PAGE_SIZE {
        across_pages.push(None);
    }

    let table: Vec<(&str, Vec<Option<ReportDelivery>>)> = vec![
        ("one report, nothing delivered", vec![None]),
        ("one report, delivered", vec![recorded.first().cloned()]),
        ("a mixed account", mixed),
        ("every report delivered", every),
        ("more reports than one page carries", across_pages),
    ];

    for (case, deliveries) in table {
        let cur = Cur::new();
        cur.seed(
            deliveries
                .iter()
                .enumerate()
                .map(|(index, delivery)| {
                    stored_with(&format!("report-{index}"), delivery.clone())
                })
                .collect(),
        );

        let listed = cur.describe_all().await;
        assert_eq!(
            listed.len(),
            deliveries.len(),
            "case {case:?}: expected every seeded report to be listed, got {listed:?}"
        );
        for (index, reported) in listed.iter().enumerate() {
            let case = format!("{case} / element {index}");
            assert_eq!(
                reported["ReportName"],
                json!(format!("report-{index}")),
                "case {case:?}: expected the reports in creation order, got {listed:?}"
            );
            assert_delivery_status(&case, reported, deliveries[index].as_ref());
        }
    }
}

#[tokio::test]
async fn a_requested_page_states_the_delivery_status_of_each_of_its_elements() {
    // The page size the request asks for changes which definitions one response carries,
    // and the rule holds on each element of each page: the modelled `MaxResults` is pinned
    // to 5 (`smithy_query(operation="DescribeReportDefinitions")`), so the sizes spanned
    // are an absent one and that bound.
    let delivered = ReportDelivery {
        at: at_epoch_seconds(1_700_000_000),
        status: "SUCCESS".to_string(),
    };
    let seeded: Vec<Option<ReportDelivery>> = vec![
        None,
        Some(delivered.clone()),
        None,
        None,
        Some(delivered),
        None,
    ];

    let requests = [
        ("no MaxResults", None),
        ("the modelled MaxResults", Some(PAGE_SIZE)),
    ];
    for (case, requested) in requests {
        let cur = Cur::new();
        cur.seed(
            seeded
                .iter()
                .enumerate()
                .map(|(index, delivery)| {
                    stored_with(&format!("paged-report-{index}"), delivery.clone())
                })
                .collect(),
        );

        let mut position = 0_usize;
        let mut page = match requested {
            Some(max) => json!({ "MaxResults": max }),
            None => json!({}),
        };
        loop {
            let (status, body) = cur.describe(&page).await;
            assert_eq!(status, 200, "case {case:?}: got {body:?}");
            let definitions = body["ReportDefinitions"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            for reported in &definitions {
                let case = format!("{case} / element {position}");
                assert_delivery_status(&case, reported, seeded[position].as_ref());
                position += 1;
            }
            match body["NextToken"].as_str() {
                Some(token) => page = json!({ "NextToken": token }),
                None => break,
            }
        }
        assert_eq!(
            position,
            seeded.len(),
            "case {case:?}: expected every seeded report to be paged over"
        );
    }
}

#[tokio::test]
async fn the_delivery_status_is_stated_whatever_call_the_read_follows() {
    // Rows differ in the call the read follows: the create, each update, a create refused
    // as a duplicate name, a tagging, and a delete. Nothing in this service's modelled
    // surface delivers a report, so no read after any of them states a delivery status.
    let cur = Cur::new();
    let (status, body) = cur.put(&minimal_definition("lifecycle-report"), None).await;
    assert_eq!(status, 200, "case create: got {body:?}");

    let mut renamed = minimal_definition("lifecycle-report");
    renamed["TimeUnit"] = json!("HOURLY");
    let mut second_update = minimal_definition("renamed-report");
    second_update["Compression"] = json!("ZIP");

    let calls: Vec<(&str, &str, Value, u16)> = vec![
        (
            "after the first update",
            "ModifyReportDefinition",
            json!({ "ReportName": "lifecycle-report", "ReportDefinition": renamed }),
            200,
        ),
        (
            "after a rename",
            "ModifyReportDefinition",
            json!({
                "ReportName": "lifecycle-report",
                "ReportDefinition": minimal_definition("renamed-report"),
            }),
            200,
        ),
        (
            "after the second update",
            "ModifyReportDefinition",
            json!({ "ReportName": "renamed-report", "ReportDefinition": second_update }),
            200,
        ),
        (
            "after a create refused as a duplicate name",
            "PutReportDefinition",
            json!({ "ReportDefinition": minimal_definition("renamed-report") }),
            400,
        ),
        (
            "after a tagging",
            "TagResource",
            json!({
                "ReportName": "renamed-report",
                "Tags": [{ "Key": "team", "Value": "fin" }],
            }),
            200,
        ),
        (
            "after a second report is created",
            "PutReportDefinition",
            json!({ "ReportDefinition": minimal_definition("second-report") }),
            200,
        ),
        (
            "after the second report is deleted",
            "DeleteReportDefinition",
            json!({ "ReportName": "second-report" }),
            200,
        ),
    ];

    for (case, operation, request, expected_status) in calls {
        let (status, body) = cur.call(operation, &request).await;
        assert_eq!(
            status, expected_status,
            "case {case:?}: expected {operation} to answer {expected_status}, got {status} \
             {body:?}"
        );

        let listed = cur.describe_all().await;
        assert!(
            !listed.is_empty(),
            "case {case:?}: expected the account to still hold a report, got {listed:?}"
        );
        for (index, reported) in listed.iter().enumerate() {
            assert_delivery_status(&format!("{case} / element {index}"), reported, None);
        }
    }
}

#[tokio::test]
async fn a_recorded_delivery_is_what_the_status_values_come_from() {
    // One row per delivery history, so the VALUES are pinned against the record they come
    // from rather than against one moment: no delivery, and every value of the modelled
    // `LastStatus` catalog at a different instant. The reported instant is checked for the
    // form this service states a report timestamp in, and for not moving between reads.
    for delivery in deliveries() {
        let case = match &delivery {
            None => "no delivery recorded".to_string(),
            Some(delivery) => format!("a delivery that ended {}", delivery.status),
        };
        let cur = Cur::new();
        cur.seed(vec![stored_with("status-report", delivery.clone())]);

        let first = cur.describe_all().await;
        let reported = first.first().cloned().unwrap_or(Value::Null);
        assert_delivery_status(&case, &reported, delivery.as_ref());
        assert_eq!(
            cur.describe_all().await,
            first,
            "case {case:?}: a delivery status must not move between reads, got {first:?}"
        );
    }
}
