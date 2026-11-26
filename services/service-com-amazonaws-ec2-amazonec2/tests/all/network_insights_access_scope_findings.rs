// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for what a Network Access Scope analysis FINDS —
//! `StartNetworkInsightsAccessScopeAnalysis` into
//! `GetNetworkInsightsAccessScopeAnalysisFindings` — driven over the wire the service actually
//! speaks: `ec2Query`, through the crate's shared [`super::ec2_query`] harness.
//!
//! The rules under test are:
//!
//! 1. the analysis is RUN against the account's own network: the clock takes it to `succeeded`
//!    carrying the number of network interfaces it analyzed and whether the scope has findings,
//!    and a finding is one path the stored network carries from one interface to another that
//!    the scope matched;
//! 2. `GetNetworkInsightsAccessScopeAnalysisFindings` reports exactly those findings — each
//!    naming the analysis, the scope and the components of the path it matched — beside the
//!    analysis's own status, and pages them with `MaxResults`;
//! 3. an analysis still running has no findings to report, and an id the account holds no
//!    analysis under is refused.
//!
//! The rows differ along what the rule generalises over — the NETWORK the scope is analyzed
//! against: an account with no interface at all, one with two interfaces of one VPC that can
//! reach each other, and one whose interfaces sit in different VPCs, which no path of this
//! analysis joins.

use std::time::Duration;

use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, elements, error_code};
use super::wire::{child_text, document, param, Param};

/// The Region every call here is signed for.
const REGION: &str = "us-west-2";

/// How long a clock-owed transition takes to fall due: the service's own span, plus a margin
/// so a slow machine cannot read the analysis before it is due.
async fn past_the_transition() {
    tokio::time::sleep(ec2::state::TRANSITION_DELAY + Duration::from_millis(400)).await;
}

