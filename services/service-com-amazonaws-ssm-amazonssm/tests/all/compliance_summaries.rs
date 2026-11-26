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
//! computation: a `Filters` entry narrows the items the roll-up is over — the same filter keys
//! the item read narrows by — a key that names no member of a compliance item is refused
//! `InvalidFilter`, the page is capped by the one `MaxResults` the member's documentation names
//! ("Currently, you can specify null or 50", so every other in-range value is refused
//! `ValidationException`) with a `NextToken` that resumes after the type it stopped at, and a
//! token this endpoint did not mint is refused `InvalidNextToken`.
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
//! many items each holds, whether they agree on the status, the severities they carry, whether
//! the type is a caller's own or Patch Manager's, and which `MaxResults` the request carries —
//! the documented value, none, values the modelled range admits and values past either of its
//! ends.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::compliance_items::{put, register, MANAGED_INSTANCE};
use crate::parameter_store::{call_in_region, error_code, error_message, router_with_engine, REGION};

/// The ONE page size this read serves beside an absent `MaxResults`: "The maximum number of
/// items to return for this call. Currently, you can specify null or 50"
/// (`smithy_query(query="doc", shape="ListComplianceSummariesRequest", member="MaxResults")`),
/// written out here so the cases pin the documented value rather than echo the emulator's.
const ONLY_PAGE_SIZE: i64 = 50;

