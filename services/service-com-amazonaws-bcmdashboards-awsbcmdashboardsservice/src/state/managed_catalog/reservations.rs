// SPDX-License-Identifier: CC0-1.0

//! The panels the `Reservations` entry publishes.
//!
//! Recorded here are: the sixteen `WidgetId`s this entry publishes; the EC2 cost-by-instance-type
//! panel in full — its published title and description, its wider layout, its `BAR` graph of
//! unblended cost and the `costAndUsage` query grouped by instance type over the EC2
//! services; and the EC2 reservation-coverage panel in full, which the catalog publishes
//! here and on the compute entry alike. Four of this entry's panels are the EC2 and database
//! reservation panels the catalog also publishes on the dashboard of the spend they cover, so
//! they are defined once in [`super::shared_panels`].
//!
//! The remaining panels are this emulator's reading of the reserved part of the bill under
//! the ids the catalog publishes for them: the record does not disclose their title,
//! description or query, and the model only bounds that text.

use crate::ops::types::Widget;

use super::panels::{
    dimension_filter, graph_display, monthly_cost, panel, reservation_coverage,
    reservation_utilization, table_display, trailing_year, wide_panel, DIMENSION_INSTANCE_TYPE,
    DIMENSION_LINKED_ACCOUNT, DIMENSION_PLATFORM, DIMENSION_PURCHASE_TYPE, DIMENSION_REGION,
    DIMENSION_SERVICE, DIMENSION_TENANCY, METRIC_AMORTIZED_COST, METRIC_UNBLENDED_COST,
    PANEL_WIDTH_WIDE, VISUAL_TYPE_BAR, VISUAL_TYPE_STACK,
};
use super::scopes::{EC2_COMPUTE_SERVICE, EC2_SERVICES, PURCHASE_TYPE_RESERVED_INSTANCES};
use super::shared_panels::{
    database_reservation_coverage, database_reservation_utilization, ec2_reservation_coverage,
    ec2_reservation_utilization,
};

/// The published `WidgetId`s of this entry's own panels, from its recorded detail read.
const WIDGET_ID_EC2_COST_PER_INSTANCE_TYPE: &str = "77e82a3edc9e977a00c56ba2e80842af";
const WIDGET_ID_RESERVED_INSTANCE_COST: &str = "539c389259acdcf07bec62ac4161446e";
const WIDGET_ID_RESERVATION_COST_PER_ACCOUNT: &str = "7ada8c46db87963f7a2d35137b1726dc";
const WIDGET_ID_EC2_COVERAGE_PER_INSTANCE_TYPE: &str = "910ca6ed6bbb0c4f0a0be48dc2ca40a4";
const WIDGET_ID_COVERAGE_PER_ACCOUNT: &str = "a60b3f8f5f8e072d81a4fb3499a1a5f9";
const WIDGET_ID_UTILIZATION_PER_ACCOUNT: &str = "ad7743135629887f412539aba11687a7";
const WIDGET_ID_UTILIZATION_TREND: &str = "c1cf6a66575e56af84757e176f34bf0e";
const WIDGET_ID_COVERAGE_TREND: &str = "c9f6e32918eb4dac492753fcaead75d4";
const WIDGET_ID_EC2_COST_PER_PURCHASE_TYPE: &str = "cea32ea06a810136bfe98f419c077616";
const WIDGET_ID_EC2_COST_PER_REGION: &str = "dbd67a9ddce0bf151bc13d92f6d64170";
const WIDGET_ID_EC2_COST_PER_PLATFORM: &str = "e405c05549af8c0591a81d7d63f10c24";
const WIDGET_ID_EC2_COST_PER_TENANCY: &str = "e5c0db177b28e60f14ff86415df385fb";

