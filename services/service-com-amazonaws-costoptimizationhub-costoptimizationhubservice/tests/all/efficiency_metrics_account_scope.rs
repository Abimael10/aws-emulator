// SPDX-License-Identifier: CC0-1.0

//! Which ACCOUNTS' rows an aggregated read of Cost Optimization Hub reports
//! over, driven end-to-end over the wire (`awsJson1_0`) through the same
//! multi-service router the emulator boots.
//!
//! The rule under test: a read of this service is scoped to the accounts its
//! request NAMES and, naming none, to the calling account alone — whatever
//! organization role the store records for that account — so an aggregated read
//! that carries no member naming accounts (`ListEfficiencyMetrics` has no
//! `filter`) reports over exactly the rows an unfiltered list read of the same
//! caller reports, and over no row only a wider read could select.
//!
//! Every figure here is checked against another view of the SAME store — the
//! rows `ListRecommendations` serves this caller and the groups
//! `ListRecommendationSummaries` totals them into — never against a number
//! written into this file. The tables span how many member accounts the
//! organization records, whether a member is enrolled, whether its analysis has
//! come due, how many catalog rows of its own it owns, each grouping dimension,
//! both granularities, and each organization role the store can record for the
//! caller.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cost_optimization_hub::{
    all_ids, catalog_rows_for, enrolled, ids, ok, record_enrollment, router_with_store, Store,
};
use crate::efficiency_metrics::{
    deduped, expected, groups, interval_request, label, last_month_request, totals,
};

/// Member accounts of the organization the store records: twelve digits, which
/// is what the `AccountId` shape's `^[0-9]{12}$` pattern admits, and none of
/// them the caller.
const MEMBER_ONE: &str = "222233334444";
const MEMBER_TWO: &str = "555566667777";
const MEMBER_OPTED_OUT: &str = "888899990000";

/// An account OTHER than the one the emulator presents, for the store that
/// records the caller as a MEMBER of an organization it does not manage.
const FOREIGN_MANAGEMENT_ACCOUNT: &str = "111122223333";

/// The dimensions this suite groups along, each paired with the
/// `Recommendation` member it names (the names are the model's own:
/// `smithy_query(query="shape", shape="Recommendation")`), so a case can total
/// the list view along the same member. `None` is the request that names no
/// dimension, whose answer is one group covering every resource.
const GROUPINGS: &[(Option<&str>, Option<&str>)] = &[
    (None, None),
    (Some("AccountId"), Some("accountId")),
    (Some("Region"), Some("region")),
    (Some("ResourceType"), Some("currentResourceType")),
];

/// One member account the store records: its id, its enrollment status, whether
/// the analysis that enrollment started is already due, and how many catalog
/// rows of its own it owns.
type Member = (&'static str, &'static str, bool, usize);

/// Record `members` the way an Organizations control plane this emulator does
/// not have would have left them: an enrollment record per account, and catalog
/// rows owned by it.
fn record_members(store: &Store, members: &[Member]) -> Vec<String> {
    let mut recorded = Vec::new();
    for (account, status, due, rows) in members {
        record_enrollment(store, account, status, *due);
        recorded.extend(catalog_rows_for(store, account, *rows));
    }
    recorded
}

/// An enrolled router whose store records `management` as the management
/// account of its organization — `None` for the store that records no
/// organization at all, which is where every client of a freshly booted
/// emulator starts — with `members` recorded beside the caller.
///
/// The caller is enrolled the one way every suite of this crate establishes
/// "this account's data is readable" (`cost_optimization_hub::enrolled`: the
/// Compute Optimizer opt-in, the enrollment, and a bounded poll for the
/// catalog); the members are recorded afterwards, because this emulator has no
/// Organizations control plane to create another account's records.
async fn scoped_router(
    management: Option<&str>,
    members: &[Member],
) -> (MultiServiceRouter, Vec<String>) {
    let (router, store) = router_with_store();
    if let Some(account) = management {
        store.lock().organization_management_account_id = Some(account.to_string());
    }
    let router = enrolled(router).await;
    let member_rows = record_members(&store, members);
    (router, member_rows)
}

/// What one answer totals to, per value of the grouping dimension:
/// `(group, spend, savings)` in whole cents, by group label — the form the
/// oracle (`efficiency_metrics::expected`) states the same totals in.
type GroupTotals = Vec<(Option<String>, i64, i64)>;

