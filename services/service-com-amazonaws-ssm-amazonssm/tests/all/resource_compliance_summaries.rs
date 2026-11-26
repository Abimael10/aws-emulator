// SPDX-License-Identifier: CC0-1.0

//! The per-RESOURCE roll-up of a Region's compliance, end to end: `PutComplianceItems` writes
//! the items and `ListResourceComplianceSummaries` reports the summary computed from them.
//!
//! RULE under test: **`ListResourceComplianceSummaries` "returns a resource-level summary count.
//! The summary includes information about compliant and non-compliant statuses and detailed
//! compliance-item severity counts, according to the filter criteria you specify", so every
//! resource the Region holds compliance items about is answered with one
//! `ResourceComplianceSummaryItem` per compliance type, COMPUTED from those items: each count
//! and each `SeveritySummary` count is of that resource's ITEMS of that type under the status it
//! sits beside ("a list of items that are compliant for the resource"), `Status` is "the
//! compliance status for the resource" — compliant only while every one of those items is —
//! `OverallSeverity` is "the highest severity item found for the resource", and
//! `ExecutionSummary` is the execution that reported them last**
//! (`smithy_query(query="closure", operation="ListResourceComplianceSummaries")`). Around that
//! computation: a `Filters` entry narrows the items the roll-up is over, a key that names no
//! member of a compliance item is refused `InvalidFilter`, the page is capped by `MaxResults`
//! with a `NextToken` that resumes after the summary it stopped at, and a token this read did
//! not mint — a token of the type-level roll-up beside it included — is refused
//! `InvalidNextToken`.
//!
//! Every case goes through the front door a client uses: the shared harness in
//! `all/parameter_store.rs` dispatches `POST /` with `X-Amz-Target: AmazonSSM.<Operation>` and an
//! `awsJson1_1` body. Nothing is computed in the test: every item a summary counts was written
//! through `PutComplianceItems` itself (the put and the node seeding are `compliance_items`' own,
//! so the compliance suites write compliance one way), and the ONE thing seeded through the
//! engine is the registered managed node, because no call of this service registers one.
//!
//! The rows span what the rule generalises over: how many resources hold items, how many items
//! each holds of a type, how many types it holds, whether those items agree on the status, which
//! severities they carry, which execution reported each, the filter key and comparison the read
//! narrows by, and the page size it asks for.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::compliance_items::{put, register, MANAGED_INSTANCE};
use crate::parameter_store::{call_in_region, error_code, router_with_engine, REGION};

/// The nodes the cases report compliance about: node ids of the forms the modelled `InstanceId`
/// pattern admits.
const FIRST_NODE: &str = "i-0123456789abcdef0";
const SECOND_NODE: &str = "i-0abc1234";

/// Two compliance types of the caller's own namespace ("specify `Association` …, `Patch`, or
/// `Custom:string`", `PutComplianceItemsRequest$ComplianceType`), so a case covers a resource
/// holding more than one type at once.
const SCAN_TYPE: &str = "Custom:Scan";
const AUDIT_TYPE: &str = "Custom:Audit";

/// The two statuses an item carries, and the two a summary reports
/// (`ComplianceItem$Status`, `ResourceComplianceSummaryItem$Status`).
const COMPLIANT: &str = "COMPLIANT";
const NON_COMPLIANT: &str = "NON_COMPLIANT";

/// What a row expects one summary to report: `(Status, OverallSeverity, the compliant items it
/// counts, the items that are not)`.
type Expected = (&'static str, &'static str, i64, i64);