/// The sixteen panels this entry publishes.
pub(super) fn widgets() -> Vec<Widget> {
    let ec2 = || dimension_filter(DIMENSION_SERVICE, &EC2_SERVICES);
    let reserved = || dimension_filter(DIMENSION_PURCHASE_TYPE, &PURCHASE_TYPE_RESERVED_INSTANCES);
    vec![
        wide_panel(
            WIDGET_ID_EC2_COST_PER_INSTANCE_TYPE,
            "EC2 Cost By Instance Type",
            "Monthly unblended EC2 cost by instance type.",
            PANEL_WIDTH_WIDE,
            monthly_cost(
                METRIC_UNBLENDED_COST,
                &[DIMENSION_INSTANCE_TYPE],
                Some(ec2()),
            ),
            graph_display(METRIC_UNBLENDED_COST, VISUAL_TYPE_BAR),
        ),
        panel(
            WIDGET_ID_RESERVED_INSTANCE_COST,
            "Reserved Instance Cost",
            "Monthly amortized cost of the reservations you hold, by service.",
            monthly_cost(
                METRIC_AMORTIZED_COST,
                &[DIMENSION_SERVICE],
                Some(reserved()),
            ),
            graph_display(METRIC_AMORTIZED_COST, VISUAL_TYPE_STACK),
        ),
        panel(
            WIDGET_ID_RESERVATION_COST_PER_ACCOUNT,
            "Reservation Cost By Account",
            "Monthly amortized cost of your reservations by account.",
            monthly_cost(
                METRIC_AMORTIZED_COST,
                &[DIMENSION_LINKED_ACCOUNT],
                Some(reserved()),
            ),
            graph_display(METRIC_AMORTIZED_COST, VISUAL_TYPE_STACK),
        ),
        panel(
            WIDGET_ID_EC2_COVERAGE_PER_INSTANCE_TYPE,
            "EC2 Reservation Coverage By Instance Type",
            "Shows percentage of eligible EC2 usage covered by reservations, by instance type.",
            reservation_coverage(
                trailing_year(),
                &[DIMENSION_INSTANCE_TYPE],
                Some(dimension_filter(DIMENSION_SERVICE, &EC2_COMPUTE_SERVICE)),
            ),
            table_display(),
        ),
        panel(
            WIDGET_ID_COVERAGE_PER_ACCOUNT,
            "Reservation Coverage By Account",
            "Shows percentage of eligible usage covered by reservations, by account.",
            reservation_coverage(trailing_year(), &[DIMENSION_LINKED_ACCOUNT], None),
            table_display(),
        ),
        panel(
            WIDGET_ID_UTILIZATION_PER_ACCOUNT,
            "Reservation Utilization By Account",
            "Shows how much of your reservation commitment each account uses.",
            reservation_utilization(trailing_year(), &[DIMENSION_LINKED_ACCOUNT], None),
            table_display(),
        ),
        panel(
            WIDGET_ID_UTILIZATION_TREND,
            "Reservation Utilization Trend",
            "Shows how much of your reservation commitment is used each month.",
            reservation_utilization(trailing_year(), &[], None),
            table_display(),
        ),
        panel(
            WIDGET_ID_COVERAGE_TREND,
            "Reservation Coverage Trend",
            "Shows percentage of eligible usage covered by reservations each month.",
            reservation_coverage(trailing_year(), &[], None),
            table_display(),
        ),
        panel(
            WIDGET_ID_EC2_COST_PER_PURCHASE_TYPE,
            "EC2 Cost By Purchase Type",
            "Monthly amortized EC2 cost by purchase type.",
            monthly_cost(
                METRIC_AMORTIZED_COST,
                &[DIMENSION_PURCHASE_TYPE],
                Some(ec2()),
            ),
            graph_display(METRIC_AMORTIZED_COST, VISUAL_TYPE_STACK),
        ),
        panel(
            WIDGET_ID_EC2_COST_PER_REGION,
            "EC2 Cost By Region",
            "Monthly unblended EC2 cost by Region.",
            monthly_cost(METRIC_UNBLENDED_COST, &[DIMENSION_REGION], Some(ec2())),
            graph_display(METRIC_UNBLENDED_COST, VISUAL_TYPE_STACK),
        ),
        panel(
            WIDGET_ID_EC2_COST_PER_PLATFORM,
            "EC2 Cost By Platform",
            "Monthly unblended EC2 cost by operating platform.",
            monthly_cost(METRIC_UNBLENDED_COST, &[DIMENSION_PLATFORM], Some(ec2())),
            graph_display(METRIC_UNBLENDED_COST, VISUAL_TYPE_BAR),
        ),
        panel(
            WIDGET_ID_EC2_COST_PER_TENANCY,
            "EC2 Cost By Tenancy",
            "Monthly unblended EC2 cost by instance tenancy.",
            monthly_cost(METRIC_UNBLENDED_COST, &[DIMENSION_TENANCY], Some(ec2())),
            graph_display(METRIC_UNBLENDED_COST, VISUAL_TYPE_BAR),
        ),
        // The reservation panels this entry shares with the dashboards of the spend the
        // reservations cover.
        ec2_reservation_utilization(),
        ec2_reservation_coverage(),
        database_reservation_utilization(),
        database_reservation_coverage(),
    ]
}
