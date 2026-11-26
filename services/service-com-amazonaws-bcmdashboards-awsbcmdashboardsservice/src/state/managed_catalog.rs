// SPDX-License-Identifier: CC0-1.0

//! The AWS-managed dashboard catalog every account already holds.
//!
//! These dashboards are the SERVICE's own published data rather than the account's: an
//! account that has created nothing still enumerates them, every account enumerates the
//! same ones, and each is reachable by the ARN the catalog publishes for it. So the catalog
//! is a fixed table here, seeded into the one dashboard store
//! ([`super::ComAmazonawsBcmdashboardsAwsbcmdashboardsserviceState::default`]) and read back
//! through the same map, the same projection and the same pagination as a dashboard a caller
//! created — no handler branches on whether a dashboard is managed, and nothing about the
//! catalog is derived from a request.
//!
//! # Where the table comes from
//!
//! The model says nothing about the catalog's CONTENTS: `DashboardType` enumerates the custom
//! case alone (`smithy_query(query="shape",shape="DashboardType")`), so what the catalog
//! holds is observed behaviour. Published, and therefore written out here, are each entry's
//! resource id, name and description, the publication and revision instants its entries
//! carry ([`MANAGED_CATALOG_CREATED_AT`], [`MANAGED_CATALOG_REVISED_AT`]), and the whole
//! `WidgetId` list of every entry — a panel's id is the identity of the panel, which is why
//! the same id appears on two entries wherever both publish the same reading
//! ([`shared_panels`]), and why a client may store one: a scheduled report names the widgets
//! it reports by id.
//!
//! Each entry's module states which of its panels the wire record spells out in full and
//! which carry prose and a query this emulator wrote to the reading the published panel
//! shows. None of it is derived from a request, and none of it is per-run: two stores answer
//! the same catalog, and so does the same store twice.
//!
//! Everything else is the model's. `Widget` fixes a panel's members and their bounds — a
//! 32-hexadecimal-character `id`, a 2..50 `title`, a 1..200 `description`, 1..2 `configs` and
//! a `width`/`height`/`horizontalOffset` layout inside the declared ranges (2..6, 4..10) —
//! `QueryParameters` fixes which queries a panel may run, and `Dimension` fixes the keys a
//! query groups and filters by
//! (`smithy_query(query="shape",shape="Widget,QueryParameters,CostAndUsageQuery,Dimension")`).

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime};

use crate::ops::types::Widget;

use super::{Dashboard, ARN_NAMESPACE, DASHBOARD_RESOURCE_TYPE, DASHBOARD_TYPE_AWS_MANAGED};

mod compute;
mod cost_overview;
mod database;
mod panels;
mod reservations;
mod savings_plans;
mod scopes;
mod shared_panels;

/// The resource-id prefix of every AWS-managed dashboard's ARN, which is what tells one apart
/// from a dashboard a caller created: the catalog's ARNs are
/// `arn:aws:bcm-dashboards::<account>:dashboard/managed-<32 hex>`, while
/// [`super::ComAmazonawsBcmdashboardsAwsbcmdashboardsserviceState::mint_dashboard_arn`] mints
/// a bare 32-hex resource id. Both spellings satisfy `DashboardArn`'s `[-a-z0-9]+` resource
/// segment.
const MANAGED_DASHBOARD_ID_PREFIX: &str = "managed-";

/// When the AWS-managed catalog was published, and when it was last revised, as seconds since
/// the Unix epoch.
///
/// The catalog is Amazon Web Services' own data, not the account's: its dashboards exist
/// before a caller writes anything, so their `createdAt`/`updatedAt` are the instants the
/// catalog was published and last revised rather than anything this emulator's clock produced
/// — which is why they are stored constants and are stable across reads and across restarts.
/// A dashboard a CALLER creates is stamped from the clock (`CreateDashboard`), as its own
/// record's instants.
///
/// A revision is per ENTRY: an entry revised since the catalog was published carries the
/// revision instant, and one that has not been revised reads its publication instant back as
/// its `updatedAt` ([`PUBLISHED_CATALOG`] states which).
const MANAGED_CATALOG_CREATED_AT: u64 = 1_786_644_000;
const MANAGED_CATALOG_REVISED_AT: u64 = 1_787_079_600;

