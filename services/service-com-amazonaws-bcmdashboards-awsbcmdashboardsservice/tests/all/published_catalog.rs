// SPDX-License-Identifier: CC0-1.0

//! The AWS-managed dashboard catalog is PUBLISHED data, end to end over the wire.
//!
//! The rule under test: an AWS-managed dashboard is the service's own published resource,
//! so the catalog every account holds is the same published set — each entry reachable by
//! the ARN the catalog publishes for it, reading back that entry's published name,
//! description, `AWS_MANAGED` type, publication instants and whole widget list — and no
//! entry outside that set is served.
//!
//! The tables below are therefore written out here rather than read back from the service:
//! the entries' resource ids, names and descriptions are the published catalog's, as the
//! wire record of `ListDashboards` and `GetDashboard` on a brand-new account shows them, and
//! so are every entry's whole `WidgetId` list ([`PUBLISHED_WIDGET_IDS`]) and the panels whose
//! configuration that record spells out ([`RECORDED_PANELS`], each row carrying only what the
//! record states for that panel). Everything else asserted here is the model's own:
//! `DashboardReference` requires `arn`, `name`, `type`, `createdAt` and `updatedAt`,
//! `GetDashboard` additionally requires `widgets`, `WidgetId` is 32 hexadecimal characters and
//! `Widget$configs` carries 1..2 configurations
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

