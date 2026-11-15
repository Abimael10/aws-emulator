// SPDX-License-Identifier: CC0-1.0

//! The AWS-managed dashboard catalog is PUBLISHED data, end to end over the wire.
//!
//! The rule under test: an AWS-managed dashboard is the service's own published resource,
//! so the catalog every account holds is the same published set — each entry reachable by
//! the ARN the catalog publishes for it, reading back that entry's published name,
//! description, `AWS_MANAGED` type, publication instants and whole widget list — and no
//! entry outside that set is served.
//!
//! The table below is therefore written out here rather than read back from the service:
//! the entries' resource ids, names and descriptions are the published catalog's, as the
//! wire record of `ListDashboards` and `GetDashboard` on a brand-new account shows them,
//! and so are the nine `WidgetId`s of the `Cost Overview and Trends` entry and the three of
//! its panels whose configuration that record spells out. Everything else asserted here is
//! the model's own: `DashboardReference` requires `arn`, `name`, `type`, `createdAt` and
//! `updatedAt`, `GetDashboard` additionally requires `widgets`, `WidgetId` is 32
//! hexadecimal characters and `Widget$configs` carries 1..2 configurations
//! (`smithy_query(query="shape",shape="DashboardReference,Widget")`).
//!
//! Every call goes through the real router, so what these tests assert is what an SDK
//! client sees.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::dashboard_lifecycle::{call, ok_call, router};
// The scaffolding this suite shares with its siblings rather than copying: the paginated
// listing walk and the `WidgetId` shape check live in the catalog suite, and the one
// statement of what this service's not-found answer looks like in the not-found suite.
use super::managed_dashboard_catalog::{every_reference, is_widget_id, AWS_MANAGED};
use super::resource_not_found::assert_not_found;

/// When the catalog was published, as seconds since the Unix epoch: the `createdAt` every
/// published entry carries, from the recorded listing.
pub(crate) const PUBLISHED_AT: f64 = 1_786_644_000.0;

/// When the catalog was last revised, as seconds since the Unix epoch: the `updatedAt` of
/// every entry revised since publication, from the recorded listing.
pub(crate) const REVISED_AT: f64 = 1_787_079_600.0;

/// The published catalog: one row per entry, as
/// `(resource id, name, description, updatedAt)`.
///
/// Source: the recorded `ListDashboards` of a brand-new account and the recorded
/// `GetDashboard` of each of these ARNs — the catalog's own ids, text and instants, which
/// every account holds alike. The `Reservations` entry has not been revised since the
/// catalog was published, so its `updatedAt` is the publication instant itself.
pub(crate) const PUBLISHED_CATALOG: [(&str, &str, &str, f64); 5] = [
    (
        "4dc5619130f2463687fc45e003e4b930",
        "Cost Overview and Trends",
        "High-level view of your AWS costs, usage trends, and forecasts",
        REVISED_AT,
    ),
    (
        "b0e4f3ab71454cf48a7d148287222f9d",
        "Compute",
        "Compute spend, efficiency, and commitment health across your compute services",
        REVISED_AT,
    ),
    (
        "d2effbcc2dfd44eeb8ebec3f6e83382a",
        "Database",
        "Database spend, efficiency, and commitment health across your database services",
        REVISED_AT,
    ),
    (
        "a8f4e2b1c3d6490ea1b7f5c8d2e94a03",
        "Reservations",
        "Reserved Instance cost, utilization, and coverage health",
        PUBLISHED_AT,
    ),
    (
        "7b3c91d4e5f2a0186c4d8e9f1a2b3c4d",
        "Savings Plans",
        "Savings Plans utilization and coverage health",
        REVISED_AT,
    ),
];

/// The `WidgetId`s of the nine widgets the `Cost Overview and Trends` entry publishes, from
/// that entry's recorded detail read.
const COST_OVERVIEW_WIDGET_IDS: [&str; 9] = [
    "2a7e51a22e1c00af3843c5cccb82b046",
    "35d71261755663fa7f5aa289556472ef",
    "48381c58fdf60999c778deb93fc9284a",
    "a15901be3f2d2c9f07f2c90938ca73d3",
    "a7a5c864d98bfa0bcb1359bd4e3529f3",
    "a80cc87050b1e60b97c65e38be2876ae",
    "b8fe9c2e86f1737da142b62be6e865a8",
    "ce2a10fcd1ab3961dc32b6a3c595166c",
    "d771f500c22cacaa952f1bba40260445",
];

