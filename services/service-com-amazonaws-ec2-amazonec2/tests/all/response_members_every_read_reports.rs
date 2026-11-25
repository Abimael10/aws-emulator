// SPDX-License-Identifier: CC0-1.0

//! Cover for the members EC2 reports on EVERY read of a resource, driven over the wire the
//! service actually speaks: `ec2Query` — an `application/x-www-form-urlencoded` POST whose
//! body carries `Action=…`, answered with XML (README.md / OPERATIONS.md "Wire protocol").
//!
//! The rule under test: a member of a reported shape whose value the service always knows is
//! present in EVERY response that reports that shape, carrying the value in force — never
//! left out for a client to tell "nothing" from "this answer does not say". Two families of
//! it are asserted here, each over a table of inputs the rule generalises over:
//!
//! 1. a customer gateway reports whether the tunnels to it pin its certificate, on the
//!    create's `<customerGateway>` and on every `<item>` of a describe's set, in each state
//!    the gateway can be read in (coming up, in service, and after a delete) and whatever
//!    members the request gave it;
//! 2. a Network Access Scope analysis reports the number of network interfaces it has
//!    analyzed, from the start that hands it back through the describe and on past the end
//!    the clock owes it — a counter of work done, which is zero before any is.
//!
//! Neither member is read off the request: the first follows from the store holding no
//! gateway certificate pinning was ever enabled for, and the second from the analysis having
//! analyzed no interface. The suites of the operations themselves
//! (`customer_gateways`, `network_insights_access_scope_analyses`, `placement_groups`) cover
//! what else each answer carries; this one is about the members every read of the shape must
//! carry.

use std::time::Duration;

