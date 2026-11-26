// SPDX-License-Identifier: CC0-1.0

//! The panels the `Database` entry publishes.
//!
//! Recorded here are: the fourteen `WidgetId`s this entry publishes; the `SERVICE` values its
//! panels narrow the bill to ([`super::scopes::DATABASE_SERVICES`]); and the cost-per-service
//! pair in full — the unblended `STACK` graph grouped by service over the trailing year with
//! its published title and description, and its amortized counterpart over the same filter.
//! Three of its panels are the reservation and commitment panels the catalog also publishes
//! on another entry, so they are defined once in [`super::shared_panels`].
//!
//! The remaining panels are this emulator's reading of the database bill under the ids the
//! catalog publishes for them: the record does not disclose their title, description or
//! query, and the model only bounds that text.

use crate::ops::types::Widget;

use super::panels::{
    cost_and_usage, dimension_filter, graph_display, latest_month, monthly_cost, panel,
    table_display, DIMENSION_DATABASE_ENGINE, DIMENSION_DEPLOYMENT_OPTION, DIMENSION_INSTANCE_TYPE,
    DIMENSION_LINKED_ACCOUNT, DIMENSION_PURCHASE_TYPE, DIMENSION_REGION, DIMENSION_SERVICE,
    GRANULARITY_DAILY, METRIC_AMORTIZED_COST, METRIC_UNBLENDED_COST, METRIC_USAGE_QUANTITY,
    VISUAL_TYPE_BAR, VISUAL_TYPE_LINE, VISUAL_TYPE_STACK,
};
use super::scopes::DATABASE_SERVICES;
use super::shared_panels::{
    database_reservation_coverage, database_reservation_utilization,
    database_savings_plans_coverage,
};

/// The published `WidgetId`s of this entry's own panels, from its recorded detail read.
const WIDGET_ID_COST_PER_SERVICE: &str = "9cfcef554e56b7042da915140598b563";
const WIDGET_ID_AMORTIZED_PER_SERVICE: &str = "01216d7f7d6e870db27b792d5d0345a8";
const WIDGET_ID_COST_PER_ENGINE: &str = "0d5ddc308ef25720bd8562bf4bba82b6";
const WIDGET_ID_COST_PER_REGION: &str = "423d943a56564575c875bdd732402fba";
const WIDGET_ID_COST_PER_ACCOUNT: &str = "4727ff333834e765dc2f7fa729258736";
const WIDGET_ID_COST_PER_INSTANCE_TYPE: &str = "4f605948b37e4a9280d3b650c7803828";
const WIDGET_ID_COST_PER_PURCHASE_TYPE: &str = "559f2986324410639d4c80f5beb3cd0b";
const WIDGET_ID_USAGE_TREND: &str = "721eb011eeaf1e687e19d7929defc99b";
const WIDGET_ID_COST_TABLE: &str = "ad179972fea2e5f44a33275b363fee6f";
const WIDGET_ID_DAILY_COST: &str = "c38bf72d2d4bc27a135ed376bd1fa3a4";
const WIDGET_ID_COST_PER_DEPLOYMENT: &str = "fdef4552aa22ac47727c7ed8e5de4f84";

