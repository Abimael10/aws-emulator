// SPDX-License-Identifier: CC0-1.0

//! The panels the `Cost Overview and Trends` entry publishes.
//!
//! This is the entry whose detail read the wire record spells out furthest. Recorded here
//! are: the ten `WidgetId`s it publishes; the forecast panel in full (its title,
//! description, layout, `BAR` graph of unblended cost and the `costAndUsage` query whose
//! range runs into the forecast window); the published descriptions of the by-service,
//! purchase-type and budget panels; the budget panel's `budget` query and its wider layout;
//! the cost-efficiency panel's `costEfficiency` query, its range and its `EfficiencyScore`
//! graph; the two panels drawn as tables and what each of them queries; and that the
//! commitment-spend panels are drawn as graphs over a `costAndUsage` query.
//!
//! The remaining prose is this emulator's, written to the reading each published panel
//! carries: a title and a description are text the record does not disclose for every
//! panel, and the model only bounds them (`WidgetTitle` 2..50, `Description` 1..200).

use crate::ops::types::Widget;

use super::panels::{
    budgets, cost_and_usage, cost_efficiency, dimension_filter, graph_display, monthly_cost, panel,
    table_display, trailing_quarter, trailing_year_with_forecast, wide_panel,
    DIMENSION_LINKED_ACCOUNT, DIMENSION_PURCHASE_TYPE, DIMENSION_REGION, DIMENSION_SERVICE,
    DIMENSION_USAGE_TYPE, GRANULARITY_MONTHLY, METRIC_AMORTIZED_COST, METRIC_UNBLENDED_COST,
    PANEL_WIDTH_WIDE, SERIES_EFFICIENCY_SCORE, VISUAL_TYPE_BAR, VISUAL_TYPE_LINE,
    VISUAL_TYPE_STACK,
};
use super::scopes::{PURCHASE_TYPE_RESERVED_INSTANCES, PURCHASE_TYPE_SAVINGS_PLANS};

/// The published `WidgetId`s of this entry's panels, from its recorded detail read.
const WIDGET_ID_TOTAL_AND_FORECAST: &str = "a18b58807f3934dbcfebd21ad359afaa";
const WIDGET_ID_COST_EFFICIENCY: &str = "a15901be3f2d2c9f07f2c90938ca73d3";
const WIDGET_ID_COST_PER_SERVICE: &str = "2a7e51a22e1c00af3843c5cccb82b046";
const WIDGET_ID_COST_PER_REGION: &str = "35d71261755663fa7f5aa289556472ef";
const WIDGET_ID_COMMITTED_VS_FLEXIBLE: &str = "48381c58fdf60999c778deb93fc9284a";
const WIDGET_ID_USAGE_TYPE_TABLE: &str = "a7a5c864d98bfa0bcb1359bd4e3529f3";
const WIDGET_ID_ACCOUNT_TABLE: &str = "a80cc87050b1e60b97c65e38be2876ae";
const WIDGET_ID_SAVINGS_PLANS_SPEND: &str = "b8fe9c2e86f1737da142b62be6e865a8";
const WIDGET_ID_RESERVATION_SPEND: &str = "ce2a10fcd1ab3961dc32b6a3c595166c";
const WIDGET_ID_BUDGET_HEALTH: &str = "d771f500c22cacaa952f1bba40260445";

