// SPDX-License-Identifier: CC0-1.0

//! The LINES an Amazon RDS engine writes, counted by a METRIC FILTER over the log group the
//! export delivers them to — the three services an observability pipeline over an engine log
//! needs.
//!
//! RULE under test: **a statement a client sends over a DB resource's engine endpoint is run
//! by that engine, and every message the engine writes at or above the severity its own
//! configuration logs at is an INGESTION into the CloudWatch Logs group
//! `EnableCloudwatchLogsExports` streams that log type to — so it is run against every metric
//! filter the group holds exactly as a client-put event is, and CloudWatch answers for the
//! series each matching line published: a filter selecting one severity counts the lines of
//! that severity and no other, and a message the engine sent the client without logging it
//! publishes nothing at all.**
//!
//! Only a cross-service suite can pin this: the lines are written by the ENGINE behind the
//! cluster's published endpoint (Amazon RDS), extracted on ingest by CloudWatch Logs, and
//! answered for by CloudWatch, over ONE engine assembled the way `core/router/src/lib.rs`
//! mounts them. The statements are driven over the PostgreSQL protocol itself, so what makes
//! the engine write is what a stock driver sends — a client's way of making an engine log a
//! line is the anonymous code block (`DO $$ BEGIN RAISE <level> '…'; END $$`), which is why
//! every severity below is raised by one.
//!
//! The sibling suite `rds_engine_log_export_to_cloudwatch_logs` pins WHICH group each
//! resource's lines reach and which severities the log carries; what only this suite reaches
//! is the extraction over those lines, so its harness — the cluster, the wire client — is
//! reused rather than copied.
//!
//! The rows differ along what the rule generalises over: the form of the filter pattern (a
//! severity label, a term of the message the block raised, a term that must NOT appear beside
//! one that must), which severity it selects (the two `ERROR`s, the `LOG`, the `WARNING`), the
//! form of the metric value (a literal 1, a fraction), whether the transformation declares a
//! unit, and the two patterns nothing the engine logged matches.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudwatch_alarm_action_reaches_its_subscribed_queue::{cw, router};
use crate::rds_engine_log_export_to_cloudwatch_logs::{
    available_cluster, filtered_events, logs, Session, BOOT_STACK_SIZE, DATABASE, MASTER_USER,
    POLL_INTERVAL, POSTGRESQL_LOG_TYPE, REGION,
};

/// The cluster whose engine writes every line below, and the log group its PostgreSQL log is
/// exported to.
const CLUSTER: &str = "obs-metric-filter-cluster";

/// The namespace every series below is published into, and the dimensionless series each row
/// reads.
const NAMESPACE: &str = "EmuRds/EngineLog";

/// The markers the blocks raise, one per severity: each is a bare term of the filter-pattern
/// syntax (its word characters are the alphanumerics, `_` and `-`), so a row can name one as
/// its whole pattern.
const LOG_MARKER: &str = "engine-log-marker";
const WARNING_MARKER: &str = "engine-warning-marker";
const NOTICE_MARKER: &str = "engine-notice-marker";
const EXCEPTION_MARKER: &str = "engine-exception-marker";

/// The relation the one failing query names, which no database here holds: the engine refuses
/// it, and the refusal is an `ERROR` line of its log carrying that name.
const MISSING_RELATION: &str = "a_relation_that_is_not_there";

/// How many lines the session below leaves in the group: the `LOG`, the `WARNING` and the two
/// `ERROR`s. The `NOTICE` is sent to the client and not logged, so it is not among them.
const LINES_LOGGED: usize = 4;

/// How many of those lines are `ERROR` lines.
const ERROR_LINES: usize = 2;

/// How long a read waits for the lines and the series they publish, and the window it reads
/// the series over. The budget is far longer than anything here takes, so a loaded host is
/// slower without the rule being any different, and nothing below asserts on elapsed time.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

/// The window a series is read over and the period it is read in: wide enough that every line
/// of the session falls inside it, and read as several periods whose datapoints are
/// aggregated, so a line straddling a period boundary is the same reading.
const WINDOW_SECONDS: f64 = 600.0;
const PERIOD_SECONDS: i64 = 300;

