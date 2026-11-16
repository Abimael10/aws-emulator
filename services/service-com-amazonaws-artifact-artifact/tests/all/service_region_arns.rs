// SPDX-License-Identifier: CC0-1.0

//! RULE: every ARN this service reports names THIS SERVICE's own Region — the one its
//! partitional endpoint resolves to — and never the Region the caller's credential scope
//! happens to carry.
//!
//! AWS Artifact is reached through a partitional endpoint: the model binds one endpoint
//! per partition, resolved at that partition's implicit global Region
//! (`smithy_query(query="service_info")` reports `standardPartitionalEndpoints` over
//! `PartitionResult#implicitGlobalRegion`), so a call is served in that one Region
//! whatever Region a client was configured for, and every resource the service reports is
//! homed there. An ARN a client parses, stores and addresses a resource by therefore
//! carries that Region alone.
//!
//! The rows differ along what the rule generalises over:
//!
//! * the Region a request is signed for — this service's own, two others of the same
//!   partition, a three-word one, two of other partitions' shapes, a scope whose region
//!   field is not a Region at all, and no credential scope at all;
//! * the KIND of resource reported — a published report version (service-owned, no
//!   account), the term governing a report, a customer-agreement and a compliance inquiry
//!   (both account-owned);
//! * the operation that reports it — `ListReports`, `ListReportVersions`,
//!   `GetReportMetadata`, `ListCustomerAgreements` and `CreateComplianceInquiry`.
//!
//! Every assertion goes over the router, so each case exercises the path a client takes:
//! routing, deserialization, the handler, the persisted state and serialization.

use bytes::Bytes;
use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{emulator, send_signed, signed_for, Answer, CATALOG, HOME_REGION};

/// One signing scope a case drives the service with: the label, and the `authorization`
/// header value — `None` for a request that presents no credential scope at all.
struct Scope {
    case: &'static str,
    authorization: Option<String>,
}

/// The scopes every rule below is asserted under.
fn scopes() -> Vec<Scope> {
    let regions = [
        ("this service's own Region", HOME_REGION),
        ("another Region of this partition", "eu-west-1"),
        ("the Region a default client is configured for", "us-west-2"),
        ("a three-word Region", "ap-southeast-3"),
        ("a Region of the China partition", "cn-north-1"),
        ("a Region of the GovCloud partition", "us-gov-west-1"),
        ("a scope whose region field is not a Region", "not_a_region"),
        ("a scope with an empty region field", ""),
    ];
    let mut rows: Vec<Scope> = regions
        .into_iter()
        .map(|(case, region)| Scope {
            case,
            authorization: Some(signed_for(region)),
        })
        .collect();
    rows.push(Scope {
        case: "no credential scope at all",
        authorization: None,
    });
    rows
}

/// One GET, under `scope`'s credential scope (or none at all) — through the harness's one
/// request builder, so this suite grows no second copy of it.
async fn read(router: &MultiServiceRouter, scope: &Scope, uri: &str) -> Answer {
    send_signed(
        router,
        "GET",
        uri,
        Bytes::new(),
        scope.authorization.as_deref(),
    )
    .await
}

/// One `CreateComplianceInquiry`, under `scope`'s credential scope (or none at all).
async fn create(router: &MultiServiceRouter, scope: &Scope, name: &str) -> Answer {
    let body = json!({
        "name": name,
        "inquiryContent": { "query": "Which controls does this report cover?" },
        "supportMode": "AI_ONLY"
    });
    send_signed(
        router,
        "POST",
        "/v1/compliance-inquiry/create",
        Bytes::from(body.to_string()),
        scope.authorization.as_deref(),
    )
    .await
}

/// How many fields an ARN has: `arn`, the partition, the vendor, the Region, the account
/// and the resource — the form every ARN this service writes takes.
const ARN_FIELDS: usize = 6;

/// Assert `arn` is an ARN of this service naming [`HOME_REGION`], and answer its Region
/// field so a caller can report it.
fn region_of(case: &str, member: &str, arn: &str) -> String {
    let fields: Vec<&str> = arn.splitn(ARN_FIELDS, ':').collect();
    assert_eq!(
        fields.len(),
        ARN_FIELDS,
        "case {case:?}: {member} must be an ARN of six fields, got {arn:?}"
    );
    assert_eq!(
        (fields[0], fields[1], fields[2]),
        ("arn", "aws", "artifact"),
        "case {case:?}: {member} must be an ARN of this service, got {arn:?}"
    );
    fields[3].to_string()
}

