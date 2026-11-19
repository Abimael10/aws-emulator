// SPDX-License-Identifier: CC0-1.0

//! What an invocation PRINTS is counted by EVERY metric filter standing on the function's log
//! group — and deleting one of them stops that one counting, without stopping its siblings or
//! taking away what it already published.
//!
//! RULE: every invocation Lambda runs delivers that run's log to the function's own CloudWatch
//! Logs group — the group its `LoggingConfig$LogGroup` names, else the default
//! `/aws/lambda/<function name>` — as one event per record, and each delivered event is run
//! against the filters the group holds AT THAT INGEST: so each filter publishes one value per
//! record its own pattern matches into the metric its own transformation names, and a filter
//! taken off the group counts no record of a later run while the datapoints it published stay
//! readable and the filters still on the group go on counting.
//!
//! What the neighbours already pin, and this suite therefore does not restate:
//! [`crate::invocation_log_delivery`] pins WHAT one invocation leaves in the group over a table
//! of handlers and roles; [`crate::invocation_emf_metric_materialisation`] pins the metrics a
//! printed Embedded Metric Format document publishes by itself; the assembled-router suite over
//! the log-metric-filter chain pins that an invocation's printed lines reach the group inside
//! the runtime's framing and that a filter's metric reports their count, through to the alarm,
//! its SNS action and the subscribed queue; and the router suite over the extraction pins that a
//! `DeleteMetricFilter` stops extracting from later INGESTS while the sum it published stays
//! readable. What only this suite reaches is the COMPOSITION of those two: a group whose events
//! arrive from invocations rather than from a client's own `PutLogEvents`, carrying several
//! filters at once, one of which is deleted between runs — so the delete is shown to be scoped
//! to the one filter and the log write-through to be unaffected by it.
//!
//! Four services have to agree for that to hold — Lambda runs the handler and delivers the run's
//! log, CloudWatch Logs holds the group and extracts the filters' metrics, CloudWatch answers for
//! the series, and IAM holds the execution role those writes are made under — so the suite mounts
//! the four on ONE engine ([`crate::invocation_emf_metric_materialisation`] builds that emulator)
//! and asserts through their own APIs, never through a handler's Rust.
//!
//! The filters differ along what the rule generalises over: the FORM of the pattern (a JSON
//! pattern over a member of each printed document, an unstructured term over a line's text, and a
//! JSON pattern no printed line matches), how many of one run's lines each matches (2, 1, 0), and
//! whether it is the one deleted between the runs. Every expected count is computed from those.
//!
//! The test here RUNS handlers, so it takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mapping_role_access::{allow, given_role};
use crate::invocation_emf_metric_materialisation::{
    emulator, invoke_printing, messages, PRINTING_HANDLER,
};
// Now in the epoch seconds a stored datapoint is stamped with, which is what a read's window is
// stated in: the one writer of that clock this target has.
use crate::invocation_error_indicator::now_secs;
use crate::invocation_log_delivery::{
    create_function, logs_answer, Created, LOGS, LOG_WRITES, PYTHON_RUNTIME,
};
use crate::kinesis_event_source_delivery::cloudwatch;

/// The prefix of the log group of a function whose `LoggingConfig` names none: "By default,
/// Lambda functions send logs to a default log group named `/aws/lambda/<function name>`"
/// (`smithy_query(query="doc",shape="LoggingConfig",member="LogGroup")`).
const DEFAULT_GROUP_PREFIX: &str = "/aws/lambda/";

/// The fixed framing the runtime writes around every run's own output, and the fixed field
/// names of the closing report. Source: the records Lambda writes for one invocation — `START
/// RequestId: <id> Version: <version>`, `END RequestId: <id>`, and `REPORT RequestId: <id>`
/// carrying the run's duration and the function's memory as named fields.
const START_PREFIX: &str = "START RequestId: ";
const END_PREFIX: &str = "END RequestId: ";
const REPORT_PREFIX: &str = "REPORT RequestId: ";
const REPORT_FIELDS: &[&str] = &[
    "Duration:",
    "Billed Duration:",
    "Memory Size:",
    "Max Memory Used:",
];