/// The ten panels this entry publishes.
pub(super) fn widgets() -> Vec<Widget> {
    vec![
        // The one panel whose range runs PAST the current month: it charts the bills to
        // date and the forecast for the months after them.
        panel(
            WIDGET_ID_TOTAL_AND_FORECAST,
            "Total Current And Forecasted Cost",
            "Historical monthly unblended cost with forecasted cost.",
            cost_and_usage(
                METRIC_UNBLENDED_COST,
                GRANULARITY_MONTHLY,
                trailing_year_with_forecast(),
                &[],
                None,
            ),
            graph_display(METRIC_UNBLENDED_COST, VISUAL_TYPE_BAR),
        ),
        panel(
            WIDGET_ID_COST_EFFICIENCY,
            "Cost Efficiency Score",
            "Monthly score of how efficiently your account spends.",
            cost_efficiency(trailing_quarter(), &[]),
            graph_display(SERIES_EFFICIENCY_SCORE, VISUAL_TYPE_LINE),
        ),
        panel(
            WIDGET_ID_COST_PER_SERVICE,
            "Top Services By Cost",
            "Top services driving your total monthly unblended cost.",
            monthly_cost(METRIC_UNBLENDED_COST, &[DIMENSION_SERVICE], None),
            graph_display(METRIC_UNBLENDED_COST, VISUAL_TYPE_STACK),
        ),
        panel(
            WIDGET_ID_COST_PER_REGION,
            "Top Regions By Cost",
            "Top regions driving your total monthly unblended cost.",
            monthly_cost(METRIC_UNBLENDED_COST, &[DIMENSION_REGION], None),
            graph_display(METRIC_UNBLENDED_COST, VISUAL_TYPE_STACK),
        ),
        panel(
            WIDGET_ID_COMMITTED_VS_FLEXIBLE,
            "Committed vs Flexible Spend",
            "Total monthly amortized cost split by purchase type.",
            monthly_cost(METRIC_AMORTIZED_COST, &[DIMENSION_PURCHASE_TYPE], None),
            graph_display(METRIC_AMORTIZED_COST, VISUAL_TYPE_STACK),
        ),
        panel(
            WIDGET_ID_USAGE_TYPE_TABLE,
            "Cost By Usage Type",
            "Monthly unblended cost of each usage type on your bill, as a table.",
            monthly_cost(METRIC_UNBLENDED_COST, &[DIMENSION_USAGE_TYPE], None),
            table_display(),
        ),
        wide_panel(
            WIDGET_ID_ACCOUNT_TABLE,
            "Cost Breakdown By Account",
            "Monthly unblended cost of each account in your organization, as a table.",
            PANEL_WIDTH_WIDE,
            monthly_cost(METRIC_UNBLENDED_COST, &[DIMENSION_LINKED_ACCOUNT], None),
            table_display(),
        ),
        // The two panels reporting what each kind of commitment costs: the spend the
        // Savings Plans account for, and the spend the reservations account for, both
        // amortized over the months they cover.
        panel(
            WIDGET_ID_SAVINGS_PLANS_SPEND,
            "Savings Plans Spend",
            "Monthly amortized spend covered by your Savings Plans.",
            monthly_cost(
                METRIC_AMORTIZED_COST,
                &[],
                Some(dimension_filter(
                    DIMENSION_PURCHASE_TYPE,
                    &PURCHASE_TYPE_SAVINGS_PLANS,
                )),
            ),
            graph_display(METRIC_AMORTIZED_COST, VISUAL_TYPE_LINE),
        ),
        panel(
            WIDGET_ID_RESERVATION_SPEND,
            "Reservation Spend",
            "Monthly amortized spend covered by your reservations.",
            monthly_cost(
                METRIC_AMORTIZED_COST,
                &[],
                Some(dimension_filter(
                    DIMENSION_PURCHASE_TYPE,
                    &PURCHASE_TYPE_RESERVED_INSTANCES,
                )),
            ),
            graph_display(METRIC_AMORTIZED_COST, VISUAL_TYPE_LINE),
        ),
        // The budget panel reports the account's budgets themselves — the one panel of the
        // catalog whose query is the `budget` member of `QueryParameters` rather than a
        // cost-and-usage query, and it carries no filter, so it reports every budget the
        // account holds.
        wide_panel(
            WIDGET_ID_BUDGET_HEALTH,
            "Budget Health",
            "Visualizes your custom AWS Budgets, showing actual spend against budget limits \
             and forecasted spend.",
            PANEL_WIDTH_WIDE,
            budgets(None),
            table_display(),
        ),
    ]
}
