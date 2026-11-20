// SPDX-License-Identifier: CC0-1.0

//! CloudWatch Metrics Insights: the `SELECT` query a `MetricDataQuery$Expression` may carry,
//! evaluated over the series this store holds.
//!
//! "This field can contain either a Metrics Insights query, or a metric math expression."
//! (`smithy_query(query="doc",shape="MetricDataQuery",member="Expression"`) — a Metrics Insights
//! query is a statement in a SQL dialect rather than an expression of the metric math language
//! ([`super::metric_math`]), so it is read with the maintained SQL grammar this crate already
//! reads one with (`sqlparser`) and evaluated here:
//!
//! ```text
//! SELECT FUNCTION(metricName)
//!   FROM namespace | SCHEMA(namespace[, labelKey, ...])
//!   [ WHERE labelKey operator 'labelValue' [ AND | OR ... ] ]
//!   [ GROUP BY labelKey [, ...] ]
//!   [ ORDER BY FUNCTION() [ ASC | DESC ] ]
//!   [ LIMIT number ]
//! ```
//!
//! What each clause selects is the metric store's own identity of a series — "a metric is
//! uniquely defined by a name, a namespace, and zero or more dimensions" — so the `FROM` names
//! the NAMESPACE (and, as `SCHEMA(...)`, the exact dimension keys a series must carry), the
//! `SELECT`'s argument names the METRIC, and the keys a `WHERE` filters and a `GROUP BY` groups
//! by are DIMENSION names.
//!
//! The result is a time series per group: the query's function folded over every series of the
//! group, window by window, at the granularity the query is read at. A query with no `GROUP BY`
//! returns ONE series over everything it selected, which is why only a grouped query is graded
//! as a vector of series ([`super::alarm_evaluation::grades_a_vector_of_series`]), and the groups
//! come back in the order the statement's own `ORDER BY` ranks them — the ranking a caller asked
//! the service to make, rather than one the reader of the result makes for itself.
//!
//! Nothing here mutates: every value is folded out of the datapoints `PutMetricData` wrote,
//! through the same window fold every other reader of a series uses ([`super::windows_of`]), so a
//! datapoint is in the same window for this reader as for `GetMetricData` and the alarm
//! evaluation.

use std::collections::{BTreeMap, BTreeSet};

use ::sqlparser::ast::{
    BinaryOperator, Expr, FunctionArg, FunctionArgExpr, FunctionArguments, GroupByExpr, LimitClause,
    OrderByKind, Select, SelectItem, SetExpr, Statement, TableFactor, Value, ValueWithSpan,
};

use super::metric_math::{MathSeries, SeriesMetric};
use super::{BasicStatistics, ComAmazonawsCloudwatchGraniteserviceversion20100801State as State, MetricKey};

/// The keyword a Metrics Insights query opens with, which is what tells one from a metric math
/// expression ([`super::metric_math::is_metrics_insights_query`]).
const SELECT_KEYWORD: &str = "SELECT";

/// How many series ONE query selects at most: "A GetMetricData operation that does not include a
/// query can retrieve as many as 500 different metrics in a single request." (`GetMetricData`) —
/// the same ceiling the server-side search of this service answers under, so neither discovers
/// more of the store than one call may read.
const MAX_SELECTED_SERIES: usize = 500;

/// How many groups one query's result carries at most, for a query that states no `LIMIT`: one
/// group per selected series is the most a grouping can produce, so the series ceiling bounds
/// them too.
const MAX_GROUPS: usize = MAX_SELECTED_SERIES;

/// How many steps one query's `WHERE` clause may carry: its label predicates and the
/// connectives that combine them ([`Filter`]).
///
/// The clause comes from the request, so its size is the caller's choice and is bounded before
/// anything is built: a clause past this is refused the way every other statement this parser
/// cannot read is. Generous beside the predicates a query of this language states, and far below
/// anything whose one pass costs a reader more than the series it then selects over.
const MAX_PREDICATE_STEPS: usize = 256;

/// Whether `expression` is a Metrics Insights query rather than a metric math expression.
#[must_use]
pub fn opens_a_query(expression: &str) -> bool {
    expression
        .trim_start()
        .get(..SELECT_KEYWORD.len())
        .is_some_and(|start| start.eq_ignore_ascii_case(SELECT_KEYWORD))
}

/// The function a query folds each group by.
///
/// The aggregates Metrics Insights states over a metric: each one is an aggregate of the
/// observations a window holds, which is why every one of them is computed from the four
/// aggregates a datapoint carries ([`BasicStatistics`]) rather than from the individual values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Aggregate {
    /// `AVG(metric)`: the mean of the observations.
    Avg,
    /// `COUNT(metric)`: how many observations there were.
    Count,
    /// `MAX(metric)`: the largest of them.
    Max,
    /// `MIN(metric)`: the smallest of them.
    Min,
    /// `SUM(metric)`: their sum.
    Sum,
}

impl Aggregate {
    /// The aggregate `name` spells, or `None` for a name that is none of them.
    fn of(name: &str) -> Option<Self> {
        match name.to_ascii_uppercase().as_str() {
            "AVG" => Some(Self::Avg),
            "COUNT" => Some(Self::Count),
            "MAX" => Some(Self::Max),
            "MIN" => Some(Self::Min),
            "SUM" => Some(Self::Sum),
            _ => None,
        }
    }