/// The metric name every filter's transformation publishes under. Each filter publishes into a
/// NAMESPACE of its own, so the series stay apart whatever they are named.
const METRIC: &str = "MatchedRecords";

/// The `metricValue` every transformation publishes: one per matching record, which is what
/// makes each series a COUNT of the records the runs wrote.
const COUNT_PER_RECORD: &str = "1";

/// The `Period` every read below is answered at, in the seconds the member carries: one
/// minute, the resolution an extracted metric is stored at.
const PERIOD: i64 = 60;

/// How far either side of the read's instant the window reaches, in seconds. The records are
/// stamped with the run's own instant, so the window only has to cover the periods the runs
/// fall in with room to spare — nothing here asserts on elapsed time.
const WINDOW_SECONDS: f64 = 600.0;

/// How close a reported `Sum` has to be to the count computed for it. Every value a
/// transformation here publishes is [`COUNT_PER_RECORD`], so the total is a whole number and
/// this only absorbs the representation of a sum of integers.
const COUNT_TOLERANCE: f64 = 0.5;

/// The lines ONE invocation prints, in order: two documents a JSON pattern selects, one it does
/// not, and one unstructured line carrying a term.
const PRINTED: &[&str] = &[
    r#"{"level":"ERROR","stage":"charge","line":0}"#,
    r#"{"level":"ERROR","stage":"settle","line":1}"#,
    r#"{"level":"INFO","stage":"charge"}"#,
    "payment DECLINED for order 1",
];

/// How many invocations are made while EVERY filter stands. More than one, so a filter that
/// counted a line once per function rather than once per run is caught.
const RUNS_BEFORE_THE_DELETE: usize = 2;

/// One filter on the group: its pattern, where it publishes, how many of [`PRINTED`] it matches,
/// and whether it is the one deleted between the runs.
struct Filter {
    /// What this filter is called in every assertion about it.
    case: &'static str,
    /// Its `filterPattern`.
    pattern: &'static str,
    /// The namespace its transformation publishes into — one per filter, so no filter's series
    /// carries another's values.
    namespace: &'static str,
    /// How many of [`PRINTED`] its pattern matches, read off the pattern.
    matching: usize,
    /// Whether the suite takes it off the group after [`RUNS_BEFORE_THE_DELETE`] runs.
    deleted: bool,
}

/// The filters the group carries, one per form of pattern.
fn filters() -> Vec<Filter> {
    vec![
        Filter {
            case: "a JSON pattern over a member of each printed document, deleted between runs",
            pattern: r#"{ $.level = "ERROR" }"#,
            namespace: "Emulator/LambdaLogFilterJson",
            matching: 2,
            deleted: true,
        },
        Filter {
            case: "an unstructured term over a line's text, left on the group",
            pattern: "DECLINED",
            namespace: "Emulator/LambdaLogFilterTerm",
            matching: 1,
            deleted: false,
        },
        Filter {
            case: "a JSON pattern no printed line matches, left on the group",
            pattern: r#"{ $.level = "CRITICAL" }"#,
            namespace: "Emulator/LambdaLogFilterQuiet",
            matching: 0,
            deleted: false,
        },
    ]
}