/// The fourteen panels this entry publishes.
pub(super) fn widgets() -> Vec<Widget> {
    let databases = || dimension_filter(DIMENSION_SERVICE, &DATABASE_SERVICES);
    vec![
        panel(
            WIDGET_ID_COST_PER_SERVICE,
            "Database Cost Per Service",
            "Monthly unblended cost across top spending database services.",
            monthly_cost(
                METRIC_UNBLENDED_COST,
                &[DIMENSION_SERVICE],
                Some(databases()),
            ),
            graph_display(METRIC_UNBLENDED_COST, VISUAL_TYPE_STACK),
        ),
        panel(
            WIDGET_ID_AMORTIZED_PER_SERVICE,
            "Amortized Database Cost Per Service",
            "Monthly amortized cost across top spending database services.",
            monthly_cost(
                METRIC_AMORTIZED_COST,
                &[DIMENSION_SERVICE],
                Some(databases()),
            ),
            graph_display(METRIC_AMORTIZED_COST, VISUAL_TYPE_STACK),
        ),
        panel(
            WIDGET_ID_COST_PER_ENGINE,
            "Database Cost By Engine",
            "Monthly unblended database cost by database engine.",
            monthly_cost(
                METRIC_UNBLENDED_COST,
                &[DIMENSION_DATABASE_ENGINE],
                Some(databases()),
            ),
            graph_display(METRIC_UNBLENDED_COST, VISUAL_TYPE_STACK),
        ),
        panel(
            WIDGET_ID_COST_PER_REGION,
            "Database Cost By Region",
            "Monthly unblended cost of your database services by Region.",
            monthly_cost(
                METRIC_UNBLENDED_COST,
                &[DIMENSION_REGION],
                Some(databases()),
            ),
            graph_display(METRIC_UNBLENDED_COST, VISUAL_TYPE_STACK),
        ),
        panel(
            WIDGET_ID_COST_PER_ACCOUNT,
            "Database Cost By Account",
            "Monthly unblended cost of your database services by account.",
            monthly_cost(
                METRIC_UNBLENDED_COST,
                &[DIMENSION_LINKED_ACCOUNT],
                Some(databases()),
            ),
            graph_display(METRIC_UNBLENDED_COST, VISUAL_TYPE_STACK),
        ),
        panel(
            WIDGET_ID_COST_PER_INSTANCE_TYPE,
            "Database Cost By Instance Type",
            "Monthly unblended cost of your database services by instance type.",
            monthly_cost(
                METRIC_UNBLENDED_COST,
                &[DIMENSION_INSTANCE_TYPE],
                Some(databases()),
            ),
            graph_display(METRIC_UNBLENDED_COST, VISUAL_TYPE_BAR),
        ),
        panel(
            WIDGET_ID_COST_PER_PURCHASE_TYPE,
            "Database Cost By Purchase Type",
            "Monthly amortized cost of your database services by purchase type.",
            monthly_cost(
                METRIC_AMORTIZED_COST,
                &[DIMENSION_PURCHASE_TYPE],
                Some(databases()),
            ),
            graph_display(METRIC_AMORTIZED_COST, VISUAL_TYPE_STACK),
        ),
        panel(
            WIDGET_ID_USAGE_TREND,
            "Database Usage Trend",
            "Monthly usage quantity of your database services.",
            monthly_cost(METRIC_USAGE_QUANTITY, &[], Some(databases())),
            graph_display(METRIC_USAGE_QUANTITY, VISUAL_TYPE_LINE),
        ),
        panel(
            WIDGET_ID_COST_TABLE,
            "Database Cost Breakdown",
            "Monthly unblended cost of each database service you use, as a table.",
            monthly_cost(
                METRIC_UNBLENDED_COST,
                &[DIMENSION_SERVICE],
                Some(databases()),
            ),
            table_display(),
        ),
        panel(
            WIDGET_ID_DAILY_COST,
            "Daily Database Cost",
            "Daily unblended cost of your database services over the latest month.",
            cost_and_usage(
                METRIC_UNBLENDED_COST,
                GRANULARITY_DAILY,
                latest_month(),
                &[],
                Some(databases()),
            ),
            graph_display(METRIC_UNBLENDED_COST, VISUAL_TYPE_LINE),
        ),
        panel(
            WIDGET_ID_COST_PER_DEPLOYMENT,
            "Database Cost By Deployment Option",
            "Monthly unblended database cost by deployment option.",
            monthly_cost(
                METRIC_UNBLENDED_COST,
                &[DIMENSION_DEPLOYMENT_OPTION],
                Some(databases()),
            ),
            graph_display(METRIC_UNBLENDED_COST, VISUAL_TYPE_BAR),
        ),
        // The commitment panels this entry shares with the dashboards published for the
        // commitments themselves.
        database_reservation_utilization(),
        database_reservation_coverage(),
        database_savings_plans_coverage(),
    ]
}
