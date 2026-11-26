// SPDX-License-Identifier: CC0-1.0

//! How one panel of the AWS-managed catalog is built.
//!
//! Every entry's panel list is written in terms of these builders, so a panel is one row of
//! data — its published id, title, description, layout, query and display — rather than a
//! block of structure literals, and the modelled shapes are spelled out in exactly one
//! place.
//!
//! The vocabulary below is the model's: `Granularity`, `MetricName`, `VisualType`,
//! `Dimension` and `GroupDefinitionType` are enumerations, and `Widget` fixes a panel's
//! layout members and their ranges
//! (`smithy_query(query="shape",shape="Widget,QueryParameters,CostAndUsageQuery,Dimension")`).

use std::collections::HashMap;

use crate::ops::types::{
    BudgetQuery, CostAndUsageQuery, CostEfficiencyQuery, DateTimeRange, DateTimeValue,
    DimensionValues, DisplayConfig, Expression, GraphDisplayConfig, GroupDefinition,
    QueryParameters, ReservationCoverageQuery, ReservationUtilizationQuery,
    SavingsPlansCoverageQuery, SavingsPlansUtilizationQuery, TableDisplayConfigStruct, Widget,
    WidgetConfig,
};
use crate::state::DATE_TIME_TYPE_RELATIVE;

/// The two `width`s the catalog's panels are published at: a panel beside another, and one
/// across twice that span. Both sit inside `WidgetWidth`'s modelled 2..6 range.
pub(super) const PANEL_WIDTH_HALF: i32 = 2;
pub(super) const PANEL_WIDTH_WIDE: i32 = 4;

/// The `height` every catalog panel is published at, and the column it starts in — the
/// bottom of `WidgetHeight`'s modelled 4..10 range, and `Widget$horizontalOffset`'s
/// modelled `@default`.
pub(super) const PANEL_HEIGHT: i32 = 4;
pub(super) const PANEL_HORIZONTAL_OFFSET: i32 = 0;

/// The `Granularity` the catalog's panels aggregate at: a month, which is the period a bill
/// covers, or a day for a panel charting the latest month.
pub(super) const GRANULARITY_MONTHLY: &str = "MONTHLY";
pub(super) const GRANULARITY_DAILY: &str = "DAILY";

/// The `GroupDefinitionType` a grouping by a cost dimension carries — also the member's
/// modelled `@default`.
const GROUP_DEFINITION_TYPE_DIMENSION: &str = "DIMENSION";

/// The `MetricName`s the catalog's panels report, from the valid values each query member's
/// own `metrics` documentation states.
pub(super) const METRIC_UNBLENDED_COST: &str = "UnblendedCost";
pub(super) const METRIC_AMORTIZED_COST: &str = "AmortizedCost";
pub(super) const METRIC_USAGE_QUANTITY: &str = "UsageQuantity";
pub(super) const METRIC_SPEND_COVERED_BY_SAVINGS_PLANS: &str = "SpendCoveredBySavingsPlans";
pub(super) const METRIC_COST: &str = "Cost";
pub(super) const METRIC_HOUR: &str = "Hour";

/// The score a cost-efficiency panel draws. It is not a `MetricName`: a graph keys its
/// drawing instructions by a `GenericString` (`GraphDisplayConfigMap`), and this is the
/// series the recorded cost-efficiency panel draws.
pub(super) const SERIES_EFFICIENCY_SCORE: &str = "EfficiencyScore";

/// The `VisualType`s the catalog's graph panels draw with.
pub(super) const VISUAL_TYPE_LINE: &str = "LINE";
pub(super) const VISUAL_TYPE_BAR: &str = "BAR";
pub(super) const VISUAL_TYPE_STACK: &str = "STACK";

