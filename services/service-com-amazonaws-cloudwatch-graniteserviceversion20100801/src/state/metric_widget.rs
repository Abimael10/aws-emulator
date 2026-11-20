// SPDX-License-Identifier: CC0-1.0

//! The metric widget definition `GetMetricWidgetImage` renders, read over this store.
//!
//! "A JSON string that defines the bitmap graph to be retrieved. The string includes the
//! metrics to include in the graph, statistics, annotations, title, axis limits, and so on."
//! (`GetMetricWidgetImageInput$MetricWidget`) — so a widget definition is a DOCUMENT, parsed
//! here with the maintained JSON parser, and what it names is resolved over the series this
//! store holds rather than drawn from anything in the request.
//!
//! The resolution is the SAME one `GetMetricData` answers from and a metric-math alarm is
//! graded by ([`super::metric_query`]): a widget's `metrics` array becomes the
//! [`MetricDataQuery`] array that resolution takes, so a graph and a read of the same metric
//! over the same window cannot disagree about what the series is. What is local to a widget is
//! the document's own vocabulary — the shorthand its `metrics` entries are written in, the
//! members that place the graph's window and axes, and the defaults it takes for the ones it
//! leaves out — and the bitmap the resolved series are drawn into ([`render`]).
//!
//! Every member this reading does not act on (`region`, `accountId`, `timezone`, `legend`,
//! `liveData`, `sparkline`, `labels`, `setPeriodToTimeRange`) is accepted and ignored: the store
//! holds one account's and Region's series, and the rest change how a console lays a graph out
//! rather than what the graph is of.

use ::serde_json::{Map, Value};

use super::metric_math::{MathSeries, MathValue};
use super::metric_query::{self, QueryRefusal, Resolution, StoreSeriesSource};
use super::ComAmazonawsCloudwatchGraniteserviceversion20100801State as State;
use crate::ops::types::{Dimension, Metric, MetricDataQuery, MetricStat};

/// The bitmap a resolved widget is drawn into: the graph the operation answers with.
pub mod render;

/// The request member a widget definition arrives on, as a refusal of something inside it
/// names it (`GetMetricWidgetImageInput$MetricWidget`).
pub const WIDGET_MEMBER: &str = "MetricWidget";

/// The member of the definition that names the metrics the graph is of.
const METRICS_MEMBER: &str = "metrics";

/// "Up to 100 KB uncompressed payload." (`GetMetricWidgetImage`) — the limit is on the CALL's
/// payload, so it is enforced on the request's own body before it is routed to the operation
/// (`crate::service`'s payload gate) rather than on the definition after it is read.
pub const MAX_PAYLOAD_BYTES: usize = 100 * 1024;

/// "As many as 100 metrics in the graph." (`GetMetricWidgetImage`)
pub const MAX_WIDGET_METRICS: usize = 100;

/// What CloudWatch refuses a `MetricWidget` that is not a JSON OBJECT with.
///
/// The sentence names the definition by the property the request reader reads it from rather
/// than by the request member's own casing, and says nothing about the document's contents: a
/// payload that is no JSON at all and one that is JSON but not an object are the same failure to
/// a caller, which is that the definition could not be read.
const BAD_JSON_CONTENT: &str = "MetricWidget property 'metricWidget' has a bad JSON content";

/// How a refusal states that the definition is missing a member the widget structure requires
/// of it: the condition the document itself failed, and the member that has to be there.
///
/// The widget document is validated as a DOCUMENT — the same vocabulary [`should_be`] states a
/// node's type in — so a member the structure requires is named as the property the document
/// should have, and nothing of the submitted document is quoted back. The node that failed is
/// the document's ROOT, which no pointer leads to, so the condition stands on its own rather
/// than after a path.
fn should_have(member: &str) -> String {
    format!("Should have required property '{member}'")
}

/// How a refusal states that the array at `pointer` holds fewer elements than the widget
/// structure requires there — "metrics — An array of metrics", of which a graph is of at least
/// one.
fn should_not_have_fewer_items(pointer: &str, fewest: usize) -> String {
    format!("{pointer} should NOT have fewer than {fewest} items")
}

/// The fewest elements the `metrics` array carries: a graph is of at least one metric.
const FEWEST_METRICS: usize = 1;

/// The graph's width in pixels when the definition names none: "width — The width of the
/// graph in pixels. The default is 600." (the metric widget structure reference
/// `GetMetricWidgetImageInput$MetricWidget` points at).
pub const DEFAULT_WIDTH: u32 = 600;

/// The graph's height in pixels when the definition names none: "height — The height of the
/// graph in pixels. The default is 395." (the same reference).
pub const DEFAULT_HEIGHT: u32 = 395;

/// The widest and tallest graph this emulator renders.
///
/// Not a published AWS limit: it is the bound on the ALLOCATION a caller can ask for here, so
/// a definition naming a larger graph is refused rather than served from a bitmap sized by the
/// request. Comfortably above the largest graph a dashboard widget is rendered at.
pub const MAX_IMAGE_SIDE: u32 = 2_000;

