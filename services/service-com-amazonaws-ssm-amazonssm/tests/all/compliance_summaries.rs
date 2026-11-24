// SPDX-License-Identifier: CC0-1.0

//! The ROLL-UP of a Region's compliance, end to end: `PutComplianceItems` writes the items and
//! `ListComplianceSummaries` reports the counts computed from them.
//!
//! RULE under test: **`ListComplianceSummaries` "returns a summary count of compliant and
//! non-compliant resources for a compliance type", so each compliance type the Region holds
//! items of is answered with one `ComplianceSummaryItem` whose counts are COMPUTED from those
//! items: `CompliantSummary$CompliantCount` is "the total number of resources that are
//! compliant" of that type — a resource counts as compliant only while every item it holds of
//! the type is — `NonCompliantSummary$NonCompliantCount` is the resources that are not, and a
//! `SeveritySummary` count is "the total number of resources or compliance items that have a
//! severity level of" that level, which is what the ITEMS of that status carry**
//! (`smithy_query(query="closure", operation="ListComplianceSummaries")`). Around that
//! computation: a `Filters` entry narrows the items the roll-up is over, a key that names no
//! member of a compliance item is refused `InvalidFilter`, the page is capped by `MaxResults`
//! with a `NextToken` that resumes after the type it stopped at, and a token this endpoint did
//! not mint is refused `InvalidNextToken`.
//!
//! Every case goes through the front door a client uses: the shared harness in
//! `all/parameter_store.rs` dispatches `POST /` with
//! `X-Amz-Target: AmazonSSM.<Operation>` and an `awsJson1_1` body. Nothing is computed in the
//! test: every item the summaries count was written through `PutComplianceItems` itself (the
//! put and the node seeding are `compliance_items`' own, so the two suites write compliance one
//! way), and the ONE thing seeded through the engine is the registered managed node, because no
//! call of this service registers one.
//!
//! The rows span what the rule generalises over: how many resources hold items of a type, how
//! many items each holds, whether they agree on the status, the severities they carry, and
//! whether the type is a caller's own or Patch Manager's.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::compliance_items::{put, register, MANAGED_INSTANCE};
use crate::parameter_store::{call_in_region, error_code, router_with_engine, REGION};

/// The nodes the cases report compliance about: node ids of the forms the modelled
/// `InstanceId` pattern admits.
const FIRST_NODE: &str = "i-0123456789abcdef0";
const SECOND_NODE: &str = "i-0abc1234";

/// Two compliance types of the caller's own namespace ("specify `Association` …, `Patch`, or
/// `Custom:string`", `PutComplianceItemsRequest$ComplianceType`), so a case covers more than
/// one type at once.
const SCAN_TYPE: &str = "Custom:Scan";
const AUDIT_TYPE: &str = "Custom:Audit";

/// The two statuses an item carries (`ComplianceItem$Status`).
const COMPLIANT: &str = "COMPLIANT";
const NON_COMPLIANT: &str = "NON_COMPLIANT";

/// The summary this read reports for `compliance_type`, or `None` when it reports none.
async fn summary_of(
    router: &MultiServiceRouter,
    case: &str,
    compliance_type: &str,
) -> Option<Value> {
    summaries(router, case, &json!({}))
        .await
        .into_iter()
        .find(|item| item["ComplianceType"].as_str() == Some(compliance_type))
}

/// Every `ComplianceSummaryItem` the read answers `body` with, which must be served.
async fn summaries(router: &MultiServiceRouter, case: &str, body: &Value) -> Vec<Value> {
    let (status, reported) = call_in_region(router, REGION, "ListComplianceSummaries", body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the read must be served, got {status} with {reported}"
    );
    reported["ComplianceSummaryItems"]
        .as_array()
        .unwrap_or_else(|| {
            panic!("case {case:?}: a read reports ComplianceSummaryItems, got {reported}")
        })
        .clone()
}

/// The `(CompliantCount, NonCompliantCount)` one summary reports.
fn counts(case: &str, summary: &Value) -> (i64, i64) {
    (
        summary["CompliantSummary"]["CompliantCount"]
            .as_i64()
            .unwrap_or_else(|| panic!("case {case:?}: expected a CompliantCount, got {summary}")),
        summary["NonCompliantSummary"]["NonCompliantCount"]
            .as_i64()
            .unwrap_or_else(|| panic!("case {case:?}: expected a NonCompliantCount, got {summary}")),
    )
}

/// One severity count of one half of a summary.
fn severity(case: &str, summary: &Value, half: &str, member: &str) -> i64 {
    summary[half]["SeveritySummary"][member]
        .as_i64()
        .unwrap_or_else(|| panic!("case {case:?}: expected {half}.{member}, got {summary}"))
}

/// One item as a put carries it.
fn item(id: &str, severity: &str, status: &str) -> Value {
    json!({ "Id": id, "Severity": severity, "Status": status })
}

