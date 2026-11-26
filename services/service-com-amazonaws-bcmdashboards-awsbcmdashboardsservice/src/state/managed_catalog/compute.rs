// SPDX-License-Identifier: CC0-1.0

//! The panels the `Compute` entry publishes.
//!
//! Recorded here are: the sixteen `WidgetId`s this entry publishes; the `SERVICE` values its
//! panels narrow the bill to ([`super::scopes::COMPUTE_SERVICES`]); and the cost-per-service
//! pair in full — the unblended `STACK` graph grouped by service over the trailing year with
//! its published title and description, and its amortized counterpart over the same filter.
//! Three of its panels are the EC2 and commitment panels the catalog also publishes on
//! another entry, so they are defined once in [`super::shared_panels`].
//!
//! The remaining panels are this emulator's reading of the compute bill under the ids the
//! catalog publishes for them: the record does not disclose their title, description or
//! query, and the model only bounds that text.

use crate::ops::types::Widget;

use super::panels::{
    dimension_filter, graph_display, monthly_cost, panel, savings_plans_utilization, table_display,
    trailing_year, DIMENSION_AZ, DIMENSION_INSTANCE_TYPE, DIMENSION_LINKED_ACCOUNT,
    DIMENSION_PURCHASE_TYPE, DIMENSION_REGION, DIMENSION_SERVICE, DIMENSION_USAGE_TYPE,
    METRIC_AMORTIZED_COST, METRIC_UNBLENDED_COST, METRIC_USAGE_QUANTITY, VISUAL_TYPE_BAR,
    VISUAL_TYPE_LINE, VISUAL_TYPE_STACK,
};
use super::scopes::{COMPUTE_SERVICES, CONTAINER_SERVICES, LAMBDA_SERVICE};
use super::shared_panels::{
    compute_savings_plans_coverage, ec2_reservation_coverage, ec2_reservation_utilization,
};

/// The published `WidgetId`s of this entry's own panels, from its recorded detail read.
const WIDGET_ID_COST_PER_SERVICE: &str = "5b409f68aa9093525bfe5195b04cb975";
const WIDGET_ID_AMORTIZED_PER_SERVICE: &str = "548b95216f045ec2f5da23067f8e3768";
const WIDGET_ID_COST_PER_REGION: &str = "0241f9abbdbd20e3d93b196f40678f0f";
const WIDGET_ID_COST_PER_ACCOUNT: &str = "064adb1bb9690bf0fa1c85e9876c9ae4";
const WIDGET_ID_COST_PER_INSTANCE_TYPE: &str = "0874d882ac0b7b8d1343d6f54245ce8f";
const WIDGET_ID_COST_PER_PURCHASE_TYPE: &str = "16d66087316cf44454a2efdc86c0e2cf";
const WIDGET_ID_USAGE_TREND: &str = "176354389adbb656d157b533fd239069";
const WIDGET_ID_COST_TABLE: &str = "31d834ccdeedda0007ee07a0c2f5e811";
const WIDGET_ID_SAVINGS_PLANS_UTILIZATION: &str = "5cb4febf03139a4babd954e306ae690f";
const WIDGET_ID_COST_PER_ZONE: &str = "b896d55ac55b132f02d9308b64cf7eaa";
const WIDGET_ID_CONTAINER_COST: &str = "efa71a19fa4273a8572a2a583ab62af8";
const WIDGET_ID_COST_PER_USAGE_TYPE: &str = "f8b45d4e6069b5b3ba5ff44dfb5394b0";
const WIDGET_ID_FUNCTION_COST: &str = "da7ad4ebb647e09f893b338b1cf42ad4";