use ec2::state::{
    ANALYSIS_STATUS_RUNNING, GATEWAY_TYPE_IPSEC_1, RESOURCE_TYPE_CUSTOMER_GATEWAY,
    VPN_STATE_AVAILABLE, VPN_STATE_DELETED,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::call;
use super::wire::{child, child_text, document, param, set_items, Param};

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// The element the live wire renders on every reported customer gateway saying whether the
/// tunnels to it pin the gateway's certificate. This checkout's `CustomerGateway` shape
/// declares no member for it.
const CERTIFICATE_PINNING: &str = "certificatePinning";

/// `certificatePinning` of a gateway certificate pinning is not enabled for: every gateway of
/// this store, no operation of this service enabling it.
const NOT_PINNED: &str = "false";

/// The element a reported Network Access Scope analysis counts the interfaces it analyzed in
/// (`NetworkInsightsAccessScopeAnalysis$AnalyzedEniCount`, "the number of network interfaces
/// analyzed").
const ANALYZED_ENI_COUNT: &str = "analyzedEniCount";

/// The count an analysis that has analyzed no network interface reports.
const ANALYZED_NOTHING: &str = "0";

/// The emulator the tests drive, as the shared harness builds it.
fn emulator() -> MultiServiceRouter {
    super::ec2_query::emulator().0
}

/// One `action` call carrying `params`, as its HTTP status and XML body.
async fn api(router: &MultiServiceRouter, action: &str, params: &[Param]) -> (u16, String) {
    call(router, REGION, action, params).await
}

/// One `action` call that has to succeed, and the body it answered.
async fn ok(case: &str, router: &MultiServiceRouter, action: &str, params: &[Param]) -> String {
    let (status, body) = api(router, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// The `TagSpecification.1.*` fields that tag the customer gateway a create mints.
fn gateway_tags(tags: &[(&str, &str)]) -> Vec<Param> {
    if tags.is_empty() {
        return Vec::new();
    }
    let mut params = vec![param(
        "TagSpecification.1.ResourceType",
        RESOURCE_TYPE_CUSTOMER_GATEWAY,
    )];
    for (index, (key, value)) in tags.iter().enumerate() {
        params.push(param(
            &format!("TagSpecification.1.Tag.{}.Key", index + 1),
            key,
        ));
        params.push(param(
            &format!("TagSpecification.1.Tag.{}.Value", index + 1),
            value,
        ));
    }
    params
}

/// Every element of `body` that reports a customer gateway — the create's own
/// `<customerGateway>` and each `<item>` of a describe's `<customerGatewaySet>` are both
/// elements carrying a `<customerGatewayId>` — as `(the gateway's id, its state, the text of
/// its certificate pinning element if it carried one)`.
fn reported_gateways(case: &str, body: &str) -> Vec<(String, String, Option<String>)> {
    let parsed = document(case, body);
    parsed
        .descendants()
        .filter(|node| node.is_element() && child(*node, "customerGatewayId").is_some())
        .map(|node| {
            (
                child_text(node, "customerGatewayId").unwrap_or_default(),
                child_text(node, "state").unwrap_or_default(),
                child_text(node, CERTIFICATE_PINNING),
            )
        })
        .collect()
}

/// Rule 1: every path that reports a customer gateway carries its certificate pinning flag.
///
/// The rows differ along what makes two gateways different inputs — which outside address
/// family, which ASN member carries the number, whether the device has a name, whether it is
/// identified by a certificate, and how many tags it holds — and each row is read on the
/// create, on a describe that names the gateway, on the unnarrowed describe, and once more
/// after the gateway has been deleted, which is the fourth state a gateway is reported in.
#[tokio::test]
async fn every_report_of_a_customer_gateway_carries_its_certificate_pinning_flag() {
    let router = emulator();

    let cases: Vec<(&str, Vec<Param>)> = vec![
        ("nothing but the required type", Vec::new()),
        (
            "an IPv4 device with a name and two tags",
            [
                vec![
                    param("IpAddress", "203.0.113.12"),
                    param("BgpAsn", "65000"),
                    param("DeviceName", "branch-office-router"),
                ],
                gateway_tags(&[("Name", "branch-office"), ("Owner", "TeamA")]),
            ]
            .concat(),
        ),
        (
            "an IPv6 device with an extended ASN",
            vec![
                param("IpAddress", "2001:db8::1"),
                param("BgpAsnExtended", "4200000000"),
            ],
        ),
        (
            "a device identified by a certificate",
            vec![
                param("IpAddress", "192.0.2.44"),
                param(
                    "CertificateArn",
                    "arn:aws:acm:us-east-1:123456789012:certificate/\
                     12345678-1234-1234-1234-123456789012",
                ),
            ],
        ),
    ];

    for (case, extra) in cases {
        let mut params = vec![param("Type", GATEWAY_TYPE_IPSEC_1)];
        params.extend(extra);
        let created = ok(case, &router, "CreateCustomerGateway", &params).await;
        let reported = reported_gateways(case, &created);
        assert_eq!(
            reported.len(),
            1,
            "case {case:?}: the create must answer exactly one gateway, got {reported:?} in \
             {created}"
        );
        let (customer_gateway_id, state, pinning) = reported[0].clone();
        assert_eq!(
            (state.as_str(), pinning.as_deref()),
            (VPN_STATE_AVAILABLE, Some(NOT_PINNED)),
            "case {case:?}: the created gateway must report its state and its pinning flag, \
             got {reported:?} in {created}"
        );

        // Every read of the same gateway carries the flag too: narrowed by its id, and in the
        // unnarrowed report beside every other gateway the account holds.
        for (read, params) in [
            ("narrowed by the gateway's id", vec![param("CustomerGatewayId.1", &customer_gateway_id)]),
            ("the unnarrowed report", Vec::new()),
        ] {
            let body = ok(case, &router, "DescribeCustomerGateways", &params).await;
            let missing: Vec<(String, String, Option<String>)> = reported_gateways(case, &body)
                .into_iter()
                .filter(|(_, _, pinning)| pinning.as_deref() != Some(NOT_PINNED))
                .collect();
            assert!(
                missing.is_empty(),
                "case {case:?}: {read}: every reported gateway must carry \
                 <{CERTIFICATE_PINNING}>{NOT_PINNED}, got {missing:?} in {body}"
            );
            assert!(
                set_items(document(case, &body).root_element(), "customerGatewaySet")
                    .iter()
                    .any(|item| child_text(*item, "customerGatewayId").as_deref()
                        == Some(customer_gateway_id.as_str())),
                "case {case:?}: {read}: the gateway {customer_gateway_id} must be reported: \
                 {body}"
            );
        }

        // A deleted gateway is still a gateway the API reports — in the terminal state — so it
        // carries the flag in that state as well.
        ok(
            case,
            &router,
            "DeleteCustomerGateway",
            &[param("CustomerGatewayId", &customer_gateway_id)],
        )
        .await;
        let body = ok(
            case,
            &router,
            "DescribeCustomerGateways",
            &[param("CustomerGatewayId.1", &customer_gateway_id)],
        )
        .await;
        let reported = reported_gateways(case, &body);
        assert_eq!(
            reported,
            vec![(
                customer_gateway_id.clone(),
                VPN_STATE_DELETED.to_string(),
                Some(NOT_PINNED.to_string()),
            )],
            "case {case:?}: a deleted gateway must still report its pinning flag, got \
             {reported:?} in {body}"
        );
    }
}

/// The analysis a response reports, as `(its id, its status, the text of its analyzed
/// interface count if it carried one)`.
fn reported_analyses(case: &str, body: &str) -> Vec<(String, String, Option<String>)> {
    let parsed = document(case, body);
    parsed
        .descendants()
        .filter(|node| {
            node.is_element() && child(*node, "networkInsightsAccessScopeAnalysisId").is_some()
        })
        .map(|node| {
            (
                child_text(node, "networkInsightsAccessScopeAnalysisId").unwrap_or_default(),
                child_text(node, "status").unwrap_or_default(),
                child_text(node, ANALYZED_ENI_COUNT),
            )
        })
        .collect()
}

/// The one analysis `DescribeNetworkInsightsAccessScopeAnalyses` reports for `analysis_id`.
async fn described_analysis(
    case: &str,
    router: &MultiServiceRouter,
    analysis_id: &str,
) -> (String, String, Option<String>) {
    let body = ok(
        case,
        router,
        "DescribeNetworkInsightsAccessScopeAnalyses",
        &[param("NetworkInsightsAccessScopeAnalysisId.1", analysis_id)],
    )
    .await;
    let reported = reported_analyses(case, &body);
    assert_eq!(
        reported.len(),
        1,
        "case {case:?}: exactly the one analysis must be reported, got {reported:?} in {body}"
    );
    reported.into_iter().next().expect("one analysis")
}

/// Rule 2: an analysis reports the number of interfaces it has analyzed on every read — from
/// the start that hands it back, through the describe, and on past the end the clock owes it.
///
/// The rows differ along how many analyses one scope carries and which of them is read, which
/// is what the counter being a fact of the ANALYSIS rather than of the call generalises over.
/// The state after the end is waited for by POLLING the status, not by sleeping for the span
/// the clock owes it, so a loaded machine cannot read the analysis before the move is applied.
#[tokio::test]
async fn every_read_of_an_access_scope_analysis_counts_the_interfaces_it_analyzed() {
    let router = emulator();
    let case = "three analyses of one scope";

    let created = ok(
        case,
        &router,
        "CreateNetworkInsightsAccessScope",
        &[param("ClientToken", "scope-for-the-counter")],
    )
    .await;
    let scope_id = super::ec2_query::element(&created, "networkInsightsAccessScopeId")
        .unwrap_or_else(|| panic!("case {case:?}: the create must answer a scope: {created}"));

    let mut analysis_ids: Vec<String> = Vec::new();
    for token in ["first", "second", "third"] {
        let started = ok(
            case,
            &router,
            "StartNetworkInsightsAccessScopeAnalysis",
            &[
                param("NetworkInsightsAccessScopeId", &scope_id),
                param("ClientToken", token),
            ],
        )
        .await;
        let reported = reported_analyses(case, &started);
        assert_eq!(
            reported.len(),
            1,
            "case {case:?}: the start must answer exactly one analysis, got {reported:?} in \
             {started}"
        );
        let (analysis_id, status, counted) = reported[0].clone();
        assert_eq!(
            (status.as_str(), counted.as_deref()),
            (ANALYSIS_STATUS_RUNNING, Some(ANALYZED_NOTHING)),
            "case {case:?}: analysis {token:?} is running and has analyzed nothing yet, got \
             {reported:?} in {started}"
        );

        // The read path answers the same count, off the same record.
        let (_, _, described) = described_analysis(case, &router, &analysis_id).await;
        assert_eq!(
            described.as_deref(),
            Some(ANALYZED_NOTHING),
            "case {case:?}: the describe of analysis {token:?} must report the same count, \
             got {described:?}"
        );
        analysis_ids.push(analysis_id);
    }

    // Past the end the clock owes each analysis the count is still reported: nothing analyzed
    // the scope, so the counter stands at none rather than disappearing.
    for analysis_id in &analysis_ids {
        let (_, status, counted) = wait_until_ended(case, &router, analysis_id).await;
        assert_ne!(
            status, ANALYSIS_STATUS_RUNNING,
            "case {case:?}: analysis {analysis_id} must have ended"
        );
        assert_eq!(
            counted.as_deref(),
            Some(ANALYZED_NOTHING),
            "case {case:?}: an ended analysis that analyzed nothing still counts none, got \
             {counted:?} for {analysis_id}"
        );
    }
}

/// Poll `analysis_id` until the describe reports it in a status other than `running` — the
/// terminal status the clock owes it — and answer that report.
///
/// Waits for the CONDITION rather than for a span: the deadline is generous, because what is
/// under test is what the analysis reports rather than how fast the clock runs.
async fn wait_until_ended(
    case: &str,
    router: &MultiServiceRouter,
    analysis_id: &str,
) -> (String, String, Option<String>) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        let reported = described_analysis(case, router, analysis_id).await;
        if reported.1 != ANALYSIS_STATUS_RUNNING {
            return reported;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: the analysis {analysis_id} must reach its terminal status, last \
             read {reported:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
