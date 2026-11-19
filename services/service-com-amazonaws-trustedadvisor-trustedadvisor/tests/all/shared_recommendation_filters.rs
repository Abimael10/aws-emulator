// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the ONE filter rule BOTH recommendation listings of this
//! service apply — `ListRecommendations` over the account-level recommendations
//! and `ListOrganizationRecommendations` over the organization-level ones — and
//! for the ONE projection all four recommendation views report, driven over HTTP
//! through the same `MultiServiceRouter` the emulator serves with.
//!
//! The filter rule, over a TABLE of filter combinations rather than one call per
//! operation: a listing reports exactly the stored recommendations its filters
//! keep, in key order, where `pillar` and `awsService` are membership tests over
//! the lists the recommendation holds, `afterLastUpdatedAt` and
//! `beforeLastUpdatedAt` are INCLUSIVE bounds on the stored `lastUpdatedAt`,
//! every other filter is an exact equality on the recommendation's own member,
//! and a filter the request omitted keeps every recommendation. The two listings
//! read the same filters off the two entry types through one predicate, so they
//! cannot select differently; the cursor scope those filters contribute is the
//! same one pair, which is why a token issued under one filter set — or by the
//! other listing — resumes neither.
//!
//! The projection rule, over a table of recommendation types, languages and
//! lifecycle states: a stored recommendation is reported by one projection, so
//! every member two of its views declare carries the same value in both — the
//! single read against the listing at each level, and the organization views
//! against the account views of the text they share — and the full shapes add to
//! that projection only the members they model beyond the summary.
//!
//! The filter names come from the two operations
//! (`smithy_query(query="operation",operation="ListRecommendations,ListOrganizationRecommendations",
//! model_path="services/service-com-amazonaws-trustedadvisor-trustedadvisor/smithy-model/com-amazonaws-trustedadvisor-trustedadvisor.json")`)
//! and the values the rows span from the model's own enums
//! (`RECOMMENDATION_TYPE_VALUES`, `RECOMMENDATION_STATUS_VALUES`,
//! `RECOMMENDATION_PILLAR_VALUES`, `RECOMMENDATION_LANGUAGE_VALUES`,
//! `RECOMMENDATION_LIFECYCLE_STAGE_VALUES` and
//! `UPDATE_RECOMMENDATION_LIFECYCLE_STAGE_REASON_CODE_VALUES` in
//! `ops::types::shared_types`); the wire contract of each read is pinned by
//! `tests/all/recommendation_loop.rs` and
//! `tests/all/organization_recommendations.rs`.
//!
//! Every call is made as an account whose support plan includes the Trusted
//! Advisor API; the gate ahead of every handler is pinned in
//! `tests/all/support_level_gate.rs`.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use service_com_amazonaws_trustedadvisor_trustedadvisor::ops::types::shared_types::RECOMMENDATION_LANGUAGE_VALUES;
use service_com_amazonaws_trustedadvisor_trustedadvisor::ops::types::{
    RECOMMENDATION_LIFECYCLE_STAGE_VALUES, RECOMMENDATION_PILLAR_VALUES,
    RECOMMENDATION_STATUS_VALUES, RECOMMENDATION_TYPE_VALUES,
    UPDATE_RECOMMENDATION_LIFECYCLE_STAGE_REASON_CODE_VALUES,
};
use service_com_amazonaws_trustedadvisor_trustedadvisor::state::SupportPlan;
use smithy_server_runtime::MultiServiceRouter;

use crate::organization_recommendation_lifecycle::organization_recommendation_arn;
use crate::recommendation_loop::{
    call, check_arn, emulator_on_plan, error_code, field_of_each, label, recommendation_arn,
    PRIORITIZED_CHECK_IDS,
};

/// The classification members of one seeded check's recommendation: what the
/// eight shared filters select on. Written out by hand; its source is the `SEED`
/// table in `src/state.rs`.
///
/// The account-level and the organization-level entry of one check carry the
/// same values, so one row describes the stored recommendation at both levels —
/// except that only a PRIORITIZED recommendation has an organization-level entry
/// at all (see [`Level::holds`]).
struct Stored {
    check_id: &'static str,
    r#type: &'static str,
    status: &'static str,
    pillars: &'static [&'static str],
    aws_services: &'static [&'static str],
    source: &'static str,
    /// The stored `lastUpdatedAt`, as the RFC 3339 UTC instant a request names a
    /// bound with. Every instant here and in the rows below is written in that
    /// one form, so comparing the strings compares the instants.
    last_updated_at: &'static str,
}