/// The `Dimension`s the catalog's panels group and filter by.
pub(super) const DIMENSION_SERVICE: &str = "SERVICE";
pub(super) const DIMENSION_LINKED_ACCOUNT: &str = "LINKED_ACCOUNT";
pub(super) const DIMENSION_REGION: &str = "REGION";
pub(super) const DIMENSION_PURCHASE_TYPE: &str = "PURCHASE_TYPE";
pub(super) const DIMENSION_DATABASE_ENGINE: &str = "DATABASE_ENGINE";
pub(super) const DIMENSION_INSTANCE_TYPE: &str = "INSTANCE_TYPE";
pub(super) const DIMENSION_USAGE_TYPE: &str = "USAGE_TYPE";
pub(super) const DIMENSION_DEPLOYMENT_OPTION: &str = "DEPLOYMENT_OPTION";
pub(super) const DIMENSION_PLATFORM: &str = "PLATFORM";
pub(super) const DIMENSION_TENANCY: &str = "TENANCY";
pub(super) const DIMENSION_SAVINGS_PLANS_TYPE: &str = "SAVINGS_PLANS_TYPE";
pub(super) const DIMENSION_AZ: &str = "AZ";

/// The year of bills a trend panel covers, as the relative bounds the catalog publishes:
/// the twelve months up to the current one.
pub(super) fn trailing_year() -> DateTimeRange {
    relative_range("-P12M", "P0M")
}

/// The quarter of bills the cost-efficiency panel covers.
pub(super) fn trailing_quarter() -> DateTimeRange {
    relative_range("-P3M", "P0M")
}

/// The latest month, for a panel charting the current bill day by day.
pub(super) fn latest_month() -> DateTimeRange {
    relative_range("-P1M", "P0M")
}

/// The trailing year EXTENDED into the forecast window: the range the recorded forecast
/// panel runs over, whose end bound is months ahead of the current one.
pub(super) fn trailing_year_with_forecast() -> DateTimeRange {
    relative_range("-P12M", "P4M")
}

/// A widget time range running from `start` to `end`, both relative offsets (ISO 8601
/// durations, as `DateTimeValue$value` carries them for a `RELATIVE` bound).
fn relative_range(start: &str, end: &str) -> DateTimeRange {
    DateTimeRange {
        start_time: DateTimeValue {
            r#type: DATE_TIME_TYPE_RELATIVE.to_string(),
            value: start.to_string(),
        },
        end_time: DateTimeValue {
            r#type: DATE_TIME_TYPE_RELATIVE.to_string(),
            value: end.to_string(),
        },
    }
}

/// A `DisplayConfig` drawing one series as `visual_type` (a `VisualType`: `LINE`, `BAR` or
/// `STACK`).
pub(super) fn graph_display(series: &str, visual_type: &str) -> DisplayConfig {
    DisplayConfig::Graph(HashMap::from([(
        series.to_string(),
        GraphDisplayConfig {
            visual_type: visual_type.to_string(),
        },
    )]))
}

/// A `DisplayConfig` drawing the query's result as a table.
pub(super) fn table_display() -> DisplayConfig {
    DisplayConfig::Table(TableDisplayConfigStruct {})
}

/// The `GroupDefinition`s grouping a query by each `Dimension` in `dimensions`.
///
/// A query that groups by nothing carries the EMPTY list rather than no `groupBy` at all,
/// which is how the catalog's ungrouped panels read back.
fn group_by_dimensions(dimensions: &[&str]) -> Vec<GroupDefinition> {
    dimensions
        .iter()
        .map(|dimension| GroupDefinition {
            key: dimension.to_string(),
            r#type: GROUP_DEFINITION_TYPE_DIMENSION.to_string(),
        })
        .collect()
}

/// An `Expression` narrowing a query to the `values` of one `Dimension` — how a panel scoped
/// to part of the bill (one family of services, one purchase type) states that scope.
pub(super) fn dimension_filter(dimension: &str, values: &[&str]) -> Expression {
    Expression {
        dimensions: Some(DimensionValues {
            key: dimension.to_string(),
            values: values.iter().map(|value| value.to_string()).collect(),
            match_options: None,
        }),
        ..Expression::default()
    }
}

/// A `costAndUsage` query of one metric at one granularity over `range`, grouped by the
/// dimensions in `group_by` and narrowed to `filter`.
pub(super) fn cost_and_usage(
    metric: &str,
    granularity: &str,
    range: DateTimeRange,
    group_by: &[&str],
    filter: Option<Expression>,
) -> QueryParameters {
    QueryParameters::costAndUsage(CostAndUsageQuery {
        metrics: vec![metric.to_string()],
        time_range: range,
        granularity: granularity.to_string(),
        group_by: Some(group_by_dimensions(group_by)),
        filter,
    })
}