/// The granularity the graph's windows take when the definition names none: "period — The
/// default is 300." (the metric widget structure reference).
pub const DEFAULT_PERIOD: i32 = 300;

/// The statistic a metric of the graph is folded by when neither it nor the definition names
/// one: "stat — The default is Average." (the same reference).
pub const DEFAULT_STAT: &str = "Average";

/// How far back the graph's window reaches when the definition names no `start`: three hours,
/// the span the relative default `-PT3H` names (the same reference).
pub const DEFAULT_RANGE_SECS: f64 = 3.0 * 60.0 * 60.0;

/// The `view` values the metric widget structure documents ("view — The default is
/// timeSeries. Valid values: timeSeries | singleValue | gauge | bar | pie"), which is the set
/// a definition naming something else is refused against.
const VIEWS: [&str; 5] = ["timeSeries", "singleValue", "gauge", "bar", "pie"];

/// Which plot the resolved series are drawn as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    /// The value over time, as a line through the series' points — the default view, and the
    /// plot the views that are not a column chart are drawn as: a graph of the same resolved
    /// series is what every one of them reports.
    TimeSeries,
    /// The value over time, as one column per window (`view: bar`).
    Bar,
}

/// One widget definition, resolved against the clock: its window, its axes, and the queries
/// its `metrics` array names.
#[derive(Debug, Clone)]
pub struct MetricWidget {
    /// The text written over the graph: "title — The title to be displayed for the graph or
    /// number." (the metric widget structure reference), `None` when the definition names none.
    pub title: Option<String>,
    /// The graph's width in pixels.
    pub width: u32,
    /// The graph's height in pixels.
    pub height: u32,
    /// Which plot the series are drawn as.
    pub view: View,
    /// Whether the graph is drawn on the dark theme (`theme: dark`).
    pub dark: bool,
    /// The first instant of the graph's window, in epoch seconds.
    pub start: f64,
    /// The first instant after the graph's window, in epoch seconds.
    pub end: f64,
    /// The granularity the window is folded into.
    pub period: i32,
    /// The value the left axis starts at, when `yAxis.left.min` fixes it.
    pub y_min: Option<f64>,
    /// The value the left axis ends at, when `yAxis.left.max` fixes it.
    pub y_max: Option<f64>,
    /// The values the graph draws a horizontal annotation line at
    /// (`annotations.horizontal[].value`).
    pub annotations: Vec<f64>,
    /// The queries the `metrics` array names, in the order it lists them — the array the
    /// shared resolution takes.
    pub queries: Vec<MetricDataQuery>,
    /// How each of those queries is drawn, by position in [`MetricWidget::queries`].
    pub plots: Vec<Plot>,
}

/// How one query of the definition is drawn.
#[derive(Debug, Clone)]
pub struct Plot {
    /// The `Id` the query resolves under.
    pub id: String,
    /// The colour the series is drawn in: the one the entry's `color` option names, else the
    /// one the graph palette assigns it.
    pub color: [u8; 3],
    /// Whether the series is drawn at all ("visible — Set this to false to hide the metric
    /// from the graph", the metric widget structure reference), which is the same thing the
    /// resolution calls returning the query's data.
    pub visible: bool,
}

/// One series the graph draws: its label, its colour, and its points oldest first.
#[derive(Debug, Clone)]
pub struct PlottedSeries {
    /// The label the graph legends the series under.
    pub label: String,
    /// The colour it is drawn in.
    pub color: [u8; 3],
    /// Its `(epoch seconds, value)` points, oldest first.
    pub points: Vec<(f64, f64)>,
    /// Whether part of the window the graph asks for could not be loaded for this series: "If
    /// any metric on the graph could not load all the requested data points, an orange triangle
    /// with an exclamation point appears next to the graph legend."
    /// (`GetMetricWidgetImageInput$MetricWidget`) \u2014 so the shortfall is reported INSIDE the
    /// image, beside this series' legend entry, rather than as a refusal of the call.
    pub incomplete: bool,
}

/// The colours a graph assigns its series when their entries name none.
///
/// "color — The six-digit HTML hex color code to be used for this metric. … If you omit this
/// field, a color is assigned automatically." (the metric widget structure reference) — the
/// assignment cycles this palette in the order the `metrics` array lists its entries, so the
/// same definition always draws the same series in the same colour.
const PALETTE: [[u8; 3]; 10] = [
    [0x1f, 0x77, 0xb4],
    [0xff, 0x7f, 0x0e],
    [0x2c, 0xa0, 0x2c],
    [0xd6, 0x27, 0x28],
    [0x94, 0x67, 0xbd],
    [0x8c, 0x56, 0x4b],
    [0xe3, 0x77, 0xc2],
    [0x7f, 0x7f, 0x7f],
    [0xbc, 0xbd, 0x22],
    [0x17, 0xbe, 0xcf],
];