/// RULE: the counts are of RESOURCES, per compliance type — a resource is compliant for a type
/// only while every item it holds of it is — and the severity counts are of the items under
/// each status.
#[tokio::test]
async fn the_summary_of_a_type_counts_the_resources_and_severities_of_its_items() {
    let (router, engine) = router_with_engine();
    register(&engine, REGION, FIRST_NODE);
    register(&engine, REGION, SECOND_NODE);

    // The first node is compliant for the scan type (both its items are) and the second is
    // not (one of its three is not), so that type holds one resource of each.
    let case = "two resources reporting the same type";
    let (status, answered) = put(
        &router,
        REGION,
        MANAGED_INSTANCE,
        FIRST_NODE,
        SCAN_TYPE,
        json!([
            item("disk", "LOW", COMPLIANT),
            item("kernel", "HIGH", COMPLIANT),
        ]),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the put must be served, got {status} with {answered}"
    );
    let (status, answered) = put(
        &router,
        REGION,
        MANAGED_INSTANCE,
        SECOND_NODE,
        SCAN_TYPE,
        json!([
            item("disk", "MEDIUM", COMPLIANT),
            item("kernel", "CRITICAL", NON_COMPLIANT),
            item("agent", "INFORMATIONAL", NON_COMPLIANT),
        ]),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the put must be served, got {status} with {answered}"
    );

    let scan = summary_of(&router, case, SCAN_TYPE)
        .await
        .unwrap_or_else(|| panic!("case {case:?}: expected a summary for {SCAN_TYPE}"));
    assert_eq!(
        counts(case, &scan),
        (1, 1),
        "case {case:?}: expected one compliant resource and one that is not, got {scan}"
    );
    // The severities are the items': three compliant ones (LOW, HIGH, MEDIUM) and two that
    // are not (CRITICAL, INFORMATIONAL).
    for (half, member, expected) in [
        ("CompliantSummary", "LowCount", 1),
        ("CompliantSummary", "HighCount", 1),
        ("CompliantSummary", "MediumCount", 1),
        ("CompliantSummary", "CriticalCount", 0),
        ("NonCompliantSummary", "CriticalCount", 1),
        ("NonCompliantSummary", "InformationalCount", 1),
        ("NonCompliantSummary", "LowCount", 0),
    ] {
        assert_eq!(
            severity(case, &scan, half, member),
            expected,
            "case {case:?}: expected {half}.{member} = {expected}, got {scan}"
        );
    }

    // A SECOND type over one of the same resources is a summary of its own: the types are
    // independent reports about the resource.
    let case = "a second type over one of the same resources";
    let (status, answered) = put(
        &router,
        REGION,
        MANAGED_INSTANCE,
        FIRST_NODE,
        AUDIT_TYPE,
        json!([item("policy", "CRITICAL", NON_COMPLIANT)]),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the put must be served, got {status} with {answered}"
    );
    let audit = summary_of(&router, case, AUDIT_TYPE)
        .await
        .unwrap_or_else(|| panic!("case {case:?}: expected a summary for {AUDIT_TYPE}"));
    assert_eq!(
        counts(case, &audit),
        (0, 1),
        "case {case:?}: expected the one resource counted non-compliant, got {audit}"
    );
    let scan = summary_of(&router, case, SCAN_TYPE)
        .await
        .unwrap_or_else(|| panic!("case {case:?}: expected the first type to stay reported"));
    assert_eq!(
        counts(case, &scan),
        (1, 1),
        "case {case:?}: expected the first type's counts to be untouched, got {scan}"
    );

    // And the roll-up FOLLOWS the items: a put that overwrites a resource's items of a type
    // ("this call overwrites existing compliance information on the resource") moves that
    // type's counts, and nothing else's.
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
    let scan = summary_of(&router, case, SCAN_TYPE)
        .await
        .unwrap_or_else(|| panic!("case {case:?}: expected a summary for {SCAN_TYPE}"));
    assert_eq!(
        counts(case, &scan),
        (2, 0),
        "case {case:?}: expected both resources compliant once the items say so, got {scan}"
    );
    assert_eq!(
        severity(case, &scan, "NonCompliantSummary", "CriticalCount"),
        0,
        "case {case:?}: expected the severities of the items that are gone to go too, got {scan}"
    );
}