    /// The value this aggregate takes over one window's aggregates, or `None` over a window that
    /// holds no observation at all — which is reported as no datapoint rather than as a zero.
    fn over(self, folded: BasicStatistics) -> Option<f64> {
        if folded.sample_count <= 0.0 {
            return None;
        }
        let value = match self {
            Self::Avg => folded.sum / folded.sample_count,
            Self::Count => folded.sample_count,
            Self::Max => folded.maximum,
            Self::Min => folded.minimum,
            Self::Sum => folded.sum,
        };
        value.is_finite().then_some(value)
    }

    /// The name of the CloudWatch statistic this aggregate folds a series by, which is the
    /// statistic the series a query returns was folded under
    /// ([`super::is_cloudwatch_statistic`] spells them).
    fn statistic(self) -> &'static str {
        match self {
            Self::Avg => "Average",
            Self::Count => "SampleCount",
            Self::Max => "Maximum",
            Self::Min => "Minimum",
            Self::Sum => "Sum",
        }
    }
}

/// One predicate a `WHERE` clause states about ONE label of a series.
#[derive(Debug, Clone)]
enum Term {
    /// `labelKey = 'value'` / `labelKey != 'value'`: the series carries that dimension with that
    /// value, or carries anything else.
    Compares {
        /// The dimension name the predicate reads.
        key: String,
        /// The value it compares against.
        value: String,
        /// Whether the predicate holds when the values are EQUAL.
        equal: bool,
    },
    /// `labelKey LIKE 'pat%'` / `NOT LIKE`: the dimension's value matches the pattern, in which
    /// `%` stands for any run of characters.
    Matches {
        /// The dimension name the predicate reads.
        key: String,
        /// The pattern it matches against.
        pattern: String,
        /// Whether the predicate holds when the pattern MATCHES.
        matching: bool,
    },
    /// `labelKey IN ('a', 'b')` / `NOT IN`: the dimension's value is one of a set, or is none of
    /// them.
    Member {
        /// The dimension name the predicate reads.
        key: String,
        /// The values of the set.
        values: Vec<String>,
        /// Whether the predicate holds when the value IS one of them.
        inside: bool,
    },
}

impl Term {
    /// Whether the series `key` satisfies this predicate.
    ///
    /// A dimension the series does not carry satisfies NO predicate about it: the predicate is
    /// about a label of the series, and a series without that label is not one the query selected
    /// — so `!=` over an absent dimension is false as well, the way a `WHERE` over a missing
    /// label is.
    fn holds(&self, key: &MetricKey) -> bool {
        match self {
            Self::Compares { key: label, value, equal } => key
                .dimensions
                .get(label)
                .is_some_and(|held| (held == value) == *equal),
            Self::Matches { key: label, pattern, matching } => key
                .dimensions
                .get(label)
                .is_some_and(|held| matches_pattern(held, pattern) == *matching),
            Self::Member { key: label, values, inside } => key
                .dimensions
                .get(label)
                .is_some_and(|held| values.iter().any(|value| value == held) == *inside),
        }
    }
}

/// One step of a flattened `WHERE` clause: a predicate about one label, or the connective that
/// combines the two results before it.
#[derive(Debug, Clone)]
enum Step {
    /// A predicate about one label.
    Term(Term),
    /// `AND`: both of the two results before it hold.
    Both,
    /// `OR`: either of them holds.
    Either,
}

/// How a `WHERE` clause selects the series a query reads, FLATTENED: the predicates it states
/// and the connectives that combine them, each connective after the two results it combines.
///
/// Flat rather than a tree of boxes, because the clause comes from the request: a chain of `AND`s
/// as long as the expression the caller sent would make a tree as DEEP as that chain, and a walk
/// of such a tree — reading it, evaluating it, or dropping it — would descend once per link. In
/// this form every one of those is one pass over a `Vec` with a stack of booleans beside it, and
/// how long that `Vec` may be is bounded before it is built ([`MAX_PREDICATE_STEPS`]).
#[derive(Debug, Clone, Default)]
struct Filter {
    /// The clause's steps, each connective after the two results it combines.
    steps: Vec<Step>,
}

impl Filter {
    /// Whether the series `key` satisfies this clause.
    ///
    /// One pass over the steps, with the result of each predicate on a stack: a connective takes
    /// the two results before it and leaves one. A clause that states nothing selects every
    /// series, which is what a query carrying no `WHERE` at all asks for.
    fn holds(&self, key: &MetricKey) -> bool {
        let mut held: Vec<bool> = Vec::with_capacity(self.steps.len());
        for step in &self.steps {
            match step {
                Step::Term(term) => held.push(term.holds(key)),
                Step::Both | Step::Either => {
                    // Both operands are there by construction: a connective is only ever
                    // appended after the two results it combines.
                    let right = held.pop().unwrap_or(false);
                    let left = held.pop().unwrap_or(false);
                    held.push(match step {
                        Step::Either => left || right,
                        _ => left && right,
                    });
                }
            }
        }
        held.pop().unwrap_or(true)
    }
}

