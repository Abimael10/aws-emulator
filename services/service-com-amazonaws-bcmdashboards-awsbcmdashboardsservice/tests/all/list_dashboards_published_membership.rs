// SPDX-License-Identifier: CC0-1.0

//! What `ListDashboards` reports for the AWS-managed dashboards, end to end over the wire.
//!
//! The rule under test: an AWS-managed dashboard is the SERVICE's own published resource,
//! so the AWS-managed membership of `ListDashboards` is exactly the published catalog —
//! every published entry present under the ARN the catalog publishes for it, and nothing
//! outside the catalog present — in every form of the request a client may send, and
//! whatever the account has written of its own. Membership is asserted in BOTH directions,
//! because a listing that omits a published entry hides a dashboard the account really
//! holds, and one that adds an entry hands a client an ARN that resolves nowhere while
//! `GetDashboard`, `UpdateDashboard` and a scheduled report's `dashboardArn` all take an
//! ARN discovered here.
//!
//! The request forms below are the model's own: `maxResults` is `@default 20` with
//! `@range 1..100`, `nextToken` is the paginated cursor and `dashboards` is @required on the
//! output (`smithy_query(query="operation",operation="ListDashboards")`). The expected
//! membership is the published catalog table the sibling suite states, read from there
//! rather than retyped.
//!
//! Every call goes through the real router, so what these tests assert is what an SDK
//! client sees.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::dashboard_lifecycle::{graph_config, ok_call, router};
use super::managed_dashboard_catalog::AWS_MANAGED;
use super::published_catalog::{published_arn, PUBLISHED_AT, PUBLISHED_CATALOG};

/// The page size a request that leaves `maxResults` out is served at: `maxResults`'
/// modelled `@default`.
const MODELLED_DEFAULT_MAX_RESULTS: i64 = 20;

/// The forms of the page request this suite walks the listing in, as
/// `(case, maxResults)` — `None` leaves the member out of the request altogether.
///
/// Source: `maxResults`, `@default 20`, `@range 1..100` — the default (left out and named
/// explicitly), each published bound, and sizes between them, so the pages fall differently
/// in every row.
const PAGE_FORMS: [(&str, Option<i64>); 6] = [
    ("maxResults absent", None),
    ("maxResults at the published minimum", Some(1)),
    ("maxResults 2", Some(2)),
    ("maxResults 4", Some(4)),
    ("maxResults at the modelled default", Some(MODELLED_DEFAULT_MAX_RESULTS)),
    ("maxResults at the published maximum", Some(100)),
];

/// What one walk of the whole listing reported, split by `DashboardType`: the AWS-managed
/// references keyed by ARN, and the ARNs of the account's own dashboards.
struct Listing {
    managed: BTreeMap<String, Value>,
    own: BTreeSet<String>,
}

/// Walk every page of `ListDashboards` under one request form and split what it reported.
async fn walk(router: &MultiServiceRouter, case: &str, max_results: Option<i64>) -> Listing {
    let page_limit = max_results.unwrap_or(MODELLED_DEFAULT_MAX_RESULTS);
    let mut managed: BTreeMap<String, Value> = BTreeMap::new();
    let mut own: BTreeSet<String> = BTreeSet::new();
    let mut token: Option<String> = None;
    let mut seen = 0_usize;
    loop {
        let mut request = json!({});
        if let Some(limit) = max_results {
            request["maxResults"] = json!(limit);
        }
        if let Some(token) = &token {
            request["nextToken"] = json!(token);
        }
        let page = ok_call(router, "ListDashboards", &request).await;
        let items = page["dashboards"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case}: dashboards is @required, got {page}"))
            .clone();
        assert!(
            i64::try_from(items.len()).unwrap_or(i64::MAX) <= page_limit,
            "case {case}: a page carries at most {page_limit} references, got {}",
            items.len()
        );
        for item in &items {
            let arn = item["arn"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case}: a reference carries an arn, got {item}"))
                .to_string();
            let kind = item["type"].as_str().unwrap_or("<none>");
            let fresh = if kind == AWS_MANAGED {
                managed.insert(arn.clone(), item.clone()).is_none()
            } else {
                own.insert(arn.clone())
            };
            assert!(
                fresh,
                "case {case}: the walk must report {arn} once, but it repeats"
            );
        }
        seen += items.len();
        token = page
            .get("nextToken")
            .and_then(Value::as_str)
            .map(str::to_string);
        if token.is_none() {
            return Listing { managed, own };
        }
        assert!(
            seen <= 1_000,
            "case {case}: pagination did not terminate after {seen} references"
        );
    }
}

/// Every ARN the catalog publishes.
fn published_arns() -> BTreeSet<String> {
    PUBLISHED_CATALOG
        .into_iter()
        .map(|(resource_id, _, _, _)| published_arn(resource_id))
        .collect()
}