/// One metric filter stored over the group, and what CloudWatch must then answer for the
/// series it publishes into.
struct Row {
    /// What this row varies, named in every assertion.
    case: &'static str,
    /// Names this row's filter and its metric, so no two rows share a series.
    name: &'static str,
    /// The filter's `filterPattern`.
    pattern: &'static str,
    /// The transformation's `metricValue`.
    metric_value: &'static str,
    /// The transformation's `unit`, where the row declares one.
    unit: Option<&'static str>,
    /// The `(Sum, SampleCount)` over the whole window, or `None` for a series that must hold
    /// no datapoint at all.
    expected: Option<(f64, f64)>,
}

/// The rows, each a filter put over the ONE group before the engine writes anything — which is
/// what makes every line below an ingestion run against all of them.
const ROWS: &[Row] = &[
    Row {
        case: "the engine's ERROR lines, counted one each",
        name: "engine-errors",
        pattern: "ERROR",
        metric_value: "1",
        unit: Some("Count"),
        expected: Some((2.0, 2.0)),
    },
    Row {
        case: "the same ERROR lines, each publishing a fraction",
        name: "engine-errors-weighted",
        pattern: "ERROR",
        metric_value: "0.5",
        unit: None,
        expected: Some((1.0, 2.0)),
    },
    Row {
        case: "the ERROR lines that are not the refused query, by a term that must not appear",
        name: "engine-errors-except-the-query",
        pattern: "ERROR -a_relation_that_is_not_there",
        metric_value: "1",
        unit: None,
        expected: Some((1.0, 1.0)),
    },
    Row {
        case: "the LOG line a code block raised",
        name: "engine-log-lines",
        pattern: LOG_MARKER,
        metric_value: "1",
        unit: Some("Count"),
        expected: Some((1.0, 1.0)),
    },
    Row {
        case: "the WARNING line a code block raised",
        name: "engine-warning-lines",
        pattern: WARNING_MARKER,
        metric_value: "1",
        unit: None,
        expected: Some((1.0, 1.0)),
    },
    Row {
        case: "a message the engine sent the client and did not log",
        name: "engine-notice-lines",
        pattern: NOTICE_MARKER,
        metric_value: "1",
        unit: None,
        expected: None,
    },
    Row {
        case: "a severity the engine wrote nothing at",
        name: "engine-panic-lines",
        pattern: "PANIC",
        metric_value: "1",
        unit: None,
        expected: None,
    },
];

/// The series one row publishes into.
fn metric_of(row: &Row) -> String {
    format!("{}-count", row.name)
}

/// Wall-clock now as epoch seconds, the clock a series' window is measured against.
fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |since| since.as_secs_f64())
}