/// Whether `value` matches the `LIKE` pattern `pattern`, in which `%` stands for any run of
/// characters (including none) and every other character stands for itself.
///
/// The match is case-sensitive, as a comparison of a dimension VALUE is everywhere else in this
/// service: a dimension's value is part of the series' identity.
fn matches_pattern(value: &str, pattern: &str) -> bool {
    let mut segments = pattern.split('%');
    let Some(first) = segments.next() else {
        return pattern == value;
    };
    let Some(mut rest) = value.strip_prefix(first) else {
        return false;
    };
    let mut last: Option<&str> = None;
    for segment in segments {
        // The pattern's last segment has to sit at the END of the value, so it is held back
        // until the pattern is exhausted.
        if let Some(previous) = last.take() {
            match rest.find(previous) {
                Some(at) => rest = rest.get(at.saturating_add(previous.len())..).unwrap_or(""),
                None => return false,
            }
        }
        last = Some(segment);
    }
    match last {
        Some(tail) => rest.len() >= tail.len() && rest.ends_with(tail),
        // A pattern of one segment carries no `%` at all, so the whole value had to be it.
        None => rest.is_empty(),
    }
}

/// How a statement's `ORDER BY` ranks the groups of its result.
#[derive(Debug, Clone, Copy)]
struct Ranking {
    /// The function the ranking is made on, over each group's whole range.
    by: Aggregate,
    /// Whether the ranking runs from the smallest value up — SQL's own default for a clause
    /// that states no direction.
    ascending: bool,
}

/// One Metrics Insights query, as the statement states it.
#[derive(Debug, Clone)]
pub struct Query {
    /// The function each group is folded by.
    aggregate: Aggregate,
    /// The namespace the series it selects belong to.
    namespace: String,
    /// The metric those series carry.
    metric_name: String,
    /// The dimension keys a `SCHEMA(...)` requires a series to carry EXACTLY, or `None` for a
    /// `FROM` that names the namespace alone and so selects every series of it.
    schema: Option<BTreeSet<String>>,
    /// The predicate a `WHERE` selects those series by, when the statement carries one.
    filter: Option<Filter>,
    /// The dimension keys the result is grouped by, in the order the clause names them; empty
    /// for a statement that groups nothing.
    group_by: Vec<String>,
    /// How an `ORDER BY` ranks the groups, when the statement carries one.
    ranking: Option<Ranking>,
    /// How many groups a `LIMIT` keeps, when the statement carries one.
    limit: Option<usize>,
}

/// One group of a query's result: the labels that identify it, and the series it folded.
#[derive(Debug, Clone)]
pub struct Group {
    /// The dimension keys the query grouped by and this group's values for them — what
    /// identifies this group among the result's entries.
    pub labels: BTreeMap<String, String>,
    /// The series the group's function folded, window by window.
    pub series: MathSeries,
}

/// What evaluating one query over the store cost, and what it answered.
#[derive(Debug, Clone, Default)]
pub struct QueryResult {
    /// The groups of the result, in the order the statement's `ORDER BY` ranks them.
    pub groups: Vec<Group>,
    /// How many stored data points the fold visited, which its caller charges to whatever
    /// budget pays for the read.
    pub datapoints: usize,
}

impl Query {
    /// Whether this query's result is a VECTOR of series rather than one: a statement carrying a
    /// `GROUP BY`, which returns one series per group of the values it groups by.
    #[must_use]
    pub fn groups_its_result(&self) -> bool {
        !self.group_by.is_empty()
    }

    /// Whether the series `key` is one this query selects: its namespace, its metric name, the
    /// dimension keys a `SCHEMA(...)` requires, and the predicate a `WHERE` states.
    #[must_use]
    pub fn selects(&self, key: &MetricKey) -> bool {
        if key.namespace != self.namespace || key.metric_name != self.metric_name {
            return false;
        }
        if let Some(schema) = self.schema.as_ref() {
            let carried: BTreeSet<&String> = key.dimensions.keys().collect();
            if carried.len() != schema.len()
                || !schema.iter().all(|wanted| carried.contains(wanted))
            {
                return false;
            }
        }
        self.filter.as_ref().is_none_or(|filter| filter.holds(key))
    }

    /// This query's result over `[start, end)`, at the granularity `period_secs`.
    ///
    /// The series it selects are folded into the windows of that range by the SHARED window fold
    /// ([`super::windows_of`]), so a datapoint lands in the same window here as it does for a
    /// read of its own metric; the aggregates of each window are then combined ACROSS the series
    /// of a group, which is what one series per group means.
    #[must_use]
    pub fn evaluate(
        &self,
        store: &State,
        start: f64,
        end: f64,
        period_secs: i32,
        now: f64,
    ) -> QueryResult {
        let mut answered = QueryResult::default();
        // The granularity a RANGE can be read at is the one its start time's retention tier
        // still holds, the same precondition every other reader of a window applies.
        if !super::granularity_available_from(start, period_secs, now) {
            return answered;
        }
        let rounded = super::rounded_start(start, period_secs, now);
        let period = f64::from(period_secs.max(1));

        // The series this query selects, in a stable order — the store is a hash map, so the
        // keys are sorted before anything is folded, and a query cannot read more of the store
        // than one call may ([`MAX_SELECTED_SERIES`]).
        let mut selected: Vec<&MetricKey> = store
            .metrics
            .keys()
            .filter(|key| self.selects(key))
            .collect();
        selected.sort();
        selected.truncate(MAX_SELECTED_SERIES);

        // One accumulator per group: the windows it holds, and the whole range's aggregates a
        // ranking is made on.
        let mut grouped: BTreeMap<Vec<String>, Grouped> = BTreeMap::new();
        for key in selected {
            let Some(labels) = self.labels_of(key) else {
                continue;
            };
            let stored = store.metrics.get(key).map_or(&[][..], Vec::as_slice);
            answered.datapoints = answered.datapoints.saturating_add(stored.len());
            // A query names no unit, so the fold resolves the series' own unit exactly as every
            // other reader of one does.
            let windows = super::windows_of(stored, rounded, end, period_secs, None, now);
            let values: Vec<String> = labels.values().cloned().collect();
            if grouped.len() >= MAX_GROUPS && !grouped.contains_key(&values) {
                continue;
            }
            let group = grouped.entry(values).or_insert_with(|| Grouped::of(labels));
            group.folded(key);
            for (index, data) in windows {
                let folded = BasicStatistics::of(data);
                group.add(index, folded);
            }
        }

        let mut groups: Vec<(Vec<String>, Grouped)> = grouped.into_iter().collect();
        self.rank(&mut groups);
        answered.groups = groups
            .into_iter()
            .map(|(_, group)| group.into_group(self, rounded, period))
            .take(self.limit.unwrap_or(MAX_GROUPS))
            .collect();
        answered
    }