/// A monthly `costAndUsage` query over the trailing year — the shape most of the catalog's
/// cost panels take, differing only in the metric they report, what they group by and the
/// part of the bill they narrow to.
pub(super) fn monthly_cost(
    metric: &str,
    group_by: &[&str],
    filter: Option<Expression>,
) -> QueryParameters {
    cost_and_usage(metric, GRANULARITY_MONTHLY, trailing_year(), group_by, filter)
}

/// A `budget` query of the budgets `filter` selects, or of every budget the account holds
/// when it selects none.
pub(super) fn budgets(filter: Option<Expression>) -> QueryParameters {
    QueryParameters::Budget(BudgetQuery { filter })
}

/// A monthly `costEfficiency` query over `range`, grouped by the dimensions in `group_by`.
pub(super) fn cost_efficiency(range: DateTimeRange, group_by: &[&str]) -> QueryParameters {
    QueryParameters::costEfficiency(CostEfficiencyQuery {
        time_range: range,
        granularity: Some(GRANULARITY_MONTHLY.to_string()),
        group_by: Some(group_by_dimensions(group_by)),
        filter: None,
    })
}

/// A monthly `savingsPlansCoverage` query over `range`, narrowed to `filter`.
pub(super) fn savings_plans_coverage(
    range: DateTimeRange,
    group_by: &[&str],
    filter: Option<Expression>,
) -> QueryParameters {
    QueryParameters::savingsPlansCoverage(SavingsPlansCoverageQuery {
        time_range: range,
        metrics: Some(vec![METRIC_SPEND_COVERED_BY_SAVINGS_PLANS.to_string()]),
        granularity: Some(GRANULARITY_MONTHLY.to_string()),
        group_by: Some(group_by_dimensions(group_by)),
        filter,
    })
}

/// A monthly `savingsPlansUtilization` query over `range`, narrowed to `filter`.
pub(super) fn savings_plans_utilization(
    range: DateTimeRange,
    filter: Option<Expression>,
) -> QueryParameters {
    QueryParameters::savingsPlansUtilization(SavingsPlansUtilizationQuery {
        time_range: range,
        granularity: Some(GRANULARITY_MONTHLY.to_string()),
        filter,
    })
}

/// A monthly `reservationUtilization` query over `range`, narrowed to `filter`.
pub(super) fn reservation_utilization(
    range: DateTimeRange,
    group_by: &[&str],
    filter: Option<Expression>,
) -> QueryParameters {
    QueryParameters::reservationUtilization(ReservationUtilizationQuery {
        time_range: range,
        group_by: Some(group_by_dimensions(group_by)),
        granularity: Some(GRANULARITY_MONTHLY.to_string()),
        filter,
    })
}

/// A monthly `reservationCoverage` query over `range`, narrowed to `filter`, reporting the
/// metrics the recorded coverage panels report.
pub(super) fn reservation_coverage(
    range: DateTimeRange,
    group_by: &[&str],
    filter: Option<Expression>,
) -> QueryParameters {
    QueryParameters::reservationCoverage(ReservationCoverageQuery {
        time_range: range,
        group_by: Some(group_by_dimensions(group_by)),
        granularity: Some(GRANULARITY_MONTHLY.to_string()),
        filter,
        metrics: Some(vec![METRIC_COST.to_string(), METRIC_HOUR.to_string()]),
    })
}

/// One published panel at the catalog's narrow width: the modelled [`Widget`] a reader
/// projects, carrying `query` drawn as `display`.
pub(super) fn panel(
    id: &str,
    title: &str,
    description: &str,
    query: QueryParameters,
    display: DisplayConfig,
) -> Widget {
    wide_panel(id, title, description, PANEL_WIDTH_HALF, query, display)
}

/// One published panel at `width` grid columns, for the panels the catalog publishes across
/// more than the narrow span.
pub(super) fn wide_panel(
    id: &str,
    title: &str,
    description: &str,
    width: i32,
    query: QueryParameters,
    display: DisplayConfig,
) -> Widget {
    Widget {
        id: Some(id.to_string()),
        title: title.to_string(),
        description: Some(description.to_string()),
        width,
        height: PANEL_HEIGHT,
        horizontal_offset: PANEL_HORIZONTAL_OFFSET,
        configs: vec![WidgetConfig {
            query_parameters: query,
            display_config: display,
        }],
    }
}