/// The `(group, spend, savings)` triples one `ListEfficiencyMetrics` answer
/// reports, by group label, in whole cents.
fn totalled(answered: &[Value]) -> GroupTotals {
    let mut totalled: GroupTotals = answered
        .iter()
        .map(|group| {
            let (spend, savings) = totals(group);
            (label(group), spend, savings)
        })
        .collect();
    totalled.sort_by(|a, b| a.0.cmp(&b.0));
    totalled
}

/// The groups a `ListRecommendationSummaries` answer carries, in order.
async fn summary_groups(router: &MultiServiceRouter, group_by: &str) -> Vec<String> {
    let payload = ok(
        router,
        "ListRecommendationSummaries",
        &json!({"groupBy": group_by}),
    )
    .await;
    payload["items"]
        .as_array()
        .unwrap_or_else(|| panic!("case summaries: expected a list of groups in {payload}"))
        .iter()
        .map(|item| item["group"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// The metrics report over the rows the list readers report to the SAME caller:
/// a request that names no account is scoped to the calling account, so a member
/// account's rows — recorded in the organization the store holds, owned by an
/// enrolled member whose analysis has come due, selectable by this caller when
/// it NAMES that member — are totalled into no group of this answer.
///
/// The table spans how many members the organization records, whether each is
/// enrolled, whether its analysis is due, how many rows it owns, and every
/// grouping dimension beside the request that names none.
#[tokio::test]
async fn the_metrics_total_the_rows_an_unfiltered_list_read_reports() {
    let caller = engine::emulator_account_id().to_string();

    // (case, the member accounts the organization records)
    let cases: Vec<(&str, Vec<Member>)> = vec![
        ("no member accounts recorded", Vec::new()),
        (
            "one enrolled member with rows",
            vec![(MEMBER_ONE, "Active", true, 2)],
        ),
        (
            "one enrolled member whose analysis is not due",
            vec![(MEMBER_ONE, "Active", false, 2)],
        ),
        (
            "a member that opted out",
            vec![(MEMBER_OPTED_OUT, "Inactive", false, 2)],
        ),
        (
            "two enrolled members with rows",
            vec![
                (MEMBER_ONE, "Active", true, 2),
                (MEMBER_TWO, "Active", true, 3),
            ],
        ),
        (
            "an enrolled member, one not due and one opted out",
            vec![
                (MEMBER_ONE, "Active", true, 1),
                (MEMBER_TWO, "Active", false, 2),
                (MEMBER_OPTED_OUT, "Inactive", false, 2),
            ],
        ),
    ];

    let mut baseline: Option<GroupTotals> = None;
    for (case, members) in &cases {
        // The caller is the organization's MANAGEMENT account here: the caller
        // whose reads of another account the service admits at all, and so the
        // one whose aggregated read could widen.
        let (router, member_rows) = scoped_router(Some(&caller), members).await;

        // The oracle: the rows this caller's own unfiltered list read reports,
        // reduced to one recommendation per resource — the selection the
        // metrics aggregate.
        let rows = deduped(&router).await;
        assert!(
            !rows.is_empty(),
            "case {case:?}: expected the enrolled caller's own rows to total, got {rows:?}"
        );
        for row in &rows {
            let owner = row["accountId"].as_str().unwrap_or_default();
            assert_eq!(
                owner, caller,
                "case {case:?}: expected an unfiltered list read to report the caller's own \
                 rows only, got a row of {owner:?} in {row}"
            );
        }

        for (dimension, member) in GROUPINGS {
            let case = format!("{case}, grouped by {dimension:?}");
            let mut body = last_month_request();
            if let Some(dimension) = dimension {
                body["groupBy"] = json!(dimension);
            }
            let answered = groups(&ok(&router, "ListEfficiencyMetrics", &body).await);
            let reported = totalled(&answered);
            let wanted = expected(&rows, *member);
            assert_eq!(
                reported, wanted,
                "case {case:?}: expected the groups {wanted:?} the list read totals to, got \
                 {reported:?}"
            );
            for (group, _, _) in &reported {
                assert!(
                    group.as_deref().is_none_or(|group| group != MEMBER_ONE
                        && group != MEMBER_TWO
                        && group != MEMBER_OPTED_OUT),
                    "case {case:?}: expected no member account's group, got {reported:?}"
                );
            }
        }

        // The other aggregated reader of the same store agrees about the
        // accounts: both report the caller's account and no other.
        let summarised = summary_groups(&router, "AccountId").await;
        assert_eq!(
            summarised,
            vec![caller.clone()],
            "case {case:?}: expected the summaries to group the caller's account alone, got \
             {summarised:?}"
        );
        let mut body = last_month_request();
        body["groupBy"] = json!("AccountId");
        let metric_groups: Vec<Option<String>> = totalled(&groups(
            &ok(&router, "ListEfficiencyMetrics", &body).await,
        ))
        .into_iter()
        .map(|(group, _, _)| group)
        .collect();
        assert_eq!(
            metric_groups,
            vec![Some(caller.clone())],
            "case {case:?}: expected the metrics to group the caller's account alone, got \
             {metric_groups:?}"
        );

        // The case is not vacuous: the rows left out exist, and this caller CAN
        // select them — by naming their owner, which this operation has no
        // member for — so what keeps them out of the metrics is the scope the
        // request resolves to.
        let selectable: Vec<String> = members
            .iter()
            .filter(|(_, status, due, _)| *status == "Active" && *due)
            .flat_map(|(account, _, _, _)| {
                member_rows
                    .iter()
                    .filter(|id| id.starts_with(account))
                    .cloned()
                    .collect::<Vec<String>>()
            })
            .collect();
        if !selectable.is_empty() {
            let named: Vec<String> = members
                .iter()
                .map(|(account, _, _, _)| (*account).to_string())
                .collect();
            let listed = ids(&ok(
                &router,
                "ListRecommendations",
                &json!({
                    "includeAllRecommendations": true,
                    "filter": {"accountIds": named},
                }),
            )
            .await);
            assert_eq!(
                listed, selectable,
                "case {case:?}: expected the members' rows to be selectable by naming them, got \
                 {listed:?}"
            );
            let own = all_ids(&router).await;
            for id in &selectable {
                assert!(
                    !own.contains(id),
                    "case {case:?}: expected {id:?} to stay out of the unfiltered list read, got \
                     {own:?}"
                );
            }
        }

        // Every case reports the same figures: what the organization records
        // beside the caller changes nothing about the caller's own series.
        let mut body = last_month_request();
        body["groupBy"] = json!("Region");
        let reported = totalled(&groups(
            &ok(&router, "ListEfficiencyMetrics", &body).await,
        ));
        match &baseline {
            None => baseline = Some(reported),
            Some(first) => assert_eq!(
                &reported, first,
                "case {case:?}: expected the caller's own figures {first:?}, got {reported:?}"
            ),
        }
    }
}

/// The scope does not widen with the organization ROLE the store records: the
/// store that records no organization, the one that records the caller as its
/// organization's management account, and the one that records it as a member of
/// an organization another account manages all report the same series over the
/// caller's own rows — at either granularity.
#[tokio::test]
async fn the_metrics_do_not_widen_with_the_organization_role_the_store_records() {
    let caller = engine::emulator_account_id().to_string();
    let members: Vec<Member> = vec![
        (MEMBER_ONE, "Active", true, 2),
        (MEMBER_TWO, "Active", true, 3),
    ];

    // (case, the account the store records as the organization's management
    // account)
    let roles: Vec<(&str, Option<String>)> = vec![
        ("no organization recorded", None),
        ("the caller manages the organization", Some(caller.clone())),
        (
            "the caller is a member of another account's organization",
            Some(FOREIGN_MANAGEMENT_ACCOUNT.to_string()),
        ),
    ];

    let mut reported: Vec<(String, GroupTotals)> = Vec::new();
    for (case, management) in &roles {
        let (router, _) = scoped_router(management.as_deref(), &members).await;

        for granularity in ["Monthly", "Daily"] {
            let case = format!("{case}, {granularity}");
            let mut body = interval_request(granularity);
            body["groupBy"] = json!("AccountId");
            let answered = totalled(&groups(
                &ok(&router, "ListEfficiencyMetrics", &body).await,
            ));
            let labels: Vec<Option<String>> = answered
                .iter()
                .map(|(group, _, _)| group.clone())
                .collect();
            assert_eq!(
                labels,
                vec![Some(caller.clone())],
                "case {case:?}: expected the caller's account alone, got {answered:?}"
            );
            if granularity == "Monthly" {
                reported.push((case, answered));
            }
        }
    }

    let (first_case, first) = reported
        .first()
        .expect("every role reports a monthly series")
        .clone();
    for (case, answered) in &reported {
        assert_eq!(
            answered, &first,
            "case {case:?}: expected the figures {first_case:?} reports ({first:?}), got \
             {answered:?}"
        );
    }
}