/// The `(Sum, SampleCount)` CloudWatch reports for `metric` over the window around `at`,
/// totalled over the periods it falls in, or `None` for a series it reports no datapoint for.
async fn reported(
    router: &MultiServiceRouter,
    case: &str,
    metric: &str,
    at: f64,
) -> Option<(f64, f64)> {
    let answered = cw(
        router,
        case,
        REGION,
        "GetMetricStatistics",
        &json!({
            "Namespace": NAMESPACE,
            "MetricName": metric,
            "StartTime": at - WINDOW_SECONDS,
            "EndTime": at + WINDOW_SECONDS,
            "Period": PERIOD_SECONDS,
            "Statistics": ["Sum", "SampleCount"],
        }),
    )
    .await;
    let points = answered["Datapoints"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if points.is_empty() {
        return None;
    }
    Some((
        points
            .iter()
            .filter_map(|point| point["Sum"].as_f64())
            .sum(),
        points
            .iter()
            .filter_map(|point| point["SampleCount"].as_f64())
            .sum(),
    ))
}

/// Store one row's metric filter over `group`.
async fn put_filter(router: &MultiServiceRouter, group: &str, row: &Row) {
    let mut transformation = json!({
        "metricName": metric_of(row),
        "metricNamespace": NAMESPACE,
        "metricValue": row.metric_value,
    });
    if let Some(unit) = row.unit {
        transformation["unit"] = json!(unit);
    }
    logs(
        router,
        "PutMetricFilter",
        json!({
            "logGroupName": group,
            "filterName": row.name,
            "filterPattern": row.pattern,
            "metricTransformations": [transformation],
        }),
        row.case,
    )
    .await;
}

/// Make the engine write one line at each severity, over one session on the cluster's own
/// endpoint, and answer with what the client was told about the two statements it refused.
///
/// Every line is produced the way a client produces one: an anonymous code block whose `RAISE`
/// writes at the level it names, plus a query the engine itself refuses.
async fn drive_the_engine(case: &str, address: &str, port: u16) {
    let mut session = Session::open(case, address, port).await;

    // A `LOG`: written to the server's log, and — at the default `client_min_messages` — not
    // sent to the client. This is the statement the whole pipeline hangs on: a server that
    // refuses the block writes no line, and the group stays empty.
    let said = session
        .query(case, &format!("DO $$ BEGIN RAISE LOG '{LOG_MARKER}'; END $$"))
        .await
        .unwrap_or_else(|refusal| {
            panic!("case {case:?}: a code block raising a LOG must be served, got {refusal:?}")
        });
    assert!(
        said.notices.is_empty(),
        "case {case:?}: a LOG message is not sent to the client at the default \
         client_min_messages, got {:?}",
        said.notices
    );

    // A `WARNING`: both logged and sent to the client.
    let said = session
        .query(
            case,
            &format!("DO $$ BEGIN RAISE WARNING '{WARNING_MARKER}'; END $$"),
        )
        .await
        .unwrap_or_else(|refusal| {
            panic!("case {case:?}: a code block raising a WARNING must be served, got {refusal:?}")
        });
    assert_eq!(
        said.notices
            .iter()
            .map(|notice| notice.message.as_str())
            .collect::<Vec<_>>(),
        vec![WARNING_MARKER],
        "case {case:?}: the WARNING must reach the client"
    );

    // A `NOTICE`: sent to the client and NOT logged, so nothing is delivered for it.
    let said = session
        .query(
            case,
            &format!("DO $$ BEGIN RAISE NOTICE '{NOTICE_MARKER}'; END $$"),
        )
        .await
        .unwrap_or_else(|refusal| {
            panic!("case {case:?}: a code block raising a NOTICE must be served, got {refusal:?}")
        });
    assert_eq!(
        said.notices
            .iter()
            .map(|notice| notice.message.as_str())
            .collect::<Vec<_>>(),
        vec![NOTICE_MARKER],
        "case {case:?}: the NOTICE must reach the client"
    );

    // The first `ERROR`: a query the engine refuses, whose own sentence is the line.
    let refusal = session
        .query(case, &format!("SELECT * FROM {MISSING_RELATION}"))
        .await
        .expect_err("a query over a relation that does not exist is refused");
    assert_eq!(
        refusal.code, "42P01",
        "case {case:?}: an undefined relation is refused under 42P01, got {refusal:?}"
    );
    assert!(
        refusal.message.contains(MISSING_RELATION),
        "case {case:?}: the refusal names the relation it could not resolve, got {refusal:?}"
    );

    // The second `ERROR`: a `RAISE EXCEPTION`, which aborts the block under `raise_exception`.
    let refusal = session
        .query(
            case,
            &format!("DO $$ BEGIN RAISE EXCEPTION '{EXCEPTION_MARKER}'; END $$"),
        )
        .await
        .expect_err("a code block that raises an exception is refused");
    assert_eq!(
        (refusal.code.as_str(), refusal.message.as_str()),
        ("P0001", EXCEPTION_MARKER),
        "case {case:?}: a RAISE EXCEPTION is reported under P0001 with its own sentence, got \
         {refusal:?}"
    );
}

/// Every line the engine wrote, once the group holds at least `at_least` of them or the
/// deadline passes: the delivery is made before the statement that wrote the line is answered,
/// so this is a look rather than a wait — repeated anyway, so a loaded machine cannot turn it
/// into a timing assertion.
async fn lines_in(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    at_least: usize,
) -> Vec<String> {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let held: Vec<String> = filtered_events(router, case, group, None)
            .await
            .into_iter()
            .map(|(_, message)| message)
            .collect();
        if held.len() >= at_least || Instant::now() >= deadline {
            return held;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The engine's own lines are counted by the metric filters over the group they are exported
/// to, each filter seeing exactly the lines its pattern selects.
#[test]
fn a_metric_filter_over_the_exported_engine_log_counts_the_lines_the_engine_wrote() {
    std::thread::Builder::new()
        .name("rds-engine-log-metric-filter".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let router = router();
            // `enable_all`, which is what the emulator's own boot path builds: a cluster
            // create opens the engine's listener through the core, and binding a socket needs
            // the runtime's I/O driver.
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("a current-thread runtime");
            runtime.block_on(async {
                let case = "the engine log of an exporting cluster";
                let (address, port) =
                    available_cluster(&router, case, CLUSTER, &[POSTGRESQL_LOG_TYPE]).await;
                let group = format!("/aws/rds/cluster/{CLUSTER}/{POSTGRESQL_LOG_TYPE}");

                // The group and its filters exist BEFORE the engine writes: extraction happens
                // on ingest, so a filter stored afterwards would see nothing the engine has
                // already written.
                logs(
                    &router,
                    "CreateLogGroup",
                    json!({ "logGroupName": group }),
                    case,
                )
                .await;
                for row in ROWS {
                    put_filter(&router, &group, row).await;
                }

                let at = now_secs();
                drive_the_engine(case, &address, port).await;

                // What the engine wrote is in the group: the four lines above, and no line for
                // the message it only sent the client.
                let lines = lines_in(&router, case, &group, LINES_LOGGED).await;
                assert_eq!(
                    lines.len(),
                    LINES_LOGGED,
                    "case {case:?}: the engine's lines must reach {group}, got {lines:?}"
                );
                assert!(
                    !lines.iter().any(|line| line.contains(NOTICE_MARKER)),
                    "case {case:?}: a message below the server's log floor is not delivered, \
                     got {lines:?}"
                );
                let errors = filtered_events(&router, case, &group, Some("ERROR")).await;
                assert_eq!(
                    errors.len(),
                    ERROR_LINES,
                    "case {case:?}: the group's ERROR lines are the statements the engine \
                     refused, got {errors:?}"
                );

                // The series the first row publishes into is read until it reports what those
                // lines published, which is what says every line has been ingested; the rest
                // are then read once, because one ingest extracted through all of the filters.
                let first = &ROWS[0];
                let metric = metric_of(first);
                let deadline = Instant::now() + SETTLE_DEADLINE;
                let mut settled = reported(&router, first.case, &metric, at).await;
                while settled != first.expected && Instant::now() < deadline {
                    tokio::time::sleep(POLL_INTERVAL).await;
                    settled = reported(&router, first.case, &metric, at).await;
                }
                assert_eq!(
                    settled, first.expected,
                    "case {:?}: the series {metric} must carry what the engine's lines \
                     published",
                    first.case
                );
                #[allow(clippy::cast_precision_loss)]
                let error_lines = errors.len() as f64;
                assert_eq!(
                    settled.map(|(_, samples)| samples),
                    Some(error_lines),
                    "case {:?}: the filter counts one sample per ERROR line the group holds",
                    first.case
                );

                for row in &ROWS[1..] {
                    let metric = metric_of(row);
                    let answered = reported(&router, row.case, &metric, at).await;
                    assert_eq!(
                        answered, row.expected,
                        "case {:?}: the series {metric} must report what the lines matching \
                         {:?} published, got {answered:?}",
                        row.case, row.pattern
                    );
                }

                // The lines carry the session the engine served them for — the user and the
                // database of the connection the statements were sent on — so what was counted
                // is this engine's own log and not a record written from anywhere else.
                let session_prefix = format!("{MASTER_USER}@{DATABASE}");
                for line in &lines {
                    assert!(
                        line.contains(&session_prefix),
                        "case {case:?}: the line {line:?} must name the session the engine \
                         wrote it for"
                    );
                }
            });
        })
        .expect("the suite's thread starts")
        .join()
        .expect("the suite's thread finishes");
}