    /// The labels that identify the group the series `key` belongs to, or `None` for a series
    /// that carries no value for one of the keys the query groups by — such a series is in no
    /// group of the result at all.
    ///
    /// A query that groups nothing puts every series it selected in ONE group, which carries no
    /// labels: its result is a single series over all of them.
    fn labels_of(&self, key: &MetricKey) -> Option<BTreeMap<String, String>> {
        let mut labels = BTreeMap::new();
        for grouped in &self.group_by {
            let value = key.dimensions.get(grouped)?;
            labels.insert(grouped.clone(), value.clone());
        }
        Some(labels)
    }

    /// Order the groups the way the statement asks for: by the value its `ORDER BY` function
    /// takes over each group's whole range, and by the group's own labels where the statement
    /// ranks nothing or two groups rank alike — so one query's result is in one order however
    /// the store happened to hold the series.
    fn rank(&self, groups: &mut [(Vec<String>, Grouped)]) {
        let Some(ranking) = self.ranking else {
            return;
        };
        groups.sort_by(|(left_values, left), (right_values, right)| {
            let of = |group: &Grouped| ranking.by.over(group.range).unwrap_or(f64::NAN);
            let ordered = of(left).total_cmp(&of(right));
            let ordered = if ranking.ascending {
                ordered
            } else {
                ordered.reverse()
            };
            ordered.then_with(|| left_values.cmp(right_values))
        });
    }
}

/// What one group of a result has accumulated as the fold runs.
#[derive(Debug, Clone)]
struct Grouped {
    /// The labels that identify the group.
    labels: BTreeMap<String, String>,
    /// The aggregates of each window of the range, by the window's index from its start.
    windows: BTreeMap<i64, BasicStatistics>,
    /// The aggregates of the WHOLE range, which a ranking is made on.
    range: BasicStatistics,
    /// How many of the store's series this group folded, so a group that is exactly one series
    /// can still be reported as that metric's own.
    series: usize,
    /// The identity of that one series, while the group holds only it.
    only: Option<MetricKey>,
}

impl Grouped {
    /// The accumulator of the group `labels` identify.
    fn of(labels: BTreeMap<String, String>) -> Self {
        Self {
            labels,
            windows: BTreeMap::new(),
            range: BasicStatistics::default(),
            series: 0,
            only: None,
        }
    }

    /// Fold one window's aggregates into the group.
    fn add(&mut self, index: i64, folded: BasicStatistics) {
        let window = self.windows.entry(index).or_default();
        window.sample_count += folded.sample_count;
        window.sum += folded.sum;
        window.minimum = window.minimum.min(folded.minimum);
        window.maximum = window.maximum.max(folded.maximum);
        self.range.sample_count += folded.sample_count;
        self.range.sum += folded.sum;
        self.range.minimum = self.range.minimum.min(folded.minimum);
        self.range.maximum = self.range.maximum.max(folded.maximum);
    }

    /// Remember which series the group has folded, so a group of exactly one is reported as that
    /// metric's own series.
    fn folded(&mut self, key: &MetricKey) {
        self.series = self.series.saturating_add(1);
        self.only = match self.series {
            1 => Some(key.clone()),
            _ => None,
        };
    }

    /// The group as the result reports it: its labels, and the series the query's function folded
    /// over its windows.
    fn into_group(self, query: &Query, start: f64, period: f64) -> Group {
        let mut points: Vec<(f64, f64)> = Vec::with_capacity(self.windows.len());
        for (index, folded) in &self.windows {
            // A window that holds no observation reports no datapoint, so the points of a
            // series are the periods the data covers.
            if let Some(value) = query.aggregate.over(*folded) {
                points.push((super::window_start(start, *index, period), value));
            }
        }
        let series = MathSeries::new(label_of(&query.metric_name, &self.labels), points);
        // A group that folded exactly ONE stored series still IS that metric's series, folded by
        // the statistic the query's function names — so what is keyed on a series' identity (an
        // anomaly detection model) finds it. A group that folded several is a computed series and
        // is no metric's.
        let series = match self.only {
            Some(key) => series.of_metric(SeriesMetric {
                namespace: key.namespace,
                metric_name: key.metric_name,
                dimensions: key.dimensions,
                stat: query.aggregate.statistic().to_string(),
            }),
            _ => series,
        };
        Group {
            labels: self.labels,
            series,
        }
    }
}