impl MetricWidget {
    /// The widget definition `document` names, resolved against the instant `now`, or the
    /// sentence refusing it.
    ///
    /// Every refusal names the member path inside the definition that has to change, in the
    /// sentences this service refuses a value in ([`super::value_is_invalid_message`] and the
    /// parameter sentences beside it), because that path is what locates the node a caller
    /// repairs. A node whose JSON TYPE is not the one the structure requires at its position is
    /// named by its pointer and the type it must have instead ([`should_be`]): for an array of
    /// arrays, WHERE the wrong-shaped node sits is the whole of what the caller repairs from.
    pub fn parse(document: &str, now: f64) -> Result<Self, String> {
        let widget = ::serde_json::from_str::<Value>(document)
            .ok()
            .and_then(|parsed| match parsed {
                Value::Object(members) => Some(members),
                _ => None,
            })
            .ok_or_else(|| BAD_JSON_CONTENT.to_string())?;

        // "title — The title to be displayed for the graph or number." (the metric widget
        // structure reference) — the text belongs to the graph, so it is drawn into it.
        let title = string_member(&widget, WIDGET_MEMBER, "title")?.map(String::from);
        let width = pixels(&widget, "width", DEFAULT_WIDTH)?;
        let height = pixels(&widget, "height", DEFAULT_HEIGHT)?;
        let view = view_of(&widget)?;
        // "theme — A value of light or dark" (the metric widget structure reference); light is
        // the theme a graph is drawn on when the definition names none.
        let dark = string_member(&widget, WIDGET_MEMBER, "theme")? == Some("dark");

        let period = period_of(&widget, WIDGET_MEMBER)?;
        let (start, end) = window_of(&widget, period, now)?;

        let stat = string_member(&widget, WIDGET_MEMBER, "stat")?
            .unwrap_or(DEFAULT_STAT)
            .to_string();
        let (y_min, y_max) = left_axis_of(&widget)?;
        let annotations = annotations_of(&widget)?;

        let entries = match widget.get(METRICS_MEMBER).filter(|value| !value.is_null()) {
            None => return Err(should_have(METRICS_MEMBER)),
            Some(value) => value.as_array().ok_or_else(|| {
                should_be(&pointer(&[METRICS_MEMBER]), JSON_ARRAY)
            })?,
        };
        if entries.len() < FEWEST_METRICS {
            return Err(should_not_have_fewer_items(
                &pointer(&[METRICS_MEMBER]),
                FEWEST_METRICS,
            ));
        }
        if entries.len() > MAX_WIDGET_METRICS {
            return Err(super::collection_too_large_message(
                &path(METRICS_MEMBER),
                MAX_WIDGET_METRICS,
            ));
        }

        let mut queries: Vec<MetricDataQuery> = Vec::with_capacity(entries.len());
        let mut plots: Vec<Plot> = Vec::with_capacity(entries.len());
        for (index, entry) in entries.iter().enumerate() {
            let (query, plot) = read_entry(entry, index, period, &stat)?;
            queries.push(query);
            plots.push(plot);
        }
        // The ids of the array are judged by the one rule every `MetricDataQuery` array is
        // judged by ([`super::metric_query::query_id_refusal`]): an expression refers to the
        // other entries BY id, so an array using one id twice names a value nothing can
        // resolve. The generated ids are unique by construction, so only ids the definition
        // supplies itself can collide.
        if let Some(message) = metric_query::query_id_refusal(&queries, &path(METRICS_MEMBER)) {
            return Err(message);
        }

        Ok(Self {
            title,
            width,
            height,
            view,
            dark,
            start,
            end,
            period,
            y_min,
            y_max,
            annotations,
            queries,
            plots,
        })
    }
}

/// The path a refusal names a member of the definition by: the request member, then the
/// member inside the document.
fn path(member: &str) -> String {
    format!("{WIDGET_MEMBER}.{member}")
}

/// The JSON pointer a refusal names a NODE of the definition by: the request member, then the
/// members and array positions that lead to the node, joined by `/` with each position written
/// as its ZERO-based index — `MetricWidget/metrics/0/0` for the first element of the first
/// `metrics` entry.
///
/// A node whose JSON TYPE is not the one the widget structure requires at that position is
/// refused by this pointer rather than by the member-path sentences beside it
/// ([`should_be`]): what such a refusal has to say is where in the submitted document the
/// wrong-shaped node sits, which no member path can state for an array of arrays.
fn pointer(segments: &[&str]) -> String {
    let mut pointer = WIDGET_MEMBER.to_string();
    for segment in segments {
        pointer.push('/');
        pointer.push_str(segment);
    }
    pointer
}

/// How a refusal states that the node at `pointer` is of the wrong JSON type: the type the
/// widget structure requires there, named as JSON names its types.
fn should_be(pointer: &str, json_type: &str) -> String {
    format!("{pointer} should be {json_type}")
}