/// How many CUSTOM compliance types one resource may hold reports of ("you can specify a
/// maximum of 10 different types", `ComplianceTypeCountLimitExceededException`), which is what
/// spreads a paging case's types over several nodes.
const CUSTOM_TYPES_PER_RESOURCE: usize = 10;

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
    // One type past the page this read serves, so the first page is full and one type is left
    // waiting behind the token it hands out. The types are spread over as many nodes as the
    // per-resource ceiling on CUSTOM types needs ("you can specify a maximum of 10 different
    // types", `ComplianceTypeCountLimitExceededException`).
    let types: Vec<String> = (0..=ONLY_PAGE_SIZE)
        .map(|position| format!("Custom:Page{position:03}"))
        .collect();
    let mut registered: Vec<String> = Vec::new();
    for (position, compliance_type) in types.iter().enumerate() {
        let node = format!("i-{:08}", position / CUSTOM_TYPES_PER_RESOURCE);
        if !registered.contains(&node) {
            register(&engine, REGION, &node);
            registered.push(node.clone());
        }
        let (status, answered) = put(
            &router,
            REGION,
            MANAGED_INSTANCE,
            &node,
            compliance_type,
            json!([item("probe", "LOW", COMPLIANT)]),
        )
        .await;
        assert_eq!(
            status, 200,
            "the setup put of {compliance_type} must be served, got {status} with {answered}"
        );
    }

    // (what the row covers, the body the first page is asked for with)
    let rows: &[(&str, Value)] = &[
        ("the page size this read serves", json!({ "MaxResults": ONLY_PAGE_SIZE })),
        ("no page size at all, which is served the same page", json!({})),
    ];
    for (case, body) in rows {
        let (status, first) = call_in_region(&router, REGION, "ListComplianceSummaries", body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the read must be served, got {status} with {first}"
        );
        let page = first["ComplianceSummaryItems"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            page.len() as i64,
            ONLY_PAGE_SIZE,
            "case {case:?}: expected a full page of {ONLY_PAGE_SIZE}, got {} in {first}",
            page.len()
        );
        // The types are answered in ascending order, so the first page carries all but the
        // last of them.
        let reported: Vec<&str> = page
            .iter()
            .filter_map(|summary| summary["ComplianceType"].as_str())
            .collect();
        assert_eq!(
            reported,
            types[..types.len() - 1]
                .iter()
                .map(String::as_str)
                .collect::<Vec<&str>>(),
            "case {case:?}: expected the first page in ascending order, got {reported:?}"
        );
        let token = first["NextToken"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: a full page hands out a token, got {first}"));

        let next = summaries(&router, case, &json!({ "NextToken": token })).await;
        let resumed: Vec<&str> = next
            .iter()
            .filter_map(|summary| summary["ComplianceType"].as_str())
            .collect();
        assert_eq!(
            resumed,
            vec![types[types.len() - 1].as_str()],
            "case {case:?}: expected the type the first page stopped before, got {resumed:?}"
        );
    }

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

/// RULE: the member's own documentation narrows the page sizes this read serves — "Currently,
/// you can specify null or 50" — so an absent `MaxResults` and that one value are served and
/// every OTHER value inside the modelled 1..50 range is refused `ValidationException` naming
/// the operation and the set it serves, while a value outside the modelled range keeps the
/// constraint clause of that range.
#[tokio::test]
async fn only_the_documented_page_sizes_are_served() {
    let (router, engine) = router_with_engine();
    register(&engine, REGION, FIRST_NODE);
    let (status, answered) = put(
        &router,
        REGION,
        MANAGED_INSTANCE,
        FIRST_NODE,
        SCAN_TYPE,
        json!([item("probe", "LOW", COMPLIANT)]),
    )
    .await;
    assert_eq!(
        status, 200,
        "the setup put must be served, got {status} with {answered}"
    );

    // The values the read SERVES: the one the documentation names, and no member at all.
    for body in [json!({ "MaxResults": ONLY_PAGE_SIZE }), json!({})] {
        let case = format!("the served page size {body}");
        let reported = summaries(&router, &case, &body).await;
        assert_eq!(
            reported.len(),
            1,
            "case {case:?}: expected the one type the Region holds, got {reported:?}"
        );
    }

    // Every other value inside the modelled range, including both of its ends and a value in
    // between: the operation's own sentence, which names it and the set it serves.
    let in_range_refusal = format!("ListComplianceSummaries only support MaxResults as null or {ONLY_PAGE_SIZE}");
    for requested in [1, 2, 7, 25, ONLY_PAGE_SIZE - 1] {
        let case = format!("the in-range page size {requested}");
        let (status, answered) = call_in_region(
            &router,
            REGION,
            "ListComplianceSummaries",
            &json!({ "MaxResults": requested }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the request refused, got {status} with {answered}"
        );
        assert_eq!(
            error_code(&answered),
            "ValidationException",
            "case {case:?}: expected a ValidationException, got {answered}"
        );
        assert_eq!(
            error_message(&answered),
            in_range_refusal,
            "case {case:?}: expected {in_range_refusal:?}, got {answered}"
        );
    }

    // A value OUTSIDE the modelled range is the range's own refusal: the envelope and the
    // clause stating the member, the value and the bound it broke.
    let case = "one past the modelled maximum";
    let (status, answered) = call_in_region(
        &router,
        REGION,
        "ListComplianceSummaries",
        &json!({ "MaxResults": ONLY_PAGE_SIZE + 1 }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected the request refused, got {status} with {answered}"
    );
    assert_eq!(
        error_code(&answered),
        "ValidationException",
        "case {case:?}: expected a ValidationException, got {answered}"
    );
    assert_eq!(
        error_message(&answered),
        format!(
            "1 validation error detected: Value '{}' at 'maxResults' failed to satisfy \
             constraint: Member must have value less than or equal to {ONLY_PAGE_SIZE}",
            ONLY_PAGE_SIZE + 1
        ),
        "case {case:?}: expected the modelled range's clause, got {answered}"
    );

    // Below the modelled minimum is the same class of refusal — the range's clause rather
    // than the narrower sentence, which is about values the range admits.
    for requested in [0, -1] {
        let case = format!("the page size {requested}, below the modelled minimum");
        let (status, answered) = call_in_region(
            &router,
            REGION,
            "ListComplianceSummaries",
            &json!({ "MaxResults": requested }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the request refused, got {status} with {answered}"
        );
        let stated = error_message(&answered);
        assert!(
            stated.starts_with("1 validation error detected:")
                && stated.contains("at 'maxResults' failed to satisfy constraint")
                && !stated.contains("only support MaxResults"),
            "case {case:?}: expected the modelled range's clause, got {stated:?}"
        );
    }
}