/// One call that has to succeed, and the body it answered.
async fn ok(case: &str, router: &MultiServiceRouter, action: &str, params: &[Param]) -> String {
    let (status, body) = call(router, REGION, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// A VPC with one subnet, as (VPC id, subnet id).
async fn network(case: &str, router: &MultiServiceRouter, cidr: &str, subnet_cidr: &str) -> (String, String) {
    let body = ok(case, router, "CreateVpc", &[param("CidrBlock", cidr)]).await;
    let vpc_id = element(&body, "vpcId").unwrap_or_else(|| panic!("case {case:?}: no vpcId"));
    let body = ok(
        case,
        router,
        "CreateSubnet",
        &[param("VpcId", &vpc_id), param("CidrBlock", subnet_cidr)],
    )
    .await;
    let subnet_id =
        element(&body, "subnetId").unwrap_or_else(|| panic!("case {case:?}: no subnetId"));
    (vpc_id, subnet_id)
}

/// One network interface of `subnet_id`, as its id. No group is named, so the interface gets
/// its VPC's default security group — the group "your VPC automatically comes with", whose
/// rules admit traffic from the interfaces that share it.
async fn interface(case: &str, router: &MultiServiceRouter, subnet_id: &str) -> String {
    let body = ok(
        case,
        router,
        "CreateNetworkInterface",
        &[param("SubnetId", subnet_id)],
    )
    .await;
    element(&body, "networkInterfaceId")
        .unwrap_or_else(|| panic!("case {case:?}: no networkInterfaceId in {body}"))
}

/// A Network Access Scope that constrains nothing, as its id: a scope naming no path matches
/// every path, which is the scope a caller asks "what access is there at all?" with.
async fn unconstrained_scope(case: &str, router: &MultiServiceRouter) -> String {
    let body = ok(
        case,
        router,
        "CreateNetworkInsightsAccessScope",
        &[param("ClientToken", "scope-token")],
    )
    .await;
    element(&body, "networkInsightsAccessScopeId")
        .unwrap_or_else(|| panic!("case {case:?}: no networkInsightsAccessScopeId in {body}"))
}

/// The analysis started on `scope_id`, as its id.
async fn start(case: &str, router: &MultiServiceRouter, scope_id: &str) -> String {
    let body = ok(
        case,
        router,
        "StartNetworkInsightsAccessScopeAnalysis",
        &[
            param("NetworkInsightsAccessScopeId", scope_id),
            param("ClientToken", "analysis-token"),
        ],
    )
    .await;
    element(&body, "networkInsightsAccessScopeAnalysisId")
        .unwrap_or_else(|| panic!("case {case:?}: no analysis id in {body}"))
}

/// The analysis as `DescribeNetworkInsightsAccessScopeAnalyses` reports it: its status,
/// whether findings were found, and how many interfaces it analyzed.
async fn analysis(
    case: &str,
    router: &MultiServiceRouter,
    analysis_id: &str,
) -> (String, Option<String>, Option<String>) {
    let body = ok(
        case,
        router,
        "DescribeNetworkInsightsAccessScopeAnalyses",
        &[param("NetworkInsightsAccessScopeAnalysisId.1", analysis_id)],
    )
    .await;
    let parsed = document(case, &body);
    let item = parsed
        .descendants()
        .find(|node| node.is_element() && node.tag_name().name() == "item")
        .unwrap_or_else(|| panic!("case {case:?}: no analysis reported in {body}"));
    (
        child_text(item, "status").unwrap_or_default(),
        child_text(item, "findingsFound"),
        child_text(item, "analyzedEniCount"),
    )
}

/// The findings `GetNetworkInsightsAccessScopeAnalysisFindings` reports for `analysis_id`
/// with `params` as extra fields: the body, and the finding ids in it.
async fn findings(
    case: &str,
    router: &MultiServiceRouter,
    analysis_id: &str,
    params: &[Param],
) -> (String, Vec<String>) {
    let mut fields = vec![param("NetworkInsightsAccessScopeAnalysisId", analysis_id)];
    fields.extend(params.iter().cloned());
    let body = ok(
        case,
        router,
        "GetNetworkInsightsAccessScopeAnalysisFindings",
        &fields,
    )
    .await;
    let ids = elements(&body, "findingId");
    (body, ids)
}

/// RULE 1 and RULE 2: the analysis is run against the stored network, and the findings it
/// reports are the paths that network carries.
///
/// The rows are the networks the one unconstrained scope is analyzed against.
#[tokio::test]
async fn an_analysis_reports_the_paths_the_stored_network_carries() {
    // (case, how many interfaces, are they in ONE VPC?, are findings expected?)
    let cases: [(&str, usize, bool, bool); 4] = [
        ("an account with no interface at all", 0, true, false),
        ("one interface, which has nowhere to reach", 1, true, false),
        ("two interfaces of one VPC", 2, true, true),
        ("two interfaces in different VPCs", 2, false, false),
    ];

    for (case, interfaces, one_vpc, expect_findings) in cases {
        let (router, _engine) = super::ec2_query::emulator();
        let (_vpc, subnet) = network(case, &router, "10.60.0.0/16", "10.60.1.0/24").await;
        let (_other_vpc, other_subnet) =
            network(case, &router, "10.61.0.0/16", "10.61.1.0/24").await;
        let mut created = Vec::new();
        for index in 0..interfaces {
            let subnet_id = if one_vpc || index == 0 {
                subnet.as_str()
            } else {
                other_subnet.as_str()
            };
            created.push(interface(case, &router, subnet_id).await);
        }

        let scope_id = unconstrained_scope(case, &router).await;
        let analysis_id = start(case, &router, &scope_id).await;

        // Straight after the start the analysis is still running, and an analysis that has
        // not run has no findings to report.
        let (status, _found, _count) = analysis(case, &router, &analysis_id).await;
        assert_eq!(
            status,
            ec2::state::ANALYSIS_STATUS_RUNNING,
            "case {case:?}: a caller reading straight after the start still sees it running"
        );
        let (running_body, running_findings) = findings(case, &router, &analysis_id, &[]).await;
        assert!(
            running_findings.is_empty(),
            "case {case:?}: a running analysis has produced no finding yet, got {running_body}"
        );
        assert_eq!(
            element(&running_body, "analysisStatus").as_deref(),
            Some(ec2::state::ANALYSIS_STATUS_RUNNING),
            "case {case:?}: the findings report carries the analysis's own status, got \
             {running_body}"
        );

        past_the_transition().await;

        let (status, found, analyzed) = analysis(case, &router, &analysis_id).await;
        assert_eq!(
            status,
            ec2::state::ANALYSIS_STATUS_SUCCEEDED,
            "case {case:?}: the clock runs the analysis to a result"
        );
        assert_eq!(
            analyzed.as_deref(),
            Some(interfaces.to_string().as_str()),
            "case {case:?}: every interface of the account was analyzed, got {analyzed:?}"
        );
        let expected_found = if expect_findings {
            ec2::state::FINDINGS_FOUND_TRUE
        } else {
            ec2::state::FINDINGS_FOUND_FALSE
        };
        assert_eq!(
            found.as_deref(),
            Some(expected_found),
            "case {case:?}: expected findingsFound {expected_found}, got {found:?}"
        );

        let (body, reported) = findings(case, &router, &analysis_id, &[]).await;
        assert_eq!(
            reported.is_empty(),
            !expect_findings,
            "case {case:?}: expected findings: {expect_findings}, got {reported:?}"
        );
        assert_eq!(
            element(&body, "analysisStatus").as_deref(),
            Some(ec2::state::ANALYSIS_STATUS_SUCCEEDED),
            "case {case:?}: the findings report carries the terminal status, got {body}"
        );
        if expect_findings {
            // Each finding names the analysis and the scope it belongs to, and carries the
            // components of the path it matched — which name the interfaces of that path.
            for scope in elements(&body, "networkInsightsAccessScopeId") {
                assert_eq!(
                    scope, scope_id,
                    "case {case:?}: every finding names the scope it is of, got {body}"
                );
            }
            for analysis in elements(&body, "networkInsightsAccessScopeAnalysisId") {
                assert_eq!(
                    analysis, analysis_id,
                    "case {case:?}: every finding names its analysis, got {body}"
                );
            }
            assert!(
                !elements(&body, "sequenceNumber").is_empty(),
                "case {case:?}: a finding carries the components of the path it matched, got \
                 {body}"
            );
            for interface_id in &created {
                assert!(
                    body.contains(interface_id.as_str()),
                    "case {case:?}: the matched path names {interface_id}, got {body}"
                );
            }
            // One page per finding: `MaxResults` cuts the page and a token follows it.
            let (paged, first_page) =
                findings(case, &router, &analysis_id, &[param("MaxResults", "1")]).await;
            assert_eq!(
                first_page.len(),
                1,
                "case {case:?}: MaxResults=1 reports one finding, got {first_page:?}"
            );
            assert!(
                element(&paged, "nextToken").is_some(),
                "case {case:?}: a token follows a page a finding comes after, got {paged}"
            );
        }
    }
}

/// RULE 3: an analysis id the account holds none under is refused rather than answered with an
/// empty report, with this resource's own not-found code — the one wording every read of this
/// surface answers. The rows are the forms such a value can take.
#[tokio::test]
async fn an_unresolvable_analysis_id_is_refused() {
    let (router, _engine) = super::ec2_query::emulator();
    let prefix = ec2::state::NETWORK_INSIGHTS_ACCESS_SCOPE_ANALYSIS_ID_PREFIX;
    let cases: [(&str, String); 3] = [
        ("the long form", format!("{prefix}-0123456789abcdef0")),
        ("another resource's id", "nis-0123456789abcdef0".to_string()),
        ("free text", "not-an-analysis".to_string()),
    ];
    for (case, analysis_id) in cases {
        let (status, body) = call(
            &router,
            REGION,
            "GetNetworkInsightsAccessScopeAnalysisFindings",
            &[param("NetworkInsightsAccessScopeAnalysisId", &analysis_id)],
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            ec2::state::INVALID_NETWORK_INSIGHTS_ACCESS_SCOPE_ANALYSIS_ID_NOT_FOUND,
            "case {case:?}: expected {}, got {body}",
            ec2::state::INVALID_NETWORK_INSIGHTS_ACCESS_SCOPE_ANALYSIS_ID_NOT_FOUND
        );
        assert_eq!(
            super::ec2_query::error_message(&body),
            format!("{analysis_id} does not exist"),
            "case {case:?}: the refusal names the id the request carried, got {body}"
        );
    }
}

/// RULE 2: the findings page in the order the analysis found them, however many pages a page
/// size cuts the report into — including past the first ten, where a cursor that sorted its
/// positions as text would re-report findings it had already served.
///
/// Four interfaces of one VPC reach each other both ways, so the analysis finds twelve paths;
/// the rows are the page sizes that cut twelve findings into one, two and several pages.
#[tokio::test]
async fn the_findings_page_in_the_order_the_analysis_found_them() {
    let case = "twelve findings";
    let (router, _engine) = super::ec2_query::emulator();
    let (_vpc, subnet) = network(case, &router, "10.62.0.0/16", "10.62.1.0/24").await;
    for _ in 0..4 {
        interface(case, &router, &subnet).await;
    }
    let scope_id = unconstrained_scope(case, &router).await;
    let analysis_id = start(case, &router, &scope_id).await;
    past_the_transition().await;

    let (_body, every) = findings(case, &router, &analysis_id, &[]).await;
    assert_eq!(
        every.len(),
        12,
        "case {case:?}: four interfaces of one VPC reach each other both ways, got {every:?}"
    );

    for page_size in [1usize, 5, 10, 12] {
        let mut seen: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        // One page more than the report needs, so a pager that fails to advance is caught by
        // the bound rather than by looping forever.
        for _ in 0..=every.len() {
            let mut params = vec![param("MaxResults", &page_size.to_string())];
            if let Some(next) = token.clone() {
                params.push(param("NextToken", &next));
            }
            let (body, page) = findings(case, &router, &analysis_id, &params).await;
            assert!(
                page.len() <= page_size,
                "case {case:?} / page size {page_size}: a page holds at most what was asked \
                 for, got {page:?}"
            );
            seen.extend(page);
            token = element(&body, "nextToken");
            if token.is_none() {
                break;
            }
        }
        assert_eq!(
            seen, every,
            "case {case:?} / page size {page_size}: paging must report every finding once, in \
             order"
        );
    }
}