/// The resource ids of the published catalog's entries, each the 32-hex id the catalog
/// publishes for that dashboard (the `managed-` prefix is added when the ARN is built).
const COST_OVERVIEW_ID: &str = "4dc5619130f2463687fc45e003e4b930";
const COMPUTE_ID: &str = "b0e4f3ab71454cf48a7d148287222f9d";
const DATABASE_ID: &str = "d2effbcc2dfd44eeb8ebec3f6e83382a";
const RESERVATIONS_ID: &str = "a8f4e2b1c3d6490ea1b7f5c8d2e94a03";
const SAVINGS_PLANS_ID: &str = "7b3c91d4e5f2a0186c4d8e9f1a2b3c4d";

/// One entry of the published catalog: the resource id, name and description a listing
/// reports for it, the instant it was last revised, and the panels a detail read of it serves.
struct PublishedEntry {
    resource_id: &'static str,
    name: &'static str,
    description: &'static str,
    revised_at: u64,
    /// The panels this entry publishes, in the order the catalog publishes them. Each entry
    /// has its OWN builder, so an entry serves its panels and no other entry's.
    panels: fn() -> Vec<Widget>,
}

/// The published catalog's entries, in the order the catalog publishes them.
const PUBLISHED_CATALOG: [PublishedEntry; 5] = [
    PublishedEntry {
        resource_id: COST_OVERVIEW_ID,
        name: "Cost Overview and Trends",
        description: "High-level view of your AWS costs, usage trends, and forecasts",
        revised_at: MANAGED_CATALOG_REVISED_AT,
        panels: cost_overview::widgets,
    },
    PublishedEntry {
        resource_id: COMPUTE_ID,
        name: "Compute",
        description: "Compute spend, efficiency, and commitment health across your compute \
                      services",
        revised_at: MANAGED_CATALOG_REVISED_AT,
        panels: compute::widgets,
    },
    PublishedEntry {
        resource_id: DATABASE_ID,
        name: "Database",
        description: "Database spend, efficiency, and commitment health across your database \
                      services",
        revised_at: MANAGED_CATALOG_REVISED_AT,
        panels: database::widgets,
    },
    PublishedEntry {
        resource_id: RESERVATIONS_ID,
        name: "Reservations",
        description: "Reserved Instance cost, utilization, and coverage health",
        // Not revised since the catalog was published, so its `updatedAt` is its
        // `createdAt`.
        revised_at: MANAGED_CATALOG_CREATED_AT,
        panels: reservations::widgets,
    },
    PublishedEntry {
        resource_id: SAVINGS_PLANS_ID,
        name: "Savings Plans",
        description: "Savings Plans utilization and coverage health",
        revised_at: MANAGED_CATALOG_REVISED_AT,
        panels: savings_plans::widgets,
    },
];

/// One stored dashboard of the AWS-managed catalog, built the same way a created one is: the
/// record carries its own ARN, name, description, type and widgets, and every reader projects
/// it from the store.
fn managed_dashboard(entry: &PublishedEntry) -> Dashboard {
    Dashboard {
        arn: published_dashboard_arn(entry.resource_id),
        name: entry.name.to_string(),
        description: Some(entry.description.to_string()),
        dashboard_type: DASHBOARD_TYPE_AWS_MANAGED.to_string(),
        widgets: (entry.panels)(),
        created_at: SystemTime::UNIX_EPOCH + Duration::from_secs(MANAGED_CATALOG_CREATED_AT),
        updated_at: SystemTime::UNIX_EPOCH + Duration::from_secs(entry.revised_at),
        // The catalog carries no tags and no resource-based policy: a tag is an account's own
        // metadata, and this model declares no operation that attaches a policy at all.
        resource_tags: Vec::new(),
        resource_policy: None,
    }
}

/// The AWS-managed dashboard catalog every account already holds, keyed by ARN exactly as a
/// created dashboard is.
///
/// `pub` so a test can state the catalog a listing must report alongside its own dashboards
/// from the one place that defines it.
pub fn managed_dashboard_catalog() -> BTreeMap<String, Dashboard> {
    PUBLISHED_CATALOG
        .iter()
        .map(|entry| {
            let dashboard = managed_dashboard(entry);
            (dashboard.arn.clone(), dashboard)
        })
        .collect()
}

/// The ARN the catalog publishes for the entry under `resource_id`.
fn published_dashboard_arn(resource_id: &str) -> String {
    ::engine::emulator_arn(
        ARN_NAMESPACE,
        "",
        &format!("{DASHBOARD_RESOURCE_TYPE}/{MANAGED_DASHBOARD_ID_PREFIX}{resource_id}"),
    )
}
