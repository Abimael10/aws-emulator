// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the ONE predicate both organization-level lists make
//! the modelled `affectedAccountId` test through
//! (`service_com_amazonaws_trustedadvisor_trustedadvisor::state::affected_account_keeps`),
//! driven over HTTP through the same `MultiServiceRouter` the emulator serves
//! with.
//!
//! The rule: a read that names no `affectedAccountId` reports every row of the
//! organization recommendation, and a read that names one reports only the rows
//! THAT account owns — a row's owner being the account derived from the row's
//! own ARN, the same derivation the `accountId` reported beside it goes through,
//! so a row whose ARN names no account at all is owned by no named account. The
//! two organization-level lists make that one test the one way:
//!
//! * `ListOrganizationRecommendationAccounts` tests each member account
//!   recommendation's own ARN;
//! * `ListOrganizationRecommendationResources` tests each member resource's own
//!   ARN.
//!
//! `affectedAccountId` is modelled identically on the two operations
//! (`AccountId`, `length 12..12, pattern ^\d+$`:
//! `smithy_query(query="operation",operation="ListOrganizationRecommendationAccounts,ListOrganizationRecommendationResources",
//! model_path="services/service-com-amazonaws-trustedadvisor-trustedadvisor/smithy-model/com-amazonaws-trustedadvisor-trustedadvisor.json")`),
//! so the rows below span several 12-digit accounts and are asserted on BOTH
//! lists for every one of them; the modelled refusals outside that constraint
//! are pinned in `tests/all/organization_recommendation_accounts.rs` and
//! `tests/all/organization_recommendation_resources.rs`.
//!
//! Every call is made as an account whose support plan includes the Trusted
//! Advisor API; the gate ahead of every handler is pinned in
//! `tests/all/support_level_gate.rs`.

use service_com_amazonaws_trustedadvisor_trustedadvisor::state::{
    affected_account_keeps, SupportPlan,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::organization_recommendation_lifecycle::organization_recommendation_arn;
use crate::owning_account_from_arn::{
    enrol_member_account, member_recommendation_arn, member_resource_arn,
};
use crate::recommendation_loop::{call, emulator_on_plan, label, recommendation_arn};

/// The check both lists are read for: its recommendation is `priority`, the only
/// type with an organization recommendation, and it has three seeded resources
/// so one member account contributes more than one resource row. Source: the
/// `SEED` table in `src/state.rs`.
const CHECK: &str = "low-utilization-ec2-instances";

/// The ids of [`CHECK`]'s seeded resources, written out by hand; their source is
/// the `SEED` table in `src/state.rs`. Every member account below holds a copy
/// of each of them, under its own ARN.
const RESOURCE_IDS: &[&str] = &["res-ec2-0001", "res-ec2-0002", "res-ec2-0003"];

/// The member accounts whose rows the two lists report: the emulator's own
/// account (the seeded member) plus three enrolled ones, differing along the
/// account value itself — two unrelated ids and one with leading zeros, every
/// one a 12-digit `AccountId`.
fn member_accounts() -> Vec<&'static str> {
    vec![
        engine::EMULATOR_ACCOUNT_ID,
        "111122223333",
        "999988887777",
        "000000000012",
    ]
}

/// One read of a list: the case name and the `affectedAccountId` the request
/// names, `None` when it names none.
struct Case {
    name: &'static str,
    affected_account_id: Option<&'static str>,
}

/// The reads both lists are driven through: no filter, each member account in
/// turn (the calling account among them), and an account that owns no row of
/// this organization recommendation.
fn cases() -> Vec<Case> {
    let mut cases = vec![Case {
        name: "no affectedAccountId",
        affected_account_id: None,
    }];
    for account in member_accounts() {
        cases.push(Case {
            name: account,
            affected_account_id: Some(account),
        });
    }
    cases.push(Case {
        name: "an account that owns no row",
        affected_account_id: Some("222233334444"),
    });
    cases
}

/// A mounted emulator on a plan that includes the Trusted Advisor API, with
/// every account of [`member_accounts`] enrolled as a member of [`CHECK`]'s
/// organization recommendation.
fn emulator_with_members() -> MultiServiceRouter {
    let (router, engine) = emulator_on_plan(SupportPlan::Business);
    for account in member_accounts()
        .into_iter()
        .filter(|account| *account != engine::EMULATOR_ACCOUNT_ID)
    {
        enrol_member_account(&engine, CHECK, account);
    }
    router
}

/// The modelled URI of one organization-level list of [`CHECK`]'s organization
/// recommendation, under the `affectedAccountId` this read names.
fn list_path(suffix: &str, affected_account_id: Option<&str>) -> String {
    let base = format!(
        "/v1/organization-recommendations/{}/{suffix}?maxResults=200",
        label(&organization_recommendation_arn(CHECK))
    );
    match affected_account_id {
        Some(account) => format!("{base}&affectedAccountId={account}"),
        None => base,
    }
}