/// The label one group of a result is reported under: the metric the query selected, followed by
/// the values of the labels that identify the group — what tells two series of one metric apart,
/// the same way a series this service discovered server-side is labelled.
fn label_of(metric_name: &str, labels: &BTreeMap<String, String>) -> String {
    let mut label = metric_name.to_string();
    for value in labels.values() {
        label.push(' ');
        label.push_str(value);
    }
    label
}

/// Why `expression` is not a Metrics Insights query this service evaluates.
#[derive(Debug, Clone)]
pub struct QueryError {
    /// What is wrong with the query, in the words the refusal reports.
    pub message: String,
}

impl QueryError {
    /// An error carrying `message`.
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// Read `expression` as a Metrics Insights query, or say why it is not one this service
/// evaluates.
///
/// The statement is read by the maintained SQL grammar, so the clauses are recognised only where
/// the language puts a clause: a metric name, a dimension value or a string literal carrying the
/// words of one is part of a projection or a predicate.
pub fn parse(expression: &str) -> ::std::result::Result<Query, QueryError> {
    let statements = ::sqlparser::parser::Parser::parse_sql(
        &::sqlparser::dialect::GenericDialect {},
        expression,
    )
    .map_err(|error| QueryError::new(format!("Invalid query: {error}")))?;
    let [statement] = statements.as_slice() else {
        return Err(QueryError::new(
            "a Metrics Insights query is one SELECT statement",
        ));
    };
    let Statement::Query(query) = statement else {
        return Err(QueryError::new(
            "a Metrics Insights query is a SELECT statement",
        ));
    };
    let SetExpr::Select(select) = query.body.as_ref() else {
        return Err(QueryError::new(
            "a Metrics Insights query is a SELECT statement",
        ));
    };
    let aggregate = projected_aggregate(select)?;
    let (namespace, schema) = selected_source(select)?;
    let filter = select
        .selection
        .as_ref()
        .map(read_filter)
        .transpose()?;
    let group_by = grouped_keys(select)?;
    let ranking = read_ranking(query.order_by.as_ref())?;
    let limit = read_limit(query.limit_clause.as_ref())?;
    Ok(Query {
        aggregate: aggregate.0,
        metric_name: aggregate.1,
        namespace,
        schema,
        filter,
        group_by,
        ranking,
        limit,
    })
}

/// The function and the metric one statement's projection states: `FUNCTION(metricName)`, the
/// only projection a Metrics Insights query carries.
fn projected_aggregate(select: &Select) -> ::std::result::Result<(Aggregate, String), QueryError> {
    let [item] = select.projection.as_slice() else {
        return Err(QueryError::new(
            "a Metrics Insights query selects one aggregate of one metric",
        ));
    };
    let expression = match item {
        SelectItem::UnnamedExpr(expression) => expression,
        SelectItem::ExprWithAlias { expr, .. } => expr,
        SelectItem::QualifiedWildcard(..) | SelectItem::Wildcard(_) => {
            return Err(QueryError::new(
                "a Metrics Insights query selects one aggregate of one metric",
            ))
        }
    };
    let Expr::Function(function) = expression else {
        return Err(QueryError::new(
            "a Metrics Insights query selects one aggregate of one metric",
        ));
    };
    let named = function
        .name
        .0
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<String>>()
        .join(".");
    let Some(aggregate) = Aggregate::of(trimmed_name(&named).as_str()) else {
        return Err(QueryError::new(format!(
            "{named} is not a function a Metrics Insights query selects"
        )));
    };
    let FunctionArguments::List(arguments) = &function.args else {
        return Err(QueryError::new(format!(
            "{named} takes the metric to aggregate"
        )));
    };
    let [FunctionArg::Unnamed(FunctionArgExpr::Expr(argument))] = arguments.args.as_slice() else {
        return Err(QueryError::new(format!(
            "{named} takes the metric to aggregate"
        )));
    };
    let Some(metric_name) = named_label(argument) else {
        return Err(QueryError::new(format!(
            "{named} takes the metric to aggregate"
        )));
    };
    Ok((aggregate, metric_name))
}

/// The namespace a statement's `FROM` names, and the dimension keys a `SCHEMA(...)` requires the
/// series to carry exactly.
fn selected_source(
    select: &Select,
) -> ::std::result::Result<(String, Option<BTreeSet<String>>), QueryError> {
    let [table] = select.from.as_slice() else {
        return Err(QueryError::new(
            "a Metrics Insights query reads one namespace",
        ));
    };
    if !table.joins.is_empty() {
        return Err(QueryError::new(
            "a Metrics Insights query reads one namespace",
        ));
    }
    let TableFactor::Table { name, args, .. } = &table.relation else {
        return Err(QueryError::new(
            "a Metrics Insights query reads one namespace",
        ));
    };
    let named = name
        .0
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<String>>()
        .join(".");
    let named = trimmed_name(&named);
    let Some(arguments) = args.as_ref() else {
        // `FROM namespace` selects every series of the namespace, whatever dimensions it
        // carries.
        return Ok((named, None));
    };
    if !named.eq_ignore_ascii_case("SCHEMA") {
        return Err(QueryError::new(format!(
            "{named} is not a function a Metrics Insights query reads from"
        )));
    }
    let mut namespace: Option<String> = None;
    let mut keys: BTreeSet<String> = BTreeSet::new();
    for argument in &arguments.args {
        let FunctionArg::Unnamed(FunctionArgExpr::Expr(expression)) = argument else {
            return Err(QueryError::new(
                "SCHEMA takes a namespace and the dimension keys of a series",
            ));
        };
        let Some(value) = named_label(expression) else {
            return Err(QueryError::new(
                "SCHEMA takes a namespace and the dimension keys of a series",
            ));
        };
        match namespace {
            None => namespace = Some(value),
            Some(_) => {
                keys.insert(value);
            }
        }
    }
    let Some(namespace) = namespace else {
        return Err(QueryError::new(
            "SCHEMA takes a namespace and the dimension keys of a series",
        ));
    };
    Ok((namespace, Some(keys)))
}

/// The dimension keys a statement's `GROUP BY` groups its result by, in the order the clause
/// names them.
fn grouped_keys(select: &Select) -> ::std::result::Result<Vec<String>, QueryError> {
    let expressions = match &select.group_by {
        GroupByExpr::Expressions(expressions, modifiers) if modifiers.is_empty() => expressions,
        GroupByExpr::Expressions(..) | GroupByExpr::All(..) => {
            return Err(QueryError::new(
                "a Metrics Insights query groups by the labels of a series",
            ))
        }
    };
    let mut keys = Vec::with_capacity(expressions.len());
    for expression in expressions {
        let Some(key) = named_label(expression) else {
            return Err(QueryError::new(
                "a Metrics Insights query groups by the labels of a series",
            ));
        };
        keys.push(key);
    }
    Ok(keys)
}

/// How a statement's `ORDER BY` ranks the groups of its result.
fn read_ranking(
    order_by: Option<&::sqlparser::ast::OrderBy>,
) -> ::std::result::Result<Option<Ranking>, QueryError> {
    let Some(order_by) = order_by else {
        return Ok(None);
    };
    let OrderByKind::Expressions(expressions) = &order_by.kind else {
        return Err(QueryError::new(
            "a Metrics Insights query orders by one function of the series it selects",
        ));
    };
    let [ordered] = expressions.as_slice() else {
        return Err(QueryError::new(
            "a Metrics Insights query orders by one function of the series it selects",
        ));
    };
    let Expr::Function(function) = &ordered.expr else {
        return Err(QueryError::new(
            "a Metrics Insights query orders by one function of the series it selects",
        ));
    };
    let named = function
        .name
        .0
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<String>>()
        .join(".");
    let Some(by) = Aggregate::of(trimmed_name(&named).as_str()) else {
        return Err(QueryError::new(format!(
            "{named} is not a function a Metrics Insights query orders by"
        )));
    };
    Ok(Some(Ranking {
        by,
        // A clause that states no direction ranks the way this SQL dialect's own default does:
        // ascending.
        ascending: ordered.options.asc.unwrap_or(true),
    }))
}

/// How many groups a statement's `LIMIT` keeps.
fn read_limit(
    limit: Option<&LimitClause>,
) -> ::std::result::Result<Option<usize>, QueryError> {
    let Some(clause) = limit else {
        return Ok(None);
    };
    let LimitClause::LimitOffset { limit, offset, limit_by } = clause else {
        return Err(QueryError::new(
            "a Metrics Insights query limits its result to a number of groups",
        ));
    };
    if offset.is_some() || !limit_by.is_empty() {
        return Err(QueryError::new(
            "a Metrics Insights query limits its result to a number of groups",
        ));
    }
    let Some(limit) = limit.as_ref() else {
        return Ok(None);
    };
    let Expr::Value(ValueWithSpan { value: Value::Number(number, _), .. }) = limit else {
        return Err(QueryError::new(
            "a Metrics Insights query limits its result to a number of groups",
        ));
    };
    let kept = number.parse::<usize>().map_err(|_| {
        QueryError::new("a Metrics Insights query limits its result to a number of groups")
    })?;
    Ok(Some(kept.min(MAX_GROUPS)))
}

/// Read a `WHERE` clause as the flattened selection it makes over the labels of a series.
///
/// The walk keeps its own stack ON THE HEAP rather than recursing: how long a chain of `AND`s or
/// `OR`s the clause carries is the CALLER's choice, so the clause's size is bounded explicitly
/// ([`MAX_PREDICATE_STEPS`]) and one past that is refused instead of growing this thread's stack
/// — the way every other statement this parser cannot read is refused.
fn read_filter(expression: &Expr) -> ::std::result::Result<Filter, QueryError> {
    /// What the walk still has to do: read one node of the clause, or append one connective
    /// after the two results it combines.
    enum Pending<'clause> {
        /// A node of the parsed clause.
        Node(&'clause Expr),
        /// A connective, appended once both its operands have been read.
        Connective(Step),
    }

    let mut filter = Filter::default();
    let mut open: Vec<Pending<'_>> = vec![Pending::Node(expression)];
    while let Some(next) = open.pop() {
        // What the clause may cost is checked as the walk runs, over everything it has read and
        // everything it still has to read, so neither half can grow past the ceiling.
        if filter.steps.len().saturating_add(open.len()) >= MAX_PREDICATE_STEPS {
            return Err(QueryError::new(format!(
                "a Metrics Insights query selects by as many as {MAX_PREDICATE_STEPS} label \
                 predicates"
            )));
        }
        let node = match next {
            Pending::Connective(step) => {
                filter.steps.push(step);
                continue;
            }
            Pending::Node(node) => node,
        };
        match node {
            Expr::Nested(inner) => open.push(Pending::Node(inner)),
            Expr::BinaryOp {
                left,
                op: op @ (BinaryOperator::And | BinaryOperator::Or),
                right,
            } => {
                // Each operand is read before the connective that combines them, so the steps
                // come out with every connective after its own two results.
                open.push(Pending::Connective(match op {
                    BinaryOperator::Or => Step::Either,
                    _ => Step::Both,
                }));
                open.push(Pending::Node(right));
                open.push(Pending::Node(left));
            }
            other => filter.steps.push(Step::Term(read_term(other)?)),
        }
    }
    Ok(filter)
}

/// Read one predicate of a `WHERE` clause as the selection it makes over ONE label of a series.
fn read_term(expression: &Expr) -> ::std::result::Result<Term, QueryError> {
    match expression {
        Expr::BinaryOp { left, op, right } => match op {
            BinaryOperator::Eq | BinaryOperator::NotEq => Ok(Term::Compares {
                key: label_operand(left)?,
                value: value_operand(right)?,
                equal: matches!(op, BinaryOperator::Eq),
            }),
            other => Err(QueryError::new(format!(
                "{other} is not an operator a Metrics Insights query selects by"
            ))),
        },
        Expr::Like { negated, expr, pattern, .. } => Ok(Term::Matches {
            key: label_operand(expr)?,
            pattern: value_operand(pattern)?,
            matching: !negated,
        }),
        Expr::InList { expr, list, negated } => {
            if list.len() > MAX_PREDICATE_STEPS {
                return Err(QueryError::new(format!(
                    "a Metrics Insights query compares a label against as many as \
                     {MAX_PREDICATE_STEPS} values"
                )));
            }
            let mut values = Vec::with_capacity(list.len());
            for value in list {
                values.push(value_operand(value)?);
            }
            Ok(Term::Member {
                key: label_operand(expr)?,
                values,
                inside: !negated,
            })
        }
        other => Err(QueryError::new(format!(
            "{other} is not a predicate a Metrics Insights query selects by"
        ))),
    }
}

/// The label one side of a predicate names.
fn label_operand(expression: &Expr) -> ::std::result::Result<String, QueryError> {
    named_label(expression).ok_or_else(|| {
        QueryError::new("a Metrics Insights query selects by the labels of a series")
    })
}

/// The value one side of a predicate compares against.
fn value_operand(expression: &Expr) -> ::std::result::Result<String, QueryError> {
    match expression {
        Expr::Value(ValueWithSpan {
            value: Value::SingleQuotedString(text) | Value::DoubleQuotedString(text),
            ..
        }) => Ok(text.clone()),
        Expr::Value(ValueWithSpan {
            value: Value::Number(number, _),
            ..
        }) => Ok(number.clone()),
        other => Err(QueryError::new(format!(
            "{other} is not a label value a Metrics Insights query compares against"
        ))),
    }
}

/// The name an identifier states, however the statement quoted it: a namespace carries
/// characters a bare identifier cannot (`"AWS/EC2"`), so a quoted identifier and a quoted string
/// both name one.
fn named_label(expression: &Expr) -> Option<String> {
    match expression {
        Expr::Identifier(identifier) => Some(identifier.value.clone()),
        Expr::CompoundIdentifier(parts) => Some(
            parts
                .iter()
                .map(|part| part.value.clone())
                .collect::<Vec<String>>()
                .join("."),
        ),
        Expr::Value(ValueWithSpan {
            value: Value::SingleQuotedString(text) | Value::DoubleQuotedString(text),
            ..
        }) => Some(text.clone()),
        _ => None,
    }
}

/// A name as the statement spells it, with the quoting the grammar kept around it removed: an
/// identifier is rendered back with its own quotes, and what names a namespace or a label is the
/// text inside them.
fn trimmed_name(named: &str) -> String {
    named
        .trim()
        .trim_matches('"')
        .trim_matches('`')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::{matches_pattern, parse, Aggregate};

    /// The `LIKE` pattern match: `%` stands for any run of characters and every other character
    /// stands for itself, matched case-sensitively, and the walk is one pass over the pattern
    /// however the two carry the wildcard.
    #[test]
    fn a_like_pattern_stands_for_the_runs_its_wildcards_cover() {
        // (case, the value, the pattern, whether the pattern matches it)
        let rows: [(&str, &str, &str, bool); 14] = [
            ("a pattern with no wildcard", "api", "api", true),
            ("the same pattern against another value", "apix", "api", false),
            ("a prefix", "web-1", "web%", true),
            ("a prefix that does not open the value", "a-web-1", "web%", false),
            ("a suffix", "a-web", "%web", true),
            ("a suffix that does not close the value", "a-web-1", "%web", false),
            ("a run inside the value", "a-web-1", "%web%", true),
            ("two runs", "wxbyb", "w%b%b", true),
            ("two runs the value does not close", "wxbyb", "w%b%y", false),
            ("the whole value", "anything", "%", true),
            ("the empty value under the whole-value pattern", "", "%", true),
            ("an empty pattern against a value", "api", "", false),
            ("an empty pattern against the empty value", "", "", true),
            // A dimension value differs in case from another dimension value: the comparison
            // is the one a series' identity is compared by.
            ("a value differing only in case", "API", "api%", false),
        ];
        for (case, value, pattern, expected) in rows {
            let matched = matches_pattern(value, pattern);
            assert_eq!(
                matched, expected,
                "case {case:?}: expected {pattern:?} to match {value:?}: {expected}, got {matched}"
            );
        }
    }

    /// What the parser reads off a statement, and what it refuses: the clauses are the query
    /// language's, so a statement stating anything else is no query this service evaluates.
    #[test]
    fn a_statement_is_read_as_the_query_language_states_one() {
        let read = parse(
            "SELECT SUM(Latency) FROM SCHEMA(\"Custom/Insights\", Service) \
             WHERE Service = 'api' GROUP BY Service ORDER BY MAX() DESC LIMIT 3",
        )
        .expect("the query language states this statement");
        assert_eq!(
            (
                read.aggregate,
                read.namespace.as_str(),
                read.metric_name.as_str(),
                read.group_by.as_slice(),
                read.limit,
                read.groups_its_result(),
            ),
            (
                Aggregate::Sum,
                "Custom/Insights",
                "Latency",
                ["Service".to_string()].as_slice(),
                Some(3),
                true,
            ),
            "expected every clause of the statement to be read, got {read:?}"
        );
        assert!(
            read.schema
                .as_ref()
                .is_some_and(|schema| schema.len() == 1 && schema.contains("Service")),
            "expected the schema's one dimension key, got {read:?}"
        );

        // (case, a statement this service evaluates no query of)
        let refused: [(&str, &str); 7] = [
            ("text the grammar cannot read", "SELECT FROM WHERE"),
            ("a statement that is no query", "DELETE FROM \"Custom/Insights\""),
            ("a projection that is not an aggregate", "SELECT Latency FROM \"ns\""),
            ("a function no query selects", "SELECT MEDIAN(Latency) FROM \"ns\""),
            ("an aggregate of nothing", "SELECT SUM() FROM \"ns\""),
            ("two namespaces", "SELECT SUM(Latency) FROM \"a\", \"b\""),
            (
                "a predicate over something that is no label",
                "SELECT SUM(Latency) FROM \"ns\" WHERE 1 = 1",
            ),
        ];
        for (case, statement) in refused {
            let read = parse(statement);
            assert!(
                read.is_err(),
                "case {case:?}: expected the statement to be refused, got {read:?}"
            );
        }
    }

    /// A `WHERE` clause is bounded: a chain of connectives as long as the caller likes is refused
    /// rather than read, and a clause inside the bound is read and evaluated in one pass.
    #[test]
    fn a_where_clause_is_bounded_before_it_is_read() {
        let chain = |links: usize| {
            let predicates: Vec<String> = (0..links).map(|_| "Service = 'api'".to_string()).collect();
            format!(
                "SELECT SUM(Latency) FROM \"ns\" WHERE {}",
                predicates.join(" AND ")
            )
        };
        // (case, how many predicates the chain carries, whether the parser reads it)
        let rows: [(&str, usize, bool); 4] = [
            ("one predicate", 1, true),
            ("a short chain", 8, true),
            ("a chain at the ceiling", super::MAX_PREDICATE_STEPS, false),
            ("a chain far past it", super::MAX_PREDICATE_STEPS * 4, false),
        ];
        for (case, links, read) in rows {
            let parsed = parse(&chain(links));
            assert_eq!(
                parsed.is_ok(),
                read,
                "case {case:?}: expected the clause to be read: {read}, got {parsed:?}"
            );
        }

        // The clause a mixed chain states is the clause it means: `a AND b OR c` holds for a
        // series that satisfies `c` alone, and `a AND (b OR c)` does not.
        let series = |service: &str| super::MetricKey {
            namespace: "ns".to_string(),
            metric_name: "Latency".to_string(),
            dimensions: [("Service".to_string(), service.to_string())]
                .into_iter()
                .collect(),
        };
        // (case, the clause, the series it selects, the series it does not)
        let rows: [(&str, &str, &str, &str); 3] = [
            ("a conjunction", "Service = 'api'", "api", "web"),
            (
                "a disjunction",
                "Service = 'api' OR Service = 'web'",
                "web",
                "cache",
            ),
            (
                "a disjunction under a conjunction",
                "Service != 'cache' AND (Service = 'api' OR Service = 'web')",
                "api",
                "cache",
            ),
        ];
        for (case, clause, selected, rejected) in rows {
            let query = parse(&format!(
                "SELECT SUM(Latency) FROM \"ns\" WHERE {clause}"
            ))
            .unwrap_or_else(|error| panic!("case {case:?}: the clause must be read: {error:?}"));
            assert!(
                query.selects(&series(selected)),
                "case {case:?}: expected the clause to select {selected:?}"
            );
            assert!(
                !query.selects(&series(rejected)),
                "case {case:?}: expected the clause to reject {rejected:?}"
            );
        }
    }
}