/// The JSON type a `metrics` entry, and the `metrics` array itself, must have: "metrics — …
/// Each item in the array is itself an array that defines one metric or expression" (the metric
/// widget structure reference).
const JSON_ARRAY: &str = "array";

/// The JSON type the OPTIONS of an entry must have — the object the entry's last element is,
/// and the only element a one-element entry can carry.
const JSON_OBJECT: &str = "object";

/// The JSON type each element naming part of a metric must have: the namespace, the metric name
/// and the dimension name/value pairs are all names.
const JSON_STRING: &str = "string";

/// The string member `member` of the object at path `at` carries, `None` when it is left out,
/// or the refusal for a member that is not a string.
fn string_member<'a>(
    members: &'a Map<String, Value>,
    at: &str,
    member: &str,
) -> Result<Option<&'a str>, String> {
    match members.get(member) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.as_str())),
        Some(other) => Err(super::value_is_invalid_message(
            &other.to_string(),
            &format!("{at}.{member}"),
        )),
    }
}

/// The pixel size member `member` carries, else `default`.
///
/// A size is a whole number of pixels inside [`MAX_IMAGE_SIDE`]; anything else — a negative
/// number, a fraction, text, or a graph larger than this emulator renders — is refused rather
/// than clamped, so the caller is told which member to change.
fn pixels(widget: &Map<String, Value>, member: &str, default: u32) -> Result<u32, String> {
    let Some(value) = widget.get(member).filter(|value| !value.is_null()) else {
        return Ok(default);
    };
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let pixels = value
        .as_f64()
        .filter(|pixels| pixels.is_finite() && pixels.fract() == 0.0)
        .filter(|pixels| (1.0..=f64::from(MAX_IMAGE_SIDE)).contains(pixels))
        .map(|pixels| pixels as u32);
    pixels.ok_or_else(|| super::value_is_invalid_message(&value.to_string(), &path(member)))
}

/// Which plot the definition's `view` asks for.
///
/// The two plots this renderer draws are the line through the series' points and the column
/// per window; the other documented views report a graph of the same resolved series, so they
/// are drawn as that line. A value outside the documented set is refused against the set
/// itself ([`VIEWS`]), which is what a caller repairs the definition from.
fn view_of(widget: &Map<String, Value>) -> Result<View, String> {
    match string_member(widget, WIDGET_MEMBER, "view")? {
        None | Some("timeSeries") => Ok(View::TimeSeries),
        Some("bar") => Ok(View::Bar),
        Some(named) if VIEWS.contains(&named) => Ok(View::TimeSeries),
        Some(_) => Err(super::value_in_set_message(
            &path("view"),
            &VIEWS.map(String::from),
        )),
    }
}

/// The granularity the `period` of `members` names, else [`DEFAULT_PERIOD`].
///
/// `at` is the path of the object the period is a member of — the definition itself, or one
/// `metrics` entry's options — so the refusal names the member that has to change. The value
/// is judged by the one rule every period of this service is judged by
/// ([`super::period_error`]).
fn period_of(members: &Map<String, Value>, at: &str) -> Result<i32, String> {
    let Some(value) = members.get("period").filter(|value| !value.is_null()) else {
        return Ok(DEFAULT_PERIOD);
    };
    let period = value
        .as_i64()
        .and_then(|period| i32::try_from(period).ok())
        .ok_or_else(|| {
            super::value_is_invalid_message(&value.to_string(), &format!("{at}.period"))
        })?;
    match super::period_error(period) {
        Some(message) => Err(message),
        None => Ok(period),
    }
}

/// The graph's window: the `[start, end)` the definition names, aligned to the period's
/// boundaries.
///
/// "start — The start of the time range to use for each widget on the dashboard. … You can
/// specify start without specifying end to specify a relative time range that ends with the
/// current time" (the metric widget structure reference), so each of the two is either an
/// absolute ISO 8601 instant or a SIGNED ISO 8601 duration offset from now, and a definition
/// naming neither is answered for the last [`DEFAULT_RANGE_SECS`].
///
/// The window is then widened to the period's own boundaries, the same boundaries every read
/// of a series folds into ([`super::rounded_start`]): a graph is of whole periods, which is
/// also what makes two calls inside one period draw the same graph of the same store.
fn window_of(widget: &Map<String, Value>, period: i32, now: f64) -> Result<(f64, f64), String> {
    let end = match string_member(widget, WIDGET_MEMBER, "end")? {
        None => now,
        Some(value) => instant_named_by(value, now)
            .ok_or_else(|| super::value_is_invalid_message(value, &path("end")))?,
    };
    let start = match string_member(widget, WIDGET_MEMBER, "start")? {
        None => end - DEFAULT_RANGE_SECS,
        Some(value) => instant_named_by(value, now)
            .ok_or_else(|| super::value_is_invalid_message(value, &path("start")))?,
    };
    if end <= start {
        return Err(format!(
            "The parameter {} must name an instant later than {}.",
            path("end"),
            path("start")
        ));
    }
    let span = f64::from(period);
    Ok(((start / span).floor() * span, (end / span).ceil() * span))
}