/// The ARN the catalog publishes for the entry under `resource_id`:
/// `DashboardArn`'s modelled form with the catalog's `managed-` resource-id prefix.
pub(crate) fn published_arn(resource_id: &str) -> String {
    engine::emulator_arn(
        "bcm-dashboards",
        "",
        &format!("dashboard/managed-{resource_id}"),
    )
}

/// Every dashboard reference `ListDashboards` reports, keyed by the ARN it carries — the
/// sibling suite's page walk, indexed so a row of the published table can be looked up.
async fn listing_by_arn(router: &MultiServiceRouter) -> BTreeMap<String, Value> {
    let mut references: BTreeMap<String, Value> = BTreeMap::new();
    for item in every_reference(router, 100).await {
        let arn = item["arn"]
            .as_str()
            .unwrap_or_else(|| panic!("a reference carries an arn: {item}"))
            .to_string();
        assert!(
            references.insert(arn.clone(), item).is_none(),
            "case the whole listing: {arn} was reported twice"
        );
    }
    references
}

/// Each published entry is listed under the ARN the catalog publishes for it, with that
/// entry's published name, description, type and publication/revision instants — one row
/// per entry.
#[tokio::test]
async fn every_published_entry_is_listed_under_its_published_arn() {
    let router = router();
    let listed = listing_by_arn(&router).await;

    for (resource_id, name, description, revised_at) in PUBLISHED_CATALOG {
        let arn = published_arn(resource_id);
        let reference = listed
            .get(&arn)
            .unwrap_or_else(|| panic!("case {name:?}: expected a reference for {arn}, got none of {:?}", listed.keys().collect::<Vec<_>>()));
        assert_eq!(
            reference["name"].as_str(),
            Some(name),
            "case {name:?}: expected the published name, got {reference}"
        );
        assert_eq!(
            reference["description"].as_str(),
            Some(description),
            "case {name:?}: expected the published description, got {reference}"
        );
        assert_eq!(
            reference["type"].as_str(),
            Some(AWS_MANAGED),
            "case {name:?}: a published entry is AWS-managed, got {reference}"
        );
        // The instants are the catalog's own, so they are the published ones exactly — not
        // whatever the clock read when this store was built.
        assert_eq!(
            reference["createdAt"].as_f64(),
            Some(PUBLISHED_AT),
            "case {name:?}: expected the publication instant {PUBLISHED_AT}, got {reference}"
        );
        assert_eq!(
            reference["updatedAt"].as_f64(),
            Some(revised_at),
            "case {name:?}: expected the revision instant {revised_at}, got {reference}"
        );
    }
}

/// The published catalog is the WHOLE managed catalog: the account's AWS-managed references
/// are exactly the published ARNs, so no unpublished entry is served either.
#[tokio::test]
async fn the_managed_catalog_holds_exactly_the_published_entries() {
    let router = router();
    let listed = listing_by_arn(&router).await;

    let served: BTreeSet<String> = listed
        .iter()
        .filter(|(_, item)| item["type"].as_str() == Some(AWS_MANAGED))
        .map(|(arn, _)| arn.clone())
        .collect();
    let published: BTreeSet<String> = PUBLISHED_CATALOG
        .into_iter()
        .map(|(resource_id, _, _, _)| published_arn(resource_id))
        .collect();
    assert_eq!(
        served, published,
        "case a fresh account: expected exactly the published catalog, got {served:?}"
    );
}