/// The `Sum` CloudWatch reports for the series `namespace`/[`METRIC`] under NO dimensions —
/// which is the series a transformation declaring no dimension publishes into — totalled over
/// the periods around `at`, and how many datapoints carried it.
async fn summed(
    router: &MultiServiceRouter,
    case: &str,
    namespace: &str,
    at: f64,
) -> (f64, usize) {
    let (status, answered) = cloudwatch(
        router,
        "GetMetricStatistics",
        &json!({
            "Namespace": namespace,
            "MetricName": METRIC,
            "Dimensions": [],
            "StartTime": at - WINDOW_SECONDS,
            "EndTime": at + WINDOW_SECONDS,
            "Period": PERIOD,
            "Statistics": ["Sum"],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from GetMetricStatistics({namespace}/{METRIC}), got \
         {status} {answered}"
    );
    let datapoints = answered["Datapoints"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let sum = datapoints
        .iter()
        .map(|point| point["Sum"].as_f64().unwrap_or_default())
        .sum();
    (sum, datapoints.len())
}

/// The count `matching` records over `runs` runs comes to, as a `Sum` is reported.
fn expected_sum(matching: usize, runs: usize) -> f64 {
    f64::from(u32::try_from(matching * runs).expect("a handful of records per run"))
}

/// Run the function `name` once on `lines`, and hold it to having printed every one of them.
async fn printed_once(router: &MultiServiceRouter, case: &str, name: &str, lines: &[String]) {
    let payload = invoke_printing(router, case, name, lines).await;
    let expected = u64::try_from(lines.len()).expect("a run prints a handful of lines");
    assert_eq!(
        payload["printed"].as_u64(),
        Some(expected),
        "case {case:?}: expected the handler to report {expected} printed line(s), got {payload}"
    );
}

/// How many of `held` are one of the lines `printed`.
fn occurrences(held: &[String], printed: &[String]) -> usize {
    held.iter().filter(|record| printed.contains(record)).count()
}

/// How many of `held` open with `prefix`.
fn framed(held: &[String], prefix: &str) -> usize {
    held.iter()
        .filter(|record| record.starts_with(prefix))
        .count()
}

/// Each line an invocation prints reaches the function's own group inside the runtime's framing
/// and is counted by every filter whose pattern matches it; the filter taken off the group then
/// counts no further run and keeps what it published, while the filters left on it go on
/// counting.
#[tokio::test]
async fn printed_lines_are_counted_by_every_filter_until_one_is_deleted() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let case = "a group carrying a filter of each form";
    let router = emulator();
    let function = "log-filter-counter";
    let group = format!("{DEFAULT_GROUP_PREFIX}{function}");
    let lines: Vec<String> = PRINTED.iter().map(|line| (*line).to_string()).collect();
    let filters = filters();

    let role = given_role(
        &router,
        case,
        "log-filter-counter-role",
        Some(json!([allow(LOGS, &LOG_WRITES, "*")])),
    )
    .await;
    create_function(
        &router,
        &Created {
            case,
            name: function,
            source: PRINTING_HANDLER,
            runtime: PYTHON_RUNTIME,
            role: &role,
            log_group: None,
        },
    )
    .await;

    // A client that wants its filters in place before the first run creates the group itself:
    // `PutMetricFilter` names a group that has to be there.
    let (status, answered) = logs_answer(
        &router,
        case,
        "CreateLogGroup",
        &json!({ "logGroupName": group }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateLogGroup({group}), got {status} {answered}"
    );
    for filter in &filters {
        let (status, answered) = logs_answer(
            &router,
            filter.case,
            "PutMetricFilter",
            &json!({
                "logGroupName": group,
                "filterName": filter.namespace,
                "filterPattern": filter.pattern,
                "metricTransformations": [{
                    "metricName": METRIC,
                    "metricNamespace": filter.namespace,
                    "metricValue": COUNT_PER_RECORD,
                }],
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: expected HTTP 200 from PutMetricFilter, got {status} {answered}",
            filter.case
        );
    }

    // Nothing has run, so no filter's series holds anything: a datapoint found below can only
    // have come from a run's own delivered log.
    let at = now_secs();
    for filter in &filters {
        let (sum, points) = summed(&router, filter.case, filter.namespace, at).await;
        assert_eq!(
            points, 0,
            "case {:?}: expected no datapoint before the first invocation, got {points} carrying \
             {sum}",
            filter.case
        );
    }

    for _ in 0..RUNS_BEFORE_THE_DELETE {
        printed_once(&router, case, function, &lines).await;
    }

    // Every record of every run is in the group: each line the handler printed, inside the
    // runtime's own opening and closing framing.
    let held = messages(&router, case, &group).await;
    let printed = occurrences(&held, &lines);
    let expected_printed = RUNS_BEFORE_THE_DELETE * lines.len();
    assert_eq!(
        printed, expected_printed,
        "case {case:?}: expected {expected_printed} printed line(s) in {group:?}, got {printed} \
         of {held:?}"
    );
    for prefix in [START_PREFIX, END_PREFIX, REPORT_PREFIX] {
        let framing = framed(&held, prefix);
        assert_eq!(
            framing, RUNS_BEFORE_THE_DELETE,
            "case {case:?}: expected {RUNS_BEFORE_THE_DELETE} {prefix:?} record(s) in {group:?}, \
             got {framing} of {held:?}"
        );
    }
    let report = held
        .iter()
        .find(|record| record.starts_with(REPORT_PREFIX))
        .unwrap_or_else(|| panic!("case {case:?}: expected a report record in {held:?}"));
    for field in REPORT_FIELDS {
        assert!(
            report.contains(field),
            "case {case:?}: expected the report record to name {field:?}, got {report:?}"
        );
    }

    // ...and each record was offered to every filter, each of which counted the records its own
    // pattern matches and nothing else.
    let mut published: Vec<usize> = Vec::new();
    for filter in &filters {
        let expected = expected_sum(filter.matching, RUNS_BEFORE_THE_DELETE);
        let (sum, points) = summed(&router, filter.case, filter.namespace, at).await;
        assert!(
            (sum - expected).abs() < COUNT_TOLERANCE,
            "case {:?}: expected {expected} matching record(s) counted, got {sum}",
            filter.case
        );
        // A pattern that matches nothing publishes no datapoint AT ALL, rather than one carrying
        // zero.
        assert_eq!(
            points > 0,
            filter.matching > 0,
            "case {:?}: expected a datapoint: {}, got {points} carrying {sum}",
            filter.case,
            filter.matching > 0
        );
        published.push(points);
    }

    for filter in filters.iter().filter(|filter| filter.deleted) {
        let (status, answered) = logs_answer(
            &router,
            filter.case,
            "DeleteMetricFilter",
            &json!({ "logGroupName": group, "filterName": filter.namespace }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: expected HTTP 200 from DeleteMetricFilter, got {status} {answered}",
            filter.case
        );
    }

    printed_once(&router, case, function, &lines).await;

    // The run after the delete still writes its log through to the group...
    let after = messages(&router, case, &group).await;
    let printed_after = occurrences(&after, &lines);
    let expected_after = (RUNS_BEFORE_THE_DELETE + 1) * lines.len();
    assert_eq!(
        printed_after, expected_after,
        "case {case:?}: expected {expected_after} printed line(s) in {group:?} after the delete, \
         got {printed_after} of {after:?}"
    );
    // ...and that run was offered to the filters still on the group and to no other: the deleted
    // filter's series is exactly what it published before, over the same datapoints, and every
    // filter left on the group counted the run.
    for (filter, before) in filters.iter().zip(published) {
        let runs = if filter.deleted {
            RUNS_BEFORE_THE_DELETE
        } else {
            RUNS_BEFORE_THE_DELETE + 1
        };
        let expected = expected_sum(filter.matching, runs);
        let (sum, points) = summed(&router, filter.case, filter.namespace, at).await;
        assert!(
            (sum - expected).abs() < COUNT_TOLERANCE,
            "case {:?}: expected {expected} matching record(s) counted over {runs} run(s), got \
             {sum}",
            filter.case
        );
        if filter.deleted {
            assert_eq!(
                points, before,
                "case {:?}: expected the delete to leave the {before} datapoint(s) it published, \
                 got {points} carrying {sum}",
                filter.case
            );
        }
    }
}