/// One row of the table of summaries: what it covers, the resource and compliance type it is
/// about, and what that summary must report.
type SummaryRow = (&'static str, &'static str, &'static str, Expected);

/// What a filter row expects one summary it leaves to count: the resource, the compliance type,
/// the compliant items and the ones that are not.
type Counted = (&'static str, &'static str, i64, i64);

/// One item as a put carries it.
fn item(id: &str, severity: &str, status: &str) -> Value {
    json!({ "Id": id, "Severity": severity, "Status": status })
}

/// Every `ResourceComplianceSummaryItem` the read answers `body` with, which must be served.
async fn summaries(router: &MultiServiceRouter, case: &str, body: &Value) -> Vec<Value> {
    let (status, reported) =
        call_in_region(router, REGION, "ListResourceComplianceSummaries", body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the read must be served, got {status} with {reported}"
    );
    reported["ResourceComplianceSummaryItems"]
        .as_array()
        .unwrap_or_else(|| {
            panic!("case {case:?}: a read reports ResourceComplianceSummaryItems, got {reported}")
        })
        .clone()
}

/// The summary this read reports about `resource_id` under `compliance_type`, or `None` when it
/// reports none.
async fn summary_of(
    router: &MultiServiceRouter,
    case: &str,
    resource_id: &str,
    compliance_type: &str,
) -> Option<Value> {
    summaries(router, case, &json!({}))
        .await
        .into_iter()
        .find(|summary| {
            summary["ResourceId"].as_str() == Some(resource_id)
                && summary["ComplianceType"].as_str() == Some(compliance_type)
        })
}

/// The `(ResourceId, ComplianceType)` pair one summary names itself by, which is also the order
/// the summaries are answered in.
fn named(summary: &Value) -> (String, String) {
    (
        summary["ResourceId"].as_str().unwrap_or_default().to_string(),
        summary["ComplianceType"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
    )
}

/// The `(Status, OverallSeverity, CompliantCount, NonCompliantCount)` one summary reports.
fn reported(case: &str, summary: &Value) -> (String, String, i64, i64) {
    (
        summary["Status"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected a Status, got {summary}"))
            .to_string(),
        summary["OverallSeverity"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected an OverallSeverity, got {summary}"))
            .to_string(),
        summary["CompliantSummary"]["CompliantCount"]
            .as_i64()
            .unwrap_or_else(|| panic!("case {case:?}: expected a CompliantCount, got {summary}")),
        summary["NonCompliantSummary"]["NonCompliantCount"]
            .as_i64()
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected a NonCompliantCount, got {summary}")
            }),
    )
}

/// One severity count of one half of a summary.
fn severity(case: &str, summary: &Value, half: &str, member: &str) -> i64 {
    summary[half]["SeveritySummary"][member]
        .as_i64()
        .unwrap_or_else(|| panic!("case {case:?}: expected {half}.{member}, got {summary}"))
}

/// RULE: one summary per resource and compliance type, whose counts are of that resource's own
/// items of that type, whose status is compliant only while every one of them is, and whose
/// overall severity is the most severe they carry.
#[tokio::test]
async fn a_summary_per_resource_and_type_counts_that_resources_own_items() {
    let (router, engine) = router_with_engine();
    register(&engine, REGION, FIRST_NODE);
    register(&engine, REGION, SECOND_NODE);

    let case = "two resources, one of them reporting two types";
    for (node, compliance_type, items) in [
        // Every item compliant, so the resource is — and the most severe of them is HIGH.
        (
            FIRST_NODE,
            SCAN_TYPE,
            json!([item("disk", "LOW", COMPLIANT), item("kernel", "HIGH", COMPLIANT)]),
        ),
        // A second type about the same resource is a summary of its own.
        (
            FIRST_NODE,
            AUDIT_TYPE,
            json!([item("policy", "CRITICAL", NON_COMPLIANT)]),
        ),
        // One item that is not compliant decides the resource, whatever the others say.
        (
            SECOND_NODE,
            SCAN_TYPE,
            json!([
                item("disk", "MEDIUM", COMPLIANT),
                item("kernel", "CRITICAL", NON_COMPLIANT),
                item("agent", "INFORMATIONAL", NON_COMPLIANT),
            ]),
        ),
    ] {
        let (status, answered) =
            put(&router, REGION, MANAGED_INSTANCE, node, compliance_type, items).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the setup put of {compliance_type} about {node} must be served, got \
             {status} with {answered}"
        );
    }

    // (what the row covers, the resource, the type, (Status, OverallSeverity, compliant items,
    // items that are not))
    let rows: &[SummaryRow] = &[
        (
            "a resource whose every item of the type is compliant",
            FIRST_NODE,
            SCAN_TYPE,
            (COMPLIANT, "HIGH", 2, 0),
        ),
        (
            "the same resource under a second type, with one item",
            FIRST_NODE,
            AUDIT_TYPE,
            (NON_COMPLIANT, "CRITICAL", 0, 1),
        ),
        (
            "a resource holding items of both statuses",
            SECOND_NODE,
            SCAN_TYPE,
            (NON_COMPLIANT, "CRITICAL", 1, 2),
        ),
    ];
    let answered = summaries(&router, case, &json!({})).await;
    assert_eq!(
        answered.len(),
        rows.len(),
        "case {case:?}: expected one summary per resource and type, got {answered:?}"
    );
    for (row, resource_id, compliance_type, expected) in rows {
        let summary = answered
            .iter()
            .find(|summary| named(summary) == (resource_id.to_string(), compliance_type.to_string()))
            .unwrap_or_else(|| {
                panic!("case {row:?}: expected a summary about {resource_id} / {compliance_type}")
            });
        let (status, overall, compliant, non_compliant) = reported(row, summary);
        assert_eq!(
            (
                status.as_str(),
                overall.as_str(),
                compliant,
                non_compliant
            ),
            *expected,
            "case {row:?}: expected {expected:?}, got {summary}"
        );
        assert_eq!(
            summary["ResourceType"].as_str(),
            Some(MANAGED_INSTANCE),
            "case {row:?}: expected the resource type the items are about, got {summary}"
        );
    }

    // The severity counts are of the ITEMS under each status, per resource and type.
    let case = "the severities of one resource's items";
    let scan = summaries(&router, case, &json!({}))
        .await
        .into_iter()
        .find(|summary| named(summary) == (SECOND_NODE.to_string(), SCAN_TYPE.to_string()))
        .unwrap_or_else(|| panic!("case {case:?}: expected a summary about {SECOND_NODE}"));
    for (half, member, expected) in [
        ("CompliantSummary", "MediumCount", 1),
        ("CompliantSummary", "CriticalCount", 0),
        ("NonCompliantSummary", "CriticalCount", 1),
        ("NonCompliantSummary", "InformationalCount", 1),
        ("NonCompliantSummary", "MediumCount", 0),
    ] {
        assert_eq!(
            severity(case, &scan, half, member),
            expected,
            "case {case:?}: expected {half}.{member} = {expected}, got {scan}"
        );
    }

    // The roll-up FOLLOWS the items: a put that overwrites a resource's items of a type ("this
    // call overwrites existing compliance information on the resource") moves that resource's
    // summary of that type, and nothing else's.
    let case = "an overwrite of one resource's items";
    let (status, answered) = put(
        &router,
        REGION,
        MANAGED_INSTANCE,
        SECOND_NODE,
        SCAN_TYPE,
        json!([item("disk", "LOW", COMPLIANT)]),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the put must be served, got {status} with {answered}"
    );
    let scan = summary_of(&router, case, SECOND_NODE, SCAN_TYPE)
        .await
        .unwrap_or_else(|| panic!("case {case:?}: expected a summary about {SECOND_NODE}"));
    assert_eq!(
        reported(case, &scan),
        (COMPLIANT.to_string(), "LOW".to_string(), 1, 0),
        "case {case:?}: expected the summary the items now support, got {scan}"
    );
    let audit = summary_of(&router, case, FIRST_NODE, AUDIT_TYPE)
        .await
        .unwrap_or_else(|| panic!("case {case:?}: expected the other summary to stay reported"));
    assert_eq!(
        reported(case, &audit),
        (NON_COMPLIANT.to_string(), "CRITICAL".to_string(), 0, 1),
        "case {case:?}: expected the other resource's summary untouched, got {audit}"
    );

    // And the read REPORTS: two reads with nothing in between answer the same summaries.
    let case = "a second read of the same state";
    let first = summaries(&router, case, &json!({})).await;
    let again = summaries(&router, case, &json!({})).await;
    assert_eq!(
        first, again,
        "case {case:?}: expected a read to leave the compliance it reports alone, got {first:?} \
         then {again:?}"
    );
}

/// RULE: `ExecutionSummary` is the execution that reported the resource's items LAST — the
/// summary of the item with the latest `ExecutionTime` — so a later report moves it and an
/// earlier one does not.
#[tokio::test]
async fn the_execution_summary_is_the_latest_one_the_items_were_reported_under() {
    let (router, engine) = router_with_engine();
    register(&engine, REGION, FIRST_NODE);

    /// The instants the rows report at, as epoch seconds — the form the awsJson1_1 wire carries a
    /// timestamp in.
    const EARLIER: i64 = 1_699_000_000;
    const MIDDLE: i64 = 1_700_000_000;
    const LATER: i64 = 1_701_000_000;

    // (what the row covers, the item this report carries, the mode it is sent in, the instant it
    // is reported at, the instant the summary then reports, how many items it counts)
    let rows: &[(&str, &str, &str, i64, i64, i64)] = &[
        (
            "the first report of the resource",
            "disk",
            "COMPLETE",
            MIDDLE,
            MIDDLE,
            1,
        ),
        (
            "a later report, which the summary reports instead",
            "kernel",
            "PARTIAL",
            LATER,
            LATER,
            2,
        ),
        (
            "an earlier report, which does not move it",
            "agent",
            "PARTIAL",
            EARLIER,
            LATER,
            3,
        ),
    ];
    for (case, item_id, upload_type, at, expected_at, expected_items) in rows {
        // The whole body, because the row's own execution summary is what it is about — the
        // shared `put` helper reports every reports at one instant.
        let (status, answered) = call_in_region(
            &router,
            REGION,
            "PutComplianceItems",
            &json!({
                "ResourceId": FIRST_NODE,
                "ResourceType": MANAGED_INSTANCE,
                "ComplianceType": SCAN_TYPE,
                "UploadType": upload_type,
                "ExecutionSummary": {
                    "ExecutionTime": at,
                    "ExecutionId": format!("run-{at}"),
                    "ExecutionType": "Command",
                },
                "Items": [item(item_id, "LOW", COMPLIANT)],
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the put must be served, got {status} with {answered}"
        );

        let summary = summary_of(&router, case, FIRST_NODE, SCAN_TYPE)
            .await
            .unwrap_or_else(|| panic!("case {case:?}: expected a summary about {FIRST_NODE}"));
        let expected_id = format!("run-{expected_at}");
        assert_eq!(
            (
                summary["ExecutionSummary"]["ExecutionId"].as_str(),
                summary["ExecutionSummary"]["ExecutionTime"].as_f64(),
                summary["ExecutionSummary"]["ExecutionType"].as_str(),
            ),
            (
                Some(expected_id.as_str()),
                Some(*expected_at as f64),
                Some("Command"),
            ),
            "case {case:?}: expected the summary of the latest report, got {summary}"
        );
        assert_eq!(
            summary["CompliantSummary"]["CompliantCount"].as_i64(),
            Some(*expected_items),
            "case {case:?}: expected the items the resource now holds counted, got {summary}"
        );
    }
}

/// RULE: a `Filters` entry narrows the items each summary is computed from — the same filter
/// keys and comparisons the item read narrows by — and a key naming no member of a compliance
/// item is refused `InvalidFilter`.
#[tokio::test]
async fn a_filter_narrows_the_items_each_summary_is_computed_from() {
    let (router, engine) = router_with_engine();
    register(&engine, REGION, FIRST_NODE);
    register(&engine, REGION, SECOND_NODE);
    for (node, compliance_type, items) in [
        (
            FIRST_NODE,
            SCAN_TYPE,
            json!([item("disk", "LOW", NON_COMPLIANT), item("kernel", "HIGH", COMPLIANT)]),
        ),
        (
            FIRST_NODE,
            AUDIT_TYPE,
            json!([item("policy", "MEDIUM", COMPLIANT)]),
        ),
        (
            SECOND_NODE,
            SCAN_TYPE,
            json!([item("disk", "LOW", COMPLIANT)]),
        ),
    ] {
        let (status, answered) =
            put(&router, REGION, MANAGED_INSTANCE, node, compliance_type, items).await;
        assert_eq!(
            status, 200,
            "the setup put of {compliance_type} about {node} must be served, got {status} with \
             {answered}"
        );
    }

    // (what the row covers, the filter, the summaries it leaves, with the counts of each)
    let rows: &[(&str, Value, &[Counted])] = &[
        (
            "a filter on the resource, which leaves that resource's summaries",
            json!([{"Key": "ResourceId", "Values": [SECOND_NODE]}]),
            &[(SECOND_NODE, SCAN_TYPE, 1, 0)],
        ),
        (
            "a filter on the compliance type",
            json!([{"Key": "ComplianceType", "Values": [AUDIT_TYPE]}]),
            &[(FIRST_NODE, AUDIT_TYPE, 1, 0)],
        ),
        (
            "a filter on the status, which leaves the items of that status",
            json!([{"Key": "Status", "Values": [COMPLIANT]}]),
            &[
                (FIRST_NODE, AUDIT_TYPE, 1, 0),
                (FIRST_NODE, SCAN_TYPE, 1, 0),
                (SECOND_NODE, SCAN_TYPE, 1, 0),
            ],
        ),
        (
            "a filter on the severity",
            json!([{"Key": "Severity", "Values": ["HIGH"]}]),
            &[(FIRST_NODE, SCAN_TYPE, 1, 0)],
        ),
        (
            "a comparison other than equality",
            json!([{"Key": "Severity", "Values": ["HIGH"], "Type": "NOT_EQUAL"}]),
            &[
                (FIRST_NODE, AUDIT_TYPE, 1, 0),
                (FIRST_NODE, SCAN_TYPE, 0, 1),
                (SECOND_NODE, SCAN_TYPE, 1, 0),
            ],
        ),
        (
            "a filter on a value no item holds, which leaves no summary at all",
            json!([{"Key": "ResourceId", "Values": ["i-0dead999"]}]),
            &[],
        ),
    ];
    for (case, filters, expected) in rows {
        let answered = summaries(&router, case, &json!({"Filters": filters})).await;
        let counted: Vec<(String, String, i64, i64)> = answered
            .iter()
            .map(|summary| {
                let (resource_id, compliance_type) = named(summary);
                let (_, _, compliant, non_compliant) = reported(case, summary);
                (resource_id, compliance_type, compliant, non_compliant)
            })
            .collect();
        let wanted: Vec<(String, String, i64, i64)> = expected
            .iter()
            .map(|(resource_id, compliance_type, compliant, non_compliant)| {
                (
                    (*resource_id).to_string(),
                    (*compliance_type).to_string(),
                    *compliant,
                    *non_compliant,
                )
            })
            .collect();
        assert_eq!(
            counted, wanted,
            "case {case:?}: expected the narrowed summaries {wanted:?}, got {answered:?}"
        );
    }

    // A key that names no member of a compliance item cannot narrow anything, so the request is
    // refused rather than served an unnarrowed success.
    let (status, answered) = call_in_region(
        &router,
        REGION,
        "ListResourceComplianceSummaries",
        &json!({"Filters": [{"Key": "NotAMember", "Values": ["anything"]}]}),
    )
    .await;
    assert_eq!(
        status, 400,
        "case \"an unknown filter key\": expected the request refused, got {status} with {answered}"
    );
    assert_eq!(
        error_code(&answered),
        "InvalidFilter",
        "case \"an unknown filter key\": expected the modelled refusal, got {answered}"
    );
}

/// RULE: the page is capped by `MaxResults` and resumed by the `NextToken` the read handed out,
/// which names the summary it stopped at — and a token this read did not mint, a token the
/// type-level roll-up minted included, is refused `InvalidNextToken`.
#[tokio::test]
async fn the_summaries_page_over_the_resources_by_the_token_the_read_hands_out() {
    let (router, engine) = router_with_engine();
    register(&engine, REGION, FIRST_NODE);
    register(&engine, REGION, SECOND_NODE);
    for (node, compliance_type) in [
        (FIRST_NODE, AUDIT_TYPE),
        (FIRST_NODE, SCAN_TYPE),
        (SECOND_NODE, SCAN_TYPE),
    ] {
        let (status, answered) = put(
            &router,
            REGION,
            MANAGED_INSTANCE,
            node,
            compliance_type,
            json!([item("probe", "LOW", COMPLIANT)]),
        )
        .await;
        assert_eq!(
            status, 200,
            "the setup put of {compliance_type} about {node} must be served, got {status} with \
             {answered}"
        );
    }

    // The summaries are answered in ascending order of the resource and the compliance type they
    // are about, and every page of every size walks them in that one order.
    let walk = [
        (FIRST_NODE, AUDIT_TYPE),
        (FIRST_NODE, SCAN_TYPE),
        (SECOND_NODE, SCAN_TYPE),
    ];
    for page_size in 1..=walk.len() {
        let case = format!("pages of {page_size}");
        let mut seen: Vec<(String, String)> = Vec::new();
        let mut token: Option<String> = None;
        for _ in 0..=walk.len() {
            let mut body = json!({"MaxResults": page_size});
            if let Some(token) = token.as_deref() {
                body["NextToken"] = json!(token);
            }
            let (status, answered) =
                call_in_region(&router, REGION, "ListResourceComplianceSummaries", &body).await;
            assert_eq!(
                status, 200,
                "case {case:?}: the read must be served, got {status} with {answered}"
            );
            let page = answered["ResourceComplianceSummaryItems"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            assert!(
                page.len() <= page_size,
                "case {case:?}: expected at most the page the request asked for, got {answered}"
            );
            seen.extend(page.iter().map(named));
            token = answered["NextToken"].as_str().map(ToString::to_string);
            if token.is_none() {
                break;
            }
        }
        assert_eq!(
            token, None,
            "case {case:?}: expected the walk to end once every summary was served"
        );
        let expected: Vec<(String, String)> = walk
            .iter()
            .map(|(node, compliance_type)| ((*node).to_string(), (*compliance_type).to_string()))
            .collect();
        assert_eq!(
            seen, expected,
            "case {case:?}: expected every summary once, in ascending order, got {seen:?}"
        );
    }

    // A token this read never minted is refused rather than served a page it never offered — and
    // so is one the type-level roll-up minted, which names a position in another walk.
    let (status, typewise) = call_in_region(
        &router,
        REGION,
        "ListComplianceSummaries",
        &json!({"MaxResults": 1}),
    )
    .await;
    assert_eq!(
        status, 200,
        "the type-level read must be served, got {status} with {typewise}"
    );
    let borrowed = typewise["NextToken"]
        .as_str()
        .unwrap_or_else(|| panic!("the type-level read hands out a token, got {typewise}"));
    for (case, token) in [
        ("a token nothing minted", "not-a-token-this-endpoint-minted"),
        ("a token another read minted", borrowed),
    ] {
        let (status, answered) = call_in_region(
            &router,
            REGION,
            "ListResourceComplianceSummaries",
            &json!({"NextToken": token}),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the request refused, got {status} with {answered}"
        );
        assert_eq!(
            error_code(&answered),
            "InvalidNextToken",
            "case {case:?}: expected the modelled refusal, got {answered}"
        );
    }
}
