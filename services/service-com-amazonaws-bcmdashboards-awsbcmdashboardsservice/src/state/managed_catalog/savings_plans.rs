// SPDX-License-Identifier: CC0-1.0

//! The panels the `Savings Plans` entry publishes.
//!
//! Recorded here are: the fourteen `WidgetId`s this entry publishes; the total-cost panel in
//! full — its published title and description, its `LINE` graph of unblended cost and its
//! unfiltered monthly `costAndUsage` query over the trailing year; and the on-demand panel in
//! full, whose `STACK` graph is grouped by service and narrowed to the on-demand purchase
//! type, which is the spend a Savings Plan would cover. Two of its panels are the
//! commitment-coverage panels the catalog also publishes on the dashboard of the spend they
//! cover, so they are defined once in [`super::shared_panels`].
//!
//! The remaining panels are this emulator's reading of the committed part of the bill under
//! the ids the catalog publishes for them: the record does not disclose their title,
//! description or query, and the model only bounds that text.

use crate::ops::types::Widget;

use super::panels::{
    cost_and_usage, dimension_filter, graph_display, latest_month, monthly_cost, panel,
    savings_plans_coverage, savings_plans_utilization, table_display, trailing_year,
    DIMENSION_LINKED_ACCOUNT, DIMENSION_PURCHASE_TYPE, DIMENSION_SAVINGS_PLANS_TYPE,
    DIMENSION_SERVICE, GRANULARITY_DAILY, METRIC_AMORTIZED_COST, METRIC_UNBLENDED_COST,
    VISUAL_TYPE_LINE, VISUAL_TYPE_STACK,
};
use super::scopes::{PURCHASE_TYPE_ON_DEMAND, PURCHASE_TYPE_SAVINGS_PLANS};
use super::shared_panels::{compute_savings_plans_coverage, database_savings_plans_coverage};

/// The published `WidgetId`s of this entry's own panels, from its recorded detail read.
const WIDGET_ID_TOTAL_COST: &str = "340ae1c358c30fe9d9971a29113bff00";
const WIDGET_ID_ON_DEMAND_PER_SERVICE: &str = "40d727374c54df419fc4b162a03cfbe7";
const WIDGET_ID_UTILIZATION: &str = "0e672c5eb0cd76722126002798c54ba7";
const WIDGET_ID_COVERAGE: &str = "347680899500e906f573587c8bc9a9c6";
const WIDGET_ID_COST_PER_SERVICE: &str = "394418214c2967dbb8adc2e0974ea42a";
const WIDGET_ID_COMMITMENT_SPEND: &str = "3c990530b4b45ba456c93ccf6c0258aa";
const WIDGET_ID_COVERAGE_PER_ACCOUNT: &str = "4c642d0693fa7117525e53250a43b207";
const WIDGET_ID_ON_DEMAND_PER_ACCOUNT: &str = "8256bb6dab24548a7dca7a3f01d48104";
const WIDGET_ID_TOTAL_AMORTIZED_COST: &str = "bfcb4256607d135afd495710ebbafdd3";
const WIDGET_ID_COST_PER_PLAN_TYPE: &str = "cd071457df17c5708f88c46d16a30bca";
const WIDGET_ID_COMMITMENT_TABLE: &str = "e856e20f051f124e39b20bd7982f5c83";
const WIDGET_ID_DAILY_ON_DEMAND: &str = "fb6d2f8cf6a44118f898249f8b55c962";