/// RULE: a `Filters` entry narrows the items the roll-up is over — the same filter keys the
/// item read narrows by — and a key naming no member of a compliance item is refused
/// `InvalidFilter`.
#[tokio::test]
async fn a_filter_narrows_the_items_the_summary_is_computed_from() {
    let (router, engine) = router_with_engine();
    register(&engine, REGION, FIRST_NODE);
    register(&engine, REGION, SECOND_NODE);
    for (node, items) in [
        (
            FIRST_NODE,
            json!([item("disk", "LOW", NON_COMPLIANT), item("kernel", "HIGH", COMPLIANT)]),
        ),
        (SECOND_NODE, json!([item("disk", "LOW", COMPLIANT)])),
    ] {
        let (status, answered) =
            put(&router, REGION, MANAGED_INSTANCE, node, SCAN_TYPE, items).await;
        assert_eq!(
            status, 200,
            "the setup put for {node} must be served, got {status} with {answered}"
        );
    }

    // (what the row covers, the filter, the counts the narrowed roll-up reports)
    let rows: &[(&str, Value, (i64, i64))] = &[
        (
            "a filter on the resource, which leaves one resource's items",
            json!([{"Key": "ResourceId", "Values": [SECOND_NODE]}]),
            (1, 0),
        ),
        (
            "a filter on the status, which leaves the items of that status",
            json!([{"Key": "Status", "Values": [COMPLIANT]}]),
            (2, 0),
        ),
        (
            "a filter on the severity",
            json!([{"Key": "Severity", "Values": ["HIGH"]}]),
            (1, 0),
        ),
        (
            "a filter on the compliance type every item carries",
            json!([{"Key": "ComplianceType", "Values": [SCAN_TYPE]}]),
            (1, 1),
        ),
        (
            "a filter on a value no item holds",
            json!([{"Key": "ResourceId", "Values": ["i-0dead999"]}]),
            (0, 0),
        ),
    ];
    for (case, filters, expected) in rows {
        let reported = summaries(&router, case, &json!({"Filters": filters})).await;
        let counted = reported
            .iter()
            .find(|summary| summary["ComplianceType"].as_str() == Some(SCAN_TYPE))
            .map_or((0, 0), |summary| counts(case, summary));
        assert_eq!(
            counted, *expected,
            "case {case:?}: expected the narrowed counts {expected:?}, got {reported:?}"
        );
    }

    // A key that names no member of a compliance item cannot narrow anything, so the request
    // is refused rather than served an unnarrowed success.
    let (status, answered) = call_in_region(
        &router,
        REGION,
        "ListComplianceSummaries",
        &json!({"Filters": [{"Key": "NotAMember", "Values": ["anything"]}]}),
    )
    .await;
    assert_eq!(
        status, 400,
        "case \"an unknown filter key\": expected the request refused, got {status} with \
         {answered}"
    );
    assert_eq!(
        error_code(&answered),
        "InvalidFilter",
        "case \"an unknown filter key\": expected the modelled refusal, got {answered}"
    );
}

/// RULE: the page is capped by `MaxResults` and resumed by the `NextToken` the read handed
/// out, which names the compliance type it stopped at — and a token this endpoint did not mint
/// is refused `InvalidNextToken`.
#[tokio::test]
async fn the_summaries_page_over_the_types_by_the_token_the_read_hands_out() {
    let (router, engine) = router_with_engine();
    register(&engine, REGION, FIRST_NODE);
    for compliance_type in [AUDIT_TYPE, SCAN_TYPE] {
        let (status, answered) = put(
            &router,
            REGION,
            MANAGED_INSTANCE,
            FIRST_NODE,
            compliance_type,
            json!([item("probe", "LOW", COMPLIANT)]),
        )
        .await;
        assert_eq!(
            status, 200,
            "the setup put of {compliance_type} must be served, got {status} with {answered}"
        );
    }

    let case = "a page of one";
    let (status, first) = call_in_region(
        &router,
        REGION,
        "ListComplianceSummaries",
        &json!({"MaxResults": 1}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the read must be served, got {status} with {first}"
    );
    let page = first["ComplianceSummaryItems"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        page.len(),
        1,
        "case {case:?}: expected the page the request asked for, got {first}"
    );
    // The types are answered in ascending order, so the first page carries the first of them.
    assert_eq!(
        page[0]["ComplianceType"].as_str(),
        Some(AUDIT_TYPE),
        "case {case:?}: expected the first type, got {first}"
    );
    let token = first["NextToken"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: a full page hands out a token, got {first}"));

    let next = summaries(&router, case, &json!({"MaxResults": 1, "NextToken": token})).await;
    assert_eq!(
        next.len(),
        1,
        "case {case:?}: expected the type the first page stopped before, got {next:?}"
    );
    assert_eq!(
        next[0]["ComplianceType"].as_str(),
        Some(SCAN_TYPE),
        "case {case:?}: expected the second type, got {next:?}"
    );

    // A token this endpoint never minted is refused rather than served a page it never
    // offered.
    let (status, answered) = call_in_region(
        &router,
        REGION,
        "ListComplianceSummaries",
        &json!({"NextToken": "not-a-token-this-endpoint-minted"}),
    )
    .await;
    assert_eq!(
        status, 400,
        "case \"a token nothing minted\": expected the request refused, got {status} with \
         {answered}"
    );
    assert_eq!(
        error_code(&answered),
        "InvalidNextToken",
        "case \"a token nothing minted\": expected the modelled refusal, got {answered}"
    );
}
