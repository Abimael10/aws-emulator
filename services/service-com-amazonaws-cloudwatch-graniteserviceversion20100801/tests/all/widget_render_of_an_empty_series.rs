// SPDX-License-Identifier: CC0-1.0

//! `GetMetricWidgetImage` over a series that was EMPTY when the graph was first rendered.
//!
//! The rule asserted here: **the image is a function of the widget document AND of the
//! datapoints the store holds inside the window that document names** — "A JSON string that
//! defines the bitmap graph to be retrieved. The string includes the metrics to include in the
//! graph, statistics, annotations, title, axis limits, and so on"
//! (`smithy_query(query="doc",shape="GetMetricWidgetImageInput",member="MetricWidget")`), and the
//! operation models no errors, so the bytes are the only observable. A graph of a metric nothing
//! has published is answered, and is answered again byte for byte while that stays true; the
//! first datapoint published into the window the document names makes the next render of the
//! IDENTICAL document answer different bytes, and a datapoint outside that window changes
//! nothing.
//!
//! This suite's rows start from the EMPTY series — the case a client meets when it graphs a
//! metric it is about to publish — over a FIXED absolute window, with the per-metric `stat` and
//! `period` the document carries them in; they differ along where the datapoint lands relative
//! to the window, and along the statistic the entry is folded by.
//!
//! The byte COUNTS are not the requirement and none is asserted: what is asserted is the
//! dependence.

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{build_router, now_secs, ok};

/// The metric every row graphs, in a namespace of its own per row so each row starts from a
/// series this store holds nothing of.
const METRIC: &str = "WidgetMetric";

/// The granularity the rows graph at, in the seconds `period` is counted in: the windows below
/// are measured in these seconds, and [`PERIOD_MEMBER`] is the same granularity in the whole
/// seconds a document's `period` member is written in.
const PERIOD: f64 = 60.0;

/// [`PERIOD`] as the document writes it.
const PERIOD_MEMBER: i64 = 60;

/// The first eight bytes of every PNG (the signature in the PNG specification).
const PNG_MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];

/// A period-aligned instant well inside retention at this granularity, from which each row
/// places its window and its datapoint.
fn base_time() -> f64 {
    (now_secs() / PERIOD).floor() * PERIOD - 30.0 * PERIOD
}

/// `secs` as the absolute ISO 8601 instant a document's `start`/`end` carries.
fn absolute(secs: f64) -> String {
    #[allow(clippy::cast_possible_truncation)]
    let millis = (secs * 1_000.0).round() as i64;
    aws_smithy_types::DateTime::from_millis(millis)
        .fmt(aws_smithy_types::date_time::Format::DateTime)
        .expect("an instant inside the timestamp type's range")
}

/// The document a row renders: one metric of `namespace`, folded by `stat` at [`PERIOD`], over
/// the fixed absolute window `[start, start + 5 periods)`.
fn document(namespace: &str, stat: &str, start: f64) -> Value {
    json!({
        "metrics": [[namespace, METRIC, {"stat": stat, "period": PERIOD_MEMBER}]],
        "start": absolute(start),
        "end": absolute(start + 5.0 * PERIOD),
        "width": 400,
        "height": 300,
        "view": "timeSeries",
    })
}

/// Render `document` and answer the PNG bytes a client receives.
async fn rendered(router: &MultiServiceRouter, case: &str, document: &Value) -> Vec<u8> {
    let body = ok(
        router,
        "GetMetricWidgetImage",
        &json!({"MetricWidget": document.to_string()}),
    )
    .await;
    let encoded = body["MetricWidgetImage"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the answer carries no MetricWidgetImage: {body}"));
    let image = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .unwrap_or_else(|error| {
            panic!("case {case:?}: MetricWidgetImage must be base64-encoded, got {error}")
        });
    assert_eq!(
        image.get(..PNG_MAGIC.len()),
        Some(&PNG_MAGIC[..]),
        "case {case:?}: expected the PNG signature, got {:?}",
        image.get(..PNG_MAGIC.len())
    );
    image
}

/// Publish one datapoint of [`METRIC`] into `namespace`.
async fn publish(router: &MultiServiceRouter, namespace: &str, at: f64, value: f64) {
    ok(
        router,
        "PutMetricData",
        &json!({
            "Namespace": namespace,
            "MetricData": [{
                "MetricName": METRIC,
                "Timestamp": at,
                "Value": value,
                "Unit": "Count",
            }],
        }),
    )
    .await;
}

/// One row: where the row's datapoint lands, and whether the next render must differ.
struct Row {
    /// What this row is, for every assertion message.
    case: &'static str,
    /// The statistic the document's metric entry is folded by.
    stat: &'static str,
    /// Where the datapoint is stamped, in periods from the window's start — so a value inside
    /// `[0, 5)` is inside the window the document names and anything else is outside it.
    at_periods: f64,
    /// The value published there.
    value: f64,
    /// Whether the render of the identical document must then answer different bytes.
    changes: bool,
}

#[tokio::test]
async fn a_graph_of_an_empty_series_re_renders_once_a_datapoint_lands_in_its_window() {
    let rows: &[Row] = &[
        Row {
            case: "the first datapoint of the series, in the window's first period",
            stat: "Sum",
            at_periods: 0.5,
            value: 500.0,
            changes: true,
        },
        Row {
            case: "the first datapoint of the series, in the window's last period",
            stat: "Sum",
            at_periods: 4.5,
            value: 42.0,
            changes: true,
        },
        Row {
            case: "the first datapoint of a series folded by another statistic",
            stat: "Maximum",
            at_periods: 2.5,
            value: 7.5,
            changes: true,
        },
        Row {
            case: "a datapoint after the window the graph is of",
            stat: "Sum",
            at_periods: 9.5,
            value: 500.0,
            changes: false,
        },
        Row {
            case: "a datapoint before the window the graph is of",
            stat: "Sum",
            at_periods: -9.5,
            value: 500.0,
            changes: false,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let router = build_router();
        // A namespace of this row's own, so the series the row graphs starts out empty.
        let namespace = format!("MyApp/EmptyWidget{index}");
        let start = base_time();
        let document = document(&namespace, row.stat, start);

        let empty = rendered(&router, case, &document).await;
        let again = rendered(&router, case, &document).await;
        assert_eq!(
            empty,
            again,
            "case {case:?}: the same document over an unchanged store must answer the same \
             bytes, got {} then {} bytes",
            empty.len(),
            again.len()
        );

        publish(
            &router,
            &namespace,
            start + row.at_periods * PERIOD,
            row.value,
        )
        .await;
        let fresher = rendered(&router, case, &document).await;
        assert_eq!(
            fresher != empty,
            row.changes,
            "case {case:?}: expected the datapoint to {} the graph, got {} bytes then {} bytes",
            if row.changes {
                "re-render"
            } else {
                "leave unchanged"
            },
            empty.len(),
            fresher.len()
        );
        // Whatever the datapoint did to the graph, the answer is stable again over the store it
        // now reads: a client may cache what it was given.
        let repeated = rendered(&router, case, &document).await;
        assert_eq!(
            repeated,
            fresher,
            "case {case:?}: a repeat render over the same store must answer the same bytes, got \
             {} then {} bytes",
            fresher.len(),
            repeated.len()
        );
    }
}