/// The instant `value` names: an absolute ISO 8601 instant, or a signed ISO 8601 duration as
/// an offset from `now`. `None` for text naming neither.
///
/// Both forms are read through the ONE reading this workspace has of each — the maintained
/// duration grammar (`service_common::iso8601_span`) and the shared zone reader
/// (`service_common::rfc3339::split_utc_offset`) with the modelled timestamp type's own parse
/// — so no second date implementation lives here.
fn instant_named_by(value: &str, now: f64) -> Option<f64> {
    let (sign, unsigned) = match value.strip_prefix('-') {
        Some(unsigned) => (-1.0, unsigned),
        None => (1.0, value.strip_prefix('+').unwrap_or(value)),
    };
    if let Some(span) = ::service_common::iso8601_span::iso8601_span(unsigned) {
        return Some(now + sign * span.as_secs_f64());
    }
    let (civil, offset_seconds) = ::service_common::rfc3339::split_utc_offset(value)?;
    let civil = ::aws_smithy_types::DateTime::from_str(
        &format!("{civil}Z"),
        ::aws_smithy_types::date_time::Format::DateTime,
    )
    .ok()?;
    // The instant is the civil reading shifted BACK by the offset the text declares:
    // 10:00+02:00 is 08:00 UTC.
    #[allow(clippy::cast_precision_loss)]
    Some(civil.as_secs_f64() - offset_seconds as f64)
}

/// The bounds `yAxis.left` fixes, each `None` when the definition leaves it to the data.
///
/// "yAxis — … left and right each support min and max" (the metric widget structure
/// reference): an axis bound is a number, so anything else is refused for the member it
/// arrived on.
fn left_axis_of(widget: &Map<String, Value>) -> Result<(Option<f64>, Option<f64>), String> {
    let Some(axis) = widget.get("yAxis").filter(|value| !value.is_null()) else {
        return Ok((None, None));
    };
    let axis = axis
        .as_object()
        .ok_or_else(|| super::value_is_invalid_message(&axis.to_string(), &path("yAxis")))?;
    let Some(left) = axis.get("left").filter(|value| !value.is_null()) else {
        return Ok((None, None));
    };
    let left = left
        .as_object()
        .ok_or_else(|| super::value_is_invalid_message(&left.to_string(), &path("yAxis.left")))?;
    let bound = |member: &str| -> Result<Option<f64>, String> {
        match left.get(member).filter(|value| !value.is_null()) {
            None => Ok(None),
            Some(value) => value
                .as_f64()
                .filter(|number| number.is_finite())
                .map(Some)
                .ok_or_else(|| {
                    super::value_is_invalid_message(
                        &value.to_string(),
                        &path(&format!("yAxis.left.{member}")),
                    )
                }),
        }
    };
    Ok((bound("min")?, bound("max")?))
}

/// The values the definition's horizontal annotations are drawn at, in the order it lists
/// them.
///
/// "annotations — … horizontal is an array of horizontal annotations, each with a value" (the
/// metric widget structure reference). A vertical annotation marks an INSTANT rather than a
/// value, so it is not a line of the value axis and is ignored here.
fn annotations_of(widget: &Map<String, Value>) -> Result<Vec<f64>, String> {
    let Some(annotations) = widget.get("annotations").filter(|value| !value.is_null()) else {
        return Ok(Vec::new());
    };
    let annotations = annotations.as_object().ok_or_else(|| {
        super::value_is_invalid_message(&annotations.to_string(), &path("annotations"))
    })?;
    let Some(horizontal) = annotations
        .get("horizontal")
        .filter(|value| !value.is_null())
    else {
        return Ok(Vec::new());
    };
    let horizontal = horizontal.as_array().ok_or_else(|| {
        super::value_is_invalid_message(&horizontal.to_string(), &path("annotations.horizontal"))
    })?;
    let mut values = Vec::with_capacity(horizontal.len());
    for (index, annotation) in horizontal.iter().enumerate() {
        let at = super::list_element_path(&path("annotations.horizontal"), index + 1);
        let value = annotation
            .get("value")
            .and_then(Value::as_f64)
            .filter(|number| number.is_finite())
            .ok_or_else(|| super::parameter_required_message(&format!("{at}.value")))?;
        values.push(value);
    }
    Ok(values)
}