/// The sixteen panels this entry publishes.
pub(super) fn widgets() -> Vec<Widget> {
    let compute = || dimension_filter(DIMENSION_SERVICE, &COMPUTE_SERVICES);
    vec![
        panel(
            WIDGET_ID_COST_PER_SERVICE,
            "Cost Per Compute Service",
            "Monthly unblended cost across your top spending compute services.",
            monthly_cost(
                METRIC_UNBLENDED_COST,
                &[DIMENSION_SERVICE],
                Some(compute()),
            ),
            graph_display(METRIC_UNBLENDED_COST, VISUAL_TYPE_STACK),
        ),
        panel(
            WIDGET_ID_AMORTIZED_PER_SERVICE,
            "Amortized Cost Per Compute Service",
            "Monthly amortized cost across your top spending compute services.",
            monthly_cost(
                METRIC_AMORTIZED_COST,
                &[DIMENSION_SERVICE],
                Some(compute()),
            ),
            graph_display(METRIC_AMORTIZED_COST, VISUAL_TYPE_STACK),
        ),
        panel(
            WIDGET_ID_COST_PER_REGION,
            "Compute Cost By Region",
            "Monthly unblended cost of your compute services by Region.",
            monthly_cost(METRIC_UNBLENDED_COST, &[DIMENSION_REGION], Some(compute())),
            graph_display(METRIC_UNBLENDED_COST, VISUAL_TYPE_STACK),
        ),
        panel(
            WIDGET_ID_COST_PER_ACCOUNT,
            "Compute Cost By Account",
            "Monthly unblended cost of your compute services by account.",
            monthly_cost(
                METRIC_UNBLENDED_COST,
                &[DIMENSION_LINKED_ACCOUNT],
                Some(compute()),
            ),
            graph_display(METRIC_UNBLENDED_COST, VISUAL_TYPE_STACK),
        ),
        panel(
            WIDGET_ID_COST_PER_INSTANCE_TYPE,
            "Compute Cost By Instance Type",
            "Monthly unblended cost of your compute services by instance type.",
            monthly_cost(
                METRIC_UNBLENDED_COST,
                &[DIMENSION_INSTANCE_TYPE],
                Some(compute()),
            ),
            graph_display(METRIC_UNBLENDED_COST, VISUAL_TYPE_BAR),
        ),
        panel(
            WIDGET_ID_COST_PER_PURCHASE_TYPE,
            "Compute Cost By Purchase Type",
            "Monthly amortized cost of your compute services by purchase type.",
            monthly_cost(
                METRIC_AMORTIZED_COST,
                &[DIMENSION_PURCHASE_TYPE],
                Some(compute()),
            ),
            graph_display(METRIC_AMORTIZED_COST, VISUAL_TYPE_STACK),
        ),
        panel(
            WIDGET_ID_USAGE_TREND,
            "Compute Usage Trend",
            "Monthly usage quantity of your compute services.",
            monthly_cost(METRIC_USAGE_QUANTITY, &[], Some(compute())),
            graph_display(METRIC_USAGE_QUANTITY, VISUAL_TYPE_LINE),
        ),
        panel(
            WIDGET_ID_COST_TABLE,
            "Compute Cost Breakdown",
            "Monthly unblended cost of each compute service you use, as a table.",
            monthly_cost(
                METRIC_UNBLENDED_COST,
                &[DIMENSION_SERVICE],
                Some(compute()),
            ),
            table_display(),
        ),
        panel(
            WIDGET_ID_SAVINGS_PLANS_UTILIZATION,
            "Compute Savings Plans Utilization",
            "Shows how much of your compute Savings Plans commitment is used each month.",
            savings_plans_utilization(trailing_year(), Some(compute())),
            table_display(),
        ),
        panel(
            WIDGET_ID_COST_PER_ZONE,
            "Compute Cost By Availability Zone",
            "Monthly unblended cost of your compute services by Availability Zone.",
            monthly_cost(METRIC_UNBLENDED_COST, &[DIMENSION_AZ], Some(compute())),
            graph_display(METRIC_UNBLENDED_COST, VISUAL_TYPE_BAR),
        ),
        panel(
            WIDGET_ID_FUNCTION_COST,
            "Serverless Function Cost",
            "Monthly unblended cost of your serverless function usage.",
            monthly_cost(
                METRIC_UNBLENDED_COST,
                &[],
                Some(dimension_filter(DIMENSION_SERVICE, &LAMBDA_SERVICE)),
            ),
            graph_display(METRIC_UNBLENDED_COST, VISUAL_TYPE_LINE),
        ),
        panel(
            WIDGET_ID_CONTAINER_COST,
            "Container Services Cost",
            "Monthly unblended cost of your container services.",
            monthly_cost(
                METRIC_UNBLENDED_COST,
                &[DIMENSION_SERVICE],
                Some(dimension_filter(DIMENSION_SERVICE, &CONTAINER_SERVICES)),
            ),
            graph_display(METRIC_UNBLENDED_COST, VISUAL_TYPE_STACK),
        ),
        panel(
            WIDGET_ID_COST_PER_USAGE_TYPE,
            "Compute Cost By Usage Type",
            "Monthly unblended cost of your compute services by usage type.",
            monthly_cost(
                METRIC_UNBLENDED_COST,
                &[DIMENSION_USAGE_TYPE],
                Some(compute()),
            ),
            graph_display(METRIC_UNBLENDED_COST, VISUAL_TYPE_BAR),
        ),
        // The commitment panels this entry shares with the dashboards published for the
        // commitments themselves.
        ec2_reservation_utilization(),
        ec2_reservation_coverage(),
        compute_savings_plans_coverage(),
    ]
}