/// Every published ARN reads back through `GetDashboard`: the published scalars, and a
/// widget list whose ids are 32-hex and distinct — the detail read AWS never fails.
#[tokio::test]
async fn every_published_arn_reads_back_with_its_widgets() {
    let router = router();

    for (resource_id, name, description, revised_at) in PUBLISHED_CATALOG {
        let arn = published_arn(resource_id);
        let (status, fetched) = call(&router, "GetDashboard", &json!({"arn": arn})).await;
        assert_eq!(
            status, 200,
            "case {name:?}: a published ARN is served, got {status}: {fetched}"
        );
        assert_eq!(
            fetched["arn"].as_str(),
            Some(arn.as_str()),
            "case {name:?}: expected the requested arn back, got {fetched}"
        );
        assert_eq!(
            fetched["name"].as_str(),
            Some(name),
            "case {name:?}: expected the published name, got {fetched}"
        );
        assert_eq!(
            fetched["description"].as_str(),
            Some(description),
            "case {name:?}: expected the published description, got {fetched}"
        );
        assert_eq!(
            fetched["type"].as_str(),
            Some(AWS_MANAGED),
            "case {name:?}: a published entry is AWS-managed, got {fetched}"
        );
        assert_eq!(
            fetched["createdAt"].as_f64(),
            Some(PUBLISHED_AT),
            "case {name:?}: expected the publication instant {PUBLISHED_AT}, got {fetched}"
        );
        assert_eq!(
            fetched["updatedAt"].as_f64(),
            Some(revised_at),
            "case {name:?}: expected the revision instant {revised_at}, got {fetched}"
        );

        let widgets = fetched["widgets"]
            .as_array()
            .unwrap_or_else(|| panic!("case {name:?}: widgets is @required: {fetched}"));
        assert!(
            !widgets.is_empty() && widgets.len() <= 20,
            "case {name:?}: expected 1..20 published widgets, got {}",
            widgets.len()
        );
        // The ids of one dashboard's widgets are DISTINCT, including the ones this
        // emulator derives for a panel whose published id is not recorded.
        let mut ids: BTreeSet<&str> = BTreeSet::new();
        for widget in widgets {
            let id = widget["id"].as_str().unwrap_or("<none>");
            assert!(
                is_widget_id(id),
                "case {name:?}: every widget carries a 32-hex WidgetId, got {id:?}"
            );
            assert!(
                ids.insert(id),
                "case {name:?}: a widget id is unique in one dashboard, but {id} repeats"
            );
        }
    }
}

/// The `Cost Overview and Trends` entry serves its nine published widgets — every one of
/// the published ids, and nothing else.
#[tokio::test]
async fn the_overview_entry_serves_its_nine_published_widgets() {
    let router = router();
    let (resource_id, name, _, _) = PUBLISHED_CATALOG[0];
    let arn = published_arn(resource_id);

    let fetched = ok_call(&router, "GetDashboard", &json!({"arn": arn})).await;
    let served: BTreeSet<&str> = fetched["widgets"]
        .as_array()
        .unwrap_or_else(|| panic!("case {name:?}: widgets is @required: {fetched}"))
        .iter()
        .map(|widget| widget["id"].as_str().unwrap_or("<none>"))
        .collect();
    let published: BTreeSet<&str> = COST_OVERVIEW_WIDGET_IDS.into_iter().collect();
    assert_eq!(
        served, published,
        "case {name:?}: expected the published widget ids, got {served:?}"
    );
}