/// One `metrics` entry read as the query it names and the way it is drawn.
///
/// "metrics — … Each item in the array is itself an array that defines one metric or
/// expression" (the metric widget structure reference): an entry is the SHORTHAND for a
/// `MetricDataQuery`, written as the namespace, the metric name and the dimension name/value
/// pairs, optionally followed by an options object — or, for an expression, as that options
/// object alone. So this is the one place the shorthand becomes the modelled query the shared
/// resolution takes.
fn read_entry(
    entry: &Value,
    index: usize,
    widget_period: i32,
    widget_stat: &str,
) -> Result<(MetricDataQuery, Plot), String> {
    let at = super::list_element_path(&path(METRICS_MEMBER), index + 1);
    // The node this entry sits at inside the submitted document, for the refusals that state
    // the TYPE a node must have ([`pointer`]).
    let position = index.to_string();
    let node = pointer(&[METRICS_MEMBER, &position]);
    let elements = entry
        .as_array()
        .ok_or_else(|| should_be(&node, JSON_ARRAY))?;
    // The options object is the entry's LAST element when it has one; everything before it
    // names the metric.
    let (named, options) = match elements.split_last() {
        Some((Value::Object(options), named)) => (named, options.clone()),
        _ => (&elements[..], Map::new()),
    };
    let mut names: Vec<&str> = Vec::with_capacity(named.len());
    for (at_element, element) in named.iter().enumerate() {
        let name = element.as_str().ok_or_else(|| {
            should_be(
                &format!("{node}/{at_element}"),
                JSON_STRING,
            )
        })?;
        names.push(name);
    }

    let expression = string_member(&options, &at, "expression")?.map(String::from);
    // "visible — Set this to false to hide the metric from the graph", which is the same thing
    // the resolution calls returning a query's data.
    let visible = match options.get("visible").filter(|value| !value.is_null()) {
        None => true,
        Some(Value::Bool(visible)) => *visible,
        Some(other) => {
            return Err(super::value_is_invalid_message(
                &other.to_string(),
                &format!("{at}.visible"),
            ))
        }
    };
    // An entry naming no period of its own is folded at the definition's.
    let period = match options.get("period").filter(|value| !value.is_null()) {
        None => widget_period,
        Some(_) => period_of(&options, &at)?,
    };
    let label = string_member(&options, &at, "label")?.map(String::from);
    let color = match string_member(&options, &at, "color")? {
        None => PALETTE[index % PALETTE.len()],
        Some(text) => {
            rgb(text).ok_or_else(|| super::value_is_invalid_message(text, &format!("{at}.color")))?
        }
    };

    let id = match string_member(&options, &at, "id")? {
        Some(id) if super::is_metric_query_id(id) => id.to_string(),
        Some(_) => {
            return Err(super::value_pattern_message(
                &format!("{at}.id"),
                super::METRIC_QUERY_ID_PATTERN,
            ))
        }
        // The id a definition leaves out is generated from the entry's own position, which is
        // unique inside the array by construction and is the id an expression refers to the
        // entry by: `m` for a metric, `e` for an expression, as the console names them.
        None if expression.is_some() => format!("e{}", index + 1),
        None => format!("m{}", index + 1),
    };

    let metric_stat = if names.is_empty() {
        if expression.is_none() {
            return Err(super::parameter_required_message(&format!(
                "{at}.expression"
            )));
        }
        None
    } else if names.len() < METRIC_NAMES {
        // "a metric is uniquely defined by a name, a namespace, and zero or more dimensions", so
        // an entry carrying fewer names than those two names NO metric. The only other structure
        // an entry can have is the options object an expression is written as, so what the
        // document has at that position is a node that should have been that object.
        return Err(should_be(&format!("{node}/0"), JSON_OBJECT));
    } else {
        Some(metric_stat_of(&names, &at, period, &options, widget_stat)?)
    };

    Ok((
        MetricDataQuery {
            id: id.clone(),
            metric_stat,
            expression,
            label,
            return_data: Some(visible),
            // The granularity is on the query itself as well as on its `MetricStat`, because
            // an expression's constructed series is spaced at the query's own period.
            period: Some(period),
            account_id: None,
        },
        Plot {
            id,
            color,
            visible,
        },
    ))
}

/// How many names of an entry name the METRIC itself: "a metric is uniquely defined by a name,
/// a namespace, and zero or more dimensions" — the namespace and the metric name, with every
/// further pair of names a dimension.
const METRIC_NAMES: usize = 2;

/// The `MetricStat` the entry's names and options define.
///
/// "a metric is uniquely defined by a name, a namespace, and zero or more dimensions", so the
/// shorthand names the namespace, the metric name and then the dimensions as name/value pairs
/// — an entry naming half of one names no series at all.
fn metric_stat_of(
    names: &[&str],
    at: &str,
    period: i32,
    options: &Map<String, Value>,
    widget_stat: &str,
) -> Result<MetricStat, String> {
    let pairs = names.get(METRIC_NAMES..).unwrap_or_default();
    let (Some(namespace), Some(metric_name)) = (names.first(), names.get(1)) else {
        return Err(super::value_is_invalid_message(
            &Value::from(names.to_vec()).to_string(),
            at,
        ));
    };
    if !pairs.len().is_multiple_of(2) {
        return Err(super::value_is_invalid_message(
            &Value::from(names.to_vec()).to_string(),
            at,
        ));
    }
    let dimensions: Vec<Dimension> = pairs
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| Dimension {
            name: pair[0].to_string(),
            value: pair[1].to_string(),
        })
        .collect();
    if dimensions.len() > super::MAX_METRIC_DIMENSIONS {
        return Err(super::collection_too_large_message(
            at,
            super::MAX_METRIC_DIMENSIONS,
        ));
    }
    Ok(MetricStat {
        metric: Metric {
            namespace: Some((*namespace).to_string()),
            metric_name: Some((*metric_name).to_string()),
            dimensions: Some(dimensions),
        },
        period,
        // "stat — The statistic to use for this metric", else the definition's own `stat`,
        // else the default every graph of a series takes.
        stat: string_member(options, at, "stat")?
            .unwrap_or(widget_stat)
            .to_string(),
        unit: string_member(options, at, "unit")?.map(String::from),
    })
}