/// The seeded recommendations, in check-id order — which is also the order both
/// listings report them in, since each key is an ARN whose tail is that id.
const STORED: &[Stored] = &[
    Stored {
        check_id: "amazon-ebs-snapshots",
        r#type: "standard",
        status: "error",
        pillars: &["fault_tolerance"],
        aws_services: &["ebs"],
        source: "ta_check",
        last_updated_at: "2024-01-03T00:00:00Z",
    },
    Stored {
        check_id: "iam-access-key-rotation",
        r#type: "standard",
        status: "ok",
        pillars: &["security"],
        aws_services: &["iam"],
        source: "ta_check",
        last_updated_at: "2024-01-04T00:00:00Z",
    },
    Stored {
        check_id: "low-utilization-ec2-instances",
        r#type: "priority",
        status: "warning",
        pillars: &["cost_optimizing"],
        aws_services: &["ec2"],
        source: "ta_check",
        last_updated_at: "2024-01-02T00:00:00Z",
    },
    Stored {
        check_id: "rds-idle-db-instances",
        r#type: "priority",
        status: "warning",
        pillars: &["cost_optimizing", "performance"],
        aws_services: &["rds"],
        source: "rds",
        last_updated_at: "2024-01-05T00:00:00Z",
    },
];

/// The two listings the one filter rule is applied by.
#[derive(Debug, Clone, Copy)]
enum Level {
    Account,
    Organization,
}

impl Level {
    /// Both listings, so every row of the table is driven through each read.
    const ALL: &'static [Self] = &[Self::Account, Self::Organization];

    /// The modelled URI of this listing, with the row's filters as its query.
    fn path(self, query: &str) -> String {
        match self {
            Self::Account => format!("/v1/recommendations{query}"),
            Self::Organization => format!("/v1/organization-recommendations{query}"),
        }
    }

    /// The response member this listing wraps its summaries in.
    fn summaries_member(self) -> &'static str {
        match self {
            Self::Account => "recommendationSummaries",
            Self::Organization => "organizationRecommendationSummaries",
        }
    }

    /// The ARN this listing reports for one check's recommendation.
    fn reported_arn(self, check_id: &str) -> String {
        match self {
            Self::Account => recommendation_arn(check_id),
            Self::Organization => organization_recommendation_arn(check_id),
        }
    }

    /// Whether the store holds a recommendation of THIS level for `stored`: the
    /// organization surface of this service supports only prioritized
    /// recommendations, so a standard check has no organization-level entry to
    /// filter at all (that rule is pinned on its own in
    /// `tests/all/organization_prioritized_only.rs`).
    fn holds(self, stored: &Stored) -> bool {
        match self {
            Self::Account => true,
            Self::Organization => stored.r#type == "priority",
        }
    }
}

/// A mounted emulator on an entitled plan, reporting the seeded store.
fn emulator() -> MultiServiceRouter {
    emulator_on_plan(SupportPlan::Business).0
}

/// The ARNs one listing reports for `query`, asserting 200 first.
async fn listed_arns(
    router: &MultiServiceRouter,
    case: &str,
    level: Level,
    query: &str,
) -> Vec<String> {
    let (status, body) = call(router, "GET", &level.path(query), None).await;
    assert_eq!(
        status, 200,
        "case {case}: expected 200 from the {level:?}-level listing, got {status} with {body}"
    );
    field_of_each(&body, level.summaries_member(), "arn")
}

/// Whether `row`'s stored `lastUpdatedAt` lies within the bounds a request
/// named: both are INCLUSIVE, and a bound the request omitted keeps every
/// instant. Stated here once so each timestamp row below names only its bounds.
fn within_bounds(row: &Stored, after: Option<&str>, before: Option<&str>) -> bool {
    after.is_none_or(|bound| row.last_updated_at >= bound)
        && before.is_none_or(|bound| row.last_updated_at <= bound)
}

/// The ARNs the rule keeps at `level` under `keep`, in the order the listing
/// reports them.
fn expected_arns(level: Level, keep: &dyn Fn(&Stored) -> bool) -> Vec<String> {
    STORED
        .iter()
        .filter(|row| level.holds(row) && keep(row))
        .map(|row| level.reported_arn(row.check_id))
        .collect()
}