/// The whole `WidgetId` list each published entry serves, keyed by the entry's resource id,
/// from that entry's recorded detail read.
///
/// A panel's id identifies the PANEL and not its place on a dashboard, which is why an id
/// appears in two rows wherever the catalog publishes the same reading on both entries.
pub(crate) const PUBLISHED_WIDGET_IDS: [(&str, &[&str]); 5] = [
    (
        "4dc5619130f2463687fc45e003e4b930",
        &[
            "2a7e51a22e1c00af3843c5cccb82b046",
            "35d71261755663fa7f5aa289556472ef",
            "48381c58fdf60999c778deb93fc9284a",
            "a15901be3f2d2c9f07f2c90938ca73d3",
            "a18b58807f3934dbcfebd21ad359afaa",
            "a7a5c864d98bfa0bcb1359bd4e3529f3",
            "a80cc87050b1e60b97c65e38be2876ae",
            "b8fe9c2e86f1737da142b62be6e865a8",
            "ce2a10fcd1ab3961dc32b6a3c595166c",
            "d771f500c22cacaa952f1bba40260445",
        ],
    ),
    (
        "b0e4f3ab71454cf48a7d148287222f9d",
        &[
            "0241f9abbdbd20e3d93b196f40678f0f",
            "064adb1bb9690bf0fa1c85e9876c9ae4",
            "083532331e8f5e5e1487aa7ae50c74f7",
            "0874d882ac0b7b8d1343d6f54245ce8f",
            "16d66087316cf44454a2efdc86c0e2cf",
            "176354389adbb656d157b533fd239069",
            "31d834ccdeedda0007ee07a0c2f5e811",
            "434a31f03b942d9ce04a675c01c0a6f3",
            "548b95216f045ec2f5da23067f8e3768",
            "5b409f68aa9093525bfe5195b04cb975",
            "5cb4febf03139a4babd954e306ae690f",
            "b896d55ac55b132f02d9308b64cf7eaa",
            "c5465ef3f521aa1c1be28b07625cff5d",
            "da7ad4ebb647e09f893b338b1cf42ad4",
            "efa71a19fa4273a8572a2a583ab62af8",
            "f8b45d4e6069b5b3ba5ff44dfb5394b0",
        ],
    ),
    (
        "d2effbcc2dfd44eeb8ebec3f6e83382a",
        &[
            "01216d7f7d6e870db27b792d5d0345a8",
            "0d5ddc308ef25720bd8562bf4bba82b6",
            "423d943a56564575c875bdd732402fba",
            "4727ff333834e765dc2f7fa729258736",
            "4e40df2d0f9cb3ac7abb5d3688003be8",
            "4f605948b37e4a9280d3b650c7803828",
            "51f0badeb0ddc3d0e5a76fdc7baa53b8",
            "559f2986324410639d4c80f5beb3cd0b",
            "721eb011eeaf1e687e19d7929defc99b",
            "9cfcef554e56b7042da915140598b563",
            "ad179972fea2e5f44a33275b363fee6f",
            "c38bf72d2d4bc27a135ed376bd1fa3a4",
            "fb17570c6dda591b0e782b79fbb56440",
            "fdef4552aa22ac47727c7ed8e5de4f84",
        ],
    ),
    (
        "a8f4e2b1c3d6490ea1b7f5c8d2e94a03",
        &[
            "083532331e8f5e5e1487aa7ae50c74f7",
            "51f0badeb0ddc3d0e5a76fdc7baa53b8",
            "539c389259acdcf07bec62ac4161446e",
            "77e82a3edc9e977a00c56ba2e80842af",
            "7ada8c46db87963f7a2d35137b1726dc",
            "910ca6ed6bbb0c4f0a0be48dc2ca40a4",
            "a60b3f8f5f8e072d81a4fb3499a1a5f9",
            "ad7743135629887f412539aba11687a7",
            "c1cf6a66575e56af84757e176f34bf0e",
            "c5465ef3f521aa1c1be28b07625cff5d",
            "c9f6e32918eb4dac492753fcaead75d4",
            "cea32ea06a810136bfe98f419c077616",
            "dbd67a9ddce0bf151bc13d92f6d64170",
            "e405c05549af8c0591a81d7d63f10c24",
            "e5c0db177b28e60f14ff86415df385fb",
            "fb17570c6dda591b0e782b79fbb56440",
        ],
    ),
    (
        "7b3c91d4e5f2a0186c4d8e9f1a2b3c4d",
        &[
            "0e672c5eb0cd76722126002798c54ba7",
            "340ae1c358c30fe9d9971a29113bff00",
            "347680899500e906f573587c8bc9a9c6",
            "394418214c2967dbb8adc2e0974ea42a",
            "3c990530b4b45ba456c93ccf6c0258aa",
            "40d727374c54df419fc4b162a03cfbe7",
            "434a31f03b942d9ce04a675c01c0a6f3",
            "4c642d0693fa7117525e53250a43b207",
            "4e40df2d0f9cb3ac7abb5d3688003be8",
            "8256bb6dab24548a7dca7a3f01d48104",
            "bfcb4256607d135afd495710ebbafdd3",
            "cd071457df17c5708f88c46d16a30bca",
            "e856e20f051f124e39b20bd7982f5c83",
            "fb6d2f8cf6a44118f898249f8b55c962",
        ],
    ),
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

/// The widgets the entry under `resource_id` serves, read back through `GetDashboard`.
async fn widgets_of(router: &MultiServiceRouter, resource_id: &str) -> Vec<Value> {
    let arn = published_arn(resource_id);
    let fetched = ok_call(router, "GetDashboard", &json!({"arn": arn})).await;
    fetched["widgets"]
        .as_array()
        .unwrap_or_else(|| panic!("case entry {resource_id}: widgets is @required: {fetched}"))
        .clone()
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

/// EVERY published entry serves exactly the widget ids the catalog publishes for it — one
/// row per entry, from the ten-panel overview to the sixteen-panel compute and reservation
/// entries — so no published panel is missing and no panel AWS never published is served.
#[tokio::test]
async fn every_entry_serves_exactly_its_published_widget_ids() {
    let router = router();

    for (resource_id, published_ids) in PUBLISHED_WIDGET_IDS {
        let widgets = widgets_of(&router, resource_id).await;
        let served: BTreeSet<&str> = widgets
            .iter()
            .map(|widget| widget["id"].as_str().unwrap_or("<none>"))
            .collect();
        let published: BTreeSet<&str> = published_ids.iter().copied().collect();
        assert_eq!(
            served, published,
            "case entry {resource_id}: expected the published widget ids {published:?}, got \
             {served:?}"
        );
        assert_eq!(
            widgets.len(),
            published_ids.len(),
            "case entry {resource_id}: expected {} published panels, got {}",
            published_ids.len(),
            widgets.len()
        );
    }
}

/// A panel the catalog publishes on two entries is ONE panel: both reads answer the same
/// widget, member for member, so a client that followed the id from either dashboard sees
/// the same panel.
#[tokio::test]
async fn a_panel_published_on_two_entries_reads_back_identically() {
    let router = router();

    // Every panel of every entry, indexed by id, so the ids published more than once can be
    // compared across the entries publishing them.
    let mut by_id: BTreeMap<&str, Vec<(&str, Value)>> = BTreeMap::new();
    for (resource_id, published_ids) in PUBLISHED_WIDGET_IDS {
        let widgets = widgets_of(&router, resource_id).await;
        for id in published_ids {
            let panel = widgets
                .iter()
                .find(|widget| widget["id"].as_str() == Some(*id))
                .unwrap_or_else(|| {
                    panic!("case entry {resource_id}: expected the published panel {id}")
                })
                .clone();
            by_id.entry(id).or_default().push((resource_id, panel));
        }
    }

    let shared: Vec<&&str> = by_id
        .iter()
        .filter(|(_, places)| places.len() > 1)
        .map(|(id, _)| id)
        .collect();
    assert!(
        shared.len() >= 6,
        "the published catalog shares panels between entries, so the check means something: \
         got {shared:?}"
    );
    for id in shared {
        let places = &by_id[*id];
        let (first_entry, first) = &places[0];
        for (entry, panel) in &places[1..] {
            assert_eq!(
                panel, first,
                "case panel {id}: entry {entry} and entry {first_entry} publish the same id, \
                 so the panel must read alike: {panel} vs {first}"
            );
        }
    }
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

/// The `SERVICE` dimension values the recorded compute panels narrow the bill to, as those
/// panels' own filter lists them.
const COMPUTE_SERVICES: [&str; 12] = [
    "Amazon EC2 Container Registry (ECR)",
    "Amazon Elastic Container Service for Kubernetes",
    "EC2 - Other",
    "Amazon Elastic Compute Cloud - Compute",
    "Amazon Elastic Container Service",
    "AWS Lambda",
    "AWS App Runner",
    "Amazon Lightsail",
    "Amazon Elastic Container Registry Public",
    "AWS Parallel Computing Service",
    "Amazon EKS Anywhere",
    "AWS Outposts",
];

/// The `SERVICE` dimension values the recorded database panels narrow the bill to.
const DATABASE_SERVICES: [&str; 13] = [
    "Amazon Relational Database Service",
    "Amazon DynamoDB",
    "Amazon ElastiCache",
    "Amazon OpenSearch Service",
    "Amazon Redshift",
    "Amazon MemoryDB",
    "Amazon DocumentDB (with MongoDB compatibility)",
    "Amazon Neptune",
    "Amazon Keyspaces (for Apache Cassandra)",
    "Amazon Timestream",
    "AWS Database Migration Service",
    "Aurora DSQL",
    "Oracle Database@AWS",
];

/// The `SERVICE` dimension values the recorded EC2 cost panel narrows to, and the single
/// value the recorded EC2 reservation-coverage panel narrows to.
const EC2_SERVICES: [&str; 3] = [
    "Amazon EC2 Container Registry (ECR)",
    "EC2 - Other",
    "Amazon Elastic Compute Cloud - Compute",
];
const EC2_COMPUTE_SERVICE: [&str; 1] = ["Amazon Elastic Compute Cloud - Compute"];

/// The `PURCHASE_TYPE` dimension value the recorded on-demand panel narrows to.
const ON_DEMAND_PURCHASE_TYPE: [&str; 1] = ["On Demand Instances"];

/// The `height` and `horizontalOffset` every recorded panel carries.
const RECORDED_HEIGHT: i64 = 4;
const RECORDED_HORIZONTAL_OFFSET: i64 = 0;

/// Every member of the `QueryParameters` union, so a panel can be held to carrying exactly
/// the one the record shows (`smithy_query(query="shape",shape="QueryParameters")`).
const QUERY_MEMBERS: [&str; 7] = [
    "costAndUsage",
    "savingsPlansCoverage",
    "savingsPlansUtilization",
    "reservationCoverage",
    "reservationUtilization",
    "budget",
    "costEfficiency",
];

/// One panel whose configuration the wire record spells out, carrying ONLY what the record
/// states for it: a field left `None` is one the record does not disclose for that panel and
/// is not asserted here, and `absent` names the query members the record shows it without.
struct RecordedPanel {
    label: &'static str,
    /// The resource id of the entry publishing it.
    entry: &'static str,
    id: &'static str,
    title: Option<&'static str>,
    description: Option<&'static str>,
    width: Option<i64>,
    /// The `QueryParameters` union member the panel carries.
    query_member: &'static str,
    metrics: Option<&'static [&'static str]>,
    granularity: Option<&'static str>,
    /// The `Dimension`s the query groups by, empty for an ungrouped query.
    group_by: Option<&'static [&'static str]>,
    /// The relative bounds of the query's time range.
    range: Option<(&'static str, &'static str)>,
    /// The one `Dimension` the query is narrowed to, and its values.
    filter: Option<(&'static str, &'static [&'static str])>,
    absent: &'static [&'static str],
    /// `Some((series, visual type))` for a graph panel, `None` for a table panel; the visual
    /// type is `None` where the record discloses the series alone.
    graph: Option<(&'static str, Option<&'static str>)>,
}

/// The panels of the published catalog whose configuration the wire record spells out, across
/// every entry that has one: the forecast, budget, cost-efficiency, purchase-type, by-service,
/// table and commitment-spend panels of the overview entry, the cost-per-service pair of the
/// compute and database entries, the EC2 cost and reservation-coverage panels of the
/// reservation entry (the coverage panel read on both entries that publish it), and the
/// total-cost and on-demand panels of the Savings Plans entry.
const RECORDED_PANELS: [RecordedPanel; 18] = [
    RecordedPanel {
        label: "the overview forecast panel",
        entry: "4dc5619130f2463687fc45e003e4b930",
        id: "a18b58807f3934dbcfebd21ad359afaa",
        title: Some("Total Current And Forecasted Cost"),
        description: Some("Historical monthly unblended cost with forecasted cost."),
        width: Some(2),
        query_member: "costAndUsage",
        metrics: Some(&["UnblendedCost"]),
        granularity: Some("MONTHLY"),
        group_by: Some(&[]),
        // The only recorded range that runs PAST the current month, into the forecast.
        range: Some(("-P12M", "P4M")),
        filter: None,
        absent: &["filter"],
        graph: Some(("UnblendedCost", Some("BAR"))),
    },
    RecordedPanel {
        label: "the overview purchase-type panel",
        entry: "4dc5619130f2463687fc45e003e4b930",
        id: "48381c58fdf60999c778deb93fc9284a",
        title: Some("Committed vs Flexible Spend"),
        description: Some("Total monthly amortized cost split by purchase type."),
        width: None,
        query_member: "costAndUsage",
        metrics: Some(&["AmortizedCost"]),
        granularity: Some("MONTHLY"),
        group_by: Some(&["PURCHASE_TYPE"]),
        range: Some(("-P12M", "P0M")),
        filter: None,
        absent: &["filter"],
        graph: Some(("AmortizedCost", Some("STACK"))),
    },
    RecordedPanel {
        label: "the overview by-service panel",
        entry: "4dc5619130f2463687fc45e003e4b930",
        id: "2a7e51a22e1c00af3843c5cccb82b046",
        title: None,
        description: Some("Top services driving your total monthly unblended cost."),
        width: None,
        query_member: "costAndUsage",
        metrics: Some(&["UnblendedCost"]),
        granularity: Some("MONTHLY"),
        group_by: Some(&["SERVICE"]),
        range: Some(("-P12M", "P0M")),
        filter: None,
        absent: &["filter"],
        graph: Some(("UnblendedCost", Some("STACK"))),
    },
    RecordedPanel {
        // The one panel of the catalog whose query is the budgets themselves rather than a
        // cost-and-usage query, and the record shows it with no member but `filter`.
        label: "the overview budget panel",
        entry: "4dc5619130f2463687fc45e003e4b930",
        id: "d771f500c22cacaa952f1bba40260445",
        title: Some("Budget Health"),
        description: Some(
            "Visualizes your custom AWS Budgets, showing actual spend against budget limits \
             and forecasted spend.",
        ),
        width: Some(4),
        query_member: "budget",
        metrics: None,
        granularity: None,
        group_by: None,
        range: None,
        filter: None,
        absent: &["filter", "timeRange", "granularity", "metrics", "groupBy"],
        graph: None,
    },
    RecordedPanel {
        label: "the overview cost-efficiency panel",
        entry: "4dc5619130f2463687fc45e003e4b930",
        id: "a15901be3f2d2c9f07f2c90938ca73d3",
        title: None,
        description: None,
        width: None,
        query_member: "costEfficiency",
        metrics: None,
        granularity: Some("MONTHLY"),
        group_by: Some(&[]),
        range: Some(("-P3M", "P0M")),
        filter: None,
        absent: &["metrics", "filter"],
        graph: Some(("EfficiencyScore", Some("LINE"))),
    },
    RecordedPanel {
        label: "the overview usage-type table",
        entry: "4dc5619130f2463687fc45e003e4b930",
        id: "a7a5c864d98bfa0bcb1359bd4e3529f3",
        title: None,
        description: None,
        width: Some(2),
        query_member: "costAndUsage",
        metrics: Some(&["UnblendedCost"]),
        granularity: Some("MONTHLY"),
        // The grouping is the one member of this panel the record does not disclose.
        group_by: None,
        range: Some(("-P12M", "P0M")),
        filter: None,
        absent: &["filter"],
        graph: None,
    },
    RecordedPanel {
        label: "the overview account table",
        entry: "4dc5619130f2463687fc45e003e4b930",
        id: "a80cc87050b1e60b97c65e38be2876ae",
        title: None,
        description: None,
        // The second panel the overview publishes across twice the narrow span.
        width: Some(4),
        query_member: "costAndUsage",
        metrics: Some(&["UnblendedCost"]),
        granularity: Some("MONTHLY"),
        group_by: Some(&["LINKED_ACCOUNT"]),
        range: Some(("-P12M", "P0M")),
        filter: None,
        absent: &["filter"],
        graph: None,
    },
    RecordedPanel {
        // The record shows these two drawn as a graph over a cost-and-usage query; which
        // series they graph is this emulator's reading of the commitment they report.
        label: "the overview Savings Plans spend panel",
        entry: "4dc5619130f2463687fc45e003e4b930",
        id: "b8fe9c2e86f1737da142b62be6e865a8",
        title: None,
        description: None,
        width: Some(2),
        query_member: "costAndUsage",
        metrics: None,
        granularity: Some("MONTHLY"),
        group_by: None,
        range: None,
        filter: None,
        absent: &[],
        graph: Some(("AmortizedCost", None)),
    },
    RecordedPanel {
        label: "the overview reservation spend panel",
        entry: "4dc5619130f2463687fc45e003e4b930",
        id: "ce2a10fcd1ab3961dc32b6a3c595166c",
        title: None,
        description: None,
        width: Some(2),
        query_member: "costAndUsage",
        metrics: None,
        granularity: Some("MONTHLY"),
        group_by: None,
        range: None,
        filter: None,
        absent: &[],
        graph: Some(("AmortizedCost", None)),
    },
    RecordedPanel {
        label: "the compute cost-per-service panel",
        entry: "b0e4f3ab71454cf48a7d148287222f9d",
        id: "5b409f68aa9093525bfe5195b04cb975",
        title: Some("Cost Per Compute Service"),
        description: Some("Monthly unblended cost across your top spending compute services."),
        width: Some(2),
        query_member: "costAndUsage",
        metrics: Some(&["UnblendedCost"]),
        granularity: Some("MONTHLY"),
        group_by: Some(&["SERVICE"]),
        range: Some(("-P12M", "P0M")),
        filter: Some(("SERVICE", &COMPUTE_SERVICES)),
        absent: &[],
        graph: Some(("UnblendedCost", Some("STACK"))),
    },
    RecordedPanel {
        label: "the compute amortized cost-per-service panel",
        entry: "b0e4f3ab71454cf48a7d148287222f9d",
        id: "548b95216f045ec2f5da23067f8e3768",
        title: None,
        description: None,
        width: None,
        query_member: "costAndUsage",
        metrics: Some(&["AmortizedCost"]),
        granularity: Some("MONTHLY"),
        group_by: Some(&["SERVICE"]),
        range: Some(("-P12M", "P0M")),
        filter: Some(("SERVICE", &COMPUTE_SERVICES)),
        absent: &[],
        graph: Some(("AmortizedCost", Some("STACK"))),
    },
    RecordedPanel {
        label: "the EC2 reservation-coverage panel on the compute entry",
        entry: "b0e4f3ab71454cf48a7d148287222f9d",
        id: "c5465ef3f521aa1c1be28b07625cff5d",
        title: None,
        description: None,
        width: None,
        query_member: "reservationCoverage",
        metrics: Some(&["Cost", "Hour"]),
        granularity: Some("MONTHLY"),
        group_by: Some(&[]),
        range: Some(("-P12M", "P0M")),
        filter: Some(("SERVICE", &EC2_COMPUTE_SERVICE)),
        absent: &[],
        graph: None,
    },
    RecordedPanel {
        label: "the EC2 reservation-coverage panel on the reservation entry",
        entry: "a8f4e2b1c3d6490ea1b7f5c8d2e94a03",
        id: "c5465ef3f521aa1c1be28b07625cff5d",
        title: None,
        description: None,
        width: None,
        query_member: "reservationCoverage",
        metrics: Some(&["Cost", "Hour"]),
        granularity: Some("MONTHLY"),
        group_by: Some(&[]),
        range: Some(("-P12M", "P0M")),
        filter: Some(("SERVICE", &EC2_COMPUTE_SERVICE)),
        absent: &[],
        graph: None,
    },
    RecordedPanel {
        label: "the database cost-per-service panel",
        entry: "d2effbcc2dfd44eeb8ebec3f6e83382a",
        id: "9cfcef554e56b7042da915140598b563",
        title: Some("Database Cost Per Service"),
        description: Some("Monthly unblended cost across top spending database services."),
        width: Some(2),
        query_member: "costAndUsage",
        metrics: Some(&["UnblendedCost"]),
        granularity: Some("MONTHLY"),
        group_by: Some(&["SERVICE"]),
        range: Some(("-P12M", "P0M")),
        filter: Some(("SERVICE", &DATABASE_SERVICES)),
        absent: &[],
        graph: Some(("UnblendedCost", Some("STACK"))),
    },
    RecordedPanel {
        label: "the database amortized cost-per-service panel",
        entry: "d2effbcc2dfd44eeb8ebec3f6e83382a",
        id: "01216d7f7d6e870db27b792d5d0345a8",
        title: None,
        description: None,
        width: None,
        query_member: "costAndUsage",
        metrics: Some(&["AmortizedCost"]),
        granularity: Some("MONTHLY"),
        group_by: Some(&["SERVICE"]),
        range: Some(("-P12M", "P0M")),
        filter: Some(("SERVICE", &DATABASE_SERVICES)),
        absent: &[],
        graph: Some(("AmortizedCost", Some("STACK"))),
    },
    RecordedPanel {
        label: "the EC2 cost-by-instance-type panel",
        entry: "a8f4e2b1c3d6490ea1b7f5c8d2e94a03",
        id: "77e82a3edc9e977a00c56ba2e80842af",
        title: Some("EC2 Cost By Instance Type"),
        description: Some("Monthly unblended EC2 cost by instance type."),
        // The recorded panel published across twice the narrow span.
        width: Some(4),
        query_member: "costAndUsage",
        metrics: Some(&["UnblendedCost"]),
        granularity: Some("MONTHLY"),
        group_by: Some(&["INSTANCE_TYPE"]),
        range: Some(("-P12M", "P0M")),
        filter: Some(("SERVICE", &EC2_SERVICES)),
        absent: &[],
        graph: Some(("UnblendedCost", Some("BAR"))),
    },
    RecordedPanel {
        label: "the Savings Plans total-cost panel",
        entry: "7b3c91d4e5f2a0186c4d8e9f1a2b3c4d",
        id: "340ae1c358c30fe9d9971a29113bff00",
        title: Some("Total Cost"),
        description: Some("Total monthly unblended cost across all services."),
        width: Some(2),
        query_member: "costAndUsage",
        metrics: Some(&["UnblendedCost"]),
        granularity: Some("MONTHLY"),
        group_by: Some(&[]),
        range: Some(("-P12M", "P0M")),
        filter: None,
        absent: &["filter"],
        graph: Some(("UnblendedCost", Some("LINE"))),
    },
    RecordedPanel {
        label: "the Savings Plans on-demand panel",
        entry: "7b3c91d4e5f2a0186c4d8e9f1a2b3c4d",
        id: "40d727374c54df419fc4b162a03cfbe7",
        title: Some("On Demand Spend By Service"),
        description: Some("Monthly unblended on-demand spend by service."),
        width: Some(2),
        query_member: "costAndUsage",
        metrics: Some(&["UnblendedCost"]),
        granularity: Some("MONTHLY"),
        group_by: Some(&["SERVICE"]),
        range: Some(("-P12M", "P0M")),
        filter: Some(("PURCHASE_TYPE", &ON_DEMAND_PURCHASE_TYPE)),
        absent: &[],
        graph: Some(("UnblendedCost", Some("STACK"))),
    },
];

/// Every panel whose configuration the record spells out carries exactly that configuration:
/// its published prose and layout, the `QueryParameters` member it runs (and NO other member
/// of the union), that query's metrics, granularity, grouping, range and filter, and whether
/// it is drawn as a graph of one series or as a table.
#[tokio::test]
async fn every_recorded_panel_carries_its_published_configuration() {
    let router = router();

    for case in RECORDED_PANELS {
        let label = case.label;
        let widgets = widgets_of(&router, case.entry).await;
        let panel = widgets
            .iter()
            .find(|widget| widget["id"].as_str() == Some(case.id))
            .unwrap_or_else(|| {
                panic!("case {label}: expected the published panel {}, got none", case.id)
            })
            .clone();

        if let Some(title) = case.title {
            assert_eq!(
                panel["title"].as_str(),
                Some(title),
                "case {label}: expected the published title, got {panel}"
            );
        }
        if let Some(description) = case.description {
            assert_eq!(
                panel["description"].as_str(),
                Some(description),
                "case {label}: expected the published description, got {panel}"
            );
        }
        if let Some(width) = case.width {
            assert_eq!(
                panel["width"].as_i64(),
                Some(width),
                "case {label}: expected the published width {width}, got {panel}"
            );
        }
        assert_eq!(
            panel["height"].as_i64(),
            Some(RECORDED_HEIGHT),
            "case {label}: expected the published height {RECORDED_HEIGHT}, got {panel}"
        );
        assert_eq!(
            panel["horizontalOffset"].as_i64(),
            Some(RECORDED_HORIZONTAL_OFFSET),
            "case {label}: expected the published offset {RECORDED_HORIZONTAL_OFFSET}, got \
             {panel}"
        );

        let configs = panel["configs"]
            .as_array()
            .unwrap_or_else(|| panic!("case {label}: configs is @required: {panel}"));
        assert_eq!(
            configs.len(),
            1,
            "case {label}: expected the one published configuration, got {panel}"
        );
        let config = &configs[0];

        // The union carries exactly the member the record shows.
        let parameters = &config["queryParameters"];
        assert!(
            parameters[case.query_member].is_object(),
            "case {label}: expected a {} query, got {config}",
            case.query_member
        );
        for member in QUERY_MEMBERS {
            if member == case.query_member {
                continue;
            }
            assert!(
                parameters[member].is_null(),
                "case {label}: a queryParameters union carries one member, but {member} is \
                 set too: {config}"
            );
        }
        let query = &parameters[case.query_member];

        if let Some(metrics) = case.metrics {
            assert_eq!(
                query["metrics"],
                json!(metrics),
                "case {label}: expected the published metrics {metrics:?}, got {config}"
            );
        }
        if let Some(granularity) = case.granularity {
            assert_eq!(
                query["granularity"].as_str(),
                Some(granularity),
                "case {label}: expected a {granularity} query, got {config}"
            );
        }
        if let Some(group_by) = case.group_by {
            let expected: Vec<Value> = group_by
                .iter()
                .map(|key| json!({"key": key, "type": "DIMENSION"}))
                .collect();
            assert_eq!(
                query["groupBy"],
                json!(expected),
                "case {label}: expected the published grouping {group_by:?}, got {config}"
            );
        }
        if let Some((start, end)) = case.range {
            assert_eq!(
                query["timeRange"],
                json!({
                    "startTime": {"type": "RELATIVE", "value": start},
                    "endTime": {"type": "RELATIVE", "value": end}
                }),
                "case {label}: expected the {start}..{end} relative range, got {config}"
            );
        }
        if let Some((dimension, values)) = case.filter {
            assert_eq!(
                query["filter"]["dimensions"]["key"].as_str(),
                Some(dimension),
                "case {label}: expected a {dimension} filter, got {config}"
            );
            assert_eq!(
                query["filter"]["dimensions"]["values"],
                json!(values),
                "case {label}: expected the published filter values {values:?}, got {config}"
            );
        }
        for member in case.absent {
            assert!(
                query[*member].is_null(),
                "case {label}: the published panel carries no {member}, got {config}"
            );
        }

        match case.graph {
            Some((series, visual_type)) => {
                assert!(
                    config["displayConfig"]["graph"][series].is_object(),
                    "case {label}: expected a graph of {series}, got {config}"
                );
                if let Some(visual_type) = visual_type {
                    assert_eq!(
                        config["displayConfig"]["graph"][series]["visualType"].as_str(),
                        Some(visual_type),
                        "case {label}: expected a {visual_type} graph, got {config}"
                    );
                }
                assert!(
                    config["displayConfig"]["table"].is_null(),
                    "case {label}: a graph panel is not drawn as a table too, got {config}"
                );
            }
            None => {
                assert!(
                    config["displayConfig"]["table"].is_object(),
                    "case {label}: expected a table display, got {config}"
                );
                assert!(
                    config["displayConfig"]["graph"].is_null(),
                    "case {label}: a table panel is not drawn as a graph too, got {config}"
                );
            }
        }
    }
}