/// Create one dashboard of the account's own and return its ARN.
async fn create_own(router: &MultiServiceRouter, name: &str) -> String {
    let created = ok_call(
        router,
        "CreateDashboard",
        &json!({
            "name": name,
            "widgets": [{
                "title": "Spend",
                "configs": [graph_config("UnblendedCost", "LINE", "MONTHLY")]
            }]
        }),
    )
    .await;
    created["arn"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateDashboard returned no arn: {created}"))
        .to_string()
}

/// The AWS-managed membership of the listing is exactly the published catalog, in every
/// form of the page request — and each entry carries the catalog's own name, description,
/// type and publication instant whichever form reported it.
#[tokio::test]
async fn the_managed_membership_is_the_published_catalog_in_every_request_form() {
    let router = router();
    let published = published_arns();
    let mut reference_form: Option<(&str, BTreeMap<String, Value>)> = None;

    for (case, max_results) in PAGE_FORMS {
        let listing = walk(&router, case, max_results).await;
        let served: BTreeSet<String> = listing.managed.keys().cloned().collect();
        assert_eq!(
            served, published,
            "case {case}: expected exactly the published catalog, got {served:?}"
        );
        assert!(
            listing.own.is_empty(),
            "case {case}: an account that created nothing holds no dashboards of its own, got \
             {:?}",
            listing.own
        );

        for (resource_id, name, description, revised_at) in PUBLISHED_CATALOG {
            let arn = published_arn(resource_id);
            let entry = listing
                .managed
                .get(&arn)
                .unwrap_or_else(|| panic!("case {case}: expected a reference for {arn}, got none"));
            assert_eq!(
                entry["name"].as_str(),
                Some(name),
                "case {case} / {name:?}: expected the published name, got {entry}"
            );
            assert_eq!(
                entry["description"].as_str(),
                Some(description),
                "case {case} / {name:?}: expected the published description, got {entry}"
            );
            assert_eq!(
                entry["type"].as_str(),
                Some(AWS_MANAGED),
                "case {case} / {name:?}: a published entry is AWS-managed, got {entry}"
            );
            assert_eq!(
                entry["createdAt"].as_f64(),
                Some(PUBLISHED_AT),
                "case {case} / {name:?}: expected the publication instant, got {entry}"
            );
            assert_eq!(
                entry["updatedAt"].as_f64(),
                Some(revised_at),
                "case {case} / {name:?}: expected the revision instant {revised_at}, got {entry}"
            );
        }

        // The references themselves do not depend on how the pages fell.
        match &reference_form {
            None => reference_form = Some((case, listing.managed)),
            Some((first_case, first)) => assert_eq!(
                &listing.managed, first,
                "case {case}: expected the same references as case {first_case}"
            ),
        }
    }
}

/// The published catalog is the service's data, not the account's: the account's own
/// creates, updates and deletes leave the AWS-managed membership of the listing exactly as
/// it was, and its own dashboards are listed beside it.
#[tokio::test]
async fn the_accounts_own_writes_leave_the_published_membership_alone() {
    let router = router();
    let baseline = walk(&router, "a fresh account", Some(2)).await;
    assert_eq!(
        baseline.managed.keys().cloned().collect::<BTreeSet<_>>(),
        published_arns(),
        "case a fresh account: expected exactly the published catalog"
    );

    let mut own: Vec<String> = Vec::new();
    for name in ["Alpha Spend", "Bravo Spend", "Charlie Spend"] {
        own.push(create_own(&router, name).await);
        let case = format!("after creating {name:?}");
        let listing = walk(&router, &case, Some(2)).await;
        assert_eq!(
            listing.managed, baseline.managed,
            "case {case}: a create must leave the published catalog untouched"
        );
        assert_eq!(
            listing.own,
            own.iter().cloned().collect::<BTreeSet<_>>(),
            "case {case}: expected the account's own dashboards beside the catalog"
        );
    }

    ok_call(
        &router,
        "UpdateDashboard",
        &json!({"arn": own[1], "name": "Bravo Spend Revised"}),
    )
    .await;
    let listing = walk(&router, "after an update", Some(3)).await;
    assert_eq!(
        listing.managed, baseline.managed,
        "case after an update: an update of the account's own dashboard must leave the published \
         catalog untouched"
    );

    let removed = own.remove(0);
    ok_call(&router, "DeleteDashboard", &json!({"arn": removed})).await;
    let listing = walk(&router, "after a delete", Some(1)).await;
    assert_eq!(
        listing.managed, baseline.managed,
        "case after a delete: a delete of the account's own dashboard must leave the published \
         catalog untouched"
    );
    assert_eq!(
        listing.own,
        own.iter().cloned().collect::<BTreeSet<_>>(),
        "case after a delete: expected the account's remaining dashboards, got {:?}",
        listing.own
    );
}

/// Every ARN the listing reports is usable downstream: each AWS-managed reference names a
/// dashboard `GetDashboard` serves, reading back the same record the reference projected.
#[tokio::test]
async fn every_listed_managed_reference_resolves_to_a_dashboard() {
    let router = router();
    let listing = walk(&router, "the whole listing", None).await;

    for (arn, reference) in &listing.managed {
        let fetched = ok_call(&router, "GetDashboard", &json!({"arn": arn})).await;
        for member in ["arn", "name", "description", "type", "createdAt", "updatedAt"] {
            assert_eq!(
                fetched.get(member),
                reference.get(member),
                "case {arn}: the reference and the detail read are one record, but {member} \
                 differs: {reference} vs {fetched}"
            );
        }
        assert!(
            fetched["widgets"]
                .as_array()
                .is_some_and(|widgets| !widgets.is_empty()),
            "case {arn}: a published dashboard is served with its widgets, got {fetched}"
        );
    }
    assert_eq!(
        listing.managed.keys().cloned().collect::<BTreeSet<_>>(),
        published_arns(),
        "case the whole listing: every published entry must be among the references it reports"
    );
}