/// Assert every ARN in `arns` names this service's own Region.
fn assert_home_region(case: &str, member: &str, arns: &[String]) {
    assert!(
        !arns.is_empty(),
        "case {case:?}: expected at least one {member} to assert on, got none"
    );
    for arn in arns {
        let region = region_of(case, member, arn);
        assert_eq!(
            region, HOME_REGION,
            "case {case:?}: {member} must name this service's Region {HOME_REGION}, \
             got {region:?} in {arn:?}"
        );
    }
}

/// The ARNs one answer's `reports` list carries.
fn report_arns(case: &str, answer: &Answer) -> Vec<String> {
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected HTTP 200, got {} with {:?}",
        answer.status, answer.body
    );
    answer
        .reports()
        .iter()
        .map(|summary| {
            summary["arn"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: every report summary carries an arn, got {summary:?}")
                })
                .to_string()
        })
        .collect()
}

#[tokio::test]
async fn every_enumerated_report_arn_names_this_service_s_region() {
    let router = emulator();
    for scope in scopes() {
        let answer = read(&router, &scope, "/v1/report/list").await;
        assert_home_region(
            scope.case,
            "an enumerated report's arn",
            &report_arns(scope.case, &answer),
        );
    }
}

#[tokio::test]
async fn every_report_version_arn_names_this_service_s_region() {
    let router = emulator();
    for scope in scopes() {
        for expected in CATALOG {
            let case = format!("{} under {}", expected.id, scope.case);
            let answer = read(
                &router,
                &scope,
                &format!("/v1/report/listVersions?reportId={}", expected.id),
            )
            .await;
            let arns = report_arns(&case, &answer);
            assert_eq!(
                arns.len(),
                expected.versions.len(),
                "case {case:?}: one entry per published version, got {arns:?}"
            );
            assert_home_region(&case, "a report version's arn", &arns);
        }
    }
}

#[tokio::test]
async fn a_report_s_metadata_arns_name_this_service_s_region() {
    let router = emulator();
    for scope in scopes() {
        for expected in CATALOG {
            let case = format!("{} under {}", expected.id, scope.case);
            let answer = read(
                &router,
                &scope,
                &format!("/v1/report/getMetadata?reportId={}", expected.id),
            )
            .await;
            assert_eq!(
                answer.status, 200,
                "case {case:?}: expected HTTP 200, got {} with {:?}",
                answer.status, answer.body
            );
            let detail = &answer.body["reportDetails"];
            // Both ARNs the metadata read reports: the report version's own, and the one
            // identifying the term governing it.
            for member in ["arn", "termArn"] {
                let arn = detail[member]
                    .as_str()
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: the metadata read reports {member}, got {detail:?}")
                    })
                    .to_string();
                assert_home_region(&case, member, &[arn]);
            }
        }
    }
}

#[tokio::test]
async fn every_customer_agreement_arn_names_this_service_s_region() {
    let router = emulator();
    for scope in scopes() {
        let answer = read(&router, &scope, "/v1/customer-agreement/list").await;
        assert_eq!(
            answer.status, 200,
            "case {:?}: expected HTTP 200, got {} with {:?}",
            scope.case, answer.status, answer.body
        );
        let arns: Vec<String> = answer.body["customerAgreements"]
            .as_array()
            .unwrap_or_else(|| {
                panic!(
                    "case {:?}: customerAgreements is required, got {:?}",
                    scope.case, answer.body
                )
            })
            .iter()
            .map(|summary| {
                summary["arn"]
                    .as_str()
                    .unwrap_or_else(|| {
                        panic!(
                            "case {:?}: every agreement carries an arn, got {summary:?}",
                            scope.case
                        )
                    })
                    .to_string()
            })
            .collect();
        assert_home_region(scope.case, "a customer-agreement's arn", &arns);
    }
}

#[tokio::test]
async fn a_created_inquiry_s_arn_names_this_service_s_region() {
    let router = emulator();
    for scope in scopes() {
        let answer = create(&router, &scope, scope.case).await;
        assert_eq!(
            answer.status, 202,
            "case {:?}: expected HTTP 202, got {} with {:?}",
            scope.case, answer.status, answer.body
        );
        let summary = &answer.body["complianceInquirySummary"];
        let arn = summary["arn"]
            .as_str()
            .unwrap_or_else(|| {
                panic!(
                    "case {:?}: a created inquiry reports its arn, got {summary:?}",
                    scope.case
                )
            })
            .to_string();
        assert_home_region(scope.case, "a created inquiry's arn", &[arn]);
    }
}