/// The fourteen panels this entry publishes.
pub(super) fn widgets() -> Vec<Widget> {
    let on_demand = || dimension_filter(DIMENSION_PURCHASE_TYPE, &PURCHASE_TYPE_ON_DEMAND);
    let savings_plans = || dimension_filter(DIMENSION_PURCHASE_TYPE, &PURCHASE_TYPE_SAVINGS_PLANS);
    vec![
        panel(
            WIDGET_ID_TOTAL_COST,
            "Total Cost",
            "Total monthly unblended cost across all services.",
            monthly_cost(METRIC_UNBLENDED_COST, &[], None),
            graph_display(METRIC_UNBLENDED_COST, VISUAL_TYPE_LINE),
        ),
        panel(
            WIDGET_ID_ON_DEMAND_PER_SERVICE,
            "On Demand Spend By Service",
            "Monthly unblended on-demand spend by service.",
            monthly_cost(
                METRIC_UNBLENDED_COST,
                &[DIMENSION_SERVICE],
                Some(on_demand()),
            ),
            graph_display(METRIC_UNBLENDED_COST, VISUAL_TYPE_STACK),
        ),
        panel(
            WIDGET_ID_UTILIZATION,
            "Savings Plans Utilization",
            "Shows how much of your Savings Plans commitment is used each month.",
            savings_plans_utilization(trailing_year(), None),
            table_display(),
        ),
        panel(
            WIDGET_ID_COVERAGE,
            "Savings Plans Coverage",
            "Shows percentage of eligible spend covered by Savings Plans.",
            savings_plans_coverage(trailing_year(), &[], None),
            table_display(),
        ),
        panel(
            WIDGET_ID_COST_PER_SERVICE,
            "Savings Plans Cost By Service",
            "Monthly amortized cost of your Savings Plans by service.",
            monthly_cost(
                METRIC_AMORTIZED_COST,
                &[DIMENSION_SERVICE],
                Some(savings_plans()),
            ),
            graph_display(METRIC_AMORTIZED_COST, VISUAL_TYPE_STACK),
        ),
        panel(
            WIDGET_ID_COMMITMENT_SPEND,
            "Commitment Spend By Purchase Type",
            "Monthly amortized cost split by purchase type, so committed spend shows beside \
             on-demand spend.",
            monthly_cost(METRIC_AMORTIZED_COST, &[DIMENSION_PURCHASE_TYPE], None),
            graph_display(METRIC_AMORTIZED_COST, VISUAL_TYPE_STACK),
        ),
        panel(
            WIDGET_ID_COVERAGE_PER_ACCOUNT,
            "Savings Plans Coverage By Account",
            "Shows percentage of eligible spend covered by Savings Plans, by account.",
            savings_plans_coverage(trailing_year(), &[DIMENSION_LINKED_ACCOUNT], None),
            table_display(),
        ),
        panel(
            WIDGET_ID_ON_DEMAND_PER_ACCOUNT,
            "On Demand Spend By Account",
            "Monthly unblended on-demand spend by account.",
            monthly_cost(
                METRIC_UNBLENDED_COST,
                &[DIMENSION_LINKED_ACCOUNT],
                Some(on_demand()),
            ),
            graph_display(METRIC_UNBLENDED_COST, VISUAL_TYPE_STACK),
        ),
        panel(
            WIDGET_ID_TOTAL_AMORTIZED_COST,
            "Total Amortized Cost",
            "Total monthly amortized cost across all services.",
            monthly_cost(METRIC_AMORTIZED_COST, &[], None),
            graph_display(METRIC_AMORTIZED_COST, VISUAL_TYPE_LINE),
        ),
        panel(
            WIDGET_ID_COST_PER_PLAN_TYPE,
            "Savings Plans Cost By Plan Type",
            "Monthly amortized cost of your Savings Plans by plan type.",
            monthly_cost(
                METRIC_AMORTIZED_COST,
                &[DIMENSION_SAVINGS_PLANS_TYPE],
                Some(savings_plans()),
            ),
            graph_display(METRIC_AMORTIZED_COST, VISUAL_TYPE_STACK),
        ),
        panel(
            WIDGET_ID_COMMITMENT_TABLE,
            "Commitment Cost Breakdown",
            "Monthly amortized cost of each purchase type, as a table.",
            monthly_cost(METRIC_AMORTIZED_COST, &[DIMENSION_PURCHASE_TYPE], None),
            table_display(),
        ),
        panel(
            WIDGET_ID_DAILY_ON_DEMAND,
            "Daily On Demand Spend",
            "Daily unblended on-demand spend over the latest month.",
            cost_and_usage(
                METRIC_UNBLENDED_COST,
                GRANULARITY_DAILY,
                latest_month(),
                &[],
                Some(on_demand()),
            ),
            graph_display(METRIC_UNBLENDED_COST, VISUAL_TYPE_LINE),
        ),
        // The coverage panels this entry shares with the dashboards of the spend the
        // Savings Plans cover.
        compute_savings_plans_coverage(),
        database_savings_plans_coverage(),
    ]
}