/// The colour an entry's `color` option names: "The six-digit HTML hex color code to be used
/// for this metric" (the metric widget structure reference), with or without the leading `#`.
///
/// The six digits are read by the maintained hex codec this crate already decodes its opaque
/// tokens with, so no second hex reading lives here.
fn rgb(text: &str) -> Option<[u8; 3]> {
    let digits = text.strip_prefix('#').unwrap_or(text);
    let decoded = ::hex::decode(digits).ok()?;
    <[u8; 3]>::try_from(decoded.as_slice()).ok()
}

/// Every series the widget draws, resolved over `store` at the instant `now`.
///
/// The resolution is the shared one ([`super::metric_query`]): each entry naming a metric is
/// folded out of the store by its statistic into the graph's period windows, and each entry
/// carrying an expression is evaluated over the values of the entries it refers to. So the
/// graph is drawn from the datapoints `PutMetricData` stored and from nothing else — a store
/// holding no datapoint in the window draws an empty graph rather than an invented one.
///
/// An entry the definition hides contributes no series to the graph while still resolving, so
/// an expression can read it.
///
/// A graph is never REFUSED for the width of the window it asks for. This operation models no
/// error at all and documents its own degradation instead — "If any metric on the graph could
/// not load all the requested data points, an orange triangle with an exclamation point appears
/// next to the graph legend." — so a window wider than this graph can be resolved over is
/// resolved over the part it CAN be ([`resolved_window`]) and the shortfall is reported inside
/// the image ([`PlottedSeries::incomplete`]). That is what makes a multi-year chart a graph of
/// the history there is rather than a 4xx: the per-call datapoint ceiling the metric-read
/// operations publish is theirs, not this one's.
pub fn plotted_series(
    store: &State,
    widget: &MetricWidget,
    now: f64,
) -> Result<Vec<PlottedSeries>, QueryRefusal> {
    let mut resolution = Resolution::default();
    // The queries whose series the graph could not load the whole requested window of.
    let mut incomplete: Vec<&str> = Vec::new();
    for query in &widget.queries {
        let Some(stat) = query.metric_stat.as_ref() else {
            continue;
        };
        let window = resolved_window(widget, query.period.unwrap_or(stat.period), now);
        if !window.whole {
            incomplete.push(query.id.as_str());
        }
        let series =
            metric_query::retrieved_series(store, query, stat, window.start, window.end, now)?;
        // The resolution's own retrieval ceiling is reached only by a store holding more points
        // in the window than one resolution reads. A graph does not refuse that either: the
        // series it could not take is the series whose data points did not all load.
        if resolution.retrieve(query, series).is_err() && !incomplete.contains(&query.id.as_str())
        {
            incomplete.push(query.id.as_str());
        }
    }

    // A Metrics Insights query is resolved over the store like a query that names a metric
    // ([`metric_query::insights_series`]) rather than parsed as a metric math expression, and
    // the graph this renderer DRAWS has no plot of such a query's result: a widget carrying one
    // is reported as a widget whose image this service does not render yet.
    if widget
        .queries
        .iter()
        .filter_map(|query| query.expression.as_deref())
        .any(super::metric_math::is_metrics_insights_query)
    {
        return Err(QueryRefusal::MetricsInsights);
    }

    let parsed = metric_query::parse_queries(&widget.queries)?;
    let order = metric_query::evaluation_order(&widget.queries, &parsed)?;
    for id in order {
        let Some(tree) = parsed.get(id) else {
            continue;
        };
        let Some(query) = widget.queries.iter().find(|query| query.id == *id) else {
            continue;
        };
        // An expression's own series is constructed over the window at ITS granularity, so the
        // window it is evaluated over is the one that granularity can be resolved over — which
        // is what bounds the points an expression of a wide window builds.
        let period = metric_query::query_period(query);
        let window = resolved_window(widget, period_of_seconds(period), now);
        if !window.whole && !incomplete.contains(&query.id.as_str()) {
            incomplete.push(query.id.as_str());
        }
        let evaluated = resolution.evaluate(
            id,
            tree,
            &StoreSeriesSource {
                store,
                // A search inside an expression discovers series over the same window the
                // expression is evaluated over.
                requested_start: window.start,
                end: window.end,
                now,
            },
            (window.start, window.end),
            period,
        )?;
        resolution.record(id, evaluated);
    }

    let mut plotted: Vec<PlottedSeries> = Vec::with_capacity(widget.plots.len());
    for (plot, query) in widget.plots.iter().zip(&widget.queries) {
        if !plot.visible {
            continue;
        }
        let short = incomplete.contains(&plot.id.as_str());
        let Some(value) = resolution.value(&plot.id) else {
            // A series the graph holds no value for at all is still ON the graph when what is
            // missing is its data: it is legended with the shortfall rather than left out.
            if short {
                plotted.push(PlottedSeries {
                    label: legend_label(query, plot),
                    color: plot.color,
                    points: Vec::new(),
                    incomplete: true,
                });
            }
            continue;
        };
        match value {
            MathValue::Series(series) => {
                plotted.push(drawn(plot, query.label.as_deref(), series, 0, short));
            }
            // A search answers with every series it matched, each drawn in its own colour.
            MathValue::Array(list) => {
                for (position, series) in list.iter().enumerate() {
                    plotted.push(drawn(plot, query.label.as_deref(), series, position, short));
                }
            }
            // A scalar holds across the whole window, which is how a graph draws one: a level
            // at that value from the start of the window to its end.
            MathValue::Scalar(number) => plotted.push(PlottedSeries {
                label: query.label.clone().unwrap_or_else(|| plot.id.clone()),
                color: plot.color,
                points: vec![(widget.start, *number), (widget.end, *number)],
                incomplete: short,
            }),
        }
    }
    Ok(plotted)
}