/// The filter table: a query string and the rule that says which stored
/// recommendation it keeps. Rows differ along the filter in force, the value it
/// names, the combination of filters, the inclusive timestamp bounds and the
/// number of matches (all, several, one, none).
#[allow(clippy::type_complexity)]
fn rows() -> Vec<(String, Box<dyn Fn(&Stored) -> bool>)> {
    vec![
        // No filter at all keeps every stored recommendation.
        (String::new(), Box::new(|_: &Stored| true)),
        (
            "?type=priority".to_string(),
            Box::new(|row: &Stored| row.r#type == "priority"),
        ),
        (
            "?type=standard".to_string(),
            Box::new(|row: &Stored| row.r#type == "standard"),
        ),
        (
            "?status=error".to_string(),
            Box::new(|row: &Stored| row.status == "error"),
        ),
        (
            "?status=ok".to_string(),
            Box::new(|row: &Stored| row.status == "ok"),
        ),
        (
            "?status=warning".to_string(),
            Box::new(|row: &Stored| row.status == "warning"),
        ),
        // `pillar` is a MEMBERSHIP test: `performance` is the second pillar of
        // one row, and `cost_optimizing` the first of two rows.
        (
            "?pillar=cost_optimizing".to_string(),
            Box::new(|row: &Stored| row.pillars.contains(&"cost_optimizing")),
        ),
        (
            "?pillar=performance".to_string(),
            Box::new(|row: &Stored| row.pillars.contains(&"performance")),
        ),
        (
            "?pillar=fault_tolerance".to_string(),
            Box::new(|row: &Stored| row.pillars.contains(&"fault_tolerance")),
        ),
        (
            "?pillar=security".to_string(),
            Box::new(|row: &Stored| row.pillars.contains(&"security")),
        ),
        // Two pillars no stored recommendation carries: both listings keep
        // nothing.
        (
            "?pillar=operational_excellence".to_string(),
            Box::new(|row: &Stored| row.pillars.contains(&"operational_excellence")),
        ),
        (
            "?pillar=service_limits".to_string(),
            Box::new(|row: &Stored| row.pillars.contains(&"service_limits")),
        ),
        (
            "?awsService=ec2".to_string(),
            Box::new(|row: &Stored| row.aws_services.contains(&"ec2")),
        ),
        (
            "?awsService=rds".to_string(),
            Box::new(|row: &Stored| row.aws_services.contains(&"rds")),
        ),
        (
            "?awsService=iam".to_string(),
            Box::new(|row: &Stored| row.aws_services.contains(&"iam")),
        ),
        // A service no stored recommendation names, and one differing from a
        // stored value only in case: the test is exact equality, so neither
        // keeps anything.
        (
            "?awsService=lambda".to_string(),
            Box::new(|row: &Stored| row.aws_services.contains(&"lambda")),
        ),
        (
            "?awsService=EC2".to_string(),
            Box::new(|row: &Stored| row.aws_services.contains(&"EC2")),
        ),
        (
            "?source=ta_check".to_string(),
            Box::new(|row: &Stored| row.source == "ta_check"),
        ),
        (
            "?source=rds".to_string(),
            Box::new(|row: &Stored| row.source == "rds"),
        ),
        (
            "?source=well_architected".to_string(),
            Box::new(|row: &Stored| row.source == "well_architected"),
        ),
        // `afterLastUpdatedAt` / `beforeLastUpdatedAt`: inclusive bounds, so an
        // instant equal to a stored one keeps that row.
        (
            "?afterLastUpdatedAt=2024-01-04T00:00:00Z".to_string(),
            Box::new(|row: &Stored| within_bounds(row, Some("2024-01-04T00:00:00Z"), None)),
        ),
        (
            "?beforeLastUpdatedAt=2024-01-03T00:00:00Z".to_string(),
            Box::new(|row: &Stored| within_bounds(row, None, Some("2024-01-03T00:00:00Z"))),
        ),
        (
            "?afterLastUpdatedAt=2024-01-03T12:00:00Z&beforeLastUpdatedAt=2024-01-04T12:00:00Z"
                .to_string(),
            Box::new(|row: &Stored| {
                within_bounds(
                    row,
                    Some("2024-01-03T12:00:00Z"),
                    Some("2024-01-04T12:00:00Z"),
                )
            }),
        ),
        // The whole seeded span, both ends exactly on a stored instant.
        (
            "?afterLastUpdatedAt=2024-01-02T00:00:00Z&beforeLastUpdatedAt=2024-01-05T00:00:00Z"
                .to_string(),
            Box::new(|row: &Stored| {
                within_bounds(
                    row,
                    Some("2024-01-02T00:00:00Z"),
                    Some("2024-01-05T00:00:00Z"),
                )
            }),
        ),
        // Bounds outside the seeded span, and a pair no instant can satisfy.
        (
            "?afterLastUpdatedAt=2030-01-01T00:00:00Z".to_string(),
            Box::new(|row: &Stored| within_bounds(row, Some("2030-01-01T00:00:00Z"), None)),
        ),
        (
            "?beforeLastUpdatedAt=2020-01-01T00:00:00Z".to_string(),
            Box::new(|row: &Stored| within_bounds(row, None, Some("2020-01-01T00:00:00Z"))),
        ),
        (
            "?afterLastUpdatedAt=2024-01-05T00:00:00Z&beforeLastUpdatedAt=2024-01-02T00:00:00Z"
                .to_string(),
            Box::new(|row: &Stored| {
                within_bounds(
                    row,
                    Some("2024-01-05T00:00:00Z"),
                    Some("2024-01-02T00:00:00Z"),
                )
            }),
        ),
        // Combinations: filters that hold together, and filters that cannot.
        (
            "?type=priority&status=warning&pillar=performance".to_string(),
            Box::new(|row: &Stored| {
                row.r#type == "priority"
                    && row.status == "warning"
                    && row.pillars.contains(&"performance")
            }),
        ),
        (
            "?type=priority&awsService=rds&source=rds&afterLastUpdatedAt=2024-01-05T00:00:00Z"
                .to_string(),
            Box::new(|row: &Stored| {
                row.r#type == "priority"
                    && row.aws_services.contains(&"rds")
                    && row.source == "rds"
                    && within_bounds(row, Some("2024-01-05T00:00:00Z"), None)
            }),
        ),
        (
            "?type=standard&source=rds".to_string(),
            Box::new(|row: &Stored| row.r#type == "standard" && row.source == "rds"),
        ),
    ]
}

#[tokio::test]
async fn both_recommendation_listings_select_the_same_rows_with_the_same_filters() {
    let router = emulator();
    let rows = rows();

    // The rows span every value the filtered enums declare, so a value added to
    // one of them is not silently left untested.
    for value in RECOMMENDATION_TYPE_VALUES {
        assert!(
            rows.iter()
                .any(|(query, _)| query.contains(&format!("type={value}"))),
            "the table must cover the recommendation type {value}"
        );
    }
    for value in RECOMMENDATION_STATUS_VALUES {
        assert!(
            rows.iter()
                .any(|(query, _)| query.contains(&format!("status={value}"))),
            "the table must cover the recommendation status {value}"
        );
    }
    for value in RECOMMENDATION_PILLAR_VALUES {
        assert!(
            rows.iter()
                .any(|(query, _)| query.contains(&format!("pillar={value}"))),
            "the table must cover the recommendation pillar {value}"
        );
    }

    for (query, keep) in &rows {
        for level in Level::ALL {
            let case = format!("{level:?} {query:?}");
            let expected = expected_arns(*level, keep.as_ref());
            let reported = listed_arns(&router, &case, *level, query).await;
            assert_eq!(
                reported, expected,
                "case {case}: expected the filters to keep {expected:?}, got {reported:?}"
            );
        }
    }

    // `checkIdentifier` selects by the check ARN each recommendation carries —
    // one row per seeded check, at both levels.
    for row in STORED {
        let query = format!("?checkIdentifier={}", label(&check_arn(row.check_id)));
        for level in Level::ALL {
            let case = format!("{level:?} checkIdentifier={}", row.check_id);
            let expected = expected_arns(*level, &|candidate: &Stored| {
                candidate.check_id == row.check_id
            });
            let reported = listed_arns(&router, &case, *level, &query).await;
            assert_eq!(
                reported, expected,
                "case {case}: expected {expected:?}, got {reported:?}"
            );
        }
    }

    // The equality is exact there too: a check ARN whose tail differs only in
    // case names no stored recommendation at either level.
    let shouting = check_arn("LOW-UTILIZATION-EC2-INSTANCES");
    let query = format!("?checkIdentifier={}", label(&shouting));
    for level in Level::ALL {
        let case = format!("{level:?} checkIdentifier={shouting}");
        let reported = listed_arns(&router, &case, *level, &query).await;
        assert!(
            reported.is_empty(),
            "case {case}: expected no recommendation, got {reported:?}"
        );
    }
}

#[tokio::test]
async fn each_listings_cursor_belongs_to_its_own_list_and_filter_set() {
    let router = emulator();

    // One page at a time under a filter in force: following the token walks
    // exactly the rows the same rule keeps, so the scope the filters contribute
    // round-trips through the cursor at both levels.
    let keep = |row: &Stored| row.status == "warning";
    let mut issued = Vec::new();
    for level in Level::ALL {
        let case = format!("{level:?} ?status=warning&maxResults=1");
        let mut walked = Vec::new();
        let mut query = "?status=warning&maxResults=1".to_string();
        loop {
            let (status, body) = call(&router, "GET", &level.path(&query), None).await;
            assert_eq!(
                status, 200,
                "case {case}: expected 200, got {status} with {body}"
            );
            walked.extend(field_of_each(&body, level.summaries_member(), "arn"));
            match body["nextToken"].as_str() {
                Some(token) => {
                    issued.push((*level, token.to_string()));
                    query = format!("?status=warning&maxResults=1&nextToken={}", label(token));
                }
                None => break,
            }
        }
        let expected = expected_arns(*level, &keep);
        assert_eq!(
            walked, expected,
            "case {case}: expected the token walk to report {expected:?}, got {walked:?}"
        );
    }

    // A token one page issued resumes neither another filter set nor the other
    // listing: the scope carries the eight filters AND the list it was issued
    // for.
    assert!(
        !issued.is_empty(),
        "the paged walks must have issued a token to replay"
    );
    for (level, token) in &issued {
        let replays: Vec<(String, String)> = Level::ALL
            .iter()
            .flat_map(|other| {
                [
                    // The same list under a different filter set.
                    format!("?status=ok&maxResults=1&nextToken={}", label(token)),
                    // The same list with the filter dropped altogether.
                    format!("?maxResults=1&nextToken={}", label(token)),
                    // The same filter set, with a bound added to it.
                    format!(
                        "?status=warning&beforeLastUpdatedAt=2024-01-05T00:00:00Z&maxResults=1&nextToken={}",
                        label(token)
                    ),
                ]
                .map(|query| (format!("{other:?}"), other.path(&query)))
            })
            .collect();
        for (other, path) in replays {
            let case = format!("a {level:?} token on the {other} listing at {path}");
            let (status, body) = call(&router, "GET", &path, None).await;
            assert_eq!(
                status, 400,
                "case {case}: expected 400, got {status} with {body}"
            );
            assert_eq!(
                error_code(&body),
                "ValidationException",
                "case {case}: expected ValidationException, got {body}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// The one projection the four recommendation views report
// ---------------------------------------------------------------------------

/// The members `RecommendationSummary` declares: the projection of a stored
/// recommendation every one of the four recommendation views reports. Written
/// out by hand; its source is this service's model
/// (`smithy_query(query="shape",shape="Recommendation,RecommendationSummary,OrganizationRecommendation,OrganizationRecommendationSummary",
/// model_path="services/service-com-amazonaws-trustedadvisor-trustedadvisor/smithy-model/com-amazonaws-trustedadvisor-trustedadvisor.json")`),
/// where this member set is a strict SUBSET of `Recommendation`'s — the same
/// names over the same targets.
const PROJECTED_MEMBERS: &[&str] = &[
    "id",
    "type",
    "checkArn",
    "status",
    "lifecycleStage",
    "pillars",
    "source",
    "awsServices",
    "name",
    "resourcesAggregates",
    "pillarSpecificAggregates",
    "createdAt",
    "lastUpdatedAt",
    "arn",
    "statusReason",
];

/// The one member of that projection the ORGANIZATION shapes do not declare —
/// `statusReason` is on the account-level pair only — so the organization views
/// drop it (same source as above).
const ACCOUNT_ONLY_MEMBERS: &[&str] = &["statusReason"];

/// The members the model holds no value for at either level: they identify the
/// Trusted Advisor Priority person (or the Technical Account Manager) who acted
/// on the recommendation, which the store records none of (same source as
/// above).
const UNSOURCED_MEMBERS: &[&str] = &[
    "createdBy",
    "updatedOnBehalfOf",
    "updatedOnBehalfOfJobTitle",
];

/// The members of the shared projection `level`'s two views both declare.
fn projected_members(level: Level) -> Vec<&'static str> {
    PROJECTED_MEMBERS
        .iter()
        .copied()
        .filter(|member| match level {
            Level::Account => true,
            Level::Organization => !ACCOUNT_ONLY_MEMBERS.contains(member),
        })
        .collect()
}

/// Assert two views of ONE recommendation report `members` alike.
fn agree_on(
    case: &str,
    members: &[&str],
    (left_name, left): (&str, &Value),
    (right_name, right): (&str, &Value),
) {
    for member in members {
        assert_eq!(
            left[member], right[member],
            "case {case}: expected the {left_name} and the {right_name} to report the same \
             {member}, got {:?} and {:?}",
            left[member], right[member]
        );
    }
}

/// The `recommendation` `GetRecommendation` reports for one seeded check, with
/// `query` as its request query, asserting 200 first.
async fn account_read(
    router: &MultiServiceRouter,
    case: &str,
    check_id: &str,
    query: &str,
) -> Value {
    let path = format!(
        "/v1/recommendations/{}{query}",
        label(&recommendation_arn(check_id))
    );
    let (status, body) = call(router, "GET", &path, None).await;
    assert_eq!(
        status, 200,
        "case {case}: expected 200 from GetRecommendation, got {status} with {body}"
    );
    let reported = body["recommendation"].clone();
    assert!(
        reported.is_object(),
        "case {case}: expected a recommendation, got {body}"
    );
    reported
}

/// The `organizationRecommendation` `GetOrganizationRecommendation` reports for
/// one seeded check, asserting 200 first. The operation models no `language`.
async fn organization_read(router: &MultiServiceRouter, case: &str, check_id: &str) -> Value {
    let path = format!(
        "/v1/organization-recommendations/{}",
        label(&organization_recommendation_arn(check_id))
    );
    let (status, body) = call(router, "GET", &path, None).await;
    assert_eq!(
        status, 200,
        "case {case}: expected 200 from GetOrganizationRecommendation, got {status} with {body}"
    );
    let reported = body["organizationRecommendation"].clone();
    assert!(
        reported.is_object(),
        "case {case}: expected an organizationRecommendation, got {body}"
    );
    reported
}

/// One listing's summaries, keyed by the `arn` each of them reports, asserting
/// 200 first.
async fn summaries_by_arn(
    router: &MultiServiceRouter,
    case: &str,
    level: Level,
    query: &str,
) -> BTreeMap<String, Value> {
    let (status, body) = call(router, "GET", &level.path(query), None).await;
    assert_eq!(
        status, 200,
        "case {case}: expected 200 from the {level:?}-level listing, got {status} with {body}"
    );
    body[level.summaries_member()]
        .as_array()
        .unwrap_or_else(|| {
            panic!(
                "case {case}: expected a {} array, got {body}",
                level.summaries_member()
            )
        })
        .iter()
        .map(|summary| {
            let arn = summary["arn"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case}: a summary with no arn: {summary}"));
            (arn.to_string(), summary.clone())
        })
        .collect()
}

#[tokio::test]
async fn one_projection_serves_the_single_read_and_the_listing_at_both_levels() {
    let router = emulator();

    // The rows span the class the projection generalises over: every seeded
    // recommendation, so both values of `RecommendationType`, and — on the
    // account level, the only one whose operations model `language` — no
    // language at all plus every value `RecommendationLanguage` declares.
    let languages: Vec<Option<&str>> = std::iter::once(None)
        .chain(RECOMMENDATION_LANGUAGE_VALUES.iter().copied().map(Some))
        .collect();

    // The `name` and `description` each check reports to a request that names no
    // language: what the organization views, which model none, must report too.
    let mut default_text: BTreeMap<&str, (Value, Value)> = BTreeMap::new();
    // Every `name` one check reported across the language rows, so the table is
    // shown to exercise the selection rather than one stored copy.
    let mut names: BTreeMap<&str, Vec<Value>> = BTreeMap::new();

    for language in &languages {
        let read_query = language.map_or_else(String::new, |language| format!("?language={language}"));
        let list_query = language.map_or_else(
            || "?maxResults=200".to_string(),
            |language| format!("?maxResults=200&language={language}"),
        );
        let listed = summaries_by_arn(
            &router,
            &format!("account listing language={language:?}"),
            Level::Account,
            &list_query,
        )
        .await;

        for row in STORED {
            let case = format!("{} language={language:?}", row.check_id);
            let full = account_read(&router, &case, row.check_id, &read_query).await;
            let arn = recommendation_arn(row.check_id);
            let summary = listed.get(&arn).unwrap_or_else(|| {
                panic!("case {case}: the listing reported no summary for {arn}")
            });

            // One recommendation, one projection: every member the full shape
            // and the summary both declare carries the same value.
            agree_on(
                &case,
                PROJECTED_MEMBERS,
                ("single read", &full),
                ("summary", summary),
            );

            // The rules that projection applies, in BOTH views: a member the
            // model scopes to Trusted Advisor Priority is reported only for a
            // prioritized recommendation, and `lastUpdatedAt`, which carries no
            // such scoping, for either type.
            for (view_name, view) in [("single read", &full), ("summary", summary)] {
                let case = format!("{case}/{view_name}");
                let prioritized = row.r#type == "priority";
                assert_eq!(
                    view["createdAt"].is_null(),
                    !prioritized,
                    "case {case}: expected createdAt {} a {} recommendation, got {:?}",
                    if prioritized { "on" } else { "off" },
                    row.r#type,
                    view["createdAt"]
                );
                assert!(
                    !view["lastUpdatedAt"].is_null(),
                    "case {case}: expected a lastUpdatedAt whatever the type, got {:?}",
                    view["lastUpdatedAt"]
                );
                if !prioritized {
                    assert!(
                        view["lifecycleStage"].is_null(),
                        "case {case}: expected no lifecycleStage on a {} recommendation, got {:?}",
                        row.r#type,
                        view["lifecycleStage"]
                    );
                }
            }
            for member in UNSOURCED_MEMBERS {
                assert!(
                    full[member].is_null(),
                    "case {case}: expected no {member}, got {:?}",
                    full[member]
                );
            }

            names
                .entry(row.check_id)
                .or_default()
                .push(full["name"].clone());
            if language.is_none() {
                default_text.insert(
                    row.check_id,
                    (full["name"].clone(), full["description"].clone()),
                );
            }
        }
    }

    // The same projection at the organization level, whose two views report it
    // without the one member their shapes do not model.
    let listed = summaries_by_arn(
        &router,
        "organization listing",
        Level::Organization,
        "?maxResults=200",
    )
    .await;
    for row in STORED.iter().filter(|row| Level::Organization.holds(row)) {
        let case = format!("organization {}", row.check_id);
        let full = organization_read(&router, &case, row.check_id).await;
        let arn = organization_recommendation_arn(row.check_id);
        let summary = listed
            .get(&arn)
            .unwrap_or_else(|| panic!("case {case}: the listing reported no summary for {arn}"));
        agree_on(
            &case,
            &projected_members(Level::Organization),
            ("single read", &full),
            ("summary", summary),
        );
        for member in ACCOUNT_ONLY_MEMBERS.iter().chain(UNSOURCED_MEMBERS) {
            assert!(
                full[member].is_null() && summary[member].is_null(),
                "case {case}: expected no {member} at the organization level, got {:?} and {:?}",
                full[member],
                summary[member]
            );
        }

        // Neither organization operation models `language`, so both of its views
        // report the copy a request naming no language is answered in.
        let (name, description) = default_text
            .get(row.check_id)
            .unwrap_or_else(|| panic!("case {case}: no account-level text was read for this check"));
        for (view_name, view) in [("single read", &full), ("summary", summary)] {
            assert_eq!(
                &view["name"], name,
                "case {case}: expected the {view_name} to report the default-language name \
                 {name:?}, got {:?}",
                view["name"]
            );
        }
        assert_eq!(
            &full["description"], description,
            "case {case}: expected the default-language description {description:?}, got {:?}",
            full["description"]
        );
    }

    // The language rows did exercise the selection rather than one copy: some
    // check reported a different `name` for some language than for none.
    assert!(
        names
            .values()
            .any(|reported| reported.iter().any(|name| name != &reported[0])),
        "the language rows must report more than one stored copy of a name, got {names:?}"
    );
}

#[tokio::test]
async fn a_lifecycle_move_reaches_all_four_views_through_the_one_projection() {
    let router = emulator();
    // A prioritized check, so the store holds a recommendation at both levels
    // and a lifecycle update applies to each. An organization recommendation's
    // lifecycle is its OWN, so each level is moved, and read, on its own.
    let check = PRIORITIZED_CHECK_IDS[0];
    let reason_code = UPDATE_RECOMMENDATION_LIFECYCLE_STAGE_REASON_CODE_VALUES[0];

    // Rows: every value `RecommendationLifecycleStage` declares, then one that
    // OMITS both reason members — the update is an `@idempotent` PUT, so it
    // replaces the lifecycle rather than adding to it, at both levels.
    let mut rows: Vec<(&str, Option<&str>)> = RECOMMENDATION_LIFECYCLE_STAGE_VALUES
        .iter()
        .map(|stage| (*stage, Some(reason_code)))
        .collect();
    rows.push((RECOMMENDATION_LIFECYCLE_STAGE_VALUES[0], None));

    for (stage, code) in rows {
        let case = format!("{stage} updateReasonCode={code:?}");
        let lifecycle_paths = [
            (
                Level::Account,
                format!(
                    "/v1/recommendations/{}/lifecycle",
                    label(&recommendation_arn(check))
                ),
            ),
            (
                Level::Organization,
                format!(
                    "/v1/organization-recommendations/{}/lifecycle",
                    label(&organization_recommendation_arn(check))
                ),
            ),
        ];
        for (level, path) in &lifecycle_paths {
            let mut request = json!({ "lifecycleStage": stage });
            if let Some(code) = code {
                request["updateReason"] = json!("moved to prove one projection reports it");
                request["updateReasonCode"] = json!(code);
            }
            let (status, body) = call(&router, "PUT", path, Some(request)).await;
            assert_eq!(
                status, 200,
                "case {case}: expected 200 from the {level:?}-level lifecycle update, got \
                 {status} with {body}"
            );
        }

        // Both views of each level report the moved lifecycle, from the one
        // projection: the summary and the single read cannot differ on it.
        let account_full = account_read(&router, &case, check, "").await;
        let account_listed =
            summaries_by_arn(&router, &case, Level::Account, "?maxResults=200").await;
        let account_summary = account_listed
            .get(&recommendation_arn(check))
            .unwrap_or_else(|| panic!("case {case}: the account listing reported no summary"));
        agree_on(
            &case,
            PROJECTED_MEMBERS,
            ("single read", &account_full),
            ("summary", account_summary),
        );

        let organization_full = organization_read(&router, &case, check).await;
        let organization_listed =
            summaries_by_arn(&router, &case, Level::Organization, "?maxResults=200").await;
        let organization_summary = organization_listed
            .get(&organization_recommendation_arn(check))
            .unwrap_or_else(|| {
                panic!("case {case}: the organization listing reported no summary")
            });
        agree_on(
            &case,
            &projected_members(Level::Organization),
            ("single read", &organization_full),
            ("summary", organization_summary),
        );

        for (view_name, view) in [
            ("account single read", &account_full),
            ("account summary", account_summary),
            ("organization single read", &organization_full),
            ("organization summary", organization_summary),
        ] {
            assert_eq!(
                view["lifecycleStage"].as_str(),
                Some(stage),
                "case {case}: expected the {view_name} to report the stage it was moved to, got \
                 {:?}",
                view["lifecycleStage"]
            );
        }
        for (view_name, view) in [
            ("account single read", &account_full),
            ("organization single read", &organization_full),
        ] {
            assert_eq!(
                view["updateReasonCode"].as_str(),
                code,
                "case {case}: expected the {view_name} to report the reason code the update \
                 stated, got {:?}",
                view["updateReasonCode"]
            );
            assert_eq!(
                view["updateReason"].is_null(),
                code.is_none(),
                "case {case}: expected the {view_name}'s updateReason to follow the request, got \
                 {:?}",
                view["updateReason"]
            );
        }
    }
}