/// The three published panels of the overview entry whose configuration the record spells
/// out, each read back by its published id: the query a panel runs and the way it is drawn
/// are the published ones.
#[tokio::test]
async fn the_recorded_overview_panels_carry_their_published_configuration() {
    let router = router();
    let (resource_id, name, _, _) = PUBLISHED_CATALOG[0];
    let arn = published_arn(resource_id);
    let fetched = ok_call(&router, "GetDashboard", &json!({"arn": arn})).await;
    let widgets = fetched["widgets"]
        .as_array()
        .unwrap_or_else(|| panic!("case {name:?}: widgets is @required: {fetched}"))
        .clone();
    let widget = |id: &str| -> Value {
        widgets
            .iter()
            .find(|widget| widget["id"].as_str() == Some(id))
            .unwrap_or_else(|| panic!("case widget {id}: expected a published widget, got none"))
            .clone()
    };

    // 'Committed vs Flexible Spend': an AmortizedCost STACK graph over a MONTHLY
    // costAndUsage query grouped by the PURCHASE_TYPE dimension, over a RELATIVE
    // -P12M..P0M range.
    let committed = widget("48381c58fdf60999c778deb93fc9284a");
    assert_eq!(
        committed["title"].as_str(),
        Some("Committed vs Flexible Spend"),
        "case the committed-spend panel: expected the published title, got {committed}"
    );
    let config = &committed["configs"][0];
    let query = &config["queryParameters"]["costAndUsage"];
    assert_eq!(
        query["metrics"],
        json!(["AmortizedCost"]),
        "case the committed-spend panel: expected the AmortizedCost metric, got {config}"
    );
    assert_eq!(
        query["granularity"].as_str(),
        Some("MONTHLY"),
        "case the committed-spend panel: expected a MONTHLY query, got {config}"
    );
    assert_eq!(
        query["groupBy"],
        json!([{"key": "PURCHASE_TYPE", "type": "DIMENSION"}]),
        "case the committed-spend panel: expected a PURCHASE_TYPE grouping, got {config}"
    );
    assert_eq!(
        query["timeRange"],
        json!({
            "startTime": {"type": "RELATIVE", "value": "-P12M"},
            "endTime": {"type": "RELATIVE", "value": "P0M"}
        }),
        "case the committed-spend panel: expected the -P12M..P0M relative range, got {config}"
    );
    assert_eq!(
        config["displayConfig"]["graph"]["AmortizedCost"]["visualType"].as_str(),
        Some("STACK"),
        "case the committed-spend panel: expected an AmortizedCost STACK graph, got {config}"
    );

    // The service-level panel: an UnblendedCost STACK graph grouped by SERVICE.
    let by_service = widget("2a7e51a22e1c00af3843c5cccb82b046");
    let config = &by_service["configs"][0];
    assert_eq!(
        config["queryParameters"]["costAndUsage"]["metrics"],
        json!(["UnblendedCost"]),
        "case the by-service panel: expected the UnblendedCost metric, got {config}"
    );
    assert_eq!(
        config["queryParameters"]["costAndUsage"]["groupBy"],
        json!([{"key": "SERVICE", "type": "DIMENSION"}]),
        "case the by-service panel: expected a SERVICE grouping, got {config}"
    );
    assert_eq!(
        config["displayConfig"]["graph"]["UnblendedCost"]["visualType"].as_str(),
        Some("STACK"),
        "case the by-service panel: expected an UnblendedCost STACK graph, got {config}"
    );

    // 'Budget Health': drawn as a table rather than a graph.
    let budget = widget("d771f500c22cacaa952f1bba40260445");
    assert_eq!(
        budget["title"].as_str(),
        Some("Budget Health"),
        "case the budget panel: expected the published title, got {budget}"
    );
    assert!(
        budget["configs"][0]["displayConfig"]["table"].is_object(),
        "case the budget panel: expected a table display, got {budget}"
    );
}

/// The catalog is published data, not an account's: two independent stores answer the same
/// catalog. (That a read leaves the catalog unchanged is the sibling suite's
/// `reading_the_catalog_changes_nothing`.)
#[tokio::test]
async fn the_catalog_is_identical_in_every_store() {
    let first = router();
    let second = router();

    let one = listing_by_arn(&first).await;
    let other = listing_by_arn(&second).await;
    assert_eq!(
        one, other,
        "case two fresh stores: every account holds the same published catalog"
    );

    for (resource_id, name, _, _) in PUBLISHED_CATALOG {
        let arn = published_arn(resource_id);
        let from_first = ok_call(&first, "GetDashboard", &json!({"arn": arn})).await;
        let from_second = ok_call(&second, "GetDashboard", &json!({"arn": arn})).await;
        assert_eq!(
            from_first, from_second,
            "case {name:?}: the published entry must read alike in every store"
        );
    }
}

/// An ARN of the managed form the catalog does NOT publish resolves to nothing: the read is
/// refused as not found, so the catalog's membership is what the store holds and not the
/// ARN's spelling.
#[tokio::test]
async fn a_managed_arn_outside_the_catalog_is_still_not_found() {
    let router = router();

    // Well-formed `managed-` ARNs that name no published entry: one id the emulator's
    // earlier hand-written seed used, one all-zero id, and one that differs from a
    // published id in its last character alone.
    let published = PUBLISHED_CATALOG[0].0;
    let (head, last) = published.split_at(published.len() - 1);
    let near_miss = format!("{head}{}", if last == "f" { "e" } else { "f" });
    for resource_id in [
        "6b1f0c5d2a8e4f3b9c7d1e0a5f8b2c4d",
        "00000000000000000000000000000000",
        near_miss.as_str(),
    ] {
        let arn = published_arn(resource_id);
        let (status, body) = call(&router, "GetDashboard", &json!({"arn": arn})).await;
        assert_not_found(&format!("unpublished {resource_id}"), &arn, status, &body);
    }
}