/// The window one query of a graph is RESOLVED over: the part of the graph's own window that
/// query can be resolved over at all.
///
/// Two bounds place it, and each bounds the WORK a definition can ask for rather than refusing
/// it:
///
/// * A datapoint is readable at a granularity only while the retention tier of that granularity
///   still holds it ([`super::retention_secs_at_period`]), so the window opens no earlier than
///   that tier's horizon: a graph of several years is drawn of the history there is.
/// * It then spans no more than [`metric_query::MAX_RETRIEVED_POINTS`] of the query's own period
///   windows — the data points one resolution reads at all — so the windows one query folds, and
///   the points an expression's constructed series is spaced into, are bounded by this emulator
///   rather than by the `end` the definition names, which a signed ISO 8601 offset can place
///   arbitrarily far in the future. The same kind of ceiling as the bitmap's own
///   ([`MAX_IMAGE_SIDE`]): what one caller may ask this process to allocate.
///
/// `whole` is whether that is the whole window the definition asked for. A query for which it is
/// not could not load all the requested data points, which the image says beside that query's
/// legend entry ([`PlottedSeries::incomplete`]).
fn resolved_window(widget: &MetricWidget, period: i32, now: f64) -> ResolvedWindow {
    let span = f64::from(period.max(1));
    let start = widget.start.max(now - super::retention_secs_at_period(period.max(1)));
    #[allow(clippy::cast_precision_loss)]
    let widest = span * metric_query::MAX_RETRIEVED_POINTS as f64;
    let end = widget.end.min(start + widest);
    ResolvedWindow {
        start,
        end,
        whole: start <= widget.start && end >= widget.end,
    }
}

/// The window one query of a graph is resolved over ([`resolved_window`]).
#[derive(Debug, Clone, Copy)]
struct ResolvedWindow {
    /// Its first instant, in epoch seconds.
    start: f64,
    /// The first instant after it, in epoch seconds.
    end: f64,
    /// Whether it is the whole window the definition asked for.
    whole: bool,
}

/// The granularity `seconds` names, as a retention tier is keyed by — a period no reading of this
/// store takes as a wider number than it can hold.
#[allow(clippy::cast_possible_truncation)]
fn period_of_seconds(seconds: f64) -> i32 {
    seconds.max(1.0).min(f64::from(i32::MAX)) as i32
}

/// The label a series with no value at all is legended under: the one the entry asks for, else
/// the metric it names, else the id it resolves under.
fn legend_label(query: &MetricDataQuery, plot: &Plot) -> String {
    query
        .label
        .clone()
        .or_else(|| {
            query
                .metric_stat
                .as_ref()
                .and_then(|stat| stat.metric.metric_name.clone())
        })
        .unwrap_or_else(|| plot.id.clone())
}

/// One resolved series as the graph draws it: the label the entry asks for, else the one the
/// resolution reports, and the entry's colour — cycled from the palette for the series after
/// the first of one entry, so the series of a search are told apart.
fn drawn(
    plot: &Plot,
    label: Option<&str>,
    series: &MathSeries,
    position: usize,
    incomplete: bool,
) -> PlottedSeries {
    PlottedSeries {
        label: label.unwrap_or(series.label.as_str()).to_string(),
        color: if position == 0 {
            plot.color
        } else {
            PALETTE[position % PALETTE.len()]
        },
        points: series.points.clone(),
        incomplete,
    }
}