/// The ARN each row of `list` carries under `arn_member`, in the order the list
/// reported them, asserting 200 first.
async fn listed_arns(
    router: &MultiServiceRouter,
    case: &str,
    path: &str,
    list: &str,
    arn_member: &str,
) -> Vec<String> {
    let (status, body) = call(router, "GET", path, None).await;
    assert_eq!(
        status, 200,
        "case {case}: expected 200, got {status} with {body}"
    );
    body[list]
        .as_array()
        .unwrap_or_else(|| panic!("case {case}: no {list} in {body}"))
        .iter()
        .map(|row| {
            row[arn_member]
                .as_str()
                .unwrap_or_else(|| panic!("case {case}: a {list} row has no {arn_member}: {row}"))
                .to_string()
        })
        .collect()
}

/// The rows of `every_arn` the rule keeps for one read, in ARN order: all of
/// them when the read names no account, and otherwise the ones whose ARN
/// carries that account segment.
fn expected_arns(every_arn: &[String], affected_account_id: Option<&str>) -> Vec<String> {
    let mut kept: Vec<String> = every_arn
        .iter()
        .filter(|arn| match affected_account_id {
            Some(account) => arn.contains(&format!(":{account}:")),
            None => true,
        })
        .cloned()
        .collect();
    kept.sort();
    kept
}

#[tokio::test]
async fn affected_account_id_keeps_the_same_rows_in_both_organization_lists() {
    let router = emulator_with_members();
    let accounts = member_accounts();

    // Every stored row of this organization recommendation, written out by
    // hand: one member recommendation per member account, and one resource per
    // member account per seeded resource id.
    let every_recommendation_arn: Vec<String> = accounts
        .iter()
        .map(|account| member_recommendation_arn(account, CHECK))
        .collect();
    let every_resource_arn: Vec<String> = accounts
        .iter()
        .flat_map(|account| {
            RESOURCE_IDS
                .iter()
                .map(move |id| member_resource_arn(account, CHECK, id))
        })
        .collect();
    // The seeded member's ARNs are the ones this crate builds for the calling
    // account, so the rows above really name what the store holds.
    assert!(
        every_recommendation_arn.contains(&recommendation_arn(CHECK)),
        "case setup: expected the seeded member {:?} among {every_recommendation_arn:?}",
        recommendation_arn(CHECK)
    );

    for case in cases() {
        let filter = case.affected_account_id;

        let listed_recommendations = listed_arns(
            &router,
            case.name,
            &list_path("accounts", filter),
            "accountRecommendationLifecycleSummaries",
            "accountRecommendationArn",
        )
        .await;
        let expected = expected_arns(&every_recommendation_arn, filter);
        assert_eq!(
            listed_recommendations, expected,
            "case {}/accounts: expected {expected:?} in ARN order, got {listed_recommendations:?}",
            case.name
        );

        let listed_resources = listed_arns(
            &router,
            case.name,
            &list_path("resources", filter),
            "organizationRecommendationResourceSummaries",
            "arn",
        )
        .await;
        let expected = expected_arns(&every_resource_arn, filter);
        assert_eq!(
            listed_resources, expected,
            "case {}/resources: expected {expected:?} in ARN order, got {listed_resources:?}",
            case.name
        );

        // The one predicate both handlers decide with answers for each stored
        // row exactly what each list did with it, so neither list can keep a
        // row the shared rule drops (or drop one it keeps).
        for arn in every_recommendation_arn.iter().chain(&every_resource_arn) {
            let keeps = affected_account_keeps(arn, filter);
            let reported =
                listed_recommendations.contains(arn) || listed_resources.contains(arn);
            assert_eq!(
                keeps, reported,
                "case {}/{arn}: the shared predicate answers {keeps}, the lists reported it: \
                 {reported}",
                case.name
            );
        }
    }
}

#[test]
fn the_shared_predicate_keeps_an_accountless_row_only_for_an_unfiltered_read() {
    // Rows: the ARN forms this service holds, from the patterns the model
    // declares — a member recommendation and a member resource carry a 12-digit
    // account segment, while a check ARN and an organization recommendation ARN
    // carry an EMPTY one (`CheckSummary$arn`, `OrganizationRecommendationArn`),
    // as does another service's accountless resource ARN — plus strings with
    // fewer segments than an ARN's account.
    let arns: &[&str] = &[
        "arn:aws:trustedadvisor::123456789012:recommendation/a-check-recommendation",
        "arn:aws:trustedadvisor::000000000012:recommendation-resource/a-check-recommendation/res-0001",
        "arn:aws:trustedadvisor:::check/a-check",
        "arn:aws:trustedadvisor:::organization-recommendation/a-check-organization-recommendation",
        "arn:aws:s3:::a-bucket",
        "arn:aws:trustedadvisor::",
        "arn:aws:trustedadvisor:us-east-1",
        "arn",
        "",
    ];
    // Accounts a request can name: the two the rows above are owned by, and one
    // that owns none of them. Each is a 12-digit `AccountId`, the only form the
    // modelled `length 12..12, pattern ^\d+$` admits.
    let accounts: &[&str] = &["123456789012", "000000000012", "222233334444"];

    for arn in arns {
        assert!(
            affected_account_keeps(arn, None),
            "case {arn:?}/unfiltered: expected an unfiltered read to keep every row, it dropped \
             this one"
        );
        for account in accounts {
            let owned_by = arn.contains(&format!(":{account}:"));
            let keeps = affected_account_keeps(arn, Some(account));
            assert_eq!(
                keeps, owned_by,
                "case {arn:?}/{account}: expected the row to be kept: {owned_by}, got {keeps}"
            );
        }
    }
}
