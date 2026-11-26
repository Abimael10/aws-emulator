// SPDX-License-Identifier: CC0-1.0

//! The panels the catalog publishes on MORE THAN ONE dashboard.
//!
//! A panel's `WidgetId` is the identity of the panel itself, not of its place on a
//! dashboard: the recorded catalog publishes the same id on two entries wherever both show
//! the same reading — the EC2 reservation panels appear on the compute dashboard and on the
//! reservation one, the database reservation panels on the database dashboard and on the
//! reservation one, and the commitment-coverage panels on the dashboard of the spend they
//! cover and on the Savings Plans one. Each of those panels is therefore defined HERE once
//! and listed by both entries, so the two reads of it cannot drift apart.

use crate::ops::types::Widget;

use super::panels::{
    panel, reservation_coverage, reservation_utilization, savings_plans_coverage, table_display,
    trailing_year, DIMENSION_SERVICE,
};
use super::scopes::{
    COMPUTE_SERVICES, DATABASE_SERVICES, EC2_COMPUTE_SERVICE, EC2_SERVICES,
};
use super::panels::dimension_filter;

/// The published `WidgetId` of each panel this module defines, carried by every entry that
/// publishes it.
pub(super) const WIDGET_ID_EC2_RESERVATION_COVERAGE: &str = "c5465ef3f521aa1c1be28b07625cff5d";
pub(super) const WIDGET_ID_EC2_RESERVATION_UTILIZATION: &str = "083532331e8f5e5e1487aa7ae50c74f7";
pub(super) const WIDGET_ID_DATABASE_RESERVATION_COVERAGE: &str =
    "fb17570c6dda591b0e782b79fbb56440";
pub(super) const WIDGET_ID_DATABASE_RESERVATION_UTILIZATION: &str =
    "51f0badeb0ddc3d0e5a76fdc7baa53b8";
pub(super) const WIDGET_ID_COMPUTE_SAVINGS_PLANS_COVERAGE: &str =
    "434a31f03b942d9ce04a675c01c0a6f3";
pub(super) const WIDGET_ID_DATABASE_SAVINGS_PLANS_COVERAGE: &str =
    "4e40df2d0f9cb3ac7abb5d3688003be8";

/// The share of the eligible EC2 usage that reservations cover, as a table.
///
/// This panel's configuration is the recorded one: a `reservationCoverage` query reporting
/// cost and hours, monthly over the trailing year, narrowed to the EC2 compute charge, drawn
/// as a table.
pub(super) fn ec2_reservation_coverage() -> Widget {
    panel(
        WIDGET_ID_EC2_RESERVATION_COVERAGE,
        "EC2 Reservation Coverage",
        "Shows percentage of eligible EC2 usage covered by reservations.",
        reservation_coverage(
            trailing_year(),
            &[],
            Some(dimension_filter(DIMENSION_SERVICE, &EC2_COMPUTE_SERVICE)),
        ),
        table_display(),
    )
}

/// How far the EC2 reservations the account holds are used, as a table.
pub(super) fn ec2_reservation_utilization() -> Widget {
    panel(
        WIDGET_ID_EC2_RESERVATION_UTILIZATION,
        "EC2 Reservation Utilization",
        "Shows how much of your EC2 reservation commitment is used each month.",
        reservation_utilization(
            trailing_year(),
            &[],
            Some(dimension_filter(DIMENSION_SERVICE, &EC2_SERVICES)),
        ),
        table_display(),
    )
}

/// The share of the eligible database usage that reservations cover, as a table.
pub(super) fn database_reservation_coverage() -> Widget {
    panel(
        WIDGET_ID_DATABASE_RESERVATION_COVERAGE,
        "Database Reservation Coverage",
        "Shows percentage of eligible database usage covered by reservations.",
        reservation_coverage(
            trailing_year(),
            &[],
            Some(dimension_filter(DIMENSION_SERVICE, &DATABASE_SERVICES)),
        ),
        table_display(),
    )
}

/// How far the database reservations the account holds are used, as a table.
pub(super) fn database_reservation_utilization() -> Widget {
    panel(
        WIDGET_ID_DATABASE_RESERVATION_UTILIZATION,
        "Database Reservation Utilization",
        "Shows how much of your database reservation commitment is used each month.",
        reservation_utilization(
            trailing_year(),
            &[],
            Some(dimension_filter(DIMENSION_SERVICE, &DATABASE_SERVICES)),
        ),
        table_display(),
    )
}

/// The share of the eligible compute spend that Savings Plans cover, as a table.
pub(super) fn compute_savings_plans_coverage() -> Widget {
    panel(
        WIDGET_ID_COMPUTE_SAVINGS_PLANS_COVERAGE,
        "Compute Savings Plans Coverage",
        "Shows percentage of eligible compute spend covered by Savings Plans.",
        savings_plans_coverage(
            trailing_year(),
            &[],
            Some(dimension_filter(DIMENSION_SERVICE, &COMPUTE_SERVICES)),
        ),
        table_display(),
    )
}

/// The share of the eligible database spend that Savings Plans cover, as a table.
pub(super) fn database_savings_plans_coverage() -> Widget {
    panel(
        WIDGET_ID_DATABASE_SAVINGS_PLANS_COVERAGE,
        "Database Savings Plans Coverage",
        "Shows percentage of eligible database spend covered by Savings Plans.",
        savings_plans_coverage(
            trailing_year(),
            &[],
            Some(dimension_filter(DIMENSION_SERVICE, &DATABASE_SERVICES)),
        ),
        table_display(),
    )
}
